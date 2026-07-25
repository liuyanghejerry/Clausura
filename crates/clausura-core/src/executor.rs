use crate::agent::{run_agent_loop, AgentConfig};
use crate::checkpoint::CheckpointStore;
use crate::config::Config;
use crate::provider::create_provider;
use crate::rules::RuleEngine;
use crate::sarif::SarifFormatter;
use crate::snapshot::SnapshotManager;
use crate::tools::default_tools;
use crate::types::{ExecutionReport, Message, OnIncompletePolicy, ProviderError, Role, Usage};
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

    let tools = default_tools(
        config.workspace.clone(),
        &config.task.tool_allowlist,
        config.task.shell_timeout_secs,
        &config.task.shell_env_passthrough,
    );

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

    let initial_messages = if config.resume {
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

    let gate_result = RuleEngine::evaluate(&agent_result.findings, &config.task.gating_rules);

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
        &agent_result.findings,
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
        findings: agent_result.findings,
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

/// Delete archive files for the given task_id after successful execution.
/// Silently ignores errors — this is best-effort cleanup.
pub fn cleanup_archives(workspace: &Path, task_id: &str) {
    let archives_dir = workspace.join(".clausura").join("archives");
    if !archives_dir.exists() {
        return;
    }
    let prefix = format!("context-dump-{}-", task_id);
    if let Ok(entries) = std::fs::read_dir(&archives_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with(&prefix) && name_str.ends_with(".log") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
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
}
