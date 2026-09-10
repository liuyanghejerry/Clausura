use crate::agent::{run_agent_loop, AgentConfig};
use crate::checkpoint::CheckpointStore;
use crate::config::Config;
use crate::eventlog::{EventLog, RunEvent};
use crate::provider::create_provider;
use crate::rules::RuleEngine;
use crate::sarif::SarifFormatter;
use crate::snapshot::SnapshotManager;
use crate::tools::default_tools;
use crate::types::{
    ExecutionReport, Finding, IncompleteReason, Message, OnIncompletePolicy, PreflightCheck,
    ProviderError, Role, RunStatus, Severity, ShardIncompletePolicy, ShardingConfig, Usage,
};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

/// Execute a full task lifecycle.
///
/// Orchestrates: config → provider → agent → rule engine → SARIF → checkpoint.
/// Exit codes: 0 = pass, 1 = rule violation, 2 = error, 3 = config error.
pub async fn execute_task(config: &Config) -> ExecutionReport {
    if let Some(sharding) = &config.task.sharding {
        return execute_sharded_task(config, sharding).await;
    }
    execute_single_task(config).await
}

/// Guidance prepended to the user prompt for every shard review. Keeps the
/// agent's context bounded: the manifest and diffs are already inline, so
/// extra repository reads are only for validating a potential finding.
const SHARD_GUIDANCE: &str = "\
You are reviewing ONE SHARD of a larger pull request.
Read only the provided shard manifest and diff.
Inspect surrounding source only for changed hunks and only when needed
to validate a potential finding. Do not read the complete repository
or unrelated files.";

fn shard_prompt(user_prompt: &str) -> String {
    format!("{SHARD_GUIDANCE}\n\n{user_prompt}")
}

/// How many times a rate-limited shard is re-queued (with backoff) before it
/// is declared failed. Independent of the bisect budget: rate limiting is
/// transient provider capacity, while bisecting addresses oversized shards.
const MAX_SHARD_RATE_RETRIES: u32 = 3;

/// Exponential backoff between shard re-runs after rate limiting:
/// 30s, 60s, 120s.
fn rate_limit_backoff(retries: u32) -> std::time::Duration {
    std::time::Duration::from_secs(30u64 << retries.min(4))
}

/// Status of one shard's execution, recorded for the run summary.
#[derive(serde::Serialize, Debug, Clone)]
struct ShardRecord {
    shard: usize,
    files: Vec<String>,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    findings: usize,
    tokens: u64,
    attempts: u32,
}

/// Run `git <args...>` in the workspace and return stdout.
async fn run_git_output(workspace: &Path, args: &[&str]) -> Result<String, String> {
    let out = tokio::process::Command::new("git")
        .current_dir(workspace)
        .args(args)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Resolve the PR's committed range before making any model requests.
/// Missing refs/history are errors, never an empty successful review.
pub async fn resolve_review_range(
    workspace: &Path,
    base: &str,
) -> Result<(String, String), String> {
    let base_sha = run_git_output(
        workspace,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base}^{{commit}}"),
        ],
    )
    .await?;
    let head = run_git_output(workspace, &["rev-parse", "--verify", "HEAD^{commit}"]).await?;
    let merge_base =
        run_git_output(workspace, &["merge-base", base_sha.trim(), head.trim()]).await?;
    Ok((merge_base.trim().to_string(), head.trim().to_string()))
}

/// Collect per-file diffs between `sharding.base` (via merge-base) and HEAD,
/// run the deterministic risk scan, and plan shards.
async fn collect_shard_plan(
    workspace: &Path,
    sharding: &ShardingConfig,
) -> Result<
    (
        Vec<crate::shard::FileDiff>,
        Vec<crate::risk::RiskHit>,
        Vec<crate::shard::Shard>,
    ),
    String,
