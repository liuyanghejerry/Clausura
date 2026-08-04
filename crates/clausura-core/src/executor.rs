use crate::agent::{run_agent_loop, AgentConfig};
use crate::checkpoint::CheckpointStore;
use crate::config::Config;
use crate::provider::create_provider;
use crate::rules::RuleEngine;
use crate::sarif::SarifFormatter;
use crate::snapshot::SnapshotManager;
use crate::tools::default_tools;
use crate::types::{
    ExecutionReport, Finding, Message, OnIncompletePolicy, PreflightCheck, ProviderError, Role,
    Severity, Usage,
};
use std::path::Path;
use std::time::Instant;

/// Execute a full task lifecycle.
///
/// Orchestrates: config → provider → agent → rule engine → SARIF → checkpoint.
/// Exit codes: 0 = pass, 1 = rule violation, 2 = error, 3 = config error.
pub async fn execute_task(config: &Config) -> ExecutionReport {
    let start = Instant::now();
    let task_id = config.task.id.clone();

    let provider = match create_provider(
        &config.task.vendor,
        &config.task.model,
        &config.api_key.clone().unwrap_or_default(),
        config.task.timeout_secs,
    ) {
        Ok(p) => p,
        Err(e) => {
            return ExecutionReport {
                task_id,
                exit_code: 2,
                findings: vec![],
                token_usage: Usage::default(),
                duration_ms: start.elapsed().as_millis() as u64,
                snapshot_id: None,
                errors: vec![format!("Provider init error: {}", e)],
                violations: vec![],
            };
        }
    };

    let mut tools = default_tools(
        config.workspace.clone(),
        &config.task.tool_allowlist,
        config.task.shell_timeout_secs,
        &config.task.shell_env_passthrough,
    );

    // Start MCP servers and register their tools.
    // Kept alive in `_mcp_manager` for the duration of this task;
    // dropped processes are killed via kill_on_drop(true).
    let _mcp_manager = crate::mcp::McpClientManager::start(
        &config.task.mcp_servers,
        config.task.shell_timeout_secs,
    )
    .await;
    if let Some(ref mgr) = _mcp_manager {
        mgr.register_all(&mut tools);
    }

    // ── LSP tool hint injection ────────────────────────────────────────────
    // When the agent has access to LSP-like MCP tools, generate a guidance
    // note that will be injected into initial_messages.
    let lsp_hint = detect_lsp_tools(&tools);

    // ── Preflight checks ──────────────────────────────────────────────────
    // Run configured MCP tool calls *before* the agent loop. Their output is
    // parsed into deterministic Findings and merged with agent findings.
    let mut preflight_findings: Vec<Finding> = Vec::new();
    let mut preflight_summary: Option<String> = None;
    if let Some(ref mgr) = _mcp_manager {
        if !config.task.preflight.is_empty() {
            let mut all_items: Vec<Finding> = Vec::new();
            for check in &config.task.preflight {
                tracing::info!(
                    server = %check.mcp_server,
                    tool = %check.tool,
                    "Running preflight check"
                );
                match mgr
                    .call_tool(&check.mcp_server, &check.tool, check.args.clone())
                    .await
                {
                    Ok(output) => {
                        let findings = parse_preflight_result(&output, check);
                        all_items.extend(findings);
                    }
                    Err(e) => {
                        tracing::warn!(
                            server = %check.mcp_server,
                            tool = %check.tool,
                            error = %e,
                            "Preflight check failed — skipping"
                        );
                    }
                }
            }
            if !all_items.is_empty() {
                let summary = format_preflight_summary(&all_items);
                preflight_summary = Some(summary);
                preflight_findings = all_items;
            }
        }
    }

    let checkpoint_store = match CheckpointStore::new() {
        Ok(cs) => cs,
        Err(e) => {
            return ExecutionReport {
                task_id,
                exit_code: 2,
                findings: vec![],
                token_usage: Usage::default(),
                duration_ms: start.elapsed().as_millis() as u64,
                snapshot_id: None,
                errors: vec![format!("Checkpoint init error: {}", e)],
                violations: vec![],
            };
        }
    };
    let snapshot_mgr = SnapshotManager::new(checkpoint_store);

    let mut initial_messages = if config.resume {
        match snapshot_mgr.restore_snapshot(&task_id, true) {
            Ok(Some(snapshot)) => snapshot.messages,
            _ => {
                vec![Message::new(
                    Role::User,
                    config.task.prompt_template.clone(),
                )]
            }
        }
    } else {
        vec![Message::new(
            Role::User,
            config.task.prompt_template.clone(),
        )]
    };

    // Inject preflight summary into agent context (if any findings).
    if let Some(summary) = preflight_summary {
        initial_messages.insert(0, Message::new(Role::User, summary));
    }

    // Inject LSP tool guidance into agent context (if LSP tools detected).
    if let Some(hint) = &lsp_hint {
        initial_messages.push(Message::new(Role::User, hint.clone()));
    }

    let agent_config = AgentConfig {
        contract: &config.task,
        provider: provider.as_ref(),
        tools: &tools,
        initial_messages,
        workspace_root: config.workspace.clone(),
        snapshot_mgr: Some(&snapshot_mgr),
    };

    let agent_result = match run_agent_loop(agent_config).await {
        Ok(result) => result,
        Err(ProviderError::Timeout(msg)) => {
            return ExecutionReport {
                task_id,
                exit_code: 2,
                findings: vec![],
                token_usage: Usage::default(),
                duration_ms: start.elapsed().as_millis() as u64,
                snapshot_id: None,
                errors: vec![format!("Timeout: {}", msg)],
                violations: vec![],
            };
        }
        Err(e) => {
            return ExecutionReport {
                task_id,
                exit_code: 2,
                findings: vec![],
                token_usage: Usage::default(),
                duration_ms: start.elapsed().as_millis() as u64,
                snapshot_id: None,
                errors: vec![format!("Agent error: {}", e)],
                violations: vec![],
            };
        }
    };

    let snapshot_id = snapshot_mgr
        .save_snapshot(&task_id, &agent_result.messages, agent_result.truncated)
        .ok();

    // Merge preflight findings (deterministic) with agent findings.
    let all_findings = [preflight_findings, agent_result.findings].concat();

    let gate_result = RuleEngine::evaluate(&all_findings, &config.task.gating_rules);

    // Fail closed on incomplete runs (context truncated or iteration limit
    // reached): a partial sweep with zero findings must not pass gates like
    // `max_findings: 0`.
    let mut errors = Vec::new();
    let exit_code = apply_incomplete_policy(
        gate_result.exit_code,
        agent_result.truncated,
        config.task.on_incomplete,
        &mut errors,
    );

    if agent_result.truncated && config.task.on_incomplete == OnIncompletePolicy::Pass {
        eprintln!(
            "Warning: agent run incomplete (context truncated or iteration limit reached); \
             continuing with partial results (on_incomplete=pass)"
        );
    }

    if let Err(e) = SarifFormatter::write_to_file_with_status(
        &all_findings,
        &config.output,
        agent_result.truncated,
    ) {
        eprintln!("Warning: Failed to write SARIF: {}", e);
    }

    if exit_code == 0 {
        cleanup_archives(&config.workspace, &task_id);
    }

    for v in &gate_result.violations {
        if v.action == crate::types::GateAction::Warn {
            eprintln!(
                "Warning: rule '{}' violated — {} findings (max {}): {}",
                v.rule_id, v.actual_count, v.max_allowed, v.description
            );
        }
    }

    ExecutionReport {
        task_id,
        exit_code,
        findings: all_findings,
        token_usage: agent_result.usage,
        duration_ms: agent_result.duration_ms,
        snapshot_id,
        errors,
        violations: gate_result.violations,
    }
}

