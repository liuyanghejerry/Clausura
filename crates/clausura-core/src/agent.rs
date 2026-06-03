use crate::context::ContextManager;
use crate::provider::Provider;
use crate::tools::ToolRegistry;
use crate::types::{Finding, FinishReason, Message, ProviderError, Role, TaskContract, Usage};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Result from the agent loop
#[derive(Debug)]
pub struct AgentResult {
    pub messages: Vec<Message>,
    pub findings: Vec<Finding>,
    pub usage: Usage,
    pub duration_ms: u64,
    pub truncated: bool,
}

/// Configuration for the agent loop
pub struct AgentConfig<'a> {
    pub contract: &'a TaskContract,
    pub provider: &'a dyn Provider,
    pub tools: &'a ToolRegistry,
    pub initial_messages: Vec<Message>,
    pub workspace_root: PathBuf,
}

/// Run the bounded agent loop.
pub async fn run_agent_loop(config: AgentConfig<'_>) -> Result<AgentResult, ProviderError> {
    let start = Instant::now();
    let max_iterations: u32 = 10;
    let mut messages = config.initial_messages;
    let mut total_usage = Usage::default();
    let mut truncated = false;
    let mut running_tokens: u64 = 0;

    let tool_descriptions = config.tools.list_definitions();
    let tools_json = serde_json::to_string_pretty(&tool_descriptions).unwrap_or_default();
    let system_prompt = format!(
        "{}\n\nAvailable tools:\n{}\n\nRespond in JSON format with a `findings` array.",
        config.contract.prompt_template, tools_json,
    );

    messages.insert(0, Message::new(Role::System, system_prompt));

    let cm = ContextManager::new(
        config.provider,
        config.contract.token_budget,
        config.workspace_root.clone(),
    );

    for _iteration in 0..max_iterations {
        if start.elapsed() > Duration::from_secs(config.contract.timeout_secs) {
            return Err(ProviderError::Timeout("Task timeout exceeded".into()));
        }

        if running_tokens >= config.contract.token_budget || cm.should_truncate(&messages) {
            let snapshot = messages.clone();
            let (was_truncated, count) = cm.truncate_to_budget(&mut messages);
            if was_truncated && count > 0 {
                let dropped_end = 1 + (snapshot.len() - messages.len());
                let dropped: Vec<Message> = snapshot[1..dropped_end].to_vec();

                let archive_result = cm.archive(&dropped, &config.contract.id).await;

                match archive_result {
                    Ok(path) => {
                        messages.push(Message::new(
                            Role::User,
                            format!(
                                "⚠️ Context was trimmed to stay within token budget.\n\
                             {} earlier messages are archived at:\n  {}\n\
                             Use read_file to inspect if you need context from earlier iterations.",
                                dropped.len(),
                                path.display(),
                            ),
                        ));
                    }
                    Err(_) => {
                        messages.push(Message::new(
                            Role::User,
                            format!(
                                "⚠️ Context was trimmed to stay within token budget.\n\
                             {} earlier messages were dropped (archive unavailable).",
                                dropped.len(),
                            ),
                        ));
                    }
                }

                running_tokens = cm.count_tokens(&messages);

                if cm.should_truncate(&messages) || running_tokens >= config.contract.token_budget {
                    truncated = true;
                    break;
                }
                continue;
            } else {
                truncated = true;
                break;
            }
        }

        let response = config
            .provider
            .chat_with_tools(&messages, config.tools.list_definitions().as_slice())
            .await?;

        total_usage.input_tokens += response.usage.input_tokens;
        total_usage.output_tokens += response.usage.output_tokens;
        total_usage.total_tokens += response.usage.total_tokens;
        running_tokens += response.usage.total_tokens;

        match response.finish_reason {
            FinishReason::Stop => {
                messages.push(Message::new(
                    Role::Assistant,
                    response.message.content.clone(),
                ));

                let findings = extract_findings(&response.message.content);
                return Ok(AgentResult {
                    messages,
                    findings,
                    usage: total_usage,
                    duration_ms: start.elapsed().as_millis() as u64,
                    truncated,
                });
            }
            FinishReason::ToolCalls => {
                if let Some(tool_calls) = response.tool_calls {
                    let tool_call_content = serde_json::to_string(&tool_calls).unwrap_or_default();
                    messages.push(Message::new(Role::Assistant, tool_call_content));

                    for tc in &tool_calls {
                        match config.tools.get(&tc.name) {
                            Some(tool) => {
                                let result = tool.execute(tc.arguments.clone()).await;
                                match result {
                                    Ok(output) => {
                                        messages.push(Message::with_tool_call(
                                            Role::Tool,
                                            output,
                                            tc.id.clone(),
                                        ));
                                    }
                                    Err(e) => {
                                        messages.push(Message::with_tool_call(
                                            Role::Tool,
                                            format!("Error: {}", e),
                                            tc.id.clone(),
                                        ));
                                    }
                                }
                            }
                            None => {
                                messages.push(Message::with_tool_call(
                                    Role::Tool,
                                    format!("Error: Tool '{}' not found", tc.name),
                                    tc.id.clone(),
                                ));
                            }
                        }
                    }
                } else {
                    break;
                }
            }
            FinishReason::Length => {
                truncated = true;
                break;
            }
            FinishReason::ContentFilter | FinishReason::Other(_) => {
                break;
            }
        }
    }

    let last_content = messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant)
        .map(|m| m.content.clone())
        .unwrap_or_default();

    let findings = extract_findings(&last_content);

    Ok(AgentResult {
        messages,
        findings,
        usage: total_usage,
        duration_ms: start.elapsed().as_millis() as u64,
        truncated,
    })
}