> {
    let merge_base = run_git_output(workspace, &["merge-base", &sharding.base, "HEAD"])
        .await?
        .trim()
        .to_string();
    if merge_base.is_empty() {
        return Err(format!(
            "git merge-base {} HEAD returned nothing — is the base ref valid?",
            sharding.base
        ));
    }

    let mut name_args: Vec<String> = vec![
        "diff".into(),
        "--name-only".into(),
        merge_base.clone(),
        "HEAD".into(),
    ];
    if !sharding.paths.is_empty() {
        name_args.push("--".into());
        name_args.extend(sharding.paths.iter().cloned());
    }
    let name_refs: Vec<&str> = name_args.iter().map(|s| s.as_str()).collect();
    let names = run_git_output(workspace, &name_refs).await?;
    let files: Vec<String> = names
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| {
            !l.is_empty()
                // Clausura's own config/skills are audit inputs, not audit targets.
                && l != ".clausura.yaml"
                && l != ".clausura.yml"
                && !l.starts_with(".clausura/")
        })
        .collect();

    let unified = format!("--unified={}", sharding.context_lines);
    let mut file_diffs = Vec::with_capacity(files.len());
    for file in &files {
        let diff = run_git_output(
            workspace,
            &["diff", &unified, &merge_base, "HEAD", "--", file],
        )
        .await?;
        file_diffs.push(crate::shard::FileDiff::new(file.clone(), diff));
    }

    let extra_patterns: Vec<(String, String)> =
        sharding.risk_patterns.clone().into_iter().collect();
    let scan_refs: Vec<(&str, &str)> = file_diffs
        .iter()
        .map(|fd| (fd.path.as_str(), fd.diff.as_str()))
        .collect();
    let risk_hits = crate::risk::scan_files(&scan_refs, &extra_patterns);

    let shards = crate::shard::plan_shards(
        file_diffs.clone(),
        sharding.max_diff_bytes,
        sharding.max_files_per_shard,
    );
    Ok((file_diffs, risk_hits, shards))
}

/// Plan shards without running any agent (for `--dry-run` output).
pub async fn plan_sharding_preview(
    workspace: &Path,
    sharding: &ShardingConfig,
) -> Result<(usize, usize, usize), String> {
    let (file_diffs, _hits, shards) = collect_shard_plan(workspace, sharding).await?;
    let total_bytes: usize = file_diffs.iter().map(|f| f.diff_bytes).sum();
    Ok((file_diffs.len(), total_bytes, shards.len()))
}

/// Deduplicate findings across shards (same rule/location/message keeps
/// only the first occurrence).
fn dedup_findings(findings: Vec<Finding>) -> Vec<Finding> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(findings.len());
    for f in findings {
        let key = format!(
            "{}|{}|{}",
            f.rule_id,
            serde_json::to_string(&f.location).unwrap_or_default(),
            f.message
        );
        if seen.insert(key) {
            out.push(f);
        }
    }
    out
}

