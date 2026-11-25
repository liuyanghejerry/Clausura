// ---------------------------------------------------------------------------
// Deterministic Rule Engine for CI Gating
// ---------------------------------------------------------------------------
//
// Evaluates findings against gating rules using pure deterministic counting.
// No LLM calls, no heuristics — just rule_id matching, severity filtering,
// and count thresholds.
// ---------------------------------------------------------------------------

use crate::types::{Finding, GateAction, GateResult, GateRule, RuleViolation, Severity};

/// Match a gating rule pattern against a finding's `rule_id`.
///
/// Supports `*` wildcards (e.g. `"lsp-*"`, `"*secret*"`, `"*"`). Without `*`,
/// performs an exact string comparison.
pub fn match_rule_pattern(pattern: &str, rule_id: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == rule_id;
    }
    wildcard_match(pattern.as_bytes(), rule_id.as_bytes(), false)
}

/// Match a file path against a glob pattern supporting `**` (cross-directory),
/// `*` (single path segment), and `?` (single non-separator character).
pub fn match_glob_path(pattern: &str, path: &str) -> bool {
    let norm_pat = pattern.trim().trim_start_matches("./").replace('\\', "/");
    let norm_path = path.trim().trim_start_matches("./").replace('\\', "/");
    glob_match(norm_pat.as_bytes(), norm_path.as_bytes())
}

