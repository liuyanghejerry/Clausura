//! Effect-oriented evaluation harness (`clausura eval`).
//!
//! Unit tests verify behavior; this module measures *effect*. A scenario
//! couples a fixture workspace with a task config and ground-truth
//! expectations. Each variant is run `runs` times, and every run's outcome is
//! distilled from the append-only run event log into metrics that matter:
//!
//! - **success** — clean finish (no truncation / iteration cap / overflow)
//!   — the run actually completed its sweep
//! - **recall** — ground-truth findings actually reported
//! - **cost** — billed tokens, LLM round-trips, wall time
//! - **mechanism effects** — compactions, spills, repeat-call reminders,
//!   findings-recovery attempts and their success rate
//!
//! Reports are JSON + Markdown, and `--baseline` diffs two reports — so an
//! implementation switch (e.g. inline skills → progressive disclosure) can be
//! justified with numbers instead of assertions.
//!
//! Requires an API key (`CLAUSURA_API_KEY` or `--api-key`): the harness runs
//! real tasks against a real provider, exactly like CI does.

use crate::config::{Config, LogFormat};
use crate::eventlog::{EventLog, RunEvent};
use crate::executor::execute_task;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Scenario schema (eval.yaml)
// ---------------------------------------------------------------------------

/// Root of an eval configuration file.
#[derive(Debug, Clone, Deserialize)]
pub struct EvalConfig {
    #[serde(default)]
    pub scenarios: Vec<EvalScenario>,
}

/// One scenario: a fixture workspace + task config + ground truth.
#[derive(Debug, Clone, Deserialize)]
pub struct EvalScenario {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Fixture workspace directory, relative to the eval config file.
    pub workspace: PathBuf,
    /// Clausura task config file, relative to the workspace.
    pub config: PathBuf,
    /// Expected findings per rule_id.
    #[serde(default)]
    pub ground_truth: Vec<GroundTruthRule>,
    /// Config variants to compare (default: single "default" variant).
    #[serde(default)]
    pub variants: Vec<EvalVariant>,
    /// Repeats per variant — LLM output is nondeterministic. Default 3.
    #[serde(default = "default_runs")]
    pub runs: u32,
}

fn default_runs() -> u32 {
    3
}

/// A ground-truth expectation: at least `min_findings` findings with this
/// rule_id must be reported.
#[derive(Debug, Clone, Deserialize)]
pub struct GroundTruthRule {
    pub rule_id: String,
    pub min_findings: u32,
}

/// A config variant: a name plus a partial YAML object merged over the
/// scenario's config.
#[derive(Debug, Clone, Deserialize)]
pub struct EvalVariant {
    pub name: String,
    #[serde(default)]
    pub config_overrides: serde_yaml::Value,
}

impl EvalScenario {
    /// Effective variants: the configured list, or a single "default" variant.
    pub fn effective_variants(&self) -> Vec<EvalVariant> {
        if self.variants.is_empty() {
            vec![EvalVariant {
                name: "default".into(),
                config_overrides: serde_yaml::Value::Null,
            }]
        } else {
            self.variants.clone()
        }
    }
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

/// Effect metrics for a single run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunMetrics {
    pub scenario: String,
    pub variant: String,
    pub run_index: u32,
    pub exit_code: u32,
    /// Run ended without a clean Stop (context truncation, iteration cap, …).
    pub truncated: bool,
    pub duration_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    /// LLM round-trips (agent iterations).
    pub llm_requests: u32,
    pub tool_calls: u32,
    /// Oversized tool outputs preserved to disk instead of truncated away.
    pub tool_spills: u32,
    /// Context truncation events.
    pub context_truncations: u32,
    /// Truncation events with an auto-compact summary.
    pub compactions: u32,
    /// Advisories injected for repeated identical tool calls.
    pub repeat_reminders: u32,
    /// Corrective findings-recovery prompts sent.
    pub recovery_attempts: u32,
    /// Findings recovered after ≥1 recovery attempt (clean finish).
    pub recovered: bool,
    pub findings_count: u32,
    /// Ground-truth rules matched (findings ≥ min_findings).
    pub truth_matched: u32,
    /// Total ground-truth rules for this scenario.
    pub truth_total: u32,
    /// Findings whose rule_id has no ground-truth entry.
    pub unexpected_rule_ids: Vec<String>,
}