/// The sharded audit path: per-file diffs grouped into bounded shards, one
/// agent run per shard, bisect-and-retry on incomplete shards, findings
/// aggregated through the gating rules.
///
/// Exit semantics separate security findings from audit infrastructure
/// failures: a gate violation exits 1; otherwise any shard that never
/// completed exits 2 with `status: incomplete` / `reason: shard_incomplete`.
async fn execute_sharded_task(config: &Config, sharding: &ShardingConfig) -> ExecutionReport {
    let start = Instant::now();
    let task_id = config.task.id.clone();
    let workspace = config.workspace.clone();

    let (file_diffs, risk_hits, shards) = match collect_shard_plan(&workspace, sharding).await {
        Ok(plan) => plan,
        Err(e) => {
            return ExecutionReport {
                task_id,
                exit_code: 2,
                findings: vec![],
                token_usage: Usage::default(),
                duration_ms: start.elapsed().as_millis() as u64,
                snapshot_id: None,
                errors: vec![format!("Sharding setup error: {}", e)],
                violations: vec![],
                status: RunStatus::Error,
                incomplete_reason: None,
            };
        }
    };

    if file_diffs.is_empty() {
        tracing::info!("no changed files in range — nothing to audit");
    }

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
                status: RunStatus::Error,
                incomplete_reason: None,
            };
        }
    };

    let mut queue: VecDeque<(crate::shard::Shard, u32, usize, u32)> = VecDeque::new();
    for (i, shard) in shards.into_iter().enumerate() {
        queue.push_back((shard, 0, i + 1, 0));
    }

    let mut all_findings: Vec<Finding> = Vec::new();
    let mut total_usage = Usage::default();
    let mut shard_records: Vec<ShardRecord> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut incomplete_shards = 0usize;

    while let Some((shard, splits_used, shard_no, rate_retries)) = queue.pop_front() {
        let shard_task_id = format!("{}-shard-{}", task_id, shard_no);
        let mut contract = config.task.clone();
        contract.id = shard_task_id.clone();
        contract.name = format!("{} (shard {})", config.task.name, shard_no);
        if let Some(tb) = sharding.per_shard.token_budget {
            contract.token_budget = tb;
        }
        if let Some(mt) = sharding.per_shard.max_total_tokens {
            contract.max_total_tokens = Some(mt);
        }
        if let Some(mi) = sharding.per_shard.max_iterations {
            contract.max_iterations = mi;
        }
        if let Some(ts) = sharding.per_shard.timeout_secs {
            contract.timeout_secs = ts;
        }
        contract.prompt_template = shard_prompt(&config.task.prompt_template);

        let manifest = crate::shard::build_manifest(&shard, &risk_hits);
        let diffs = shard
            .files
            .iter()
            .map(|f| f.diff.clone())
            .collect::<Vec<_>>()
            .join("\n");
        let initial_messages = vec![Message::new(
            Role::User,
            format!("{manifest}\n\n=== DIFFS ===\n{diffs}"),
        )];

        let event_log = Arc::new(EventLog::new(&workspace, &shard_task_id));
        let spill_store = Arc::new(
            crate::tools::SpillStore::new(workspace.clone(), &shard_task_id)
                .with_event_log(event_log.clone()),
        );
        let mut tools = default_tools(
            workspace.clone(),
            &contract.tool_allowlist,
            contract.shell_timeout_secs,
            &contract.shell_env_passthrough,
            Some(spill_store),
        );
        if !config.task.skills.is_empty() {
            tools.register(crate::tools::SkillTool::new(config.task.skills.clone()));
        }

        let agent_config = AgentConfig {
            contract: &contract,
            provider: provider.as_ref(),
            tools: &tools,
            initial_messages,
            workspace_root: workspace.clone(),
            snapshot_mgr: None,
            event_log: Some(&event_log),
        };

        let attempt = splits_used + 1;
        let agent_result = run_agent_loop(agent_config).await;
        // Normalize a provider timeout into an incomplete result so the
        // bisect policy applies to it too (smaller shards finish faster).
        let agent_result = match agent_result {
            Ok(r) => r,
            Err(ProviderError::Timeout(msg)) => {
                tracing::warn!(shard = shard_no, %msg, "shard timed out");
                crate::agent::AgentResult {
                    messages: vec![],
                    findings: vec![],
                    usage: Usage::default(),
                    duration_ms: 0,
                    truncated: true,
                    incomplete_reason: Some(IncompleteReason::Timeout),
                }
            }
            // Rate limiting is transient capacity, not a broken shard: back
            // off and re-run the SAME shard (bisecting would only re-bill
            // the same tokens against the same quota). Bounded so a hard
            // quota wall still terminates the run.
            Err(ProviderError::RateLimited(msg)) if rate_retries < MAX_SHARD_RATE_RETRIES => {
                let backoff = rate_limit_backoff(rate_retries);
                tracing::warn!(
                    shard = shard_no,
                    retry = rate_retries + 1,
                    backoff_secs = backoff.as_secs(),
                    %msg,
                    "rate limited; backing off and re-queuing shard"
                );
                eprintln!(
                    "Warning: shard {shard_no} rate limited; retrying in {}s (retry {}/{})",
                    backoff.as_secs(),
                    rate_retries + 1,
                    MAX_SHARD_RATE_RETRIES
                );
                tokio::time::sleep(backoff).await;
                queue.push_back((shard, splits_used, shard_no, rate_retries + 1));
                continue;
            }
            Err(ProviderError::RateLimited(msg)) => {
                errors.push(format!(
                    "shard {shard_no}: rate limited after {MAX_SHARD_RATE_RETRIES} retries: {msg}"
                ));
                shard_records.push(ShardRecord {
                    shard: shard_no,
                    files: shard.paths().into_iter().map(str::to_string).collect(),
                    status: "failed",
                    reason: Some("rate_limited".to_string()),
                    findings: 0,
                    tokens: 0,
                    attempts: attempt + rate_retries,
                });
                incomplete_shards += 1;
                continue;
            }
            Err(e) => {
                errors.push(format!("shard {shard_no}: agent error: {e}"));
                shard_records.push(ShardRecord {
                    shard: shard_no,
                    files: shard.paths().into_iter().map(str::to_string).collect(),
                    status: "failed",
                    reason: Some("agent_error".to_string()),
                    findings: 0,
                    tokens: 0,
                    attempts: attempt,
                });
                incomplete_shards += 1;
                continue;
            }
        };

        total_usage.input_tokens += agent_result.usage.input_tokens;
        total_usage.output_tokens += agent_result.usage.output_tokens;
        total_usage.total_tokens += agent_result.usage.total_tokens;
        let shard_findings = agent_result.findings.len();
        all_findings.extend(agent_result.findings);

        match agent_result.incomplete_reason {
            None => {
                shard_records.push(ShardRecord {
                    shard: shard_no,
                    files: shard.paths().into_iter().map(str::to_string).collect(),
                    status: "complete",
                    reason: None,
                    findings: shard_findings,
                    tokens: agent_result.usage.total_tokens,
                    attempts: attempt,
                });
                cleanup_archives(&workspace, &shard_task_id);
            }
            Some(reason) => {
                let bisectable = matches!(
                    reason,
                    IncompleteReason::ContextLimit
                        | IncompleteReason::IterationLimit
                        | IncompleteReason::TokenCap
                        | IncompleteReason::Length
                        | IncompleteReason::Timeout
                );
                let can_bisect = bisectable
                    && sharding.on_shard_incomplete == ShardIncompletePolicy::Bisect
                    && splits_used < sharding.max_splits
                    && crate::shard::bisect_shard(&shard).is_some();

                if can_bisect {
                    tracing::warn!(
                        shard = shard_no,
                        reason = reason.as_str(),
                        split = splits_used + 1,
                        "shard incomplete; bisecting and retrying"
                    );
                    let (left, right) = crate::shard::bisect_shard(&shard).expect("checked above");
                    queue.push_back((left, splits_used + 1, shard_no, rate_retries));
                    queue.push_back((right, splits_used + 1, shard_no, rate_retries));
                    continue;
                }

                if sharding.on_shard_incomplete == ShardIncompletePolicy::Fail {
                    errors.push(format!(
                        "shard {shard_no} incomplete (incomplete_reason={}); \
                         failing closed (on_shard_incomplete=fail)",
                        reason.as_str()
                    ));
                    return ExecutionReport {
                        task_id,
                        exit_code: 2,
                        findings: dedup_findings(all_findings),
                        token_usage: total_usage,
                        duration_ms: start.elapsed().as_millis() as u64,
                        snapshot_id: None,
                        errors,
                        violations: vec![],
                        status: RunStatus::Incomplete,
                        incomplete_reason: Some(IncompleteReason::Other),
                    };
                }

                errors.push(format!(
                    "shard {shard_no} ({}) incomplete after {} attempt(s): incomplete_reason={}",
                    shard.paths().join(", "),
                    attempt,
                    reason.as_str()
                ));
                incomplete_shards += 1;
                shard_records.push(ShardRecord {
                    shard: shard_no,
                    files: shard.paths().into_iter().map(str::to_string).collect(),
                    status: "incomplete",
                    reason: Some(reason.as_str().to_string()),
                    findings: shard_findings,
                    tokens: agent_result.usage.total_tokens,
                    attempts: attempt,
                });
            }
        }
    }

    let all_findings = dedup_findings(all_findings);
    let gate_result = RuleEngine::evaluate(&all_findings, &config.task.gating_rules);

    // Security findings outrank infrastructure failures in the exit code
    // only when a gate actually fails; otherwise an incomplete audit must
    // not pass.
    let (exit_code, status, reason_code) = if gate_result.exit_code != 0 {
        (gate_result.exit_code, RunStatus::Complete, None)
    } else if incomplete_shards > 0 {
        (2, RunStatus::Incomplete, Some("shard_incomplete"))
    } else {
        (0, RunStatus::Complete, None)
    };

    if let Err(e) = SarifFormatter::write_to_file_with_status(
        &all_findings,
        &config.output,
        status == RunStatus::Incomplete,
        reason_code,
    ) {
        eprintln!("Warning: Failed to write SARIF: {}", e);
    }

    // The sharded run always writes a summary — it is the aggregation
    // contract CI consumes (per-shard statuses + overall status).
    let summary_path = config.summary.clone().unwrap_or_else(|| {
        config.output.with_file_name(
            config
                .output
                .file_name()
                .map(|n| n.to_string_lossy().to_string() + ".summary.json")
                .unwrap_or_else(|| "clausura-summary.json".into()),
        )
    });
    write_sharded_summary(
        &summary_path,
        &task_id,
        status,
        reason_code,
        &all_findings,
        exit_code,
        &total_usage,
        start.elapsed().as_millis() as u64,
        &shard_records,
    );

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
        token_usage: total_usage,
        duration_ms: start.elapsed().as_millis() as u64,
        snapshot_id: None,
        errors,
        violations: gate_result.violations,
        status,
        incomplete_reason: None,
    }
}

