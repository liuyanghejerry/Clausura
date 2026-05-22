// ---------------------------------------------------------------------------
// Deterministic Rule Engine for CI Gating
// ---------------------------------------------------------------------------
//
// Evaluates findings against gating rules using pure deterministic counting.
// No LLM calls, no heuristics — just rule_id matching, severity filtering,
// and count thresholds.
// ---------------------------------------------------------------------------

use crate::types::{Finding, GateAction, GateResult, GateRule, RuleViolation};

/// Deterministic rule engine for CI gating decisions.
///
/// Rules are evaluated purely by counting findings that match:
/// 1. rule_id pattern match
/// 2. Severity >= min_severity
/// 3. Count > max_findings → apply action
pub struct RuleEngine;

impl RuleEngine {
    /// Evaluate findings against rules. Returns GateResult with deterministic exit code.
    pub fn evaluate(findings: &[Finding], rules: &[GateRule]) -> GateResult {
        let mut violations = Vec::new();

        for rule in rules {
            // Filter findings by rule_id match
            let matching: Vec<&Finding> = findings
                .iter()
                .filter(|f| f.rule_id == rule.rule_id)
                .collect();

            // Filter by minimum severity
            let severe_enough: Vec<&Finding> = matching
                .into_iter()
                .filter(|f| f.severity >= rule.min_severity)
                .collect();

            let count = severe_enough.len() as u32;

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

    #[test]
    fn test_zero_findings_passes() {
        let result = RuleEngine::evaluate(
            &[],
            &[GateRule {
                rule_id: "critical".into(),
                description: "No critical errors".into(),
                min_severity: Severity::Error,
                max_findings: 0,
                action: GateAction::Fail,
            }],
        );
        assert_eq!(result.exit_code, 0);
        assert!(result.violations.is_empty());
    }

    #[test]
    fn test_over_limit_fails() {
        let findings = vec![make_finding("critical", Severity::Error)];
        let rules = vec![GateRule {
            rule_id: "critical".into(),
            description: "No critical errors".into(),
            min_severity: Severity::Error,
            max_findings: 0,
            action: GateAction::Fail,
        }];
        let result = RuleEngine::evaluate(&findings, &rules);
        assert_eq!(result.exit_code, 1);
        assert_eq!(result.violations.len(), 1);
        assert_eq!(result.violations[0].actual_count, 1);
        assert_eq!(result.violations[0].max_allowed, 0);
    }

    #[test]
    fn test_under_limit_passes() {
        let findings = vec![make_finding("warnings", Severity::Warning)];
        let rules = vec![GateRule {
            rule_id: "warnings".into(),
            description: "Max 5 warnings".into(),
            min_severity: Severity::Warning,
            max_findings: 5,
            action: GateAction::Warn,
        }];
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
            GateRule {
                rule_id: "critical".into(),
                description: "No critical".into(),
                min_severity: Severity::Error,
                max_findings: 0,
                action: GateAction::Fail,
            },
            GateRule {
                rule_id: "warnings".into(),
                description: "Some warnings ok".into(),
                min_severity: Severity::Warning,
                max_findings: 5,
                action: GateAction::Warn,
            },
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
        let rules = vec![GateRule {
            rule_id: "test".into(),
            description: "Only Warning+ matters".into(),
            min_severity: Severity::Warning,
            max_findings: 0,
            action: GateAction::Fail,
        }];
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
        let rules = vec![GateRule {
            rule_id: "test".into(),
            description: "Max 1 Error/Warning".into(),
            min_severity: Severity::Warning,
            max_findings: 1,
            action: GateAction::Fail,
        }];
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
            &[GateRule {
                rule_id: "critical".into(),
                description: "No critical".into(),
                min_severity: Severity::Error,
                max_findings: 0,
                action: GateAction::Fail,
            }],
        ));
    }
}