impl RunMetrics {
    /// Recall over ground-truth rules.
    pub fn recall(&self) -> f64 {
        if self.truth_total == 0 {
            1.0
        } else {
            self.truth_matched as f64 / self.truth_total as f64
        }
    }

    /// The run "succeeded": it finished cleanly (no truncation / iteration-cap
    /// / overflow). The gate exit code is a *scenario property* — a scenario
    /// whose ground truth is real issues correctly exits 1 — so success here
    /// deliberately ignores it. See `exit_code` for the gate verdict.
    pub fn success(&self) -> bool {
        !self.truncated
    }
}

/// Distill a run's effect metrics from its event log and execution report.
pub fn metrics_from_run(
    scenario: &str,
    variant: &str,
    run_index: u32,
    report: &crate::types::ExecutionReport,
    events: &[RunEvent],
    ground_truth: &[GroundTruthRule],
) -> RunMetrics {
    let mut m = RunMetrics {
        scenario: scenario.into(),
        variant: variant.into(),
        run_index,
        exit_code: report.exit_code,
        duration_ms: report.duration_ms,
        input_tokens: report.token_usage.input_tokens,
        output_tokens: report.token_usage.output_tokens,
        total_tokens: report.token_usage.total_tokens,
        findings_count: report.findings.len() as u32,
        truth_total: ground_truth.len() as u32,
        ..Default::default()
    };

    for ev in events {
        match ev {
            RunEvent::LlmRequest { .. } => m.llm_requests += 1,
            RunEvent::ToolCall { .. } => m.tool_calls += 1,
            RunEvent::ToolSpill { .. } => m.tool_spills += 1,
            RunEvent::ContextTruncated {
                compacted_summary: Some(_),
                ..
            } => {
                m.context_truncations += 1;
                m.compactions += 1;
            }
            RunEvent::ContextTruncated { .. } => m.context_truncations += 1,
            RunEvent::RepeatReminder { .. } => m.repeat_reminders += 1,
            RunEvent::FindingsRecoveryAttempt { .. } => m.recovery_attempts += 1,
            RunEvent::RunEnd { truncated, .. } => m.truncated = *truncated,
            _ => {}
        }
    }
    m.recovered = !m.truncated && m.recovery_attempts > 0;

    // Ground truth evaluation.
    let truth_ids: Vec<&str> = ground_truth.iter().map(|r| r.rule_id.as_str()).collect();
    for rule in ground_truth {
        let count = report
            .findings
            .iter()
            .filter(|f| f.rule_id == rule.rule_id)
            .count() as u32;
        if count >= rule.min_findings {
            m.truth_matched += 1;
        }
    }
    let mut unexpected: Vec<String> = report
        .findings
        .iter()
        .map(|f| f.rule_id.clone())
        .filter(|id| !truth_ids.contains(&id.as_str()))
        .collect();
    unexpected.sort();
    unexpected.dedup();
    m.unexpected_rule_ids = unexpected;

    m
}

/// Read and parse the run event log. An unreadable or partially corrupt log
/// yields what it can (never fails the eval).
pub fn read_run_events(path: &Path) -> Vec<RunEvent> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|l| serde_json::from_str::<RunEvent>(l).ok())
        .collect()
}

// ---------------------------------------------------------------------------
// Aggregation & reports
// ---------------------------------------------------------------------------

/// Aggregated metrics for one scenario × variant.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VariantSummary {
    pub scenario: String,
    pub variant: String,
    pub runs: u32,
    /// Fraction of runs that finished cleanly (no truncation / iteration-cap).
    pub success_rate: f64,
    /// Mean billed tokens per run.
    pub mean_total_tokens: f64,
    /// Mean LLM round-trips per run.
    pub mean_llm_requests: f64,
    /// Mean wall time per run (ms).
    pub mean_duration_ms: f64,
    /// Mean compactions per run.
    pub mean_compactions: f64,
    /// Mean tool spills per run.
    pub mean_tool_spills: f64,
    /// Mean repeat-call reminders per run.
    pub mean_repeat_reminders: f64,
    /// Recovery success rate: recovered runs / runs with ≥1 attempt.
    pub recovery_success_rate: f64,
    /// Recall over ground-truth rules (all runs pooled).
    pub recall: f64,
    /// Unexpected rule_ids seen across runs.
    pub unexpected_rule_ids: Vec<String>,
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