/// Write the sharded run summary JSON (always written for sharded runs).
#[allow(clippy::too_many_arguments)]
fn write_sharded_summary(
    path: &Path,
    task_id: &str,
    status: RunStatus,
    reason: Option<&str>,
    findings: &[Finding],
    exit_code: u32,
    usage: &Usage,
    duration_ms: u64,
    shard_records: &[ShardRecord],
) {
    let summary = serde_json::json!({
        "task_id": task_id,
        "status": status,
        "reason": reason,
        "findings_count": findings.len(),
        "exit_code": exit_code,
        "token_usage": usage,
        "duration_ms": duration_ms,
        "shards": shard_records,
    });
    match serde_json::to_string_pretty(&summary)
        .map_err(|e| e.to_string())
        .and_then(|content| std::fs::write(path, content).map_err(|e| e.to_string()))
    {
        Ok(()) => {}
        Err(e) => eprintln!("Warning: Failed to write summary {}: {}", path.display(), e),
    }
}

async fn execute_single_task(config: &Config) -> ExecutionReport {
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
                status: RunStatus::Error,
                incomplete_reason: None,
            };
        }
    };

    // Append-only run event log: audit trail + checkpoint fallback for
    // resume in ephemeral CI environments where ~/.clausura does not survive.
    // Created before the tools so the spill store can record ToolSpill events.
    let event_log = Arc::new(EventLog::new(&config.workspace, &task_id));

    let spill_store = Arc::new(
        crate::tools::SpillStore::new(config.workspace.clone(), &task_id)
            .with_event_log(event_log.clone()),
    );

    let mut tools = default_tools(
        config.workspace.clone(),
        &config.task.tool_allowlist,
        config.task.shell_timeout_secs,
        &config.task.shell_env_passthrough,
        Some(spill_store.clone()),
    );

    if let Some(range) = &config.review_range {
        tools.register(
            crate::tools::GitDiffTool::new(config.workspace.clone())
                .with_review_range(range.clone())
                .with_spill_maybe(Some(spill_store)),
        );
    }

    // Progressive skill disclosure: bodies served by read_skill on demand.
    if !config.task.skills.is_empty() {
        tools.register(crate::tools::SkillTool::new(config.task.skills.clone()));
    }

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
                status: RunStatus::Error,
                incomplete_reason: None,
            };
        }
    };
    let snapshot_mgr = SnapshotManager::new(checkpoint_store);

    let mut initial_messages = if config.resume {
        match snapshot_mgr.restore_snapshot(&task_id, true) {
            Ok(Some(snapshot)) => snapshot.messages,
            _ => {
                // SQLite store empty or unavailable — fall back to the last
                // checkpoint event recorded in the workspace event log.
                let from_event_log = event_log.last_checkpoint().map(|mut messages| {
                    messages.push(Message::new(
                        Role::User,
                        "You were interrupted. Continue from where you left off.".to_string(),
                    ));
                    messages
                });
                from_event_log.unwrap_or_else(|| {
                    vec![Message::new(
                        Role::User,
                        config.task.prompt_template.clone(),
                    )]
                })
            }
        }
    } else {
        vec![Message::new(
            Role::User,
            config.task.prompt_template.clone(),
        )]
    };

    if let Some((base, head)) = &config.review_range {
        initial_messages.push(Message::new(
            Role::User,
            format!(
                "Review committed changes from {base} to {head}. Start with git_diff; \
                     it is pinned to this range even in a clean checkout. Read surrounding \
                     files only as needed to validate findings."
            ),
        ));
    }

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
        event_log: Some(&event_log),
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
                status: RunStatus::Error,
                incomplete_reason: Some(IncompleteReason::Timeout),
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
                status: RunStatus::Error,
                incomplete_reason: None,
            };
        }
    };

    let snapshot_id = snapshot_mgr
        .save_snapshot(&task_id, &agent_result.messages, agent_result.truncated)
        .ok();

    // Record the final state as a checkpoint event in the run log, so
    // `--resume` can restore from the workspace even without the SQLite store.
    if let Some(id) = snapshot_id {
        event_log.append(&RunEvent::Checkpoint {
            checkpoint_id: id.to_string(),
            messages: agent_result.messages.clone(),
            truncated: agent_result.truncated,
        });
    }

    // Merge preflight findings (deterministic) with agent findings.
    let all_findings = [preflight_findings, agent_result.findings].concat();

    let gate_result = RuleEngine::evaluate(&all_findings, &config.task.gating_rules);

    // Fail closed on incomplete runs (context truncated, iteration limit,
    // token cap, or malformed final answer): a partial sweep with zero
    // findings must not pass gates like `max_findings: 0`.
    let mut errors = Vec::new();
    let exit_code = apply_incomplete_policy(
        gate_result.exit_code,
        agent_result.incomplete_reason,
        config.task.on_incomplete,
        &mut errors,
    );

    let status = if agent_result.incomplete_reason.is_some() {
        RunStatus::Incomplete
    } else {
        RunStatus::Complete
    };

    if let Some(reason) = agent_result.incomplete_reason {
        if config.task.on_incomplete == OnIncompletePolicy::Pass {
            eprintln!(
                "Warning: agent run incomplete (incomplete_reason={}); \
                 continuing with partial results (on_incomplete=pass)",
                reason.as_str()
            );
        }
    }

    let reason_code = agent_result.incomplete_reason.map(|r| r.as_str());
    if let Err(e) = SarifFormatter::write_to_file_with_status(
        &all_findings,
        &config.output,
        agent_result.incomplete_reason.is_some(),
        reason_code,
    ) {
        eprintln!("Warning: Failed to write SARIF: {}", e);
    }

    // Machine-readable run summary for CI: distinguishes "security findings"
    // (gating) from "audit infrastructure failure" (status/reason) without
    // parsing SARIF.
    if let Some(summary_path) = &config.summary {
        write_run_summary(
            summary_path,
            &task_id,
            status,
            agent_result.incomplete_reason,
            &all_findings,
            exit_code,
            &agent_result.usage,
            agent_result.duration_ms,
        );
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
        status,
        incomplete_reason: agent_result.incomplete_reason,
    }
}

