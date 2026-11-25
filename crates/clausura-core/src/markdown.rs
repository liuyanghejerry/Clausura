use crate::types::{ExecutionReport, Finding, GateAction, RunStatus, Severity};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

/// Renders a developer-friendly Markdown review summary suitable for GitHub
/// Job Summaries (`$GITHUB_STEP_SUMMARY`), PR comments, and CI artifacts.
pub struct MarkdownFormatter;

/// Stable HTML comment marker used for idempotent PR comment updates.
pub const PR_COMMENT_MARKER: &str = "<!-- clausura-review -->";

impl MarkdownFormatter {
    /// Render an [`ExecutionReport`] to Markdown.
    pub fn render(report: &ExecutionReport, task_name: &str) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{PR_COMMENT_MARKER}");

        let (verdict_label, status_text) = match report.exit_code {
            0 => ("Passed", "complete"),
            1 => ("Gate violated", "gate_failed"),
            2 => match report.status {
                RunStatus::Incomplete => ("Review incomplete", "incomplete"),
                _ => ("Runtime error", "error"),
            },
            3 => ("Invalid configuration", "config_error"),
            _ => ("Execution failed", "error"),
        };

        let _ = writeln!(out, "### Clausura Review — {task_name}\n");
        let _ = writeln!(
            out,
            "**{verdict_label}** (exit code {}, status: `{}`).\n",
            report.exit_code,
            report
                .incomplete_reason
                .map(|r| format!("{status_text}: {}", r.as_str()))
                .unwrap_or_else(|| status_text.to_string())
        );

        let errors_n = count_severity(&report.findings, &Severity::Error);
        let warnings_n = count_severity(&report.findings, &Severity::Warning);
        let infos_n = count_severity(&report.findings, &Severity::Info);
        let hints_n = count_severity(&report.findings, &Severity::Hint);

        let _ = writeln!(out, "| Findings | Violations | Tokens | Duration |");
        let _ = writeln!(out, "|---:|---:|---:|---:|");
        let _ = writeln!(
            out,
            "| {} ({} error, {} warn, {} info, {} hint) | {} | {} | {:.1}s |\n",
            report.findings.len(),
            errors_n,
            warnings_n,
            infos_n,
            hints_n,
            report.violations.len(),
            report.token_usage.total_tokens,
            report.duration_ms as f64 / 1000.0,
        );

        if let Some(ref v) = report.verification {
            let _ = writeln!(
                out,
                "**Verification** (`{}` @ threshold {:.2}): {} verified, {} filtered, {} failed, {} skipped.\n",
                v.model, v.threshold, v.verified, v.filtered, v.failed, v.skipped
            );
        }

        if !report.violations.is_empty() {
            let _ = writeln!(out, "#### Gating Rule Violations\n");
            let _ = writeln!(out, "| Rule | Action | Found | Max Allowed | Description |");
            let _ = writeln!(out, "|---|---|---:|---:|---|");
            for v in &report.violations {
                let action = match v.action {
                    GateAction::Fail => "fail",
                    GateAction::Warn => "warn",
                    GateAction::Ignore => "ignore",
                };
                let _ = writeln!(
                    out,
                    "| `{}` | `{}` | {} | {} | {} |",
                    escape_table_cell(&v.rule_id),
                    action,
                    v.actual_count,
                    v.max_allowed,
                    escape_table_cell(&v.description),
                );
            }
            out.push('\n');
        }

