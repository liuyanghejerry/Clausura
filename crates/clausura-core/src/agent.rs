use crate::context::ContextManager;
use crate::provider::Provider;
use crate::snapshot::SnapshotManager;
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
    /// True when the loop ended without a clean `Stop`: context truncation,
    /// `FinishReason::Length`, an abnormal finish reason, or exhaustion of
    /// `max_iterations`. Signals to the caller that the result may be
    /// incomplete (see `TaskContract::on_incomplete`).
    pub truncated: bool,
}

/// Configuration for the agent loop
pub struct AgentConfig<'a> {
    pub contract: &'a TaskContract,
    pub provider: &'a dyn Provider,
    pub tools: &'a ToolRegistry,
    pub initial_messages: Vec<Message>,
    pub workspace_root: PathBuf,
    pub snapshot_mgr: Option<&'a SnapshotManager>,
}

/// Run the bounded agent loop.
pub async fn run_agent_loop(config: AgentConfig<'_>) -> Result<AgentResult, ProviderError> {
    let start = Instant::now();
    let max_iterations: u32 = config.contract.max_iterations;
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

        // Cumulative cost cap (only when configured): total billed tokens
        // across all LLM calls. Independent of `token_budget`, which governs
        // context-window truncation only.
        if let Some(max_total) = config.contract.max_total_tokens {
            if running_tokens >= max_total {
                // Fall-through below marks the result truncated.
                break;
            }
        }

        if cm.should_truncate(&messages) {
            let snapshot = messages.clone();
            let (was_truncated, count) = cm.truncate_to_budget(&mut messages);
            if was_truncated && count > 0 {
                let dropped_end = 1 + (snapshot.len() - messages.len());
                let dropped: Vec<Message> = snapshot[1..dropped_end].to_vec();

                let archive_result = cm.archive(&dropped, &config.contract.id).await;

                match archive_result {
                    Ok(path) => {
                        // Insert at the truncation boundary (right after the
                        // system message), not at the end: appending a User
                        // message after an assistant message with tool_calls
                        // would leave those calls without results, which the
                        // OpenAI/Anthropic APIs reject.
                        messages.insert(
                            1,
                            Message::new(
                                Role::User,
                                format!(
                                    "⚠️ Context was trimmed to stay within token budget.\n\
                                 {} earlier messages are archived at:\n  {}\n\
                                 Use read_file to inspect if you need context from earlier iterations.",
                                    dropped.len(),
                                    path.display(),
                                ),
                            ),
                        );
                    }
                    Err(_) => {
                        messages.insert(
                            1,
                            Message::new(
                                Role::User,
                                format!(
                                    "⚠️ Context was trimmed to stay within token budget.\n\
                                 {} earlier messages were dropped (archive unavailable).",
                                    dropped.len(),
                                ),
                            ),
                        );
                    }
                }

                if cm.should_truncate(&messages) {
                    // Context cannot be reduced far enough to fit the budget;
                    // fall-through below marks the result truncated.
                    break;
                }
                continue;
            } else {
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

                let findings = extract_findings(&response.message.content)
                    .map_err(ProviderError::MalformedFindings)?;
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
                    messages.push(Message {
                        role: Role::Assistant,
                        content: String::new(),
                        tool_call_id: None,
                        tool_calls: Some(tool_calls.clone()),
                    });

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
                // Fall-through below marks the result truncated.
                break;
            }
            FinishReason::ContentFilter | FinishReason::Other(_) => {
                break;
            }
        }

        // Auto-save checkpoint every N iterations for crash recovery
        if let Some(mgr) = config.snapshot_mgr {
            let iteration = _iteration + 1;
            if mgr.should_auto_save(iteration) {
                let _ = mgr.save_snapshot(&config.contract.id, &messages, truncated);
            }
        }
    }

    let last_content = messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant)
        .map(|m| m.content.clone())
        .unwrap_or_default();

    // The loop exited without a clean `Stop` (timeout/truncation/iteration cap),
    // so there is no complete final answer to hold to the strict schema below.
    // Best-effort extraction with a warning is appropriate here. Mark the
    // result as truncated (incomplete): this fall-through path is reached on
    // Length, failed truncation, ContentFilter/Other breaks, and iteration
    // exhaustion, none of which produced a complete final answer.
    truncated = true;
    let findings = extract_findings_lenient(&last_content);

    Ok(AgentResult {
        messages,
        findings,
        usage: total_usage,
        duration_ms: start.elapsed().as_millis() as u64,
        truncated,
    })
}

