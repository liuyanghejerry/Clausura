//! Deterministic risk scanner for diff-based review shards.
//!
//! Scans only the *added* lines of a unified diff for a small set of
//! high-signal, language-agnostic patterns (new routes, SQL, string-built
//! SQL, request-body entry points, filesystem access, auth checks, logging,
//! and process execution). The output feeds the shard manifest's
//! `RISK_TAGS` and candidate-hotspot list, so the LLM reviews pre-filtered
//! candidates instead of re-deriving them from raw diffs.
//!
//! Everything here is pure and regex-based by design: no AST, no language
//! servers, no LLM calls. For deeper semantic scanning, pair this with an
//! external scanner (Semgrep, CodeQL, LSP) via MCP preflight checks.

use regex_lite::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// One candidate hotspot found on an added diff line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RiskHit {
    /// Stable risk tag, e.g. `route`, `sql`, `filesystem`.
    pub tag: String,
    /// Workspace-relative file path the hit was found in.
    pub file: String,
    /// 1-based line number in the new file version.
    pub line: u32,
    /// Trimmed added line (without the leading `+`).
    pub snippet: String,
}

/// Built-in patterns: (tag, regex over one added line). Kept deliberately
/// conservative — a missed hotspot only costs the LLM a little attention,
/// while noisy tags drown the manifest.
const DEFAULT_PATTERNS: &[(&str, &str)] = &[
    // Router registrations with a literal path: app.get("/x"), router.post('/y')
    ("route", r#"\.(get|post|put|patch|delete|all)\(\s*["'/`]"#),
    // SQL statements (any occurrence — the reviewer decides if it's parameterized)
    (
        "sql",
        r#"(?i)\b(SELECT\s|INSERT\s+INTO|UPDATE\s+\S+\s+SET|DELETE\s+FROM|CREATE\s+TABLE|DROP\s+TABLE)"#,
    ),
    // SQL built with string interpolation/concatenation — the classic injection shape
    (
        "sql-interpolation",
        r#"(?i)\b(SELECT|INSERT|UPDATE|DELETE)\b[^\n]*(\$\{|\+|FORMAT!\(|%s|\.format\()"#,
    ),
    // Request-body entry points
    (
        "request-body",
        r"(request\.body|req\.body|ctx\.request\.body|bodyParser|@Body\(|\.body\s*=)",
    ),
    // Filesystem access
    (
        "filesystem",
        r"(fs\.(read|write|open|append|unlink|mkdir|rmdir|rm|createReadStream|createWriteStream)|std::fs::|File::(create|open)|os\.remove|with_open|OpenOptions)",
    ),
    // Auth checks and session handling
    (
        "auth",
        r"(?i)(requireAuth|checkAuth|verifyToken|isAuthenticated|canActivate|@UseGuards|authenticate|authorization|verify_session)",
    ),
    // Log output (leak surface for secrets and PII)
    (
        "log",
        r"(console\.(log|error|warn|info)|logger?\.(info|warn|error|debug)|println!|eprintln!|System\.out|log\.(Info|Warning|Error|Debug))",
    ),
    // Process execution / dynamic evaluation
    (
        "exec",
        r"(child_process|subprocess|ProcessBuilder|os\.system|popen|spawn\(|exec\(|eval\(|new Function\()",
    ),
    // Crypto misuse surface
    (
        "crypto",
        r#"(?i)\b(md5|sha1)\b|Math\.random\(\)|insecure|ECB\b"#,
    ),
];

fn compiled_defaults() -> &'static Vec<(&'static str, Regex)> {
    static COMPILED: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        DEFAULT_PATTERNS
            .iter()
            .filter_map(|(tag, pat)| Regex::new(pat).ok().map(|re| (*tag, re)))
            .collect()
    })
}

