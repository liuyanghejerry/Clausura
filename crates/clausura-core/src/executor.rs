use crate::agent::{run_agent_loop, AgentConfig};
use crate::checkpoint::CheckpointStore;
use crate::config::Config;
use crate::provider::create_provider;
use crate::rules::RuleEngine;
use crate::sarif::SarifFormatter;
use crate::snapshot::SnapshotManager;
use crate::tools::default_tools;
use crate::types::{ExecutionReport, Message, ProviderError, Role, Usage};
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
            };
        }
    };

    let tools = default_tools(config.workspace.clone());

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
            };
        }
    };
    let snapshot_mgr = SnapshotManager::new(checkpoint_store);

    let initial_messages = if config.resume {
        match snapshot_mgr.restore_snapshot(&task_id, true) {
            Ok(Some(snapshot)) => snapshot.messages,
            _ => {
                vec![Message {
                    role: Role::User,
                    content: config.task.prompt_template.clone(),
                }]
            }
        }
    } else {
        vec![Message {
            role: Role::User,
            content: config.task.prompt_template.clone(),
        }]
    };

    let agent_config = AgentConfig {
        contract: &config.task,
        provider: provider.as_ref(),
        tools: &tools,
        initial_messages,
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
            };
        }
    };

    let snapshot_id = snapshot_mgr
        .save_snapshot(&task_id, &agent_result.messages, agent_result.truncated)
        .ok();

    let gate_result = RuleEngine::evaluate(&agent_result.findings, &config.task.gating_rules);

    if let Err(e) = SarifFormatter::write_to_file(&agent_result.findings, &config.output) {
        eprintln!("Warning: Failed to write SARIF: {}", e);
    }

    ExecutionReport {
        task_id,
        exit_code: gate_result.exit_code,
        findings: agent_result.findings,
        token_usage: agent_result.usage,
        duration_ms: agent_result.duration_ms,
        snapshot_id,
        errors: vec![],
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
}