/// Decide the final exit code when the agent run may be incomplete.
///
/// A complete run is returned unchanged. For an incomplete run (context
/// truncated or iteration limit reached without a clean `Stop`):
/// - `OnIncompletePolicy::Fail` fails closed: returns 2 (error) and pushes a
///   diagnostic to `errors`, regardless of the gate result — an incomplete
///   sweep must not pass gates, and a runtime error outranks a rule
///   violation.
/// - `OnIncompletePolicy::Pass` keeps the gate result unchanged; the caller
///   warns and annotates the SARIF output instead.
fn apply_incomplete_policy(
    gate_exit_code: u32,
    incomplete: bool,
    policy: OnIncompletePolicy,
    errors: &mut Vec<String>,
) -> u32 {
    if !incomplete {
        return gate_exit_code;
    }
    match policy {
        OnIncompletePolicy::Fail => {
            errors.push(
                "Agent run incomplete (context truncated or iteration limit reached); \
                 failing closed (on_incomplete=fail)"
                    .to_string(),
            );
            2
        }
        OnIncompletePolicy::Pass => gate_exit_code,
    }
}

/// Delete archive and ledger files for the given task_id after successful
/// execution. Silently ignores errors — this is best-effort cleanup.
pub fn cleanup_archives(workspace: &Path, task_id: &str) {
    let archives_dir = workspace.join(".clausura").join("archives");
    if !archives_dir.exists() {
        return;
    }
    let dump_prefix = format!("context-dump-{}-{}", task_id, "");
    let ledger_prefix = format!("findings-ledger-{}", task_id);
    if let Ok(entries) = std::fs::read_dir(&archives_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let is_dump = name_str.starts_with(&dump_prefix) && name_str.ends_with(".log");
            let is_ledger = name_str.starts_with(&ledger_prefix) && name_str.ends_with(".jsonl");
            if is_dump || is_ledger {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

// ── Preflight helpers ─────────────────────────────────────────────────────

/// Parse an MCP tool's JSON output into `Finding` objects.
///
/// The output is expected to be a JSON array of objects. Each object's fields
/// are mapped to `Finding` fields using the `PreflightCheck` configuration.
/// Items that cannot be parsed are silently skipped.
fn parse_preflight_result(output: &str, check: &PreflightCheck) -> Vec<Finding> {
    let value: serde_json::Value = match serde_json::from_str(output) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let items = match value.as_array() {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    let mut findings = Vec::new();
    for item in items {
        if let Some(msg) = item
            .get(&check.message_field)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            let severity_str = item
                .get(&check.severity_field)
                .and_then(|v| v.as_str())
                .unwrap_or(&check.default_severity);
            let severity = parse_severity_str(severity_str);

            let file = item
                .get(&check.file_field)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let line_start = item
                .get(&check.line_field)
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let col_start = item
                .get(&check.column_field)
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;

            let location = if !file.is_empty() {
                Some(crate::types::Location {
                    file,
                    line_start,
                    line_end: line_start,
                    column_start: col_start,
                    column_end: col_start,
                })
            } else {
                None
            };

            let rule_id = format!("{}{}", check.rule_id_prefix, msg);

            findings.push(Finding {
                id: uuid::Uuid::new_v4(),
                rule_id,
                severity,
                message: msg.to_string(),
                location,
                evidence: output.len().min(200).to_string(), // first 200 chars as evidence
            });
        }
    }

    findings
}

/// Convert a string like "error", "warning", "info" into `Severity`.
/// Also accepts integer-string severities (e.g. "1" → error per LSP convention).
fn parse_severity_str(s: &str) -> Severity {
    match s.to_lowercase().as_str() {
        "error" | "1" => Severity::Error,
        "warning" | "2" | "warn" => Severity::Warning,
        "info" | "3" | "information" => Severity::Info,
        "hint" | "4" => Severity::Hint,
        _ => Severity::Warning,
    }
}

/// Format a human-readable summary of preflight findings.
fn format_preflight_summary(findings: &[Finding]) -> String {
    use std::fmt::Write;
    let mut buf = String::from("Preflight diagnostics found:\n");
    for f in findings {
        let loc = f
            .location
            .as_ref()
            .map(|l| format!("{}:{}", l.file, l.line_start))
            .unwrap_or_default();
        let _ = writeln!(
            buf,
            "  [{:?}][{}] {} — {}",
            f.severity, f.rule_id, f.message, loc
        );
    }
    buf.push_str("\nConsider these findings in your review.");
    buf
}

/// Detect if any registered tools provide LSP-like capabilities and return
/// a guidance hint for the agent.
///
/// Scans tool names for common LSP-related keywords. When found, injects
/// a short usage guide so the agent prioritizes semantic tools over text
/// grep for code understanding.
fn detect_lsp_tools(registry: &crate::tools::ToolRegistry) -> Option<String> {
    let defs = registry.list_definitions();
    let lsp_keywords = [
        "diagnostics",
        "hover",
        "references",
        "definition",
        "symbol",
        "lsp",
    ];
    let has_lsp_tool = defs.iter().any(|t| {
        let name = t.name.to_lowercase();
        lsp_keywords.iter().any(|kw| name.contains(kw))
    });

    if !has_lsp_tool {
        return None;
    }

    // Collect tool names for the user message.
    let mut tool_lines: Vec<String> = Vec::new();
    for t in &defs {
        let name_lower = t.name.to_lowercase();
        if lsp_keywords.iter().any(|kw| name_lower.contains(kw)) {
            tool_lines.push(format!("  - `{}` — {}", t.name, t.description));
        }
    }

    let tools_section = tool_lines.join("\n");
    Some(format!(
        r#"📐 LSP Code Intelligence Tools Available

The following language-server tools are at your disposal. Prefer them over
plain `grep`/`read_file` when you need semantic understanding of the code:

{tools_section}

When to use each tool:
- **diagnostics** — check for compile errors, type mismatches, lints
- **hover** — get type information and documentation for a symbol
- **definition** — jump to a symbol's definition
- **references** — find all usages of a symbol across the codebase
- **symbols** — list all symbols in a file or workspace

Use these tools to answer questions about code structure and correctness
before reaching for text-based searches."#,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Finding, GateAction, GateRule, Severity};
    use tempfile::TempDir;

    // For testing, we need to make the executor work with a mock provider.
    // Since the executor creates the provider internally, integration tests
    // would need a different approach (e.g., feature gate).
    // For now, test the rule + SARIF pipeline with mocked agent results.

    #[test]
    fn test_rule_violation_exit_1() {
        let findings = vec![Finding {
            id: uuid::Uuid::new_v4(),
            rule_id: "critical".into(),
            severity: Severity::Error,
            message: "Found critical issue".into(),
            location: None,
            evidence: "test".into(),
        }];
        let rules = vec![GateRule {
            rule_id: "critical".into(),
            description: "No critical".into(),
            min_severity: Severity::Error,
            max_findings: 0,
            action: GateAction::Fail,
        }];
        let result = RuleEngine::evaluate(&findings, &rules);
        assert_eq!(result.exit_code, 1);
    }

    #[test]
    fn test_clean_exit_0() {
        let result = RuleEngine::evaluate(&[], &[]);
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn test_sarif_written() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.sarif");
        let findings = vec![Finding {
            id: uuid::Uuid::new_v4(),
            rule_id: "test".into(),
            severity: Severity::Warning,
            message: "Test warning".into(),
            location: None,
            evidence: "".into(),
        }];
        SarifFormatter::write_to_file(&findings, &path).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("warning"));
    }

    #[test]
    fn test_archives_cleaned_on_exit_zero() {
        let tmp = TempDir::new().unwrap();
        let archives_dir = tmp.path().join(".clausura").join("archives");
        std::fs::create_dir_all(&archives_dir).unwrap();

        std::fs::write(archives_dir.join("context-dump-test-task-1.log"), "data1").unwrap();
        std::fs::write(archives_dir.join("context-dump-test-task-2.log"), "data2").unwrap();
        std::fs::write(archives_dir.join("some-other-file.txt"), "other").unwrap();

        cleanup_archives(tmp.path(), "test-task");

        assert!(!archives_dir.join("context-dump-test-task-1.log").exists());
        assert!(!archives_dir.join("context-dump-test-task-2.log").exists());
        assert!(archives_dir.join("some-other-file.txt").exists());
        assert!(archives_dir.exists());
    }

    #[test]
    fn test_archives_preserved_on_exit_one() {
        let tmp = TempDir::new().unwrap();
        let archives_dir = tmp.path().join(".clausura").join("archives");
        std::fs::create_dir_all(&archives_dir).unwrap();

        std::fs::write(archives_dir.join("context-dump-other-task-1.log"), "data").unwrap();

        cleanup_archives(tmp.path(), "different-task-id");

        assert!(archives_dir.join("context-dump-other-task-1.log").exists());
    }

    #[test]
    fn test_incomplete_fail_policy_returns_exit_2_with_error() {
        let mut errors = Vec::new();
        let code = apply_incomplete_policy(0, true, OnIncompletePolicy::Fail, &mut errors);
        assert_eq!(code, 2);
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].contains("Agent run incomplete"),
            "got: {}",
            errors[0]
        );
        assert!(
            errors[0].contains("on_incomplete=fail"),
            "got: {}",
            errors[0]
        );
    }

    #[test]
    fn test_incomplete_pass_policy_keeps_gate_result() {
        let mut errors = Vec::new();
        assert_eq!(
            apply_incomplete_policy(0, true, OnIncompletePolicy::Pass, &mut errors),
            0
        );
        assert_eq!(
            apply_incomplete_policy(1, true, OnIncompletePolicy::Pass, &mut errors),
            1
        );
        assert!(errors.is_empty());
    }

    #[test]
    fn test_incomplete_fail_policy_error_outranks_violation() {
        // gate=1 + incomplete + Fail → 2: the run itself is untrustworthy,
        // so the runtime error takes precedence over the rule violation.
        let mut errors = Vec::new();
        let code = apply_incomplete_policy(1, true, OnIncompletePolicy::Fail, &mut errors);
        assert_eq!(code, 2);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn test_complete_run_unchanged_by_policy() {
        let mut errors = Vec::new();
        assert_eq!(
            apply_incomplete_policy(0, false, OnIncompletePolicy::Fail, &mut errors),
            0
        );
        assert_eq!(
            apply_incomplete_policy(1, false, OnIncompletePolicy::Fail, &mut errors),
            1
        );
        assert_eq!(
            apply_incomplete_policy(0, false, OnIncompletePolicy::Pass, &mut errors),
            0
        );
        assert!(errors.is_empty());
    }

    // ── parse_preflight_result tests ────────────────────────────────────────

    #[test]
    fn test_parse_preflight_result_basic() {
        let check = PreflightCheck::default();
        let output = r#"[
            {"severity": "error", "message": "type mismatch", "file": "src/main.rs", "line": 42},
            {"severity": "warning", "message": "unused variable", "file": "src/lib.rs", "line": 10}
        ]"#;
        let findings = parse_preflight_result(output, &check);
        assert_eq!(findings.len(), 2);

        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains("type mismatch"));
        assert_eq!(findings[0].location.as_ref().unwrap().file, "src/main.rs");
        assert_eq!(findings[0].location.as_ref().unwrap().line_start, 42);
        assert!(findings[0].rule_id.starts_with("preflight-"));

        assert_eq!(findings[1].severity, Severity::Warning);
        assert!(findings[1].message.contains("unused variable"));
    }

    #[test]
    fn test_parse_preflight_result_empty() {
        let check = PreflightCheck::default();
        let findings = parse_preflight_result(r#"[]"#, &check);
        assert!(findings.is_empty());
    }

    #[test]
    fn test_parse_preflight_result_non_json() {
        let check = PreflightCheck::default();
        let findings = parse_preflight_result("not json at all", &check);
        assert!(findings.is_empty());
    }

    #[test]
    fn test_parse_preflight_result_custom_fields() {
        let check = PreflightCheck {
            rule_id_prefix: "diag-".into(),
            severity_field: "s".into(),
            message_field: "m".into(),
            file_field: "path".into(),
            line_field: "ln".into(),
            ..Default::default()
        };
        let output = r#"[
            {"s": "error", "m": "E001: something wrong", "path": "a.rs", "ln": 1}
        ]"#;
        let findings = parse_preflight_result(output, &check);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].rule_id.starts_with("diag-"));
    }

    #[test]
    fn test_parse_severity_str() {
        assert_eq!(parse_severity_str("error"), Severity::Error);
        assert_eq!(parse_severity_str("ERROR"), Severity::Error);
        assert_eq!(parse_severity_str("1"), Severity::Error); // LSP convention
        assert_eq!(parse_severity_str("warning"), Severity::Warning);
        assert_eq!(parse_severity_str("2"), Severity::Warning);
        assert_eq!(parse_severity_str("warn"), Severity::Warning);
        assert_eq!(parse_severity_str("info"), Severity::Info);
        assert_eq!(parse_severity_str("3"), Severity::Info);
        assert_eq!(parse_severity_str("hint"), Severity::Hint);
        assert_eq!(parse_severity_str("4"), Severity::Hint);
        assert_eq!(parse_severity_str("unknown"), Severity::Warning); // fallback
    }

    #[test]
    fn test_format_preflight_summary() {
        let findings = vec![Finding {
            id: uuid::Uuid::new_v4(),
            rule_id: "test-err".into(),
            severity: Severity::Error,
            message: "Something failed".into(),
            location: Some(crate::types::Location {
                file: "src/main.rs".into(),
                line_start: 42,
                line_end: 42,
                column_start: 1,
                column_end: 1,
            }),
            evidence: "".into(),
        }];
        let summary = format_preflight_summary(&findings);
        assert!(summary.contains("Preflight diagnostics"));
        assert!(summary.contains("src/main.rs:42"));
        assert!(summary.contains("Something failed"));
    }

    #[test]
    fn test_detect_lsp_tools_no_lsp_tools_returns_none() {
        // Only built-in tools (read_file, git_diff, etc.) — no LSP hint.
        let tmp = TempDir::new().unwrap();
        let registry = default_tools(tmp.path().to_path_buf(), &[], 120, &[]);
        let hint = detect_lsp_tools(&registry);
        assert!(hint.is_none(), "no LSP tools configured → no hint");
    }
}