/// Extract findings from a completed agent response.
///
/// The response is expected to be a JSON object `{"findings": [...]}` (a bare
/// top-level JSON array is also tolerated). If the whole response isn't
/// valid JSON on its own — e.g. the model prefixed its answer with a
/// reasoning sentence, or wrapped it in a markdown code fence despite being
/// told not to — the last top-level balanced `{...}`/`[...]` block in the
/// text is recovered and parsed instead; models very commonly emit their
/// real final answer last, after any reasoning prose.
///
/// Individual findings that fail schema validation are skipped with a
/// warning (so one malformed element does not discard the entire batch).
/// Only when *every* element fails, or no JSON can be recovered at all,
/// does this return `Err`.
///
/// The `id` field (UUID v4) is auto-generated server-side when the agent
/// omits it or supplies a malformed value; agents are not required to
/// produce syntactically valid UUIDs themselves.
fn extract_findings(content: &str) -> Result<Vec<Finding>, String> {
    let trimmed = content.trim();

    let mut json: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(first_err) => {
            let recovered = find_last_balanced_block(trimmed, '{', '}')
                .or_else(|| find_last_balanced_block(trimmed, '[', ']'))
                .and_then(|block| serde_json::from_str::<serde_json::Value>(block).ok());

            recovered.ok_or_else(|| {
                format!(
                    "agent response is not valid JSON ({first_err}) and no embedded \
                     JSON object/array could be recovered:\n{content}"
                )
            })?
        }
    };

    // Accept either {"findings": [...]} or a bare top-level [...] array.
    let findings_value = if json.get("findings").is_some() {
        // Take ownership of the findings array so we can mutate elements.
        std::mem::replace(json.get_mut("findings").unwrap(), serde_json::Value::Null)
    } else {
        json
    };
    let mut elements = match findings_value {
        serde_json::Value::Array(arr) => arr,
        other => return Err(format!("expected a `findings` array, got: {other}")),
    };

    let total = elements.len();
    let mut parsed = Vec::with_capacity(total);
    let mut errors = Vec::new();
    for (i, el) in elements.iter_mut().enumerate() {
        // Fix UUID before deserialization: LLMs sometimes omit or malform
        // the id field. Since it is an internal-only identifier (not forwarded
        // to SARIF), we can safely auto-generate it server-side.
        fix_finding_uuid(el);

        match serde_json::from_value::<Finding>(el.clone()) {
            Ok(f) => parsed.push(f),
            Err(e) => errors.push(format!("findings[{i}]: {e} (raw: {el})")),
        }
    }

    if parsed.is_empty() && !errors.is_empty() {
        return Err(format!(
            "{} of {} finding(s) failed to match the Finding schema:\n{}",
            errors.len(),
            total,
            errors.join("\n")
        ));
    }

    if !errors.is_empty() {
        eprintln!(
            "Warning: {} of {} finding(s) failed schema validation and were skipped:\n{}",
            errors.len(),
            total,
            errors.join("\n")
        );
    }

    Ok(parsed)
}

/// Ensure a finding element has a valid UUID v4 in its `id` field.
///
/// If `id` is present but not a valid UUID string, replace it with a freshly
/// generated UUID v4 and print a warning. If `id` is absent altogether,
/// insert a freshly generated UUID v4 silently (the agent is not expected to
/// supply one).
fn fix_finding_uuid(el: &mut serde_json::Value) {
    let obj = match el.as_object_mut() {
        Some(o) => o,
        None => return,
    };

    match obj.get("id").and_then(|v| v.as_str()) {
        Some(id_str) => {
            if uuid::Uuid::parse_str(id_str).is_err() {
                let new_id = uuid::Uuid::new_v4().to_string();
                eprintln!(
                    "Warning: invalid UUID '{}' in agent finding `id` field, \
                     auto-generating replacement '{}'",
                    id_str, new_id
                );
                obj.insert("id".to_string(), serde_json::Value::String(new_id));
            }
        }
        None => {
            let new_id = uuid::Uuid::new_v4().to_string();
            obj.insert("id".to_string(), serde_json::Value::String(new_id));
        }
    }
}