/// Write the machine-readable run summary JSON (`--summary <PATH>`).
/// Best-effort: failures log a warning and never fail the run.
#[allow(clippy::too_many_arguments)]
fn write_run_summary(
    path: &Path,
    task_id: &str,
    status: RunStatus,
    incomplete_reason: Option<IncompleteReason>,
    findings: &[Finding],
    exit_code: u32,
    usage: &Usage,
    duration_ms: u64,
) {
    let summary = serde_json::json!({
        "task_id": task_id,
        "status": status,
        "reason": incomplete_reason,
        "findings_count": findings.len(),
        "exit_code": exit_code,
        "token_usage": usage,
        "duration_ms": duration_ms,
    });
    match serde_json::to_string_pretty(&summary)
        .map_err(|e| e.to_string())
        .and_then(|content| std::fs::write(path, content).map_err(|e| e.to_string()))
    {
        Ok(()) => {}
        Err(e) => eprintln!("Warning: Failed to write summary {}: {}", path.display(), e),
    }
}

/// Decide the final exit code when the agent run may be incomplete.
///
/// A complete run is returned unchanged. For an incomplete run (context
/// truncated, iteration/token cap, model length limit, content filter, or an
/// unparseable final answer — see `IncompleteReason`):
/// - `OnIncompletePolicy::Fail` fails closed: returns 2 (error) and pushes a
///   diagnostic (with the machine-readable reason code) to `errors`,
///   regardless of the gate result — an incomplete sweep must not pass gates,
///   and a runtime error outranks a rule violation.
/// - `OnIncompletePolicy::Pass` keeps the gate result unchanged; the caller
///   warns and annotates the SARIF output instead.
fn apply_incomplete_policy(
    gate_exit_code: u32,
    incomplete_reason: Option<IncompleteReason>,
    policy: OnIncompletePolicy,
    errors: &mut Vec<String>,
) -> u32 {
    let Some(reason) = incomplete_reason else {
        return gate_exit_code;
    };
    match policy {
        OnIncompletePolicy::Fail => {
            errors.push(format!(
                "Agent run incomplete (incomplete_reason={}); failing closed (on_incomplete=fail)",
                reason.as_str()
            ));
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
    let spill_prefix = format!("tool-output-{}", task_id);
    let event_prefix = format!("run-{}", task_id);
    if let Ok(entries) = std::fs::read_dir(&archives_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let is_dump = name_str.starts_with(&dump_prefix) && name_str.ends_with(".log");
            let is_ledger = name_str.starts_with(&ledger_prefix) && name_str.ends_with(".jsonl");
            let is_spill = name_str.starts_with(&spill_prefix) && name_str.ends_with(".txt");
            let is_event =
                name_str.starts_with(&event_prefix) && name_str.ends_with(".events.jsonl");
            if is_dump || is_ledger || is_spill || is_event {
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
        std::fs::write(archives_dir.join("run-test-task.events.jsonl"), "{}").unwrap();
        std::fs::write(archives_dir.join("some-other-file.txt"), "other").unwrap();

        cleanup_archives(tmp.path(), "test-task");

        assert!(!archives_dir.join("context-dump-test-task-1.log").exists());
        assert!(!archives_dir.join("context-dump-test-task-2.log").exists());
        assert!(!archives_dir.join("run-test-task.events.jsonl").exists());
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
        let code = apply_incomplete_policy(
            0,
            Some(IncompleteReason::IterationLimit),
            OnIncompletePolicy::Fail,
            &mut errors,
        );
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
        assert!(
            errors[0].contains("incomplete_reason=iteration_limit"),
            "got: {}",
            errors[0]
        );
    }

    #[test]
    fn test_incomplete_pass_policy_keeps_gate_result() {
        let mut errors = Vec::new();
        assert_eq!(
            apply_incomplete_policy(
                0,
                Some(IncompleteReason::ContextLimit),
                OnIncompletePolicy::Pass,
                &mut errors
            ),
            0
        );
        assert_eq!(
            apply_incomplete_policy(
                1,
                Some(IncompleteReason::MalformedJson),
                OnIncompletePolicy::Pass,
                &mut errors
            ),
            1
        );
        assert!(errors.is_empty());
    }

    #[test]
    fn test_incomplete_fail_policy_error_outranks_violation() {
        // gate=1 + incomplete + Fail → 2: the run itself is untrustworthy,
        // so the runtime error takes precedence over the rule violation.
        let mut errors = Vec::new();
        let code = apply_incomplete_policy(
            1,
            Some(IncompleteReason::TokenCap),
            OnIncompletePolicy::Fail,
            &mut errors,
        );
        assert_eq!(code, 2);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn test_complete_run_unchanged_by_policy() {
        let mut errors = Vec::new();
        assert_eq!(
            apply_incomplete_policy(0, None, OnIncompletePolicy::Fail, &mut errors),
            0
        );
        assert_eq!(
            apply_incomplete_policy(1, None, OnIncompletePolicy::Fail, &mut errors),
            1
        );
        assert_eq!(
            apply_incomplete_policy(0, None, OnIncompletePolicy::Pass, &mut errors),
            0
        );
        assert!(errors.is_empty());
    }

    #[test]
    fn test_write_run_summary_contents() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("summary.json");
        let findings = vec![Finding {
            id: uuid::Uuid::new_v4(),
            rule_id: "xss".into(),
            severity: Severity::Error,
            message: "reflected input".into(),
            location: None,
            evidence: "".into(),
        }];
        write_run_summary(
            &path,
            "task-1",
            RunStatus::Incomplete,
            Some(IncompleteReason::ContextLimit),
            &findings,
            2,
            &Usage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
            1234,
        );
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["status"], "incomplete");
        assert_eq!(parsed["reason"], "context_limit");
        assert_eq!(parsed["findings_count"], 1);
        assert_eq!(parsed["exit_code"], 2);
        assert_eq!(parsed["token_usage"]["total_tokens"], 15);
        assert_eq!(parsed["duration_ms"], 1234);

        // Complete run: reason serializes to null, not a string.
        write_run_summary(
            &path,
            "task-1",
            RunStatus::Complete,
            None,
            &[],
            0,
            &Usage::default(),
            0,
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["status"], "complete");
        assert_eq!(parsed["reason"], serde_json::Value::Null);
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
        let registry = default_tools(tmp.path().to_path_buf(), &[], 120, &[], None);
        let hint = detect_lsp_tools(&registry);
        assert!(hint.is_none(), "no LSP tools configured → no hint");
    }

    // ── sharded audit helpers ─────────────────────────────────────────────

    async fn git_cmd(root: &Path, args: &[&str]) {
        tokio::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_DATE", "2024-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2024-01-01T00:00:00Z")
            .output()
            .await
            .unwrap();
    }

    /// A git repo with a base commit and a HEAD commit touching two files,
    /// one of which contains a route registration.
    async fn setup_repo_with_pr() -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        git_cmd(&root, &["init"]).await;
        git_cmd(&root, &["config", "user.email", "t@t.com"]).await;
        git_cmd(&root, &["config", "user.name", "T"]).await;
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        git_cmd(&root, &["add", "."]).await;
        git_cmd(&root, &["commit", "-m", "base"]).await;

        std::fs::write(
            root.join("router.ts"),
            "export const r = () => app.get(\"/users\", handler);\n",
        )
        .unwrap();
        std::fs::write(root.join("plain.txt"), "hello world\n").unwrap();
        git_cmd(&root, &["add", "."]).await;
        git_cmd(&root, &["commit", "-m", "change"]).await;
        (tmp, root)
    }

    fn sharding_cfg(base: &str) -> crate::types::ShardingConfig {
        crate::types::ShardingConfig {
            base: base.to_string(),
            paths: vec![],
            max_diff_bytes: 128 * 1024,
            max_files_per_shard: 8,
            max_splits: 3,
            context_lines: 20,
            per_shard: crate::types::ShardBudget::default(),
            on_shard_incomplete: crate::types::ShardIncompletePolicy::Bisect,
            risk_patterns: Default::default(),
        }
    }

    #[tokio::test]
    async fn test_collect_shard_plan_from_git() {
        let (_tmp, root) = setup_repo_with_pr().await;
        let (file_diffs, risk_hits, shards) = collect_shard_plan(&root, &sharding_cfg("HEAD~1"))
            .await
            .unwrap();

        let mut paths: Vec<&str> = file_diffs.iter().map(|f| f.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["plain.txt", "router.ts"]);
        assert!(file_diffs
            .iter()
            .all(|f| f.diff.contains("@@") && f.diff_bytes > 0));
        assert!(
            risk_hits
                .iter()
                .any(|h| h.file == "router.ts" && h.tag == "route"),
            "route registration must be flagged, got: {risk_hits:?}"
        );
        assert_eq!(shards.len(), 1, "small PR packs into one shard");
    }

    #[tokio::test]
    async fn test_collect_shard_plan_respects_path_filter() {
        let (_tmp, root) = setup_repo_with_pr().await;
        let mut cfg = sharding_cfg("HEAD~1");
        cfg.paths = vec!["router.ts".to_string()];
        let (file_diffs, _hits, _shards) = collect_shard_plan(&root, &cfg).await.unwrap();
        assert_eq!(file_diffs.len(), 1);
        assert_eq!(file_diffs[0].path, "router.ts");
    }

    #[tokio::test]
    async fn test_collect_shard_plan_invalid_base_is_error() {
        let (_tmp, root) = setup_repo_with_pr().await;
        assert!(collect_shard_plan(&root, &sharding_cfg("no-such-ref"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_plan_sharding_preview_counts() {
        let (_tmp, root) = setup_repo_with_pr().await;
        let (files, _bytes, shards) = plan_sharding_preview(&root, &sharding_cfg("HEAD~1"))
            .await
            .unwrap();
        assert_eq!(files, 2);
        assert_eq!(shards, 1);
    }

    #[test]
    fn test_rate_limit_backoff_schedule() {
        use std::time::Duration;
        assert_eq!(rate_limit_backoff(0), Duration::from_secs(30));
        assert_eq!(rate_limit_backoff(1), Duration::from_secs(60));
        assert_eq!(rate_limit_backoff(2), Duration::from_secs(120));
        // Clamped so a miscounted retry counter cannot overflow the shift.
        assert_eq!(rate_limit_backoff(9), Duration::from_secs(30 << 4));
    }

    #[test]
    fn test_dedup_findings_across_shards() {
        let mk = |msg: &str| Finding {
            id: uuid::Uuid::new_v4(),
            rule_id: "xss".into(),
            severity: Severity::Error,
            message: msg.into(),
            location: Some(crate::types::Location {
                file: "a.ts".into(),
                line_start: 1,
                line_end: 1,
                column_start: 1,
                column_end: 1,
            }),
            evidence: "e".into(),
        };
        let deduped = dedup_findings(vec![mk("dup"), mk("dup"), mk("unique")]);
        assert_eq!(deduped.len(), 2);
    }

    #[test]
    fn test_write_sharded_summary_contents() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("summary.json");
        let records = vec![ShardRecord {
            shard: 1,
            files: vec!["a.ts".into(), "b.ts".into()],
            status: "complete",
            reason: None,
            findings: 2,
            tokens: 1000,
            attempts: 1,
        }];
        write_sharded_summary(
            &path,
            "task-x",
            RunStatus::Incomplete,
            Some("shard_incomplete"),
            &[],
            2,
            &Usage::default(),
            42,
            &records,
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["status"], "incomplete");
        assert_eq!(parsed["reason"], "shard_incomplete");
        assert_eq!(parsed["shards"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["shards"][0]["status"], "complete");
        assert_eq!(parsed["shards"][0]["findings"], 2);
        assert_eq!(parsed["shards"][0]["attempts"], 1);
    }
}