fn wildcard_match(pat: &[u8], text: &[u8], single_segment: bool) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let mut star_p: Option<usize> = None;
    let mut star_t: usize = 0;

    while t < text.len() {
        if p < pat.len()
            && (pat[p] == text[t] || (pat[p] == b'?' && (!single_segment || text[t] != b'/')))
        {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star_p = Some(p);
            star_t = t;
            p += 1;
        } else if let Some(sp) = star_p {
            if single_segment && text[star_t] == b'/' {
                return false;
            }
            star_t += 1;
            t = star_t;
            p = sp + 1;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

fn glob_match(pat: &[u8], text: &[u8]) -> bool {
    // Handle `**` via simple recursion over pattern/path slices (patterns are short).
    if pat.is_empty() {
        return text.is_empty();
    }
    if pat.starts_with(b"**") {
        // Consume consecutive `*` and optional following `/`.
        let mut rest = &pat[2..];
        while rest.starts_with(b"*") {
            rest = &rest[1..];
        }
        if rest.starts_with(b"/") {
            rest = &rest[1..];
        }
        if rest.is_empty() {
            return true;
        }
        // Try matching zero or more path prefixes.
        for i in 0..=text.len() {
            if (i == 0 || text[i - 1] == b'/') && glob_match(rest, &text[i..]) {
                return true;
            }
        }
        return false;
    }
    if let Some(slash_pos) = pat.iter().position(|&b| b == b'/') {
        let seg_pat = &pat[..slash_pos];
        let rest_pat = &pat[slash_pos + 1..];
        if let Some(text_slash) = text.iter().position(|&b| b == b'/') {
            return wildcard_match(seg_pat, &text[..text_slash], true)
                && glob_match(rest_pat, &text[text_slash + 1..]);
        }
        return false;
    }
    wildcard_match(pat, text, true)
}

fn finding_matches_rule_paths(finding: &Finding, rule: &GateRule) -> bool {
    let file = finding.location.as_ref().map(|l| l.file.as_str());
    if !rule.paths.is_empty() {
        let Some(f) = file else {
            return false;
        };
        if !rule.paths.iter().any(|p| match_glob_path(p, f)) {
            return false;
        }
    }
    if !rule.exclude_paths.is_empty() {
        if let Some(f) = file {
            if rule.exclude_paths.iter().any(|p| match_glob_path(p, f)) {
                return false;
            }
        }
    }
    true
}

/// Deterministic rule engine for CI gating decisions.
///
/// Rules are evaluated purely by counting findings that match:
/// 1. `rule_id` exact or `*`-wildcard match
/// 2. Optional `paths` / `exclude_paths` glob filters
/// 3. Severity >= `min_severity`
/// 4. Count > `max_findings` → apply `action`
pub struct RuleEngine;

impl RuleEngine {
    /// Evaluate findings against rules. Returns GateResult with deterministic exit code.
    pub fn evaluate(findings: &[Finding], rules: &[GateRule]) -> GateResult {
        let mut violations = Vec::new();
        let mut matched_finding_indices = std::collections::HashSet::new();

        for rule in rules {
            let mut count: u32 = 0;
            for (idx, f) in findings.iter().enumerate() {
                if !match_rule_pattern(&rule.rule_id, &f.rule_id) {
                    continue;
                }
                if !finding_matches_rule_paths(f, rule) {
                    continue;
                }
                matched_finding_indices.insert(idx);
                if f.severity >= rule.min_severity {
                    count += 1;
                }
            }

            if count > rule.max_findings {
                violations.push(RuleViolation {
                    rule_id: rule.rule_id.clone(),
                    description: rule.description.clone(),
                    actual_count: count,
                    max_allowed: rule.max_findings,
                    action: rule.action.clone(),
                });
            }
        }

        // Surface Warning/Error findings whose rule_id matched no rule at all
        // so rule_id typos do not silently pass CI.
        let mut ungated_high_severity_rules: Vec<String> = if rules.is_empty() {
            Vec::new()
        } else {
            findings
                .iter()
                .enumerate()
                .filter(|(idx, f)| {
                    !matched_finding_indices.contains(idx) && f.severity >= Severity::Warning
                })
                .map(|(_, f)| f.rule_id.clone())
                .collect()
        };
        ungated_high_severity_rules.sort();
        ungated_high_severity_rules.dedup();

        // Determine exit code
        let exit_code = if violations.is_empty() {
            0
        } else if violations.iter().any(|v| v.action == GateAction::Fail) {
            1
        } else {
            // Only Warn/Ignore violations — exit 0 (warnings are informational)
            0
        };

        GateResult {
            exit_code,
            violations,
            ungated_high_severity_rules,
        }
    }

    /// Convenience: returns true if the gate passes (exit_code == 0)
    pub fn is_pass(findings: &[Finding], rules: &[GateRule]) -> bool {
        Self::evaluate(findings, rules).exit_code == 0
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Finding, GateAction, GateRule, Severity};
    use uuid::Uuid;

    fn make_finding(rule_id: &str, severity: Severity) -> Finding {
        Finding {
            id: Uuid::new_v4(),
            rule_id: rule_id.to_string(),
            severity,
            message: "test".into(),
            location: None,
            evidence: "".into(),
        }
    }

    fn make_rule(
        rule_id: &str,
        description: &str,
        min_severity: Severity,
        max_findings: u32,
        action: GateAction,
    ) -> GateRule {
        GateRule {
            rule_id: rule_id.into(),
            description: description.into(),
            min_severity,
            max_findings,
            action,
            paths: vec![],
            exclude_paths: vec![],
        }
    }

    #[test]
    fn test_zero_findings_passes() {
        let result = RuleEngine::evaluate(
            &[],
            &[make_rule(
                "critical",
                "No critical errors",
                Severity::Error,
                0,
                GateAction::Fail,
            )],
        );
        assert_eq!(result.exit_code, 0);
        assert!(result.violations.is_empty());
    }

    #[test]
    fn test_over_limit_fails() {
        let findings = vec![make_finding("critical", Severity::Error)];
        let rules = vec![make_rule(
            "critical",
            "No critical errors",
            Severity::Error,
            0,
            GateAction::Fail,
        )];
        let result = RuleEngine::evaluate(&findings, &rules);
        assert_eq!(result.exit_code, 1);
        assert_eq!(result.violations.len(), 1);
        assert_eq!(result.violations[0].actual_count, 1);
        assert_eq!(result.violations[0].max_allowed, 0);
    }

    #[test]
    fn test_under_limit_passes() {
        let findings = vec![make_finding("warnings", Severity::Warning)];
        let rules = vec![make_rule(
            "warnings",
            "Max 5 warnings",
            Severity::Warning,
            5,
            GateAction::Warn,
        )];
        let result = RuleEngine::evaluate(&findings, &rules);
        assert_eq!(result.exit_code, 0);
        assert!(result.violations.is_empty());
    }

    #[test]
    fn test_multiple_rules_fail_dominates() {
        let findings = vec![
            make_finding("critical", Severity::Error),
            make_finding("warnings", Severity::Warning),
        ];
        let rules = vec![
            make_rule(
                "critical",
                "No critical",
                Severity::Error,
                0,
                GateAction::Fail,
            ),
            make_rule(
                "warnings",
                "Some warnings ok",
                Severity::Warning,
                5,
                GateAction::Warn,
            ),
        ];
        let result = RuleEngine::evaluate(&findings, &rules);
        assert_eq!(result.exit_code, 1); // Fail dominates
        assert_eq!(result.violations.len(), 1);
        assert_eq!(result.violations[0].rule_id, "critical");
    }

    #[test]
    fn test_empty_rules_pass() {
        let findings = vec![make_finding("anything", Severity::Error)];
        let result = RuleEngine::evaluate(&findings, &[]);
        assert_eq!(result.exit_code, 0);
        assert!(result.violations.is_empty());
    }

    #[test]
    fn test_severity_filtering() {
        // Finding is Info but rule requires Warning minimum
        let findings = vec![make_finding("test", Severity::Info)];
        let rules = vec![make_rule(
            "test",
            "Only Warning+ matters",
            Severity::Warning,
            0,
            GateAction::Fail,
        )];
        let result = RuleEngine::evaluate(&findings, &rules);
        assert_eq!(result.exit_code, 0); // Info < Warning, so no violation
    }

    #[test]
    fn test_mixed_severities_exceed_limit() {
        let findings = vec![
            make_finding("test", Severity::Error),
            make_finding("test", Severity::Warning),
            make_finding("test", Severity::Warning),
            make_finding("test", Severity::Info), // below Warning, filtered out
        ];
        let rules = vec![make_rule(
            "test",
            "Max 1 Error/Warning",
            Severity::Warning,
            1,
            GateAction::Fail,
        )];
        let result = RuleEngine::evaluate(&findings, &rules);
        assert_eq!(result.exit_code, 1);
        // Only Error + 2 Warnings = 3 findings >= Warning, max is 1
        assert_eq!(result.violations[0].actual_count, 3);
    }

    #[test]
    fn test_is_pass_convenience() {
        assert!(RuleEngine::is_pass(&[], &[]));
        assert!(!RuleEngine::is_pass(
            &[make_finding("critical", Severity::Error)],
            &[make_rule(
                "critical",
                "No critical",
                Severity::Error,
                0,
                GateAction::Fail,
            )],
        ));
    }

    #[test]
    fn test_wildcard_rule_matching_and_ungated_high_severity() {
        let findings = vec![
            make_finding("lsp-error", Severity::Error),
            make_finding("lsp-warning", Severity::Warning),
            make_finding("typo-rule", Severity::Error),
            make_finding("minor-note", Severity::Info),
        ];
        let rules = vec![make_rule(
            "lsp-*",
            "No LSP errors",
            Severity::Error,
            0,
            GateAction::Fail,
        )];
        let result = RuleEngine::evaluate(&findings, &rules);
        assert_eq!(result.exit_code, 1);
        assert_eq!(result.violations[0].actual_count, 1);
        assert_eq!(result.ungated_high_severity_rules, vec!["typo-rule"]);
    }

    #[test]
    fn test_path_and_exclude_path_filtering() {
        let mk_at = |file: &str| Finding {
            id: Uuid::new_v4(),
            rule_id: "panic-unwrap".into(),
            severity: Severity::Error,
            message: "unwrap".into(),
            location: Some(crate::types::Location {
                file: file.into(),
                line_start: 10,
                line_end: 10,
                column_start: 1,
                column_end: 1,
            }),
            evidence: String::new(),
        };
        let findings = vec![
            mk_at("src/api/handler.rs"),
            mk_at("tests/integration_test.rs"),
            mk_at("src/api/handler_test.rs"),
        ];
        let rules = vec![GateRule {
            rule_id: "panic-unwrap".into(),
            description: "No unwrap in src non-test files".into(),
            min_severity: Severity::Warning,
            max_findings: 0,
            action: GateAction::Fail,
            paths: vec!["src/**/*.rs".into()],
            exclude_paths: vec!["**/*_test.rs".into()],
        }];
        let result = RuleEngine::evaluate(&findings, &rules);
        assert_eq!(result.exit_code, 1);
        assert_eq!(result.violations[0].actual_count, 1);
    }
}
