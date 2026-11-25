//! Inline source-comment suppression (`clausura-ignore:`) and baseline
//! filtering (`.clausura-baseline.json`) for pragmatic incremental adoption.

use crate::rules::match_rule_pattern;
use crate::types::Finding;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Record of one finding suppressed by an inline `clausura-ignore:` directive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuppressedFinding {
    pub finding_id: uuid::Uuid,
    pub rule_id: String,
    pub file: String,
    pub line: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// Filter out findings suppressed by an inline `clausura-ignore:` directive on
/// the finding's `line_start` or immediately preceding line (`line_start - 1`).
///
/// Supported syntax (comment-prefix agnostic — works with `//`, `#`, `/*`,
/// `--`, `<!--`, etc.):
/// - `clausura-ignore: sql-injection`
/// - `clausura-ignore: sql-injection, xss -- sanitized upstream`
/// - `clausura-ignore: lsp-* -- generated code`
/// - `clausura-ignore: * -- test fixture`
pub fn apply_inline_suppressions(
    workspace: &Path,
    findings: Vec<Finding>,
) -> (Vec<Finding>, Vec<SuppressedFinding>) {
    let canonical_root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let mut file_cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
    let mut kept = Vec::with_capacity(findings.len());
    let mut suppressed = Vec::new();

    for finding in findings {
        let Some(ref loc) = finding.location else {
            kept.push(finding);
            continue;
        };
        if loc.line_start == 0 {
            kept.push(finding);
            continue;
        }

        let rel = loc.file.trim().trim_start_matches("./").replace('\\', "/");
        let lines_opt = file_cache.entry(rel.clone()).or_insert_with(|| {
            let full = canonical_root.join(&rel);
            let canon = full.canonicalize().ok()?;
            if !canon.starts_with(&canonical_root) {
                return None;
            }
            let content = fs::read_to_string(canon).ok()?;
            Some(content.lines().map(str::to_string).collect())
        });

        let Some(lines) = lines_opt else {
            kept.push(finding);
            continue;
        };

        let idx = (loc.line_start as usize).saturating_sub(1);
        let mut matched_reason: Option<String> = None;
        // Same line: either inline trailing comment or standalone comment.
        if let Some(line) = lines.get(idx) {
            if let Some((patterns, reason)) = parse_ignore_directive(line) {
                if patterns
                    .iter()
                    .any(|pat| match_rule_pattern(pat, &finding.rule_id))
                {
                    matched_reason = Some(reason);
                }
            }
        }
        // Previous line: only standalone comment lines suppress the next line
        // (a trailing comment on line N-1 applies strictly to line N-1).
        if matched_reason.is_none() && idx > 0 {
            if let Some(prev_line) = lines.get(idx - 1) {
                if is_standalone_ignore_comment(prev_line) {
                    if let Some((patterns, reason)) = parse_ignore_directive(prev_line) {
                        if patterns
                            .iter()
                            .any(|pat| match_rule_pattern(pat, &finding.rule_id))
                        {
                            matched_reason = Some(reason);
                        }
                    }
                }
            }
        }

        if let Some(reason) = matched_reason {
            suppressed.push(SuppressedFinding {
                finding_id: finding.id,
                rule_id: finding.rule_id,
                file: loc.file.clone(),
                line: loc.line_start,
                reason,
            });
        } else {
            kept.push(finding);
        }
    }

    (kept, suppressed)
}

/// True when `line` contains only comment syntax before `clausura-ignore:`,
/// meaning it is a standalone directive intended for the following line rather
/// than a trailing inline comment on an earlier statement.
fn is_standalone_ignore_comment(line: &str) -> bool {
    const MARKER: &str = "clausura-ignore:";
    let Some(pos) = line.find(MARKER) else {
        return false;
    };
    let prefix = line[..pos].trim();
    matches!(
        prefix,
        "//" | "///" | "#" | "/*" | "*" | "--" | "<!--" | ";"
    )
}

/// Extract `(rule_patterns, reason)` from a line containing `clausura-ignore:`.
fn parse_ignore_directive(line: &str) -> Option<(Vec<String>, String)> {
    const MARKER: &str = "clausura-ignore:";
    let pos = line.find(MARKER)?;
    let mut rest = line[pos + MARKER.len()..].trim();
    // Strip common trailing block-comment closers.
    for closer in ["*/", "-->"] {
        if let Some(stripped) = rest.strip_suffix(closer) {
            rest = stripped.trim();
        }
    }
    let (rules_part, reason) = match rest.split_once("--") {
        Some((r, why)) => (r.trim(), why.trim().to_string()),
        None => (rest, String::new()),
    };
    let patterns: Vec<String> = rules_part
        .split([',', ' '])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if patterns.is_empty() {
        return None;
    }
    Some((patterns, reason))
}

// ---------------------------------------------------------------------------
// Baseline filtering (`.clausura-baseline.json`)
// ---------------------------------------------------------------------------

/// Serialized baseline file storing fingerprints of accepted legacy findings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineFile {
    pub version: String,
    #[serde(default)]
    pub entries: Vec<BaselineEntry>,
}

/// One accepted finding fingerprint in a baseline file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineEntry {
    pub fingerprint: String,
    pub rule_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default)]
    pub message: String,
}