/// Aggregate per-run metrics into a variant summary.
pub fn summarize_variant(runs: &[RunMetrics]) -> VariantSummary {
    if runs.is_empty() {
        return VariantSummary::default();
    }
    let mut s = VariantSummary {
        scenario: runs[0].scenario.clone(),
        variant: runs[0].variant.clone(),
        runs: runs.len() as u32,
        ..Default::default()
    };
    s.success_rate = runs.iter().filter(|m| m.success()).count() as f64 / runs.len() as f64;
    s.mean_total_tokens = mean(
        &runs
            .iter()
            .map(|m| m.total_tokens as f64)
            .collect::<Vec<_>>(),
    );
    s.mean_llm_requests = mean(
        &runs
            .iter()
            .map(|m| m.llm_requests as f64)
            .collect::<Vec<_>>(),
    );
    s.mean_duration_ms = mean(
        &runs
            .iter()
            .map(|m| m.duration_ms as f64)
            .collect::<Vec<_>>(),
    );
    s.mean_compactions = mean(
        &runs
            .iter()
            .map(|m| m.compactions as f64)
            .collect::<Vec<_>>(),
    );
    s.mean_tool_spills = mean(
        &runs
            .iter()
            .map(|m| m.tool_spills as f64)
            .collect::<Vec<_>>(),
    );
    s.mean_repeat_reminders = mean(
        &runs
            .iter()
            .map(|m| m.repeat_reminders as f64)
            .collect::<Vec<_>>(),
    );
    let attempts: u32 = runs.iter().map(|m| m.recovery_attempts).sum();
    let recovered: u32 = runs.iter().filter(|m| m.recovered).map(|_| 1).sum();
    s.recovery_success_rate = if attempts == 0 {
        0.0
    } else {
        recovered as f64 / attempts as f64
    };
    let matched: u32 = runs.iter().map(|m| m.truth_matched).sum();
    let total: u32 = runs.iter().map(|m| m.truth_total).sum();
    s.recall = if total == 0 {
        1.0
    } else {
        matched as f64 / total as f64
    };
    let mut unexpected: Vec<String> = runs
        .iter()
        .flat_map(|m| m.unexpected_rule_ids.clone())
        .collect();
    unexpected.sort();
    unexpected.dedup();
    s.unexpected_rule_ids = unexpected;
    s
}

/// Full evaluation report, one per scenario.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvalReport {
    pub generated_at: String,
    /// Git commit the binary/repo was built from (best-effort).
    pub commit: String,
    pub branch: String,
    pub model: String,
    pub scenarios: Vec<ScenarioSummary>,
}

/// Scenario-level report.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScenarioSummary {
    pub name: String,
    pub variants: Vec<VariantSummary>,
}

impl EvalReport {
    /// Look up a variant summary across scenarios.
    pub fn find(&self, scenario: &str, variant: &str) -> Option<&VariantSummary> {
        self.scenarios
            .iter()
            .find(|s| s.name == scenario)
            .and_then(|s| s.variants.iter().find(|v| v.variant == variant))
    }

    /// Render the report as a Markdown table.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "# Eval report — {} ({} @ {})\n\nmodel: `{}`\n\n",
            self.generated_at, self.branch, self.commit, self.model
        ));
        out.push_str(
            "| scenario | variant | runs | success | recall | mean tokens | mean LLM calls | \
             mean time (s) | compactions | spills | reminders | recovery success |\n\
             |----------|---------|------|---------|--------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|\n",
        );
        for s in &self.scenarios {
            for v in &s.variants {
                out.push_str(&format!(
                    "| {} | {} | {} | {:.0}% | {:.0}% | {:.0} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.0}%\n",
                    s.name,
                    v.variant,
                    v.runs,
                    v.success_rate * 100.0,
                    v.recall * 100.0,
                    v.mean_total_tokens,
                    v.mean_llm_requests,
                    v.mean_duration_ms / 1000.0,
                    v.mean_compactions,
                    v.mean_tool_spills,
                    v.mean_repeat_reminders,
                    v.recovery_success_rate * 100.0,
                ));
            }
        }
        out
    }
}