/// Scan one file's unified diff for risk hits on added lines only.
///
/// `extra_patterns` are user-supplied `(tag, regex)` pairs (from
/// `sharding.risk_patterns`) compiled per call and matched in addition to
/// the built-ins. Invalid user regexes are silently skipped — the scan is
/// advisory, never a hard dependency.
pub fn scan_diff(file: &str, diff: &str, extra_patterns: &[(String, String)]) -> Vec<RiskHit> {
    let extra: Vec<(String, Regex)> = extra_patterns
        .iter()
        .filter_map(|(tag, pat)| Regex::new(pat).ok().map(|re| (tag.clone(), re)))
        .collect();

    let mut hits = Vec::new();
    let mut new_line: u32 = 0;
    let mut in_hunk = false;

    for raw in diff.lines() {
        if raw.starts_with("@@") {
            new_line = parse_new_start(raw).unwrap_or(0);
            in_hunk = new_line > 0;
            continue;
        }
        if !in_hunk {
            continue;
        }
        if let Some(added) = raw.strip_prefix('+') {
            let trimmed = added.trim();
            if !trimmed.is_empty() {
                for (tag, re) in compiled_defaults() {
                    if re.is_match(trimmed) {
                        hits.push(RiskHit {
                            tag: (*tag).to_string(),
                            file: file.to_string(),
                            line: new_line,
                            snippet: truncate_snippet(trimmed),
                        });
                    }
                }
                for (tag, re) in &extra {
                    if re.is_match(trimmed) {
                        hits.push(RiskHit {
                            tag: tag.clone(),
                            file: file.to_string(),
                            line: new_line,
                            snippet: truncate_snippet(trimmed),
                        });
                    }
                }
            }
            new_line += 1;
        } else if raw.starts_with(' ') || raw.is_empty() {
            new_line += 1;
        } else if raw.starts_with('\\') {
            // "\ No newline at end of file" — not a content line.
        }
        // '-' lines belong to the old version and do not advance new_line.
    }
    hits
}

/// Scan many `(file, diff)` pairs at once.
pub fn scan_files(files: &[(&str, &str)], extra_patterns: &[(String, String)]) -> Vec<RiskHit> {
    let mut all = Vec::new();
    for (file, diff) in files {
        all.extend(scan_diff(file, diff, extra_patterns));
    }
    all
}

/// Comma-joined, deduplicated, sorted tags for one file — the manifest's
/// `RISK_TAGS` field.
pub fn tags_for_file(hits: &[RiskHit], file: &str) -> String {
    let mut tags: Vec<&str> = hits
        .iter()
        .filter(|h| h.file == file)
        .map(|h| h.tag.as_str())
        .collect();
    tags.sort_unstable();
    tags.dedup();
    tags.join(", ")
}

/// Extract the new-file start line from a hunk header like
/// `@@ -10,7 +12,9 @@ optional context`.
fn parse_new_start(header: &str) -> Option<u32> {
    let token = header
        .split_whitespace()
        .find(|t| t.starts_with('+') && t.len() > 1)?;
    let start = token[1..].split(',').next()?;
    start.parse().ok()
}