/// Stable, line-number-tolerant fingerprint for a finding based on
/// `rule_id`, normalized file path, and normalized evidence (falling back to
/// message when evidence is empty).
pub fn finding_fingerprint(finding: &Finding) -> String {
    let file = finding
        .location
        .as_ref()
        .map(|l| l.file.trim().trim_start_matches("./").replace('\\', "/"))
        .unwrap_or_default();
    let basis = if !finding.evidence.trim().is_empty() {
        finding.evidence.as_str()
    } else {
        finding.message.as_str()
    };
    let norm_basis: String = basis
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let input = format!("{}|{}|{}", finding.rule_id.trim(), file, norm_basis);

    // 64-bit FNV-1a (deterministic, zero extra dependencies).
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// Load a baseline file from disk.
pub fn load_baseline(path: &Path) -> Result<BaselineFile, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read baseline {}: {e}", path.display()))?;
    serde_json::from_str(&raw)
        .map_err(|e| format!("Failed to parse baseline {}: {e}", path.display()))
}

/// Write findings as a baseline JSON file.
pub fn write_baseline(path: &Path, findings: &[Finding]) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let baseline = BaselineFile {
        version: "1".into(),
        entries: findings
            .iter()
            .map(|f| BaselineEntry {
                fingerprint: finding_fingerprint(f),
                rule_id: f.rule_id.clone(),
                file: f.location.as_ref().map(|l| l.file.clone()),
                message: f.message.chars().take(200).collect(),
            })
            .collect(),
    };
    let content = serde_json::to_string_pretty(&baseline)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(path, content)
}

/// Filter out findings already accounted for in `baseline` (by multiset
/// fingerprint counts: if a fingerprint occurs N times in the baseline, at most
/// N occurrences are suppressed and any N+1th new occurrence is kept).
///
/// Returns `(remaining_findings, baselined_count)`.
pub fn filter_baseline(findings: Vec<Finding>, baseline: &BaselineFile) -> (Vec<Finding>, usize) {
    let mut budget: HashMap<&str, usize> = HashMap::new();
    for entry in &baseline.entries {
        *budget.entry(entry.fingerprint.as_str()).or_insert(0) += 1;
    }

    let mut remaining = Vec::with_capacity(findings.len());
    let mut suppressed = 0usize;
    for f in findings {
        let fp = finding_fingerprint(&f);
        if let Some(cnt) = budget.get_mut(fp.as_str()) {
            if *cnt > 0 {
                *cnt -= 1;
                suppressed += 1;
                continue;
            }
        }
        remaining.push(f);
    }
    (remaining, suppressed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Location, Severity};
    use tempfile::TempDir;
    use uuid::Uuid;

    fn mk_finding(rule_id: &str, file: &str, line: u32, evidence: &str) -> Finding {
        Finding {
            id: Uuid::new_v4(),
            rule_id: rule_id.into(),
            severity: Severity::Error,
            message: format!("issue {rule_id}"),
            location: Some(Location {
                file: file.into(),
                line_start: line,
                line_end: line,
                column_start: 1,
                column_end: 1,
            }),
            evidence: evidence.into(),
        }
    }

    #[test]
    fn test_inline_suppression_same_and_previous_line_and_wildcards() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("app.rs"),
            "\
// clausura-ignore: sql-injection -- parameterized via macro
let q = build_query(id);
let key = \"demo\"; // clausura-ignore: hardcoded-* -- fixture
let real = \"sk-live-secret\";
",
        )
        .unwrap();

        let findings = vec![
            mk_finding("sql-injection", "src/app.rs", 2, "build_query(id)"),
            mk_finding("hardcoded-secret", "src/app.rs", 3, "demo"),
            mk_finding("hardcoded-secret", "src/app.rs", 4, "sk-live-secret"),
        ];

        let (kept, suppressed) = apply_inline_suppressions(tmp.path(), findings);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].location.as_ref().unwrap().line_start, 4);
        assert_eq!(suppressed.len(), 2);
        assert_eq!(suppressed[0].reason, "parameterized via macro");
        assert_eq!(suppressed[1].reason, "fixture");
    }

    #[test]
    fn test_baseline_roundtrip_and_multiset_filtering() {
        let tmp = TempDir::new().unwrap();
        let baseline_path = tmp.path().join(".clausura-baseline.json");

        let legacy = mk_finding("sql-injection", "src/db.rs", 10, "SELECT * FROM t + id");
        write_baseline(&baseline_path, &[legacy]).unwrap();
        let loaded = load_baseline(&baseline_path).unwrap();

        // Line shifted from 10 -> 25 due to edits above; still recognized as legacy.
        let shifted_legacy = mk_finding("sql-injection", "./src/db.rs", 25, "SELECT * FROM t + id");
        // Second identical occurrence is NEW and must NOT be hidden.
        let new_same = mk_finding("sql-injection", "src/db.rs", 40, "SELECT * FROM t + id");
        let new_other = mk_finding("xss", "src/ui.ts", 5, "el.innerHTML = x");

        let (remaining, baselined) =
            filter_baseline(vec![shifted_legacy, new_same, new_other], &loaded);
        assert_eq!(baselined, 1);
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[0].location.as_ref().unwrap().line_start, 40);
        assert_eq!(remaining[1].rule_id, "xss");
    }
}