        if !report.findings.is_empty() {
            let _ = writeln!(out, "#### Findings\n");
            for (sev, heading) in [
                (Severity::Error, "Errors"),
                (Severity::Warning, "Warnings"),
                (Severity::Info, "Info"),
                (Severity::Hint, "Hints"),
            ] {
                let group: Vec<&Finding> = report
                    .findings
                    .iter()
                    .filter(|f| f.severity == sev)
                    .collect();
                if group.is_empty() {
                    continue;
                }
                let _ = writeln!(out, "##### {heading} ({})\n", group.len());
                for f in group {
                    let loc = f
                        .location
                        .as_ref()
                        .map(|l| {
                            if l.line_start > 0 && l.line_end > l.line_start {
                                format!(" (`{}:{}-{}`)", l.file, l.line_start, l.line_end)
                            } else if l.line_start > 0 {
                                format!(" (`{}:{}`)", l.file, l.line_start)
                            } else {
                                format!(" (`{}`)", l.file)
                            }
                        })
                        .unwrap_or_default();
                    let _ = writeln!(out, "- **`{}`**{}: {}", f.rule_id, loc, f.message.trim());
                    let ev = f.evidence.trim();
                    if !ev.is_empty() {
                        let snippet: String = ev.chars().take(240).collect();
                        let _ = writeln!(out, "  > `{}`", snippet.replace('\n', " "));
                    }
                }
                out.push('\n');
            }
        }

        if !report.errors.is_empty() {
            let _ = writeln!(out, "#### Execution Errors\n");
            for err in &report.errors {
                let _ = writeln!(out, "- {err}");
            }
            out.push('\n');
        }

        out
    }

    /// Write the rendered Markdown report to `path`.
    pub fn write_to_file(
        report: &ExecutionReport,
        task_name: &str,
        path: &Path,
    ) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        fs::write(path, Self::render(report, task_name))
    }
}

fn count_severity(findings: &[Finding], target: &Severity) -> usize {
    findings.iter().filter(|f| &f.severity == target).count()
}

fn escape_table_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Location, RuleViolation, Usage};
    use uuid::Uuid;

    fn sample_report() -> ExecutionReport {
        ExecutionReport {
            task_id: "task-sec".into(),
            exit_code: 1,
            findings: vec![
                Finding {
                    id: Uuid::new_v4(),
                    rule_id: "sql-injection".into(),
                    severity: Severity::Error,
                    message: "Unsanitized SQL query".into(),
                    location: Some(Location {
                        file: "src/db.rs".into(),
                        line_start: 42,
                        line_end: 44,
                        column_start: 1,
                        column_end: 1,
                    }),
                    evidence: "format!(\"SELECT * FROM t WHERE id={}\", id)".into(),
                },
                Finding {
                    id: Uuid::new_v4(),
                    rule_id: "console-log".into(),
                    severity: Severity::Warning,
                    message: "Debug log left in handler".into(),
                    location: None,
                    evidence: String::new(),
                },
            ],
            token_usage: Usage {
                input_tokens: 1000,
                output_tokens: 250,
                total_tokens: 1250,
            },
            duration_ms: 4200,
            snapshot_id: None,
            errors: vec![],
            violations: vec![RuleViolation {
                rule_id: "sql-injection".into(),
                description: "Block SQL injection".into(),
                actual_count: 1,
                max_allowed: 0,
                action: GateAction::Fail,
            }],
            status: RunStatus::Complete,
            incomplete_reason: None,
            verification: None,
        }
    }

    #[test]
    fn test_markdown_render_contains_marker_verdict_violations_and_findings() {
        let md = MarkdownFormatter::render(&sample_report(), "security-review");
        assert!(md.starts_with(PR_COMMENT_MARKER));
        assert!(md.contains("Gate violated"));
        assert!(md.contains("exit code 1"));
        assert!(md.contains("Gating Rule Violations"));
        assert!(md.contains("`sql-injection`"));
        assert!(md.contains("`src/db.rs:42-44`"));
        assert!(md.contains("Unsanitized SQL query"));
        assert!(md.contains("Warnings (1)"));
        assert!(md.contains("1250"));
    }

    #[test]
    fn test_markdown_write_to_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("nested/report.md");
        MarkdownFormatter::write_to_file(&sample_report(), "sec", &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("Clausura Review — sec"));
    }
}