/// Extract findings from agent output JSON.
/// Tries to parse the content as JSON and extract a `findings` array.
/// If that fails, returns an empty vec.
fn extract_findings(content: &str) -> Vec<Finding> {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(content) {
        if let Some(findings) = json.get("findings").and_then(|f| f.as_array()) {
            let parsed: Vec<Finding> = findings
                .iter()
                .filter_map(|f| serde_json::from_value(f.clone()).ok())
                .collect();
            if !parsed.is_empty() {
                return parsed;
            }
        }
        if let Ok(findings) = serde_json::from_value::<Vec<Finding>>(json) {
            return findings;
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::tests::MockProvider;
    use crate::tools::default_tools;
    use crate::types::{AmbiguityPolicy, ChatResponse, ToolCall, VendorConfig};
    use tempfile::TempDir;

    fn test_contract() -> TaskContract {
        TaskContract {
            id: "test".into(),
            name: "test".into(),
            description: "".into(),
            model: "gpt-4o".into(),
            vendor: VendorConfig::openai(),
            prompt_template: "Review the code and return findings as JSON.".into(),
            tool_allowlist: vec!["git".into()],
            token_budget: 100000,
            timeout_secs: 60,
            ambiguity_policy: AmbiguityPolicy::FailClosed,
            gating_rules: vec![],
        }
    }

    #[tokio::test]
    async fn test_agent_loop_with_tool_calls() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let tools = default_tools(root.clone(), &[]);

        let mut mock = MockProvider::new("gpt-4o");
        mock.add_response(ChatResponse {
            message: Message::new(Role::Assistant, "Checking code..."),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
            finish_reason: FinishReason::ToolCalls,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                name: "git_diff".into(),
                arguments: serde_json::json!({}),
            }]),
        });
        mock.add_response(ChatResponse {
            message: Message::new(Role::Assistant, r#"{"findings": [{"id": "00000000-0000-0000-0000-000000000000", "rule_id": "test", "severity": "warning", "message": "test finding", "evidence": "test"}]}"#),
            usage: Usage {
                input_tokens: 20,
                output_tokens: 10,
                total_tokens: 30,
            },
            finish_reason: FinishReason::Stop,
            tool_calls: None,
        });

        let contract = test_contract();
        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, "Review the diff")],
            workspace_root: root.clone(),
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(!result.findings.is_empty());
        assert!(result.duration_ms > 0);
    }

    #[tokio::test]
    async fn test_agent_loop_halts_on_timeout() {
        let tmp = TempDir::new().unwrap();
        let tools = default_tools(tmp.path().to_path_buf(), &[]);

        let mut mock = MockProvider::new("slow-model");
        mock.add_slow_response(Duration::from_secs(10));

        let mut contract = test_contract();
        contract.timeout_secs = 1;

        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, "Hi")],
            workspace_root: tmp.path().to_path_buf(),
        };

        let result = run_agent_loop(config).await;
        assert!(result.is_err());
    }

    fn setup_agent_env() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        (tmp, root)
    }

    #[tokio::test]
    async fn test_agent_loop_truncates_on_budget_exceeded() {
        let (_tmp, root) = setup_agent_env();
        let tools = default_tools(root.clone(), &[]);

        let mut contract = test_contract();
        contract.token_budget = 10000;

        let tool_call = ToolCall {
            id: "call_1".into(),
            name: "git_diff".into(),
            arguments: serde_json::json!({}),
        };

        let mut mock = MockProvider::new("test-model");
        mock.add_response(ChatResponse {
            message: Message::new(Role::Assistant, "Running tool..."),
            usage: Usage {
                input_tokens: 5,
                output_tokens: 5,
                total_tokens: 10,
            },
            finish_reason: FinishReason::ToolCalls,
            tool_calls: Some(vec![tool_call.clone()]),
        });
        mock.add_response(ChatResponse {
            message: Message::new(Role::Assistant, r#"{"findings": [{"id": "00000000-0000-0000-0000-000000000000", "rule_id": "test", "severity": "warning", "message": "test finding", "evidence": "test"}]}"#),
            usage: Usage {
                input_tokens: 5,
                output_tokens: 5,
                total_tokens: 10,
            },
            finish_reason: FinishReason::Stop,
            tool_calls: None,
        });

        let huge_content = "x".repeat(40000);
        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, huge_content)],
            workspace_root: root.clone(),
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(
            !result.truncated,
            "Expected truncation to succeed (truncated=false), got truncated=true"
        );
        assert!(
            !result.findings.is_empty(),
            "Expected findings after truncation"
        );

        let archive_dir = root.join(".clausura").join("archives");
        assert!(archive_dir.exists(), "Archive directory should exist");
        let mut found = false;
        if let Ok(entries) = std::fs::read_dir(&archive_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with("context-dump-test-") {
                    found = true;
                    break;
                }
            }
        }
        assert!(found, "Archive file should exist after truncation");
    }

    #[tokio::test]
    async fn test_agent_loop_breaks_when_cannot_truncate() {
        let (_tmp, root) = setup_agent_env();
        let tools = default_tools(root.clone(), &[]);

        let mut contract = test_contract();
        contract.token_budget = 1;

        let mut mock = MockProvider::new("test-model");
        mock.add_response(ChatResponse {
            message: Message::new(Role::Assistant, "Running tool..."),
            usage: Usage {
                input_tokens: 5,
                output_tokens: 5,
                total_tokens: 10,
            },
            finish_reason: FinishReason::ToolCalls,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                name: "git_diff".into(),
                arguments: serde_json::json!({}),
            }]),
        });

        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, "Review")],
            workspace_root: root.clone(),
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(
            result.truncated,
            "Expected truncated=true when context cannot be reduced further"
        );
    }

    #[tokio::test]
    async fn test_hint_message_injected_after_truncation() {
        let (_tmp, root) = setup_agent_env();
        let tools = default_tools(root.clone(), &[]);

        let mut contract = test_contract();
        contract.token_budget = 10000;

        let tool_call = ToolCall {
            id: "call_1".into(),
            name: "git_diff".into(),
            arguments: serde_json::json!({}),
        };

        let mut mock = MockProvider::new("test-model");
        mock.add_response(ChatResponse {
            message: Message::new(Role::Assistant, "Running tool..."),
            usage: Usage {
                input_tokens: 5,
                output_tokens: 5,
                total_tokens: 10,
            },
            finish_reason: FinishReason::ToolCalls,
            tool_calls: Some(vec![tool_call.clone()]),
        });
        mock.add_response(ChatResponse {
            message: Message::new(Role::Assistant, r#"{"findings": [{"id": "00000000-0000-0000-0000-000000000000", "rule_id": "test", "severity": "warning", "message": "test finding", "evidence": "test"}]}"#),
            usage: Usage {
                input_tokens: 5,
                output_tokens: 5,
                total_tokens: 10,
            },
            finish_reason: FinishReason::Stop,
            tool_calls: None,
        });

        let huge_content = "x".repeat(40000);
        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, huge_content)],
            workspace_root: root.clone(),
        };

        let result = run_agent_loop(config).await.unwrap();
        let hint = result
            .messages
            .iter()
            .any(|m| m.role == Role::User && m.content.contains("archived at"));
        assert!(
            hint,
            "Expected a hint message about archiving after truncation"
        );
    }

    #[tokio::test]
    async fn test_agent_loop_propagates_tool_call_id() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let tools = default_tools(root.clone(), &[]);

        let mut mock = MockProvider::new("gpt-4o");
        mock.add_response(ChatResponse {
            message: Message::new(Role::Assistant, "calling tool".to_string()),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
            finish_reason: FinishReason::ToolCalls,
            tool_calls: Some(vec![ToolCall {
                id: "call_verify_tcid".into(),
                name: "git_diff".into(),
                arguments: serde_json::json!({}),
            }]),
        });
        mock.add_response(ChatResponse {
            message: Message::new(
                Role::Assistant,
                r#"{"findings":[],"stop":true}"#.to_string(),
            ),
            usage: Usage {
                input_tokens: 20,
                output_tokens: 10,
                total_tokens: 30,
            },
            finish_reason: FinishReason::Stop,
            tool_calls: None,
        });

        let contract = test_contract();
        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, "Run git diff")],
            workspace_root: root,
        };

        let result = run_agent_loop(config).await.unwrap();

        let tool_messages: Vec<&Message> = result
            .messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .collect();

        assert!(
            !tool_messages.is_empty(),
            "expected at least one tool message"
        );
        for tm in &tool_messages {
            assert!(
                tm.tool_call_id.is_some(),
                "tool message must carry tool_call_id: role={:?}, content={}",
                tm.role,
                tm.content
            );
            assert_eq!(
                tm.tool_call_id.as_deref(),
                Some("call_verify_tcid"),
                "tool_call_id should match the assistant's tool call id"
            );
        }
    }
}