/// Compare two reports (baseline → current) as a Markdown diff table.
/// Deltas are `current - baseline`; arrows show direction of change.
pub fn compare_reports(baseline: &EvalReport, current: &EvalReport) -> String {
    fn delta(cur: f64, old: f64) -> (f64, &'static str) {
        let d = cur - old;
        let arrow = if d > 0.0 {
            "▲"
        } else if d < 0.0 {
            "▼"
        } else {
            "—"
        };
        (d, arrow)
    }

    let mut out = String::new();
    out.push_str(&format!(
        "# Eval comparison — {} → {}\n\n",
        baseline.commit, current.commit
    ));
    out.push_str(
        "| scenario | variant | success | recall | mean tokens | mean LLM calls | compactions | spills | recovery success |\n\
         |----------|---------|---------|--------|-------------|----------------|-------------|--------|------------------|\n",
    );
    for s in &current.scenarios {
        for v in &s.variants {
            match baseline.find(&s.name, &v.variant) {
                Some(b) => {
                    let (sd, sa) = delta(v.success_rate, b.success_rate);
                    let (rd, ra) = delta(v.recall, b.recall);
                    let (td, ta) = delta(v.mean_total_tokens, b.mean_total_tokens);
                    let (ld, la) = delta(v.mean_llm_requests, b.mean_llm_requests);
                    let (cd, ca) = delta(v.mean_compactions, b.mean_compactions);
                    let (pd, pa) = delta(v.mean_tool_spills, b.mean_tool_spills);
                    let (yd, ya) = delta(v.recovery_success_rate, b.recovery_success_rate);
                    out.push_str(&format!(
                        "| {} | {} | {}{:+.1}pp | {}{:+.1}pp | {}{:+.0} | {}{:+.1} | {}{:+.1} | {}{:+.1} | {}{:+.1}pp\n",
                        s.name,
                        v.variant,
                        sa,
                        sd * 100.0,
                        ra,
                        rd * 100.0,
                        ta,
                        td,
                        la,
                        ld,
                        ca,
                        cd,
                        pa,
                        pd,
                        ya,
                        yd * 100.0,
                    ));
                }
                None => out.push_str(&format!(
                    "| {} | {} | — (new variant, no baseline) | | | | | | |\n",
                    s.name, v.variant
                )),
            }
        }
    }
    out.push_str(
        "\n▲ = higher than baseline, ▼ = lower than baseline \
         (lower mean tokens/LLM calls is cheaper; higher success/recall is better).\n",
    );
    out
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

/// Options for a full eval run.
#[derive(Debug, Clone)]
pub struct EvalOptions {
    /// LLM API key (flag or env; required to actually run).
    pub api_key: Option<String>,
    /// Override the model for every task.
    pub model: Option<String>,
    /// Override the vendor for every task.
    pub vendor: Option<String>,
    /// Override `runs` for every scenario.
    pub runs: Option<u32>,
    /// Only run scenarios matching this name.
    pub scenario_filter: Option<String>,
    /// Directory for SARIF artifacts, per-run event logs, and the report.
    pub out_dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("Missing API key: set CLAUSURA_API_KEY or pass --api-key")]
    MissingApiKey,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Eval config error: {0}")]
    Config(String),
    #[error("Clausura config error: {0}")]
    ClausuraConfig(#[from] crate::types::ConfigError),
    #[error("Scenario not found: {0}")]
    ScenarioNotFound(String),
}

/// Load and parse an eval config file.
pub fn load_eval_config(path: &Path) -> Result<(EvalConfig, PathBuf), EvalError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| EvalError::Config(format!("{}: {e}", path.display())))?;
    let config: EvalConfig = serde_yaml::from_str(&content)
        .map_err(|e| EvalError::Config(format!("{}: {e}", path.display())))?;
    let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    Ok((config, base))
}

