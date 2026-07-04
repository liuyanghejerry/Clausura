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
                truncated = true;
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
    // Best-effort extraction with a warning is appropriate here; `truncated`
    // already signals to the caller that this result may be incomplete.
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
/// Every element of the resulting array MUST deserialize into a `Finding` —
/// if no JSON can be recovered at all, or if one or more elements fail
/// schema validation, this returns `Err` with a diagnostic rather than
/// silently dropping the offending elements. A schema mismatch between what
/// a task's prompt asks the model to emit and what `Finding` actually
/// requires must fail loudly: silently treating it as "no findings" would
/// let a real violation slip through CI gating undetected. Tolerating
/// incidental prose/fences around otherwise-valid JSON is a separate,
/// narrower concession — it does not weaken that guarantee.
fn extract_findings(content: &str) -> Result<Vec<Finding>, String> {
    let trimmed = content.trim();

    let json: serde_json::Value = match serde_json::from_str(trimmed) {
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
    let findings_value = json.get("findings").cloned().unwrap_or(json);
    let elements = findings_value
        .as_array()
        .ok_or_else(|| format!("expected a `findings` array, got: {findings_value}"))?;

    let mut parsed = Vec::with_capacity(elements.len());
    let mut errors = Vec::new();
    for (i, el) in elements.iter().enumerate() {
        match serde_json::from_value::<Finding>(el.clone()) {
            Ok(f) => parsed.push(f),
            Err(e) => errors.push(format!("findings[{i}]: {e} (raw: {el})")),
        }
    }

    if !errors.is_empty() {
        return Err(format!(
            "{} of {} finding(s) failed to match the Finding schema:\n{}",
            errors.len(),
            elements.len(),
            errors.join("\n")
        ));
    }

    Ok(parsed)
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
            max_iterations: 10,
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
            snapshot_mgr: None,
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
            snapshot_mgr: None,
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
            snapshot_mgr: None,
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
    fn test_extract_findings_partial_schema_mismatch_is_error() {
        // One valid finding and one malformed one: the malformed one must
        // not be silently dropped just because its sibling parsed fine.
        let content = r#"{"findings": [
            {"id": "00000000-0000-0000-0000-000000000000", "rule_id": "ok", "severity": "error", "message": "m", "evidence": "e"},
            {"rule_id": "bad", "severity": "error", "file": "a.ts", "line": 1, "title": "t"}
        ]}"#;
        let err = extract_findings(content).unwrap_err();
        assert!(err.contains("1 of 2 finding(s) failed"), "got: {err}");
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
        let tools = default_tools(root.clone(), &[]);

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
