use crate::context::ContextManager;
use crate::eventlog::{EventLog, RunEvent};
use crate::provider::{is_context_overflow_err, Provider};
use crate::snapshot::SnapshotManager;
use crate::tools::ToolRegistry;
use crate::types::{Finding, FinishReason, Message, ProviderError, Role, TaskContract, Usage};
use std::collections::HashMap;
use std::path::Path;
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
    /// Append-only run event log (LLM requests/responses, tool calls,
    /// truncation and checkpoint events). Optional — tests and callers that
    /// don't need the audit trail pass `None`.
    pub event_log: Option<&'a EventLog>,
}

/// Number of corrective "reply with JSON only" attempts per recovery point.
const MAX_RECOVERY_RETRIES: usize = 2;

/// Bounded corrective retry for a final answer whose findings JSON does not
/// parse. Models occasionally end a run with prose or markdown instead of the
/// schema'd `findings` JSON; a nudge asking for a JSON-only reply recovers the
/// run instead of failing it. Returns the first parseable findings or the last
/// extraction error. Each attempt is a billed LLM call and respects the
/// cumulative token cap and the task deadline. The corrective prompt carries
/// the specific parse error and the expected schema, and every attempt is
/// recorded in the run event log when present.
#[allow(clippy::too_many_arguments)]
async fn recover_findings_json(
    contract: &TaskContract,
    provider: &dyn Provider,
    tools: &ToolRegistry,
    messages: &mut Vec<Message>,
    start: &Instant,
    running_tokens: &mut u64,
    total_usage: &mut Usage,
    event_log: Option<&EventLog>,
    first_err: &str,
) -> Result<Vec<Finding>, String> {
    let mut last_err = first_err.to_string();
    for _ in 0..MAX_RECOVERY_RETRIES {
        if start.elapsed() > Duration::from_secs(contract.timeout_secs) {
            break;
        }
        if let Some(max_total) = contract.max_total_tokens {
            if *running_tokens >= max_total {
                break;
            }
        }
        messages.push(Message::new(Role::User, findings_retry_prompt(&last_err)));
        let retry = match call_llm(provider, tools, messages, event_log).await {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("corrective call failed: {e}");
                break;
            }
        };
        total_usage.input_tokens += retry.usage.input_tokens;
        total_usage.output_tokens += retry.usage.output_tokens;
        total_usage.total_tokens += retry.usage.total_tokens;
        *running_tokens += retry.usage.total_tokens;
        messages.push(Message::new(Role::Assistant, retry.message.content.clone()));
        match extract_findings(&retry.message.content) {
            Ok(findings) => return Ok(findings),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
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

    if let Some(log) = config.event_log {
        log.append(&RunEvent::RunStart {
            task_id: config.contract.id.clone(),
            model: config.contract.model.clone(),
            workspace: config.workspace_root.display().to_string(),
        });
    }

    let cm = ContextManager::new(
        config.provider,
        config.contract.token_budget,
        config.workspace_root.clone(),
    );

    // Auto-compact state: summarize dropped messages instead of bare
    // truncation, bounded by a per-run call cap to prevent compaction loops.
    let auto_compact = config.contract.auto_compact;
    let max_compactions = config.contract.max_compactions;
    let mut compactions_used: u32 = 0;

    // Loop hygiene: consecutive identical (tool, args) call counts, keyed by
    // the canonical invocation. See `repeat_reminder_text` for thresholds.
    let mut repeat_counts: HashMap<String, u32> = HashMap::new();

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
            // Proactive compaction: the heuristic counter says we're closing
            // in on the budget. Truncate now, before the provider rejects the
            // request outright.
            if !truncate_and_hint(
                config.provider,
                &cm,
                &mut messages,
                &config.contract.id,
                config.contract.token_budget,
                config.contract.max_total_tokens,
                auto_compact,
                max_compactions,
                &mut compactions_used,
                &mut running_tokens,
                &mut total_usage,
                config.event_log,
                false,
            )
            .await
            {
                // Context could not be reduced further: fall-through below
                // marks the run truncated.
                break;
            }
            continue;
        }

        let response = match call_llm(config.provider, config.tools, &messages, config.event_log)
            .await
        {
            Ok(resp) => resp,
            Err(e) if is_context_overflow_err(&e) => {
                // Reactive compaction: the provider rejected the request as
                // over the context window. Truncate + archive (optionally
                // auto-compact) and retry the same request exactly once.
                tracing::warn!(
                    reason = %e,
                    "provider reported context overflow; compacting and retrying once"
                );
                if !truncate_and_hint(
                    config.provider,
                    &cm,
                    &mut messages,
                    &config.contract.id,
                    config.contract.token_budget,
                    config.contract.max_total_tokens,
                    auto_compact,
                    max_compactions,
                    &mut compactions_used,
                    &mut running_tokens,
                    &mut total_usage,
                    config.event_log,
                    true,
                )
                .await
                {
                    break;
                }
                match call_llm(config.provider, config.tools, &messages, config.event_log).await {
                    Ok(resp) => resp,
                    Err(e2) if is_context_overflow_err(&e2) => {
                        tracing::warn!(
                            reason = %e2,
                            "context overflow persisted after compaction; marking run truncated"
                        );
                        break;
                    }
                    Err(e2) => return Err(e2),
                }
            }
            Err(e) => return Err(e),
        };

        total_usage.input_tokens += response.usage.input_tokens;
        total_usage.output_tokens += response.usage.output_tokens;
        total_usage.total_tokens += response.usage.total_tokens;
        running_tokens += response.usage.total_tokens;

        // Persist any findings in this response to the on-disk ledger so they
        // survive later context truncation/compaction and can be merged back
        // into the final answer. Best-effort: a failed write is ignored.
        if config.contract.findings_ledger {
            let interim = extract_findings_lenient(&response.message.content);
            if !interim.is_empty() {
                let ledger = ledger_path(&config.workspace_root, &config.contract.id);
                if let Err(e) = append_findings_to_ledger(&ledger, &interim) {
                    tracing::debug!(reason = %e, "findings ledger write failed (ignored)");
                }
            }
        }

        match response.finish_reason {
            FinishReason::Stop => {
                messages.push(Message::new(
                    Role::Assistant,
                    response.message.content.clone(),
                ));

                // Strict parse; on failure, ask for a JSON-only reply before
                // giving up (bounded, billed, budget-respecting). The prompt
                // carries the specific parse error and the expected schema.
                let findings = match extract_findings(&response.message.content) {
                    Ok(findings) => findings,
                    Err(err) => recover_findings_json(
                        config.contract,
                        config.provider,
                        config.tools,
                        &mut messages,
                        &start,
                        &mut running_tokens,
                        &mut total_usage,
                        config.event_log,
                        &err,
                    )
                    .await
                    .map_err(ProviderError::MalformedFindings)?,
                };

                // Merge findings persisted earlier in the run (which may have
                // been truncated out of context since) back into the final
                // set, so the final answer is never missing early iterations.
                let findings = if config.contract.findings_ledger {
                    let ledger = read_ledger_findings(&ledger_path(
                        &config.workspace_root,
                        &config.contract.id,
                    ));
                    merge_with_ledger(findings, ledger)
                } else {
                    findings
                };

                if let Some(log) = config.event_log {
                    log.append(&RunEvent::RunEnd {
                        truncated,
                        duration_ms: start.elapsed().as_millis() as u64,
                    });
                }

                return Ok(AgentResult {
                    messages,
                    findings,
                    usage: total_usage,
                    duration_ms: start.elapsed().as_millis() as u64,
                    truncated,
                });
            }
            FinishReason::ToolCalls => {
                // Loop hygiene: track identical (tool, args) repetitions
                // across iterations and inject an escalating advisory
                // reminder after this batch's tool results.
                let mut repeat_reminders: Vec<String> = Vec::new();
                if let Some(tool_calls) = response.tool_calls {
                    messages.push(Message {
                        role: Role::Assistant,
                        // Preserve the assistant's text: models commonly emit
                        // reasoning/progress (and findings drafts) alongside
                        // tool calls; discarding it loses interim findings
                        // before they can reach the final answer.
                        content: response.message.content.clone(),
                        tool_call_id: None,
                        tool_calls: Some(tool_calls.clone()),
                    });

                    for tc in &tool_calls {
                        if let Some(log) = config.event_log {
                            log.append(&RunEvent::ToolCall {
                                call_id: tc.id.clone(),
                                name: tc.name.clone(),
                                arguments: tc.arguments.clone(),
                            });
                        }
                        let key = repeat_key(&tc.name, &tc.arguments);
                        let count = repeat_counts
                            .entry(key)
                            .and_modify(|c| *c += 1)
                            .or_insert(1);
                        if REPEAT_REMINDER_THRESHOLDS.contains(count) {
                            repeat_reminders.push(repeat_reminder_text(&tc.name, *count));
                        }
                        match config.tools.get(&tc.name) {
                            Some(tool) => {
                                let result = tool.execute(tc.arguments.clone()).await;
                                match result {
                                    Ok(output) => {
                                        if let Some(log) = config.event_log {
                                            log.append(&RunEvent::ToolResult {
                                                call_id: tc.id.clone(),
                                                name: tc.name.clone(),
                                                output: output.clone(),
                                                error: None,
                                            });
                                        }
                                        messages.push(Message::with_tool_call(
                                            Role::Tool,
                                            output,
                                            tc.id.clone(),
                                        ));
                                    }
                                    Err(e) => {
                                        if let Some(log) = config.event_log {
                                            log.append(&RunEvent::ToolResult {
                                                call_id: tc.id.clone(),
                                                name: tc.name.clone(),
                                                output: format!("Error: {}", e),
                                                error: Some(format!("{}", e)),
                                            });
                                        }
                                        messages.push(Message::with_tool_call(
                                            Role::Tool,
                                            format!("Error: {}", e),
                                            tc.id.clone(),
                                        ));
                                    }
                                }
                            }
                            None => {
                                if let Some(log) = config.event_log {
                                    log.append(&RunEvent::ToolResult {
                                        call_id: tc.id.clone(),
                                        name: tc.name.clone(),
                                        output: format!("Error: Tool '{}' not found", tc.name),
                                        error: Some(format!("Tool '{}' not found", tc.name)),
                                    });
                                }
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

                // Inject any repeat-call reminders after all tool results of
                // this batch, so tool-call/result pairing stays valid for the
                // next request.
                if !repeat_reminders.is_empty() {
                    messages.push(Message::new(Role::User, repeat_reminders.join("\n\n")));
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
                if let Ok(checkpoint_id) =
                    mgr.save_snapshot(&config.contract.id, &messages, truncated)
                {
                    if let Some(log) = config.event_log {
                        log.append(&RunEvent::Checkpoint {
                            checkpoint_id: checkpoint_id.to_string(),
                            messages: messages.clone(),
                            truncated,
                        });
                    }
                }
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
    let mut findings = extract_findings_lenient(&last_content);

    // One more bounded attempt can rescue the run: if nothing parseable came
    // out, ask for a JSON-only findings reply. A successful recovery counts as
    // a complete final answer (truncated=false), so gating evaluates it
    // normally instead of the caller failing closed on `on_incomplete`.
    if findings.is_empty() {
        match recover_findings_json(
            config.contract,
            config.provider,
            config.tools,
            &mut messages,
            &start,
            &mut running_tokens,
            &mut total_usage,
            config.event_log,
            "no findings JSON could be extracted from the final answer",
        )
        .await
        {
            Ok(recovered) => {
                findings = recovered;
                truncated = false;
            }
            Err(_) => {
                // Stay incomplete; the lenient (possibly ledger-merged)
                // findings and the truncated flag drive the caller's policy.
            }
        }
    }

    // Best-effort merge of ledger findings for the incomplete path too.
    let findings = if config.contract.findings_ledger {
        let ledger =
            read_ledger_findings(&ledger_path(&config.workspace_root, &config.contract.id));
        merge_with_ledger(findings, ledger)
    } else {
        findings
    };

    if let Some(log) = config.event_log {
        log.append(&RunEvent::RunEnd {
            truncated,
            duration_ms: start.elapsed().as_millis() as u64,
        });
    }

    Ok(AgentResult {
        messages,
        findings,
        usage: total_usage,
        duration_ms: start.elapsed().as_millis() as u64,
        truncated,
    })
}

/// Dispatch one LLM request with tool definitions, recording the full
/// request and response in the run event log when present.
async fn call_llm(
    provider: &dyn Provider,
    tools: &ToolRegistry,
    messages: &[Message],
    event_log: Option<&EventLog>,
) -> Result<crate::types::ChatResponse, ProviderError> {
    if let Some(log) = event_log {
        log.append(&RunEvent::LlmRequest {
            messages: messages.to_vec(),
        });
    }
    let response = provider
        .chat_with_tools(messages, tools.list_definitions().as_slice())
        .await?;
    if let Some(log) = event_log {
        log.append(&RunEvent::LlmResponse {
            message: response.message.clone(),
            usage: response.usage.clone(),
            finish_reason: response.finish_reason.clone(),
            tool_calls: response.tool_calls.clone(),
        });
    }
    Ok(response)
}

/// Truncate the conversation to fit the budget, archive the dropped messages,
/// optionally summarize them (auto-compact), and inject the hint at the
/// truncation boundary.
///
/// When `force` is true (reactive path — the provider already rejected the
/// request as over its context window), truncation targets 50% of the budget
/// even if the heuristic counter thinks the context fits: the 4-chars/token
/// estimate systematically underestimates code. The proactive path keeps the
/// normal 75% target.
///
/// Returns `true` when truncation was applied and the context is back under
/// the truncation threshold; `false` when the context could not be reduced
/// further (the caller must stop and mark the run truncated).
#[allow(clippy::too_many_arguments)]
async fn truncate_and_hint(
    provider: &dyn Provider,
    cm: &ContextManager<'_>,
    messages: &mut Vec<Message>,
    task_id: &str,
    token_budget: u64,
    max_total_tokens: Option<u64>,
    auto_compact: bool,
    max_compactions: u32,
    compactions_used: &mut u32,
    running_tokens: &mut u64,
    total_usage: &mut Usage,
    event_log: Option<&EventLog>,
    force: bool,
) -> bool {
    let snapshot = messages.clone();
    let (was_truncated, count) = if force {
        // Reactive: reduce to 50% of the budget, not 75% — the provider's
        // rejection is stronger evidence than our heuristic estimate.
        let dropped = cm.truncate_to(messages, 0.5);
        (dropped > 0, dropped)
    } else {
        cm.truncate_to_budget(messages)
    };
    if !was_truncated || count == 0 {
        return false;
    }

    let dropped_end = 1 + (snapshot.len() - messages.len());
    let dropped: Vec<Message> = snapshot[1..dropped_end].to_vec();

    let archive_result = cm.archive(&dropped, task_id).await;

    // Auto-compact: summarize the dropped messages with a single
    // no-tool LLM call and inject the summary at the truncation
    // boundary. Any guard failure or LLM error falls back to the
    // bare "context trimmed" hint — compaction must never fail
    // the run.
    let compact_summary: Option<String> = match &archive_result {
        Ok(path) if auto_compact && *compactions_used < max_compactions => {
            let tail_tokens = cm.count_tokens(messages);
            match try_compact(
                provider,
                cm,
                &dropped,
                path,
                token_budget,
                max_total_tokens,
                tail_tokens,
                running_tokens,
                total_usage,
            )
            .await
            {
                CompactOutcome::Ok { summary } => {
                    *compactions_used += 1;
                    Some(summary)
                }
                CompactOutcome::Skipped | CompactOutcome::Failed => None,
            }
        }
        _ => None,
    };

    let hint = match (&archive_result, &compact_summary) {
        // Insert at the truncation boundary (right after the
        // system message), not at the end: appending a User
        // message after an assistant message with tool_calls
        // would leave those calls without results, which the
        // OpenAI/Anthropic APIs reject.
        (Ok(path), Some(summary)) => compact_hint(dropped.len(), path, summary),
        (Ok(path), None) => format!(
            "⚠️ Context was trimmed to stay within token budget.\n\
             {} earlier messages are archived at:\n  {}\n\
             Use read_file to inspect if you need context from earlier iterations.",
            dropped.len(),
            path.display(),
        ),
        (Err(_), _) => format!(
            "⚠️ Context was trimmed to stay within token budget.\n\
             {} earlier messages were dropped (archive unavailable).",
            dropped.len(),
        ),
    };

    messages.insert(1, Message::new(Role::User, hint));

    if let Some(log) = event_log {
        log.append(&RunEvent::ContextTruncated {
            dropped_count: dropped.len(),
            archive_path: archive_result
                .as_ref()
                .ok()
                .map(|p| p.display().to_string()),
            compacted_summary: compact_summary.clone(),
        });
    }

    // False when the summary/hint pushed the context back over the threshold
    // (heuristic drift) — the caller stops and marks the run truncated.
    !cm.should_truncate(messages)
}

// ---------------------------------------------------------------------------
// Repeat tool-call reminder (loop hygiene)
// ---------------------------------------------------------------------------

/// Call counts at which an escalating reminder is injected. Pure advisory:
/// the agent may legitimately repeat a call (e.g. paging a file), so the
/// reminder never blocks execution — it only nudges the model to re-evaluate.
const REPEAT_REMINDER_THRESHOLDS: [u32; 3] = [3, 5, 8];

/// Canonical identity of a tool invocation: name plus arguments with object
/// keys sorted recursively, so argument order differences don't mask repeats.
fn repeat_key(name: &str, args: &serde_json::Value) -> String {
    format!("{}|{}", name, canonical_json(args))
}

/// Render a JSON value with object keys sorted recursively. Values are
/// compared for repeat-detection purposes only.
fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut pairs: Vec<(String, String)> = map
                .iter()
                .map(|(k, v)| (k.clone(), canonical_json(v)))
                .collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            let inner = pairs
                .iter()
                .map(|(k, v)| {
                    let key = serde_json::to_string(k).unwrap_or_default();
                    format!("{key}:{v}")
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{inner}}}")
        }
        serde_json::Value::Array(arr) => {
            let inner = arr.iter().map(canonical_json).collect::<Vec<_>>().join(",");
            format!("[{inner}]")
        }
        other => other.to_string(),
    }
}

/// Build the escalating reminder for repeat count `count` of tool `name`.
fn repeat_reminder_text(name: &str, count: u32) -> String {
    match count {
        3 => format!(
            "⚠️ Repeat-call reminder: you have called `{name}` with identical \
             arguments 3 times in a row. If the result is not changing, try a \
             narrower query, different arguments, or another tool."
        ),
        5 => format!(
            "⚠️ Repeat-call reminder: you have called `{name}` with identical \
             arguments 5 times in a row. This looks like a loop — stop and \
             re-evaluate your approach before the iteration budget runs out."
        ),
        _ => format!(
            "⚠️ Repeat-call reminder: you have called `{name}` with identical \
             arguments {count} times in a row. You appear stuck in a loop. \
             Change the inputs, switch tools, or conclude the review now."
        ),
    }
}

// ---------------------------------------------------------------------------
// Auto-compact
// ---------------------------------------------------------------------------

/// Outcome of an auto-compact attempt.
#[derive(Debug)]
enum CompactOutcome {
    /// Summary produced and billed; the text is ready to inject.
    Ok { summary: String },
    /// Compaction skipped by a guard (insufficient `max_total_tokens` headroom).
    Skipped,
    /// The summarization LLM call failed; callers fall back to the bare hint.
    Failed,
}

/// Serialize dropped messages to plain text for the summarization call.
///
/// Tool results are rendered inline (role + content) rather than as
/// structured `tool` messages: this keeps the summarization input portable
/// across providers (Anthropic's message converter maps `tool` messages to
/// `tool_result` with a placeholder id, which is fragile outside the agent
/// loop) and avoids feeding half-paired tool_calls into the summary.
fn dropped_to_text(dropped: &[Message]) -> String {
    let mut out = String::new();
    for m in dropped {
        match m.role {
            Role::System => out.push_str("System:\n"),
            Role::User => out.push_str("User:\n"),
            Role::Assistant => {
                if let Some(calls) = &m.tool_calls {
                    let names: Vec<&str> = calls.iter().map(|c| c.name.as_str()).collect();
                    out.push_str(&format!("Assistant (tool calls: {}):\n", names.join(", ")));
                } else {
                    out.push_str("Assistant:\n");
                }
            }
            Role::Tool => match &m.tool_call_id {
                Some(tcid) => out.push_str(&format!("Tool result ({}):\n", tcid)),
                None => out.push_str("Tool result:\n"),
            },
        }
        out.push_str(&m.content);
        out.push('\n');
        out.push('\n');
    }
    out
}

/// Render the "context compacted" hint injected at the truncation boundary.
///
/// The fixed template text is measured by `try_compact` to size the summary,
/// so this template must stay byte-identical between the two call sites —
/// the summary budget is computed from the text the hint will actually carry.
fn compact_hint(dropped_len: usize, archive_path: &Path, summary: &str) -> String {
    format!(
        "⚠️ Context was compacted to stay within token budget.\n\
         {} earlier messages were summarized below; the full transcript is archived at:\n  {}\n\
         Use read_file to inspect the archive if you need details from earlier iterations.\n\n\
         --- COMPACTED SUMMARY ---\n{}\n--- END SUMMARY ---",
        dropped_len,
        archive_path.display(),
        summary,
    )
}

/// Build the summarization request messages (system instruction + the
/// dropped conversation as a single user message).
fn compact_request_messages(dropped_text: &str, archive_path: &Path) -> Vec<Message> {
    let system = format!(
        "You are compacting a segment of a code-review conversation for a CI agent.\n\
         Produce a concise summary that preserves, verbatim where possible:\n\
         - every finding reported so far (rule_id, severity, message, evidence, location)\n\
         - key facts learned from tool outputs (files examined, diffs reviewed, test results)\n\
         - decisions made and open questions\n\
         Do NOT invent findings, facts, or conclusions that are not present in the messages.\n\
         The full transcript is archived at {} for reference.",
        archive_path.display()
    );
    vec![
        Message::new(Role::System, system),
        Message::new(Role::User, dropped_text.to_string()),
    ]
}

/// Attempt to summarize `dropped` with a single no-tool LLM call.
///
/// Guards (all checked before spending a request):
/// - `max_total_tokens` headroom: the estimated cost of the summary call
///   (input + capped output) must fit under the remaining quota, otherwise
///   compaction is skipped so it cannot eat the quota and cut the run short
///   earlier than bare truncation would.
/// - Summary output is sized to the headroom the retained tail leaves under
///   the truncation threshold (80% of budget), capped at 10% of
///   `token_budget`; oversized summaries are trimmed so a successful
///   compaction can never push the context back over the threshold and mark
///   the run incomplete.
/// - If the headroom is too small for a meaningful summary, compaction is
///   skipped (bare truncation hint).
///
/// Usage from the summarization call is added to `running_tokens` (the
/// `max_total_tokens` accumulator) and `total_usage`.
#[allow(clippy::too_many_arguments)]
async fn try_compact(
    provider: &dyn Provider,
    cm: &ContextManager<'_>,
    dropped: &[Message],
    archive_path: &Path,
    token_budget: u64,
    max_total_tokens: Option<u64>,
    tail_tokens: u64,
    running_tokens: &mut u64,
    total_usage: &mut Usage,
) -> CompactOutcome {
    let request = compact_request_messages(&dropped_to_text(dropped), archive_path);

    // Size the summary to the space left under the truncation threshold
    // (80% of budget). A fixed cap would let "75% tail + 10% summary + hint"
    // overflow the 80% line: `should_truncate` after injection would then
    // mark the run incomplete right after a *successful* compaction,
    // defeating the feature's purpose.
    let threshold = (token_budget as f64 * 0.8) as u64;
    let hint_fixed_tokens =
        provider.count_tokens(&compact_hint(dropped.len(), archive_path, "")) + 1; // +1 per-message overhead

    // Absorb the ±few-token rounding drift of the heuristic counters.
    const ROUNDING_MARGIN: u64 = 8;
    const MIN_SUMMARY_TOKENS: u64 = 200;
    let headroom = threshold
        .saturating_sub(tail_tokens)
        .saturating_sub(hint_fixed_tokens)
        .saturating_sub(ROUNDING_MARGIN);
    if headroom < MIN_SUMMARY_TOKENS {
        tracing::debug!(
            tail_tokens,
            headroom,
            "auto-compact skipped: no room for a summary under the truncation threshold"
        );
        return CompactOutcome::Skipped;
    }
    let budget_ceiling = ((token_budget as f64) * 0.10).max(200.0) as u64;
    let max_summary_tokens = headroom.min(budget_ceiling);

    // Quota guard: never spend the last of max_total_tokens on a summary.
    let input_estimate = cm.count_tokens(&request);
    if let Some(cap) = max_total_tokens {
        if *running_tokens + input_estimate + max_summary_tokens > cap {
            tracing::debug!(
                input_estimate,
                max_summary_tokens,
                running = *running_tokens,
                cap,
                "auto-compact skipped: insufficient max_total_tokens headroom"
            );
            return CompactOutcome::Skipped;
        }
    }

    match provider.chat(&request).await {
        Ok(response) => {
            let mut summary = response.message.content.clone();
            if provider.count_tokens(&summary) > max_summary_tokens {
                summary = truncate_summary_to_budget(provider, &summary, max_summary_tokens);
            }
            *running_tokens += response.usage.total_tokens;
            total_usage.input_tokens += response.usage.input_tokens;
            total_usage.output_tokens += response.usage.output_tokens;
            total_usage.total_tokens += response.usage.total_tokens;
            tracing::info!(
                dropped_messages = dropped.len(),
                input_tokens = response.usage.input_tokens,
                output_tokens = response.usage.output_tokens,
                summary_chars = summary.len(),
                "auto-compact: summarized dropped messages"
            );
            CompactOutcome::Ok { summary }
        }
        Err(e) => {
            tracing::warn!(
                reason = %e,
                "auto-compact failed, falling back to truncation hint"
            );
            CompactOutcome::Failed
        }
    }
}

/// Trim `summary` to the largest char prefix that keeps the trimmed result
/// (including the truncation marker) within `cap_tokens`, using the
/// provider's token heuristic. Appends a marker so the agent knows the
/// summary was cut.
fn truncate_summary_to_budget(provider: &dyn Provider, summary: &str, cap_tokens: u64) -> String {
    const MARKER: &str = "\n\n[summary truncated to fit token budget]";
    // Already within budget: pass through untouched.
    if provider.count_tokens(summary) <= cap_tokens {
        return summary.to_string();
    }
    let chars: Vec<char> = summary.chars().collect();
    let mut low = 0usize;
    let mut high = chars.len();
    while low < high {
        let mid = (low + high).div_ceil(2);
        let prefix: String = chars[..mid].iter().collect();
        let candidate = format!("{}{}", prefix, MARKER);
        if provider.count_tokens(&candidate) <= cap_tokens {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let mut trimmed: String = chars[..low].iter().collect();
    trimmed.push_str(MARKER);
    trimmed
}

// ---------------------------------------------------------------------------
// Findings ledger
// ---------------------------------------------------------------------------

/// Path to the findings ledger for a task: one Finding per JSON line.
/// Lives next to the context archives so cleanup can sweep both.
fn ledger_path(workspace_root: &Path, task_id: &str) -> PathBuf {
    workspace_root
        .join(".clausura")
        .join("archives")
        .join(format!("findings-ledger-{}.jsonl", task_id))
}

/// Append findings to the ledger, creating file/dirs as needed.
/// Best-effort by design: the ledger is a safety net, not a dependency.
fn append_findings_to_ledger(path: &Path, findings: &[Finding]) -> std::io::Result<()> {
    if findings.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut content = String::new();
    for f in findings {
        if let Ok(line) = serde_json::to_string(f) {
            content.push_str(&line);
            content.push('\n');
        }
    }
    use std::io::Write;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(content.as_bytes())
}

/// Read all findings previously persisted to the ledger.
/// A missing or unreadable ledger yields an empty vector.
fn read_ledger_findings(path: &Path) -> Vec<Finding> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Merge ledger findings into the final findings.
///
/// The final response wins on conflicts; ledger-only findings (e.g. from
/// iterations that were truncated out of context) are appended. Dedup key:
/// rule_id + location + message. Deterministic — no LLM involved.
fn merge_with_ledger(final_findings: Vec<Finding>, ledger: Vec<Finding>) -> Vec<Finding> {
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::with_capacity(final_findings.len() + ledger.len());
    for f in final_findings {
        if seen.insert(finding_key(&f)) {
            merged.push(f);
        }
    }
    for f in ledger {
        if seen.insert(finding_key(&f)) {
            merged.push(f);
        }
    }
    merged
}

fn finding_key(f: &Finding) -> String {
    format!(
        "{}|{}|{}",
        f.rule_id,
        serde_json::to_string(&f.location).unwrap_or_default(),
        f.message
    )
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

/// Corrective prompt sent when a Stop response's findings JSON fails to
/// parse. States the expected schema and the exact parse error so the model
/// can fix its answer on the next attempt.
fn findings_retry_prompt(err: &str) -> String {
    format!(
        "Your previous answer could not be parsed as findings JSON.\n\
         Error: {err}\n\n\
         Please answer again with ONLY a JSON object of the form:\n\
         {{\"findings\": [{{\"rule_id\": \"...\", \"severity\": \"hint|info|warning|error\", \
         \"message\": \"...\", \"evidence\": \"...\", \"location\": {{\"file\": \"...\", \
         \"line_start\": 1, \"line_end\": 1, \"column_start\": 1, \"column_end\": 1}}}}]}}\n\
         The \"location\" field is optional — omit it when the finding has no \
         file location. Do not wrap the JSON in prose or markdown fences."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::tests::MockProvider;
    use crate::tools::default_tools;
    use crate::types::{
        AmbiguityPolicy, ChatResponse, OnIncompletePolicy, Severity, ToolCall, VendorConfig,
    };
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
            auto_compact: false,
            max_compactions: 3,
            findings_ledger: true,
            skills: vec![],
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
        let tools = default_tools(root.clone(), &[], 120, &[], None);

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
            event_log: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(!result.findings.is_empty());
        assert!(result.duration_ms > 0);
    }

    #[tokio::test]
    async fn test_agent_loop_records_event_log() {
        use crate::eventlog::{EventLog, RunEvent};

        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let tools = default_tools(root.clone(), &[], 120, &[], None);
        let event_log = EventLog::new(&root, "test");

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
            message: Message::new(Role::Assistant, r#"{"findings": []}"#),
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
            event_log: Some(&event_log),
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(!result.truncated);

        let content = std::fs::read_to_string(event_log.path()).unwrap();
        let events: Vec<RunEvent> = content
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let has = |name: &str| events.iter().any(|e| event_name(e) == name);
        assert!(has("run_start"), "missing run_start: {content}");
        assert!(has("llm_request"), "missing llm_request: {content}");
        assert!(has("llm_response"), "missing llm_response: {content}");
        assert!(has("tool_call"), "missing tool_call: {content}");
        assert!(has("tool_result"), "missing tool_result: {content}");
        assert!(has("run_end"), "missing run_end: {content}");

        // The event log can restore state from checkpoint events.
        event_log.append(&RunEvent::Checkpoint {
            checkpoint_id: "c-test".into(),
            messages: vec![Message::new(Role::User, "restored")],
            truncated: false,
        });
        assert_eq!(
            event_log.last_checkpoint(),
            Some(vec![Message::new(Role::User, "restored")])
        );
    }

    /// Tag name of a `RunEvent` (mirrors the serde `type` field).
    fn event_name(e: &RunEvent) -> &'static str {
        match e {
            RunEvent::RunStart { .. } => "run_start",
            RunEvent::LlmRequest { .. } => "llm_request",
            RunEvent::LlmResponse { .. } => "llm_response",
            RunEvent::ToolCall { .. } => "tool_call",
            RunEvent::ToolResult { .. } => "tool_result",
            RunEvent::ContextTruncated { .. } => "context_truncated",
            RunEvent::Checkpoint { .. } => "checkpoint",
            RunEvent::RunEnd { .. } => "run_end",
        }
    }

    #[tokio::test]
    async fn test_agent_loop_halts_on_timeout() {
        let tmp = TempDir::new().unwrap();
        let tools = default_tools(tmp.path().to_path_buf(), &[], 120, &[], None);

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
            event_log: None,
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
        let tools = default_tools(root.clone(), &[], 120, &[], None);

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
            event_log: None,
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
        let tools = default_tools(root.clone(), &[], 120, &[], None);

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
            event_log: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(
            result.truncated,
            "Expected truncated=true when context cannot be reduced further"
        );
    }

    /// Reactive compaction: the heuristic counter thinks the context fits
    /// (under 80%), but the provider rejects the request as over its context
    /// window. The loop must truncate more aggressively (50% of budget) and
    /// retry the same request once.
    #[tokio::test]
    async fn test_agent_loop_reactive_compact_on_context_overflow() {
        let (_tmp, root) = setup_agent_env();
        let tools = default_tools(root.clone(), &[], 120, &[], None);

        // 40000 chars ≈ 10000 tokens by the mock heuristic: under the 80%
        // threshold (12000) of a 15000 budget, so the proactive check stays
        // quiet — the provider error below is what triggers compaction.
        let mut contract = test_contract();
        contract.token_budget = 15000;

        let mut mock = MockProvider::new("test-model");
        mock.add_error_response(ProviderError::BadRequest(
            "This model's maximum context length is 8192 tokens. \
             However, your request has 10000 tokens."
                .into(),
        ));
        mock.add_response(ChatResponse {
            message: Message::new(Role::Assistant, r#"{"findings": [{"id": "00000000-0000-0000-0000-000000000000", "rule_id": "test", "severity": "warning", "message": "recovered after compaction", "evidence": "e"}]}"#),
            usage: Usage {
                input_tokens: 20,
                output_tokens: 10,
                total_tokens: 30,
            },
            finish_reason: FinishReason::Stop,
            tool_calls: None,
        });

        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, "x".repeat(40000))],
            workspace_root: root.clone(),
            snapshot_mgr: None,
            event_log: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(
            !result.truncated,
            "reactive compaction must recover the run, got truncated=true"
        );
        assert_eq!(
            result.findings[0].message, "recovered after compaction",
            "the retried request's findings must be extracted"
        );
        // The compaction dropped messages and archived them.
        let archive_dir = root.join(".clausura").join("archives");
        let mut found = false;
        if let Ok(entries) = std::fs::read_dir(&archive_dir) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("context-dump-test-")
                {
                    found = true;
                }
            }
        }
        assert!(
            found,
            "dropped messages must be archived during reactive compaction"
        );
    }

    /// Reactive compaction that cannot reduce the context further must not
    /// retry forever: the run stops marked truncated after the failed
    /// truncation (no second LLM call is spent).
    #[tokio::test]
    async fn test_agent_loop_reactive_compact_gives_up_when_unable_to_reduce() {
        let (_tmp, root) = setup_agent_env();
        let tools = default_tools(root.clone(), &[], 120, &[], None);

        let mut contract = test_contract();
        contract.token_budget = 1;

        let mut mock = MockProvider::new("test-model");
        mock.add_error_response(ProviderError::BadRequest(
            "This model's maximum context length is 10 tokens.".into(),
        ));
        mock.add_response(ChatResponse {
            message: Message::new(Role::Assistant, r#"{"findings": []}"#),
            usage: Usage {
                input_tokens: 5,
                output_tokens: 5,
                total_tokens: 10,
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
            event_log: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(
            result.truncated,
            "unable to reduce → run must be marked truncated"
        );
        assert_eq!(
            result.usage.total_tokens, 0,
            "no retry LLM call may be spent after a failed reactive compaction"
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
        let tools = default_tools(root.clone(), &[], 120, &[], None);

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
            event_log: None,
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
        let tools = default_tools(root.clone(), &[], 120, &[], None);

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
            event_log: None,
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
        let tools = default_tools(root.clone(), &[], 120, &[], None);

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
            event_log: None,
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

    // -----------------------------------------------------------------
    // Auto-compact
    // -----------------------------------------------------------------

    /// Shared setup for auto-compact tests: a contract that triggers
    /// truncation on a huge initial message, with a tool-call then a
    /// Stop response queued for the agent loop.
    fn auto_compact_setup(
        token_budget: u64,
        max_total_tokens: Option<u64>,
        auto_compact: bool,
        max_compactions: u32,
    ) -> (TempDir, TaskContract, ToolCall, Vec<Message>) {
        let tmp = TempDir::new().unwrap();
        let mut contract = test_contract();
        contract.token_budget = token_budget;
        contract.max_total_tokens = max_total_tokens;
        contract.auto_compact = auto_compact;
        contract.max_compactions = max_compactions;
        let tool_call = ToolCall {
            id: "call_1".into(),
            name: "git_diff".into(),
            arguments: serde_json::json!({}),
        };
        (
            tmp,
            contract,
            tool_call,
            vec![Message::new(Role::User, "x".repeat(40000))],
        )
    }

    fn tool_call_response(tool_call: &ToolCall) -> ChatResponse {
        ChatResponse {
            message: Message::new(Role::Assistant, "Running tool..."),
            usage: Usage {
                input_tokens: 5,
                output_tokens: 5,
                total_tokens: 10,
            },
            finish_reason: FinishReason::ToolCalls,
            tool_calls: Some(vec![tool_call.clone()]),
        }
    }

    fn stop_response(content: &str) -> ChatResponse {
        ChatResponse {
            message: Message::new(Role::Assistant, content.to_string()),
            usage: Usage {
                input_tokens: 20,
                output_tokens: 10,
                total_tokens: 30,
            },
            finish_reason: FinishReason::Stop,
            tool_calls: None,
        }
    }

    #[tokio::test]
    async fn test_agent_loop_auto_compact_injects_summary() {
        let (tmp, contract, tool_call, initial) = auto_compact_setup(10000, None, true, 3);
        let root = tmp.path().to_path_buf();
        let tools = default_tools(root.clone(), &[], 120, &[], None);

        let mut mock = MockProvider::new("test-model");
        mock.add_response(tool_call_response(&tool_call));
        mock.add_summary_response(Ok("Summary: found X in file A".to_string()));
        mock.add_response(stop_response(
            r#"{"findings": [{"id": "00000000-0000-0000-0000-000000000000", "rule_id": "test", "severity": "warning", "message": "m", "evidence": "e"}]}"#,
        ));

        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: initial,
            workspace_root: root.clone(),
            snapshot_mgr: None,
            event_log: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(!result.truncated, "expected a clean run");

        // The summary sits at index 1 — immediately after the system message.
        let summary_msg = &result.messages[1];
        assert_eq!(summary_msg.role, Role::User);
        assert!(
            summary_msg.content.contains("COMPACTED SUMMARY"),
            "expected a compacted-summary marker, got: {}",
            summary_msg.content
        );
        assert!(
            summary_msg.content.contains("Summary: found X in file A"),
            "expected the LLM summary inside the hint, got: {}",
            summary_msg.content
        );
        assert!(
            summary_msg.content.contains("archived at"),
            "expected the archive path in the hint, got: {}",
            summary_msg.content
        );

        // The summarization call (mock usage 100 in / 50 out / 150 total)
        // is included in reported usage.
        assert_eq!(result.usage.total_tokens, 10 + 30 + 150);

        // Archive still written.
        let archive_dir = root.join(".clausura").join("archives");
        let mut found = false;
        if let Ok(entries) = std::fs::read_dir(&archive_dir) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("context-dump-test-")
                {
                    found = true;
                    break;
                }
            }
        }
        assert!(found, "archive file should exist after compaction");
    }

    #[tokio::test]
    async fn test_agent_loop_auto_compact_falls_back_on_llm_error() {
        let (tmp, contract, tool_call, initial) = auto_compact_setup(10000, None, true, 3);
        let root = tmp.path().to_path_buf();
        let tools = default_tools(root.clone(), &[], 120, &[], None);

        let mut mock = MockProvider::new("test-model");
        mock.add_response(tool_call_response(&tool_call));
        mock.add_summary_response(Err(ProviderError::ServerError("boom".into())));
        mock.add_response(stop_response(r#"{"findings": []}"#));

        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: initial,
            workspace_root: root.clone(),
            snapshot_mgr: None,
            event_log: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(
            !result.truncated,
            "a failed summary call must not mark the run incomplete"
        );

        // Falls back to the bare "context trimmed" hint.
        let hint = &result.messages[1];
        assert!(
            hint.content.contains("was trimmed"),
            "expected the bare hint, got: {}",
            hint.content
        );
        assert!(
            !hint.content.contains("COMPACTED SUMMARY"),
            "expected no summary on failure, got: {}",
            hint.content
        );

        // No usage from the failed summary call.
        assert_eq!(result.usage.total_tokens, 10 + 30);
    }

    #[tokio::test]
    async fn test_agent_loop_auto_compact_disabled_by_max_compactions_zero() {
        // max_compactions = 0 disables compaction even when auto_compact is
        // on: the queued summary response must never be consumed.
        let (tmp, contract, tool_call, initial) = auto_compact_setup(10000, None, true, 0);
        let root = tmp.path().to_path_buf();
        let tools = default_tools(root.clone(), &[], 120, &[], None);

        let mut mock = MockProvider::new("test-model");
        mock.add_response(tool_call_response(&tool_call));
        mock.add_summary_response(Ok("SHOULD NOT APPEAR".to_string()));
        mock.add_response(stop_response(r#"{"findings": []}"#));

        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: initial,
            workspace_root: root.clone(),
            snapshot_mgr: None,
            event_log: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        let hint = &result.messages[1];
        assert!(
            hint.content.contains("was trimmed"),
            "expected the bare hint, got: {}",
            hint.content
        );
        assert!(
            !hint.content.contains("SHOULD NOT APPEAR"),
            "compaction must not run with max_compactions = 0, got: {}",
            hint.content
        );
        assert_eq!(result.usage.total_tokens, 10 + 30);
    }

    #[tokio::test]
    async fn test_agent_loop_auto_compact_respects_max_total_tokens_headroom() {
        // max_total_tokens too small for a summary call → compaction is
        // skipped by the quota guard (bare hint), and the queued summary is
        // never consumed.
        let (tmp, contract, tool_call, initial) = auto_compact_setup(10000, Some(500), true, 3);
        let root = tmp.path().to_path_buf();
        let tools = default_tools(root.clone(), &[], 120, &[], None);

        let mut mock = MockProvider::new("test-model");
        mock.add_response(tool_call_response(&tool_call));
        mock.add_summary_response(Ok("SHOULD NOT APPEAR".to_string()));
        mock.add_response(stop_response(r#"{"findings": []}"#));

        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: initial,
            workspace_root: root.clone(),
            snapshot_mgr: None,
            event_log: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(!result.truncated);
        let hint = &result.messages[1];
        assert!(
            hint.content.contains("was trimmed"),
            "expected the bare hint, got: {}",
            hint.content
        );
        assert!(
            !hint.content.contains("SHOULD NOT APPEAR"),
            "quota guard must skip compaction, got: {}",
            hint.content
        );
        assert_eq!(result.usage.total_tokens, 10 + 30);
    }

    #[tokio::test]
    async fn test_agent_loop_auto_compact_large_summary_stays_within_budget() {
        // The summary budget is the headroom the retained tail leaves under
        // the truncation threshold (80%), not a fixed 10%: an oversized
        // summary must be trimmed so that injecting it cannot push the
        // context back over the threshold and mark the run incomplete.
        let (tmp, contract, tool_call, initial) = auto_compact_setup(10000, None, true, 3);
        let root = tmp.path().to_path_buf();
        let tools = default_tools(root.clone(), &[], 120, &[], None);

        // ~1500 tokens by the mock heuristic — far above the summary budget,
        // which is the headroom under the 80% truncation threshold.
        let mut mock = MockProvider::new("test-model");
        mock.add_response(tool_call_response(&tool_call));
        mock.add_summary_response(Ok("y".repeat(6000)));
        mock.add_response(stop_response(r#"{"findings": []}"#));

        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: initial,
            workspace_root: root.clone(),
            snapshot_mgr: None,
            event_log: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(
            !result.truncated,
            "a large summary must be trimmed to fit, not truncate the run"
        );
        // The whole point of the headroom sizing: after injection the context
        // must stay under the truncation threshold (80% of budget).
        let cm = ContextManager::new(&mock, contract.token_budget, root.clone());
        assert!(
            !cm.should_truncate(&result.messages),
            "injecting the summary must not push the context back over the threshold"
        );
        let hint = &result.messages[1];
        assert!(
            hint.content.contains("COMPACTED SUMMARY"),
            "expected the compacted hint, got: {}",
            hint.content
        );
        assert!(
            hint.content
                .contains("[summary truncated to fit token budget]"),
            "oversized summary must carry the trim marker, got: {}",
            hint.content
        );
        assert!(
            !hint.content.contains(&"y".repeat(5000)),
            "summary must not retain its full oversized body, got: {}",
            hint.content
        );
        // Summary usage (100 in / 50 out / 150 total) still billed.
        assert_eq!(result.usage.total_tokens, 10 + 150 + 30);
    }

    #[tokio::test]
    async fn test_try_compact_skipped_when_no_headroom() {
        // Retained tail already sits near the truncation threshold: the
        // headroom for a summary is below MIN_SUMMARY_TOKENS, so compaction
        // is skipped without spending an LLM call.
        let mock = MockProvider::new("test-model");
        let cm = ContextManager::new(&mock, 10000, PathBuf::from("/tmp"));
        let dropped = vec![Message::new(Role::User, "old message".to_string())];
        let archive = Path::new(".clausura/archives/context-dump-test-1.log");
        let mut running = 0u64;
        let mut usage = Usage::default();
        let outcome = try_compact(
            &mock,
            &cm,
            &dropped,
            archive,
            10000,
            None,
            7900, // tail at 79% → headroom ≈ 31 < MIN_SUMMARY_TOKENS
            &mut running,
            &mut usage,
        )
        .await;
        assert!(
            matches!(outcome, CompactOutcome::Skipped),
            "expected Skipped, got: {:?}",
            outcome
        );
        // No summary call was made: no usage, nothing billed.
        assert_eq!(usage.total_tokens, 0);
        assert_eq!(running, 0);
    }

    #[test]
    fn test_dropped_to_text_renders_roles_and_tool_results() {
        let dropped = vec![
            Message::new(Role::User, "check the diff".to_string()),
            Message {
                role: Role::Assistant,
                content: String::new(),
                tool_call_id: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".into(),
                    name: "git_diff".into(),
                    arguments: serde_json::json!({}),
                }]),
            },
            Message::with_tool_call(Role::Tool, "diff output".to_string(), "call_1".into()),
        ];
        let text = dropped_to_text(&dropped);
        assert!(text.contains("User:"));
        assert!(text.contains("check the diff"));
        assert!(text.contains("Assistant (tool calls: git_diff):"));
        assert!(text.contains("Tool result (call_1):"));
        assert!(text.contains("diff output"));
    }

    #[test]
    fn test_truncate_summary_to_budget_trims_oversized_summary() {
        let mock = MockProvider::new("test");
        // Mock counts ~1 token per 4 chars.
        let cap_tokens = 50;
        let summary = "y".repeat(400); // ~100 tokens by the mock heuristic
        let trimmed = truncate_summary_to_budget(&mock, &summary, cap_tokens);
        assert!(
            mock.count_tokens(&trimmed) <= cap_tokens,
            "trimmed summary exceeds cap: {} > {}",
            mock.count_tokens(&trimmed),
            cap_tokens
        );
        assert!(trimmed.ends_with("[summary truncated to fit token budget]"));
        assert!(trimmed.starts_with('y'));

        // Short summaries pass through untouched.
        let short = "short summary".to_string();
        let untouched = truncate_summary_to_budget(&mock, &short, cap_tokens);
        assert_eq!(untouched, short);
    }

    // -----------------------------------------------------------------
    // Repeat-call reminder
    // -----------------------------------------------------------------

    #[test]
    fn test_repeat_key_is_order_insensitive() {
        let a = serde_json::json!({"base": "main", "staged": false});
        let b = serde_json::json!({"staged": false, "base": "main"});
        assert_eq!(repeat_key("git_diff", &a), repeat_key("git_diff", &b));
        // Different arguments are different keys.
        assert_ne!(
            repeat_key("git_diff", &serde_json::json!({"base": "main"})),
            repeat_key("git_diff", &serde_json::json!({"staged": true}))
        );
        // Different tools are different keys.
        assert_ne!(
            repeat_key("git_diff", &serde_json::json!({})),
            repeat_key("grep", &serde_json::json!({}))
        );
    }

    #[test]
    fn test_repeat_reminder_text_escalates() {
        assert!(repeat_reminder_text("git_diff", 3).contains("3 times"));
        assert!(repeat_reminder_text("git_diff", 5).contains("looks like a loop"));
        assert!(repeat_reminder_text("git_diff", 8).contains("stuck in a loop"));
    }

    #[tokio::test]
    async fn test_agent_loop_injects_repeat_call_reminder() {
        let (_tmp, root) = setup_agent_env();
        let tools = default_tools(root.clone(), &[], 120, &[], None);

        let contract = test_contract();

        let tool_call = ToolCall {
            id: "call_1".into(),
            name: "git_diff".into(),
            arguments: serde_json::json!({}),
        };

        let mut mock = MockProvider::new("test-model");
        // Three identical tool-call iterations — the third crosses the
        // threshold and must produce an advisory user message.
        for _ in 0..3 {
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
        }
        mock.add_response(ChatResponse {
            message: Message::new(Role::Assistant, r#"{"findings": []}"#),
            usage: Usage {
                input_tokens: 20,
                output_tokens: 10,
                total_tokens: 30,
            },
            finish_reason: FinishReason::Stop,
            tool_calls: None,
        });

        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, "Review the diff")],
            workspace_root: root.clone(),
            snapshot_mgr: None,
            event_log: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(!result.truncated);

        let reminder = result
            .messages
            .iter()
            .find(|m| m.role == Role::User && m.content.contains("Repeat-call reminder"))
            .expect("a repeat-call reminder must be injected");
        assert!(
            reminder.content.contains("3 times"),
            "expected the first-threshold reminder, got: {}",
            reminder.content
        );
        assert!(
            reminder.content.contains("`git_diff`"),
            "reminder must name the tool, got: {}",
            reminder.content
        );

        // The reminder must follow the tool results (valid API pairing).
        let tool_msgs = result
            .messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .count();
        assert_eq!(tool_msgs, 3, "all three tool calls must have results");
    }

    #[tokio::test]
    async fn test_agent_loop_no_reminder_below_threshold() {
        let (_tmp, root) = setup_agent_env();
        let tools = default_tools(root.clone(), &[], 120, &[], None);

        let contract = test_contract();

        let tool_call = ToolCall {
            id: "call_1".into(),
            name: "git_diff".into(),
            arguments: serde_json::json!({}),
        };

        let mut mock = MockProvider::new("test-model");
        // Two identical calls — below the threshold, no reminder.
        for _ in 0..2 {
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
        }
        mock.add_response(ChatResponse {
            message: Message::new(Role::Assistant, r#"{"findings": []}"#),
            usage: Usage {
                input_tokens: 20,
                output_tokens: 10,
                total_tokens: 30,
            },
            finish_reason: FinishReason::Stop,
            tool_calls: None,
        });

        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, "Review the diff")],
            workspace_root: root.clone(),
            snapshot_mgr: None,
            event_log: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(!result.truncated);
        assert!(
            !result
                .messages
                .iter()
                .any(|m| m.content.contains("Repeat-call reminder")),
            "no reminder may be injected below the threshold"
        );
    }

    // -----------------------------------------------------------------
    // Findings-schema retry
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn test_agent_loop_retries_on_malformed_findings() {
        let (_tmp, root) = setup_agent_env();
        let tools = default_tools(root.clone(), &[], 120, &[], None);

        let contract = test_contract();

        let mut mock = MockProvider::new("test-model");
        // First Stop answer is not parseable JSON.
        mock.add_response(ChatResponse {
            message: Message::new(
                Role::Assistant,
                "I found nothing worth reporting.".to_string(),
            ),
            usage: Usage {
                input_tokens: 20,
                output_tokens: 10,
                total_tokens: 30,
            },
            finish_reason: FinishReason::Stop,
            tool_calls: None,
        });
        // The corrective prompt yields a valid answer on the second attempt.
        mock.add_response(ChatResponse {
            message: Message::new(Role::Assistant, r#"{"findings": [{"id": "00000000-0000-0000-0000-000000000000", "rule_id": "r", "severity": "error", "message": "fixed", "evidence": "e"}]}"#),
            usage: Usage {
                input_tokens: 20,
                output_tokens: 10,
                total_tokens: 30,
            },
            finish_reason: FinishReason::Stop,
            tool_calls: None,
        });

        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, "Review the diff")],
            workspace_root: root.clone(),
            snapshot_mgr: None,
            event_log: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(
            !result.truncated,
            "a corrected retry must not mark the run truncated"
        );
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].message, "fixed");

        let retry_prompt = result
            .messages
            .iter()
            .find(|m| m.role == Role::User && m.content.contains("could not be parsed"))
            .expect("the corrective retry prompt must be in the conversation");
        assert!(retry_prompt.content.contains("findings"));
    }

    #[tokio::test]
    async fn test_agent_loop_gives_up_after_malformed_findings_retries() {
        let (_tmp, root) = setup_agent_env();
        let tools = default_tools(root.clone(), &[], 120, &[], None);

        let contract = test_contract();

        let mut mock = MockProvider::new("test-model");
        // Three malformed Stop answers exhaust the bounded retry budget.
        for i in 0..3 {
            mock.add_response(ChatResponse {
                message: Message::new(Role::Assistant, format!("still not json {i}")),
                usage: Usage {
                    input_tokens: 20,
                    output_tokens: 10,
                    total_tokens: 30,
                },
                finish_reason: FinishReason::Stop,
                tool_calls: None,
            });
        }

        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, "Review the diff")],
            workspace_root: root.clone(),
            snapshot_mgr: None,
            event_log: None,
        };

        let err = run_agent_loop(config).await.unwrap_err();
        assert!(
            matches!(err, ProviderError::MalformedFindings(_)),
            "expected MalformedFindings after retries exhausted, got: {err}"
        );
    }

    #[test]
    fn test_findings_retry_prompt_names_schema() {
        let prompt = findings_retry_prompt("1 of 1 finding(s) failed: bad");
        assert!(prompt.contains("could not be parsed"));
        assert!(prompt.contains("1 of 1 finding(s) failed: bad"));
        assert!(prompt.contains("rule_id"));
        assert!(prompt.contains("severity"));
        assert!(prompt.contains("location"));
    }

    // -----------------------------------------------------------------
    // Findings ledger
    // -----------------------------------------------------------------

    fn finding(rule_id: &str, severity: Severity, message: &str) -> Finding {
        Finding {
            id: uuid::Uuid::new_v4(),
            rule_id: rule_id.into(),
            severity,
            message: message.into(),
            location: None,
            evidence: "e".into(),
        }
    }

    #[tokio::test]
    async fn test_agent_loop_ledger_merges_findings_from_earlier_iterations() {
        // An early iteration emits findings alongside a tool call; the final
        // Stop response only reports later findings. The ledger must preserve
        // the early ones and merge them back into the final result.
        let (_tmp, root) = setup_agent_env();
        let tools = default_tools(root.clone(), &[], 120, &[], None);

        let mut contract = test_contract();
        contract.findings_ledger = true;

        let tool_call = ToolCall {
            id: "call_1".into(),
            name: "git_diff".into(),
            arguments: serde_json::json!({}),
        };

        let early = finding(
            "early-issue",
            Severity::Warning,
            "found in an early iteration",
        );
        let late = finding("final-issue", Severity::Error, "found at the end");

        let mut mock = MockProvider::new("test-model");
        mock.add_response(ChatResponse {
            message: Message::new(
                Role::Assistant,
                serde_json::json!({"findings": [early]}).to_string(),
            ),
            usage: Usage {
                input_tokens: 5,
                output_tokens: 5,
                total_tokens: 10,
            },
            finish_reason: FinishReason::ToolCalls,
            tool_calls: Some(vec![tool_call.clone()]),
        });
        mock.add_response(ChatResponse {
            message: Message::new(
                Role::Assistant,
                serde_json::json!({"findings": [late]}).to_string(),
            ),
            usage: Usage {
                input_tokens: 20,
                output_tokens: 10,
                total_tokens: 30,
            },
            finish_reason: FinishReason::Stop,
            tool_calls: None,
        });

        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, "Review the diff")],
            workspace_root: root.clone(),
            snapshot_mgr: None,
            event_log: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        assert!(!result.truncated);
        let rule_ids: Vec<&str> = result.findings.iter().map(|f| f.rule_id.as_str()).collect();
        assert!(
            rule_ids.contains(&"early-issue"),
            "early finding must survive via the ledger, got: {:?}",
            rule_ids
        );
        assert!(
            rule_ids.contains(&"final-issue"),
            "final finding must be present, got: {:?}",
            rule_ids
        );
        // Final response findings come first, ledger-only findings appended.
        assert_eq!(rule_ids[0], "final-issue");

        // The ledger file was written next to the archives.
        let ledger = root
            .join(".clausura")
            .join("archives")
            .join("findings-ledger-test.jsonl");
        assert!(ledger.exists(), "ledger file should exist");
    }

    #[tokio::test]
    async fn test_agent_loop_ledger_disabled() {
        // findings_ledger = false: interim findings are not persisted and the
        // final result contains only the Stop response findings.
        let (_tmp, root) = setup_agent_env();
        let tools = default_tools(root.clone(), &[], 120, &[], None);

        let mut contract = test_contract();
        contract.findings_ledger = false;

        let tool_call = ToolCall {
            id: "call_1".into(),
            name: "git_diff".into(),
            arguments: serde_json::json!({}),
        };

        let early = finding("early-issue", Severity::Warning, "early");
        let late = finding("final-issue", Severity::Error, "final");

        let mut mock = MockProvider::new("test-model");
        mock.add_response(ChatResponse {
            message: Message::new(
                Role::Assistant,
                serde_json::json!({"findings": [early]}).to_string(),
            ),
            usage: Usage {
                input_tokens: 5,
                output_tokens: 5,
                total_tokens: 10,
            },
            finish_reason: FinishReason::ToolCalls,
            tool_calls: Some(vec![tool_call.clone()]),
        });
        mock.add_response(ChatResponse {
            message: Message::new(
                Role::Assistant,
                serde_json::json!({"findings": [late]}).to_string(),
            ),
            usage: Usage {
                input_tokens: 20,
                output_tokens: 10,
                total_tokens: 30,
            },
            finish_reason: FinishReason::Stop,
            tool_calls: None,
        });

        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, "Review the diff")],
            workspace_root: root.clone(),
            snapshot_mgr: None,
            event_log: None,
        };

        let result = run_agent_loop(config).await.unwrap();
        let rule_ids: Vec<&str> = result.findings.iter().map(|f| f.rule_id.as_str()).collect();
        assert_eq!(rule_ids, vec!["final-issue"]);

        let ledger = root
            .join(".clausura")
            .join("archives")
            .join("findings-ledger-test.jsonl");
        assert!(
            !ledger.exists(),
            "no ledger should be written when disabled"
        );
    }

    #[test]
    fn test_merge_with_ledger_dedup() {
        // The final response wins; a ledger-only finding is appended; an
        // identical (rule_id + location + message) ledger copy is dropped.
        let final_a = finding("r1", Severity::Error, "dup message");
        let final_b = finding("r2", Severity::Warning, "only final");
        let ledger_a = finding("r1", Severity::Error, "dup message");
        let ledger_c = finding("r3", Severity::Info, "only ledger");

        let merged = merge_with_ledger(vec![final_a, final_b], vec![ledger_a, ledger_c]);
        let rule_ids: Vec<&str> = merged.iter().map(|f| f.rule_id.as_str()).collect();
        assert_eq!(rule_ids, vec!["r1", "r2", "r3"]);
    }

    #[tokio::test]
    async fn test_agent_loop_propagates_tool_call_id() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let tools = default_tools(root.clone(), &[], 120, &[], None);

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
            event_log: None,
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
    // corrective findings recovery
    // -----------------------------------------------------------------

    fn valid_findings_json(message: &str) -> String {
        format!(
            r#"{{"findings": [{{"id": "00000000-0000-0000-0000-000000000000", "rule_id": "test", "severity": "warning", "message": "{}", "evidence": "x"}}]}}"#,
            message
        )
    }

    #[tokio::test]
    async fn test_agent_loop_recovers_malformed_final_json() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let tools = default_tools(root.clone(), &[], 120, &[], None);

        let mut mock = MockProvider::new("gpt-4o");
        // Final answer that is prose, not findings JSON.
        mock.add_response(ChatResponse {
            message: Message::new(
                Role::Assistant,
                "The diff appears empty. Let me try different options.",
            ),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
            finish_reason: FinishReason::Stop,
            tool_calls: None,
        });
        // Corrective answer: valid findings JSON.
        mock.add_response(ChatResponse {
            message: Message::new(Role::Assistant, valid_findings_json("recovered")),
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
            workspace_root: root,
            snapshot_mgr: None,
            event_log: None,
        };
        let result = run_agent_loop(config).await.unwrap();
        assert!(!result.truncated);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].rule_id, "test");
        assert!(result.findings[0].message.contains("recovered"));
        assert!(
            result.messages.iter().any(|m| m.role == Role::User
                && m.content.contains("could not be parsed as findings JSON")),
            "the corrective exchange must be part of the transcript"
        );
    }

    #[tokio::test]
    async fn test_agent_loop_malformed_json_retries_exhausted() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let tools = default_tools(root.clone(), &[], 120, &[], None);

        let mut mock = MockProvider::new("gpt-4o");
        // Main answer + 2 corrective attempts, all prose. The third call
        // (second recovery attempt) must fail closed with MalformedFindings.
        for _ in 0..3 {
            mock.add_response(ChatResponse {
                message: Message::new(Role::Assistant, "still not JSON"),
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    total_tokens: 15,
                },
                finish_reason: FinishReason::Stop,
                tool_calls: None,
            });
        }

        let contract = test_contract();
        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, "Review the diff")],
            workspace_root: root,
            snapshot_mgr: None,
            event_log: None,
        };
        let result = run_agent_loop(config).await;
        assert!(
            matches!(result, Err(ProviderError::MalformedFindings(_))),
            "exhausted recovery must fail closed, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_agent_loop_incomplete_path_recovers_findings() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let tools = default_tools(root.clone(), &[], 120, &[], None);

        let mut mock = MockProvider::new("gpt-4o");
        // Two tool-call turns that never reach a clean Stop, so the loop
        // exhausts max_iterations and falls through incomplete.
        for i in 0..2 {
            mock.add_response(ChatResponse {
                message: Message::new(Role::Assistant, format!("Checking tool {i}...")),
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    total_tokens: 15,
                },
                finish_reason: FinishReason::ToolCalls,
                tool_calls: Some(vec![ToolCall {
                    id: format!("call_{i}"),
                    name: "git_diff".into(),
                    arguments: serde_json::json!({}),
                }]),
            });
        }
        // Corrective answer rescued from the incomplete fall-through.
        mock.add_response(ChatResponse {
            message: Message::new(Role::Assistant, valid_findings_json("recovered")),
            usage: Usage {
                input_tokens: 20,
                output_tokens: 10,
                total_tokens: 30,
            },
            finish_reason: FinishReason::Stop,
            tool_calls: None,
        });

        let mut contract = test_contract();
        contract.max_iterations = 2;
        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, "Review the diff")],
            workspace_root: root,
            snapshot_mgr: None,
            event_log: None,
        };
        let result = run_agent_loop(config).await.unwrap();
        assert!(
            !result.truncated,
            "a successful corrective recovery counts as a complete final answer"
        );
        assert_eq!(result.findings.len(), 1);
        assert!(result.findings[0].message.contains("recovered"));
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
        let tools = default_tools(root.clone(), &[], 120, &[], None);

        // The schema-mismatched answer is retried with a corrective prompt
        // (bounded), so feed the retries equally malformed answers — after
        // the retry budget is exhausted the run must still error, never
        // silently succeed with 0 findings.
        let mut mock = MockProvider::new("gpt-4o");
        for _ in 0..3 {
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
        }

        let contract = test_contract();
        let config = AgentConfig {
            contract: &contract,
            provider: &mock,
            tools: &tools,
            initial_messages: vec![Message::new(Role::User, "Review the diff")],
            workspace_root: root,
            snapshot_mgr: None,
            event_log: None,
        };

        let result = run_agent_loop(config).await;
        let err = result.expect_err(
            "a Stop response with findings that fail schema validation must error, \
             not silently succeed with 0 findings",
        );
        assert!(matches!(err, ProviderError::MalformedFindings(_)));
    }
}