/// Find the last top-level balanced `open`/`close` delimited block in `s`
/// (e.g. `'{'`/`'}'` or `'['`/`']'`), respecting JSON string literals so
/// delimiters inside quoted strings don't confuse the matcher. Used to
/// recover a JSON value embedded in surrounding prose or markdown fences.
fn find_last_balanced_block(s: &str, open: char, close: char) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut start: Option<usize> = None;
    let mut last_block: Option<(usize, usize)> = None;

    for (i, &b) in bytes.iter().enumerate() {
        let c = b as char;
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
        } else if c == open {
            if depth == 0 {
                start = Some(i);
            }
            depth += 1;
        } else if c == close && depth > 0 {
            depth -= 1;
            if depth == 0 {
                if let Some(st) = start {
                    last_block = Some((st, i + 1));
                }
            }
        }
    }

    last_block.map(|(st, en)| &s[st..en])
}

/// Best-effort variant of [`extract_findings`] for use when the agent loop
/// did not reach a clean `Stop` response. Logs a warning instead of failing
/// the task, since there is no complete final answer here to hold to the
/// strict schema.
fn extract_findings_lenient(content: &str) -> Vec<Finding> {
    match extract_findings(content) {
        Ok(findings) => findings,
        Err(e) => {
            if !content.trim().is_empty() {
                eprintln!("Warning: could not extract findings from incomplete agent output: {e}");
            }
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::tests::MockProvider;
    use crate::tools::default_tools;
    use crate::types::{AmbiguityPolicy, ChatResponse, OnIncompletePolicy, ToolCall, VendorConfig};
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
            max_total_tokens: None,
            timeout_secs: 60,
            shell_timeout_secs: 120,
            shell_env_passthrough: vec![],
            ambiguity_policy: AmbiguityPolicy::FailClosed,
            gating_rules: vec![],
            max_iterations: 10,
            on_incomplete: OnIncompletePolicy::Fail,
            mcp_servers: vec![],
            preflight: vec![],
        }
    }

    #[tokio::test]
    async fn test_agent_loop_with_tool_calls() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let tools = default_tools(root.clone(), &[], 120, &[]);

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
            snapshot_mgr: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(!result.findings.is_empty());
        assert!(result.duration_ms > 0);
    }

    #[tokio::test]
    async fn test_agent_loop_halts_on_timeout() {
        let tmp = TempDir::new().unwrap();
        let tools = default_tools(tmp.path().to_path_buf(), &[], 120, &[]);

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
            snapshot_mgr: None,
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
        let tools = default_tools(root.clone(), &[], 120, &[]);

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
            snapshot_mgr: None,
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
        let tools = default_tools(root.clone(), &[], 120, &[]);

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
            snapshot_mgr: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(
            result.truncated,
            "Expected truncated=true when context cannot be reduced further"
        );
    }

    /// Regression test: cumulative billed tokens may exceed `token_budget`
    /// (the context-window budget) without the run being marked incomplete.
    /// Previously the loop conflated the two and broke out as soon as
    /// cumulative usage crossed `token_budget`, failing otherwise-healthy CI
    /// runs closed.
    #[tokio::test]
    async fn test_agent_loop_ignores_cumulative_tokens_for_context_budget() {
        let (_tmp, root) = setup_agent_env();
        let tools = default_tools(root.clone(), &[], 120, &[]);

        let mut contract = test_contract();
        contract.token_budget = 100000;

        let tool_call = ToolCall {
            id: "call_1".into(),
            name: "git_diff".into(),
            arguments: serde_json::json!({}),
        };

        let mut mock = MockProvider::new("test-model");
        for _ in 0..2 {
            mock.add_response(ChatResponse {
                message: Message::new(Role::Assistant, "Running tool..."),
                usage: Usage {
                    input_tokens: 55000,
                    output_tokens: 5000,
                    total_tokens: 60000,
                },
                finish_reason: FinishReason::ToolCalls,
                tool_calls: Some(vec![tool_call.clone()]),
            });
        }
        mock.add_response(ChatResponse {
            message: Message::new(Role::Assistant, r#"{"findings": [{"id": "00000000-0000-0000-0000-000000000000", "rule_id": "test", "severity": "warning", "message": "test finding", "evidence": "test"}]}"#),
            usage: Usage {
                input_tokens: 55000,
                output_tokens: 5000,
                total_tokens: 60000,
            },
            finish_reason: FinishReason::Stop,
            tool_calls: None,
        });

        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, "Review")],
            workspace_root: root.clone(),
            snapshot_mgr: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        // Cumulative billed tokens (180000) far exceed token_budget (100000),
        // but the context itself always fit, so the run completes cleanly.
        assert_eq!(result.usage.total_tokens, 180000);
        assert!(
            !result.truncated,
            "Cumulative token usage must not mark the run incomplete"
        );
        assert!(!result.findings.is_empty());
    }

    /// The optional `max_total_tokens` cap still stops the loop (marked
    /// incomplete) when cumulative billed tokens reach it.
    #[tokio::test]
    async fn test_agent_loop_breaks_on_max_total_tokens() {
        let (_tmp, root) = setup_agent_env();
        let tools = default_tools(root.clone(), &[], 120, &[]);

        let mut contract = test_contract();
        contract.token_budget = 100000;
        contract.max_total_tokens = Some(100000);

        let tool_call = ToolCall {
            id: "call_1".into(),
            name: "git_diff".into(),
            arguments: serde_json::json!({}),
        };

        let mut mock = MockProvider::new("test-model");
        for _ in 0..2 {
            mock.add_response(ChatResponse {
                message: Message::new(Role::Assistant, "Running tool..."),
                usage: Usage {
                    input_tokens: 55000,
                    output_tokens: 5000,
                    total_tokens: 60000,
                },
                finish_reason: FinishReason::ToolCalls,
                tool_calls: Some(vec![tool_call.clone()]),
            });
        }

        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, "Review")],
            workspace_root: root.clone(),
            snapshot_mgr: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        assert_eq!(result.usage.total_tokens, 120000);
        assert!(
            result.truncated,
            "Expected truncated=true once cumulative tokens reach max_total_tokens"
        );
    }

    #[tokio::test]
    async fn test_hint_message_injected_after_truncation() {
        let (_tmp, root) = setup_agent_env();
        let tools = default_tools(root.clone(), &[], 120, &[]);

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
            snapshot_mgr: None,
        };

        let result = run_agent_loop(config).await.unwrap();

        // (a) The hint sits at index 1 — immediately after the system
        // message, at the truncation boundary, not appended at the end.
        assert_eq!(
            result.messages[0].role,
            Role::System,
            "system message must stay at index 0"
        );
        let hint = &result.messages[1];
        assert_eq!(hint.role, Role::User, "hint message must be a user message");
        assert!(
            hint.content.contains("archived at"),
            "expected a hint message about archiving at index 1, got: {}",
            hint.content
        );

        // (b) Every assistant message with tool_calls is immediately
        // followed by Role::Tool messages with matching tool_call_ids.
        // (c) A Role::Tool message only ever appears right after an
        // assistant-with-tool_calls group.
        let msgs = &result.messages;
        assert!(
            msgs.iter()
                .any(|m| m.role == Role::Assistant && m.tool_calls.is_some()),
            "test setup: retained tail must contain an assistant message with tool_calls"
        );
        let mut i = 0;
        while i < msgs.len() {
            let m = &msgs[i];
            if m.role == Role::Assistant {
                if let Some(tool_calls) = &m.tool_calls {
                    let mut j = i + 1;
                    for tc in tool_calls {
                        let tm = msgs.get(j).unwrap_or_else(|| {
                            panic!("tool_call '{}' has no tool result message", tc.id)
                        });
                        assert_eq!(
                            tm.role,
                            Role::Tool,
                            "tool_call '{}' must be followed by a tool message, found {:?} at index {}",
                            tc.id,
                            tm.role,
                            j
                        );
                        assert_eq!(
                            tm.tool_call_id.as_deref(),
                            Some(tc.id.as_str()),
                            "tool message at index {} must carry matching tool_call_id",
                            j
                        );
                        j += 1;
                    }
                    i = j;
                    continue;
                }
            }
            assert_ne!(
                m.role,
                Role::Tool,
                "tool message at index {} does not follow an assistant-with-tool_calls group",
                i
            );
            i += 1;
        }
    }

    #[tokio::test]
    async fn test_agent_loop_propagates_tool_call_id() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let tools = default_tools(root.clone(), &[], 120, &[]);

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
            snapshot_mgr: None,
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

    // -----------------------------------------------------------------
    // extract_findings / extract_findings_lenient
    // -----------------------------------------------------------------

    #[test]
    fn test_extract_findings_valid() {
        let content = r#"{"findings": [{"id": "00000000-0000-0000-0000-000000000000", "rule_id": "test", "severity": "warning", "message": "test finding", "evidence": "test"}]}"#;
        let findings = extract_findings(content).expect("should parse");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "test");
    }

    #[test]
    fn test_extract_findings_empty_is_ok() {
        let findings = extract_findings(r#"{"findings": []}"#).expect("should parse");
        assert!(findings.is_empty());
    }

    #[test]
    fn test_extract_findings_bare_array_fallback() {
        let content = r#"[{"id": "00000000-0000-0000-0000-000000000000", "rule_id": "test", "severity": "error", "message": "m", "evidence": "e"}]"#;
        let findings = extract_findings(content).expect("should parse");
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn test_extract_findings_invalid_json_is_error() {
        let err = extract_findings("not json at all").unwrap_err();
        assert!(err.contains("not valid JSON"), "got: {err}");
    }

    #[test]
    fn test_extract_findings_recovers_json_after_reasoning_prose() {
        // Real observed model behavior: an explanation sentence, a blank
        // line, then the actual JSON answer. The whole response isn't valid
        // JSON on its own, but the trailing JSON block should be recovered.
        let content = "Both candidates are pre-existing `as any` casts that \
             already existed before this diff, so nothing new was introduced.\n\n\
             {\"findings\": []}";
        let findings = extract_findings(content).expect("should recover trailing JSON");
        assert!(findings.is_empty());
    }

    #[test]
    fn test_extract_findings_recovers_json_from_markdown_fence() {
        let content = "```json\n{\"findings\": [{\"id\": \"00000000-0000-0000-0000-000000000000\", \"rule_id\": \"r\", \"severity\": \"error\", \"message\": \"m\", \"evidence\": \"e\"}]}\n```";
        let findings = extract_findings(content).expect("should recover fenced JSON");
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn test_extract_findings_recovered_json_still_enforces_schema() {
        // Recovering JSON from surrounding prose must NOT weaken schema
        // validation of the findings inside it -- this is the regression
        // the original fix targeted.
        let content = "Here is my answer:\n\n\
             {\"findings\": [{\"rule_id\": \"no-new-any\", \"severity\": \"error\", \"file\": \"a.ts\", \"line\": 1, \"title\": \"t\"}]}";
        let err = extract_findings(content).unwrap_err();
        assert!(err.contains("1 of 1 finding(s) failed"), "got: {err}");
    }

    #[test]
    fn test_extract_findings_no_recoverable_json_is_still_error() {
        let err = extract_findings("I looked at the diff and found nothing notable.").unwrap_err();
        assert!(err.contains("no embedded JSON"), "got: {err}");
    }

    #[test]
    fn test_find_last_balanced_block_ignores_braces_in_strings() {
        let s = r#"prose with a "{not a real block}" quoted aside {"real": "block"}"#;
        let block = find_last_balanced_block(s, '{', '}').unwrap();
        assert_eq!(block, r#"{"real": "block"}"#);
    }

    #[test]
    fn test_find_last_balanced_block_picks_last_of_several() {
        let s = r#"{"first": 1} then later {"second": 2}"#;
        let block = find_last_balanced_block(s, '{', '}').unwrap();
        assert_eq!(block, r#"{"second": 2}"#);
    }

    #[test]
    fn test_find_last_balanced_block_none_when_absent() {
        assert!(find_last_balanced_block("no braces here", '{', '}').is_none());
    }

    #[test]
    fn test_extract_findings_schema_mismatch_is_error_not_silently_dropped() {
        // Old field names (file/line/title/description) instead of the real
        // Finding schema (id/message/evidence/location) -- this is exactly
        // the painttyServer bug: every element fails to deserialize, and
        // that must surface as an error, not as an empty, "successful" result.
        let content = r#"{"findings": [{"rule_id": "no-new-any", "severity": "error", "file": "a.ts", "line": 1, "title": "t", "description": "d", "recommendation": "r"}]}"#;
        let err = extract_findings(content).unwrap_err();
        assert!(err.contains("1 of 1 finding(s) failed"), "got: {err}");
        assert!(err.contains("findings[0]"), "got: {err}");
    }

    #[test]
    fn test_extract_findings_skips_malformed_elements() {
        // One valid finding and one malformed one (missing message/evidence):
        // the valid one is returned, the malformed one is skipped with a
        // warning.  A single bad element must not discard the whole batch.
        let content = r#"{"findings": [
            {"id": "00000000-0000-0000-0000-000000000000", "rule_id": "ok", "severity": "error", "message": "m", "evidence": "e"},
            {"rule_id": "bad", "severity": "error", "file": "a.ts", "line": 1, "title": "t"}
        ]}"#;
        let findings = extract_findings(content).expect("should return the valid finding");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "ok");
    }

    #[test]
    fn test_extract_findings_partial_schema_mismatch_with_all_bad_still_errors() {
        // When ALL findings are malformed, extraction must still error
        // (otherwise a real schema mismatch could be silently ignored).
        let content = r#"{"findings": [
            {"rule_id": "bad1", "severity": "error", "file": "a.ts", "line": 1, "title": "t1"},
            {"rule_id": "bad2", "severity": "error", "file": "b.ts", "line": 2, "title": "t2"}
        ]}"#;
        let err = extract_findings(content).unwrap_err();
        assert!(err.contains("2 of 2 finding(s) failed"), "got: {err}");
    }

    #[test]
    fn test_extract_findings_auto_generates_missing_uuid() {
        // Agent omitted the `id` field — it should be auto-generated.
        let content = r#"{"findings": [
            {"rule_id": "r", "severity": "error", "message": "m", "evidence": "e"}
        ]}"#;
        let findings = extract_findings(content).expect("should auto-generate UUID");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "r");
        // The UUID must not be nil (which would indicate no generation).
        assert!(
            !findings[0].id.is_nil(),
            "UUID should be auto-generated, not nil"
        );
    }

    #[test]
    fn test_extract_findings_fixes_malformed_uuid() {
        // Agent supplied an invalid UUID (11-char group 4 instead of 12).
        // Regression test for painttyServer PR #547.
        let content = r#"{"findings": [
            {"id": "3c4d5e6f-789a-bcde-f012-3456789abcd", "rule_id": "r", "severity": "error", "message": "m", "evidence": "e"}
        ]}"#;
        let findings = extract_findings(content).expect("should fix malformed UUID");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "r");
        assert!(!findings[0].id.is_nil(), "UUID should be replaced, not nil");
        // The replacement must NOT be the original malformed string.
        assert_ne!(
            findings[0].id.to_string(),
            "3c4d5e6f-789a-bcde-f012-3456789abcd",
            "malformed UUID must be replaced"
        );
    }

    #[test]
    fn test_extract_findings_lenient_swallows_errors() {
        // Used only for the incomplete-loop fallback path: never panics,
        // never propagates, just returns empty on anything malformed.
        assert!(extract_findings_lenient("not json").is_empty());
        assert!(extract_findings_lenient("").is_empty());
        assert!(
            extract_findings_lenient(r#"{"findings": [{"rule_id": "bad", "file": "a.ts"}]}"#)
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_agent_loop_errors_on_schema_mismatch_instead_of_empty_findings() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let tools = default_tools(root.clone(), &[], 120, &[]);

        let mut mock = MockProvider::new("gpt-4o");
        mock.add_response(ChatResponse {
            message: Message::new(
                Role::Assistant,
                r#"{"findings": [{"rule_id": "no-new-any", "severity": "error", "file": "a.ts", "line": 1, "title": "t", "description": "d"}]}"#.to_string(),
            ),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
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
            workspace_root: root,
            snapshot_mgr: None,
        };

        let result = run_agent_loop(config).await;
        let err = result.expect_err(
            "a Stop response with findings that fail schema validation must error, \
             not silently succeed with 0 findings",
        );
        assert!(matches!(err, ProviderError::MalformedFindings(_)));
    }
}