/// Keep snippets to one display line.
fn truncate_snippet(line: &str) -> String {
    const LIMIT: usize = 160;
    line.chars().take(LIMIT).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff_with(lines: &[&str]) -> String {
        let mut out = String::from("@@ -1,3 +1,3 @@\n");
        for l in lines {
            out.push_str(l);
            out.push('\n');
        }
        out
    }

    #[test]
    fn test_parse_new_start() {
        assert_eq!(parse_new_start("@@ -10,7 +12,9 @@ fn main()"), Some(12));
        assert_eq!(parse_new_start("@@ -1 +1 @@"), Some(1));
        assert_eq!(parse_new_start("@@ -10,7 @@ fn x"), None);
    }

    #[test]
    fn test_scan_detects_route() {
        let diff = diff_with(&["+app.get(\"/users\", handler)", "+const x = 1;"]);
        let hits = scan_diff("src/app.ts", &diff, &[]);
        assert_eq!(hits.len(), 1, "got: {hits:?}");
        assert_eq!(hits[0].tag, "route");
        assert_eq!(hits[0].line, 1);
        assert!(hits[0].snippet.contains("/users"));
    }

    #[test]
    fn test_scan_detects_sql_interpolation() {
        let diff = diff_with(&["+const q = \"SELECT * FROM t WHERE id = \" + req.params.id;"]);
        let hits = scan_diff("src/db.ts", &diff, &[]);
        let tags: Vec<&str> = hits.iter().map(|h| h.tag.as_str()).collect();
        assert!(tags.contains(&"sql"), "got: {tags:?}");
        assert!(tags.contains(&"sql-interpolation"), "got: {tags:?}");
    }

    #[test]
    fn test_scan_detects_filesystem_auth_log_exec() {
        let diff = diff_with(&[
            "+fs.writeFile(path, data, cb);",
            "+if (!verifyToken(token)) return 401;",
            "+console.log(\"user data\", user);",
            "+const out = exec(cmd);",
        ]);
        let hits = scan_diff("src/s.ts", &diff, &[]);
        let tags: Vec<&str> = hits.iter().map(|h| h.tag.as_str()).collect();
        assert!(tags.contains(&"filesystem"), "got: {tags:?}");
        assert!(tags.contains(&"auth"), "got: {tags:?}");
        assert!(tags.contains(&"log"), "got: {tags:?}");
        assert!(tags.contains(&"exec"), "got: {tags:?}");
        // Line numbers advance per added line.
        assert_eq!(hits[0].line, 1);
        assert_eq!(hits[1].line, 2);
    }

    #[test]
    fn test_scan_ignores_context_and_deleted_lines() {
        let diff = "@@ -5,3 +5,3 @@\n context line fs.writeFile(a,b)\n-deleted exec(cmd)\n+const ok = 1;\n";
        let hits = scan_diff("src/a.ts", diff, &[]);
        assert!(
            hits.is_empty(),
            "context/deleted lines must not match: {hits:?}"
        );
    }

    #[test]
    fn test_scan_handles_multiple_hunks() {
        let diff =
            "@@ -10,3 +10,3 @@\n+app.post(\"/a\", h);\n@@ -100,3 +200,3 @@\n+fs.unlink(p);\n";
        let hits = scan_diff("src/m.ts", diff, &[]);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].line, 10);
        assert_eq!(hits[1].line, 200, "second hunk must restart numbering");
    }

    #[test]
    fn test_scan_extra_patterns() {
        let diff = diff_with(&["+danger_zone();"]);
        let hits = scan_diff(
            "src/x.ts",
            &diff,
            &[("danger".to_string(), r"danger_zone\(".to_string())],
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].tag, "danger");
    }

    #[test]
    fn test_scan_invalid_extra_regex_skipped() {
        let diff = diff_with(&["+danger_zone();"]);
        let hits = scan_diff(
            "src/x.ts",
            &diff,
            &[("bad".to_string(), "(invalid[".to_string())],
        );
        assert!(hits.is_empty());
    }

    #[test]
    fn test_scan_files_aggregates() {
        let hits = scan_files(
            &[
                ("a.ts", diff_with(&["+app.get(\"/\", h);"]).as_str()),
                ("b.ts", diff_with(&["+fs.open(p);"]).as_str()),
            ],
            &[],
        );
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().any(|h| h.file == "a.ts" && h.tag == "route"));
        assert!(hits
            .iter()
            .any(|h| h.file == "b.ts" && h.tag == "filesystem"));
    }

    #[test]
    fn test_tags_for_file_dedup_sorted() {
        let mk = |tag: &str, file: &str| RiskHit {
            tag: tag.to_string(),
            file: file.to_string(),
            line: 1,
            snippet: "s".into(),
        };
        let hits = vec![
            mk("sql", "a.ts"),
            mk("route", "a.ts"),
            mk("sql", "a.ts"),
            mk("log", "b.ts"),
        ];
        assert_eq!(tags_for_file(&hits, "a.ts"), "route, sql");
        assert_eq!(tags_for_file(&hits, "b.ts"), "log");
        assert_eq!(tags_for_file(&hits, "c.ts"), "");
    }

    #[test]
    fn test_long_snippet_truncated() {
        let long = format!("+app.get(\"/x\", h); // {}", "c".repeat(500));
        let diff = diff_with(&[long.as_str()]);
        let hits = scan_diff("a.ts", &diff, &[]);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.chars().count() <= 160);
    }
}