/// Deep-merge `over` into `base` (mappings merge recursively, scalars replace).
pub fn deep_merge(base: &mut serde_yaml::Value, over: serde_yaml::Value) {
    match (base, over) {
        (serde_yaml::Value::Mapping(b), serde_yaml::Value::Mapping(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(bv) => deep_merge(bv, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

/// Copy a directory tree (files only) to `dst`.
fn copy_dir_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_tree(&from, &to)?;
        } else if ft.is_file() {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Initialize a throwaway git repo in `ws` so `git_diff` has a HEAD to diff
/// against. Best-effort: fixtures also work when git is unavailable.
fn init_git_repo(ws: &Path) {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(ws)
            .args(args)
            .output()
            .is_ok()
    };
    if git(&["init", "-q"]) {
        let _ = git(&["add", "-A"]);
        let _ = git(&[
            "-c",
            "user.email=eval@clausura.invalid",
            "-c",
            "user.name=Clausura Eval",
            "commit",
            "-qm",
            "eval fixture",
        ]);
    }
}

/// Best-effort git context for report provenance.
fn git_context() -> (String, String) {
    let short = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    let branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "detached".into());
    (short, branch)
}

/// Run the full eval: every scenario × variant × run. Returns the report.
pub async fn run_eval(eval_path: &Path, opts: &EvalOptions) -> Result<EvalReport, EvalError> {
    let api_key = opts
        .api_key
        .clone()
        .or_else(|| std::env::var("CLAUSURA_API_KEY").ok())
        .ok_or(EvalError::MissingApiKey)?;

    let (eval_config, base_dir) = load_eval_config(eval_path)?;
    let (commit, branch) = git_context();
    let mut report = EvalReport {
        generated_at: chrono::Utc::now().to_rfc3339(),
        commit,
        branch,
        model: opts
            .model
            .clone()
            .unwrap_or_else(|| "from config".to_string()),
        scenarios: Vec::new(),
    };

    for scenario in &eval_config.scenarios {
        if let Some(filter) = &opts.scenario_filter {
            if &scenario.name != filter {
                continue;
            }
        }
        eprintln!("Scenario: {}", scenario.name);
        let ws_src = base_dir.join(&scenario.workspace);
        if !ws_src.exists() {
            return Err(EvalError::Config(format!(
                "scenario '{}': workspace {} does not exist",
                scenario.name,
                ws_src.display()
            )));
        }

        let variants = scenario.effective_variants();
        let runs = opts.runs.unwrap_or(scenario.runs).max(1);
        let mut scenario_summary = ScenarioSummary {
            name: scenario.name.clone(),
            variants: Vec::new(),
        };

        for variant in &variants {
            eprintln!("  Variant: {} ({} runs)", variant.name, runs);
            let mut run_metrics: Vec<RunMetrics> = Vec::new();
            for run_index in 0..runs {
                let m = run_once(scenario, variant, &ws_src, run_index, &api_key, opts).await?;
                eprintln!(
                    "    run {}: exit={} truncated={} tokens={} recall={:.0}%",
                    run_index,
                    m.exit_code,
                    m.truncated,
                    m.total_tokens,
                    m.recall() * 100.0
                );
                run_metrics.push(m);
            }
            scenario_summary
                .variants
                .push(summarize_variant(&run_metrics));
        }
        report.scenarios.push(scenario_summary);
    }

    if report.scenarios.is_empty() {
        if let Some(filter) = &opts.scenario_filter {
            return Err(EvalError::ScenarioNotFound(filter.clone()));
        }
    }

    Ok(report)
}

/// One scenario × variant × run: copy fixture, materialize config, execute,
/// capture metrics.
#[allow(clippy::too_many_arguments)]
async fn run_once(
    scenario: &EvalScenario,
    variant: &EvalVariant,
    ws_src: &Path,
    run_index: u32,
    api_key: &str,
    opts: &EvalOptions,
) -> Result<RunMetrics, EvalError> {
    let run_dir = opts
        .out_dir
        .join(&scenario.name)
        .join(&variant.name)
        .join(format!("run-{run_index}"));
    std::fs::create_dir_all(&run_dir)?;

    // Isolated workspace copy: keeps fixtures pristine and supports
    // concurrent evals; git init gives `git_diff` a HEAD.
    let temp = tempfile::TempDir::new()?;
    let ws_copy = temp.path().join("workspace");
    copy_dir_tree(ws_src, &ws_copy)?;
    init_git_repo(&ws_copy);

    // Materialize the merged config inside the copy.
    let cfg_src = ws_src.join(&scenario.config);
    let mut merged: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(&cfg_src)
            .map_err(|e| EvalError::Config(format!("{}: {e}", cfg_src.display())))?,
    )
    .map_err(|e| EvalError::Config(format!("{}: {e}", cfg_src.display())))?;
    deep_merge(&mut merged, variant.config_overrides.clone());
    let merged_path = temp.path().join("merged-config.yaml");
    std::fs::write(
        &merged_path,
        serde_yaml::to_string(&merged).map_err(|e| EvalError::Config(e.to_string()))?,
    )?;

    let sarif_path = run_dir.join("output.sarif");
    let config = Config::load(
        Some(&merged_path),
        opts.model.as_deref(),
        opts.vendor.as_deref(),
        Some(api_key),
        None,
        None,
        None,
        None,
        ws_copy.clone(),
        sarif_path,
        false,
        LogFormat::Json,
    )?;

    let task_id = config.task.id.clone();
    let result = execute_task(&config).await;

    // Preserve the run event log for debugging regardless of cleanup.
    let log_path = EventLog::new(&ws_copy, &task_id).path().to_path_buf();
    let events = read_run_events(&log_path);
    if log_path.exists() {
        let _ = std::fs::copy(&log_path, run_dir.join("run.events.jsonl"));
    }

    Ok(metrics_from_run(
        &scenario.name,
        &variant.name,
        run_index,
        &result,
        &events,
        &scenario.ground_truth,
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ExecutionReport, Finding, Severity, Usage};

    fn finding(rule_id: &str) -> Finding {
        Finding {
            id: uuid::Uuid::new_v4(),
            rule_id: rule_id.into(),
            severity: Severity::Warning,
            message: "m".into(),
            location: None,
            evidence: "e".into(),
        }
    }

    fn empty_report() -> ExecutionReport {
        ExecutionReport {
            task_id: "task-x".into(),
            exit_code: 0,
            findings: vec![],
            token_usage: Usage::default(),
            duration_ms: 100,
            snapshot_id: None,
            errors: vec![],
            violations: vec![],
        }
    }

    #[test]
    fn test_deep_merge_recursive() {
        let mut base: serde_yaml::Value =
            serde_yaml::from_str("task:\n  auto_compact: false\n  token_budget: 8000\n").unwrap();
        let over: serde_yaml::Value =
            serde_yaml::from_str("task:\n  auto_compact: true\n").unwrap();
        deep_merge(&mut base, over);
        let rendered = serde_yaml::to_string(&base).unwrap();
        assert!(rendered.contains("auto_compact: true"));
        assert!(
            rendered.contains("token_budget: 8000"),
            "unrelated keys survive"
        );
    }

    #[test]
    fn test_deep_merge_scalar_replacement() {
        let mut base: serde_yaml::Value = serde_yaml::from_str("a: 1\nb: x\n").unwrap();
        let over: serde_yaml::Value = serde_yaml::from_str("a: 2\n").unwrap();
        deep_merge(&mut base, over);
        assert_eq!(base["a"], serde_yaml::Value::Number(2.into()));
        assert_eq!(base["b"], serde_yaml::Value::String("x".into()));
    }

    #[test]
    fn test_metrics_from_run_ground_truth() {
        let truth = vec![
            GroundTruthRule {
                rule_id: "sql-injection".into(),
                min_findings: 1,
            },
            GroundTruthRule {
                rule_id: "hardcoded-secret".into(),
                min_findings: 2,
            },
        ];
        let mut report = empty_report();
        report.findings = vec![
            finding("sql-injection"),
            finding("hardcoded-secret"),
            finding("hardcoded-secret"),
            finding("unexpected-rule"),
        ];
        let m = metrics_from_run("s", "v", 0, &report, &[], &truth);
        assert_eq!(m.truth_matched, 2);
        assert_eq!(m.truth_total, 2);
        assert_eq!(m.recall(), 1.0);
        assert_eq!(m.unexpected_rule_ids, vec!["unexpected-rule"]);

        report.findings = vec![finding("sql-injection"), finding("hardcoded-secret")];
        let m2 = metrics_from_run("s", "v", 0, &report, &[], &truth);
        assert_eq!(m2.truth_matched, 1, "hardcoded-secret needs 2 findings");
        assert!((m2.recall() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_metrics_from_run_event_counts() {
        let events = vec![
            RunEvent::LlmRequest { messages: vec![] },
            RunEvent::LlmRequest { messages: vec![] },
            RunEvent::ToolCall {
                call_id: "c".into(),
                name: "grep".into(),
                arguments: serde_json::json!({}),
            },
            RunEvent::ToolSpill {
                locator: "x".into(),
            },
            RunEvent::ToolSpill {
                locator: "y".into(),
            },
            RunEvent::ContextTruncated {
                dropped_count: 2,
                archive_path: None,
                compacted_summary: Some("summary".into()),
            },
            RunEvent::RepeatReminder {
                tool_name: "grep".into(),
                count: 3,
            },
            RunEvent::FindingsRecoveryAttempt {
                attempt: 1,
                error: "e".into(),
            },
            RunEvent::RunEnd {
                truncated: false,
                duration_ms: 5,
            },
        ];
        let m = metrics_from_run("s", "v", 0, &empty_report(), &events, &[]);
        assert_eq!(m.llm_requests, 2);
        assert_eq!(m.tool_calls, 1);
        assert_eq!(m.tool_spills, 2);
        assert_eq!(m.context_truncations, 1);
        assert_eq!(m.compactions, 1);
        assert_eq!(m.repeat_reminders, 1);
        assert_eq!(m.recovery_attempts, 1);
        assert!(m.recovered);
        assert!(!m.truncated);
        assert!(m.success());
    }

    #[test]
    fn test_summarize_variant() {
        let runs = vec![
            RunMetrics {
                scenario: "s".into(),
                variant: "v".into(),
                run_index: 0,
                exit_code: 0,
                truncated: false,
                total_tokens: 100,
                llm_requests: 4,
                duration_ms: 200,
                recovery_attempts: 1,
                recovered: true,
                truth_matched: 2,
                truth_total: 2,
                ..Default::default()
            },
            RunMetrics {
                scenario: "s".into(),
                variant: "v".into(),
                run_index: 1,
                exit_code: 1,
                truncated: true,
                total_tokens: 300,
                llm_requests: 6,
                duration_ms: 400,
                recovery_attempts: 1,
                recovered: false,
                truth_matched: 1,
                truth_total: 2,
                ..Default::default()
            },
        ];
        let s = summarize_variant(&runs);
        assert_eq!(s.runs, 2);
        assert!((s.success_rate - 0.5).abs() < f64::EPSILON);
        assert!((s.mean_total_tokens - 200.0).abs() < f64::EPSILON);
        assert!((s.mean_llm_requests - 5.0).abs() < f64::EPSILON);
        assert!((s.recall - 0.75).abs() < f64::EPSILON);
        assert!((s.recovery_success_rate - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_eval_config_parse_with_defaults() {
        let yaml = r#"
scenarios:
  - name: sec
    workspace: sec/workspace
    config: .clausura.yaml
    ground_truth:
      - rule_id: sql-injection
        min_findings: 1
"#;
        let config: EvalConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.scenarios.len(), 1);
        let s = &config.scenarios[0];
        assert_eq!(s.runs, 3, "default runs = 3");
        let variants = s.effective_variants();
        assert_eq!(variants.len(), 1);
        assert_eq!(variants[0].name, "default");
    }

    #[test]
    fn test_compare_reports_deltas() {
        let mk = |tokens: f64, recall: f64, success: f64| EvalReport {
            generated_at: "x".into(),
            commit: "old".into(),
            branch: "main".into(),
            model: "m".into(),
            scenarios: vec![ScenarioSummary {
                name: "sec".into(),
                variants: vec![VariantSummary {
                    scenario: "sec".into(),
                    variant: "default".into(),
                    runs: 3,
                    success_rate: success,
                    mean_total_tokens: tokens,
                    recall,
                    ..Default::default()
                }],
            }],
        };
        let baseline = mk(1000.0, 0.6, 0.8);
        let current = mk(800.0, 0.9, 1.0);
        let md = compare_reports(&baseline, &current);
        assert!(md.contains("sec"));
        assert!(md.contains("+20.0pp"), "success delta, got:\n{md}");
        assert!(md.contains("+30.0pp"), "recall delta, got:\n{md}");
        assert!(md.contains("-200"), "token delta, got:\n{md}");
    }

    #[test]
    fn test_read_run_events_ignores_garbage_lines() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("log.jsonl");
        std::fs::write(
            &p,
            "{\"type\":\"run_start\",\"task_id\":\"t\",\"model\":\"m\",\"workspace\":\"/w\"}\n\
             not json at all\n\
             {\"type\":\"run_end\",\"truncated\":false,\"duration_ms\":1}\n",
        )
        .unwrap();
        let events = read_run_events(&p);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_metrics_success_semantics() {
        let m = RunMetrics {
            exit_code: 1,
            truncated: false,
            ..Default::default()
        };
        assert!(
            m.success(),
            "clean finish is success (gate exit is a scenario property)"
        );
        let m2 = RunMetrics {
            exit_code: 0,
            truncated: true,
            ..Default::default()
        };
        assert!(!m2.success(), "incomplete run is not success");
        let m3 = RunMetrics {
            exit_code: 0,
            truncated: false,
            ..Default::default()
        };
        assert!(m3.success());
    }
}
