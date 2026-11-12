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
use crate::executor::{execute_task_with_options, ExecutionOptions};
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
/// rule_id must be reported. An optional `max_findings` bounds over-reporting
/// (precision): reporting more than that many counts as overreported —
/// useful for sweeps with decoy values that must NOT be flagged.
#[derive(Debug, Clone, Deserialize)]
pub struct GroundTruthRule {
    pub rule_id: String,
    pub min_findings: u32,
    #[serde(default)]
    pub max_findings: Option<u32>,
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
    /// Rules whose reported findings exceeded `max_findings` (over-reporting).
    pub truth_overreported: u32,
    /// Findings whose rule_id has no ground-truth entry.
    pub unexpected_rule_ids: Vec<String>,
    /// Execution outcome from the executor, independent of event-log health.
    #[serde(default)]
    pub status: crate::types::RunStatus,
    /// Runtime or incomplete diagnostics returned by the executor.
    #[serde(default)]
    pub errors: Vec<String>,
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
        truncated: report.status != crate::types::RunStatus::Complete
            || report.incomplete_reason.is_some(),
        status: report.status,
        errors: report.errors.clone(),
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
            RunEvent::RunEnd { truncated, .. } => m.truncated |= *truncated,
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
        if let Some(max) = rule.max_findings {
            if count > max {
                m.truth_overreported += 1;
            }
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
    /// Ground-truth rules matched across runs (recall numerator).
    #[serde(default)]
    pub truth_matched: u32,
    /// Ground-truth rules expected across runs (recall denominator).
    #[serde(default)]
    pub truth_total: u32,
    /// Ground-truth rules over-reported across runs (findings > max_findings).
    pub truth_overreported: u32,
    /// Unexpected rule_ids seen across runs.
    pub unexpected_rule_ids: Vec<String>,
    /// Unaggregated outcomes used to build this summary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run_metrics: Vec<RunMetrics>,
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
        run_metrics: runs.to_vec(),
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
    let attempted_runs = runs.iter().filter(|m| m.recovery_attempts > 0).count();
    let recovered_runs = runs.iter().filter(|m| m.recovered).count();
    s.recovery_success_rate = if attempted_runs == 0 {
        0.0
    } else {
        recovered_runs as f64 / attempted_runs as f64
    };
    let matched: u32 = runs.iter().map(|m| m.truth_matched).sum();
    let total: u32 = runs.iter().map(|m| m.truth_total).sum();
    s.recall = if total == 0 {
        1.0
    } else {
        matched as f64 / total as f64
    };
    s.truth_matched = matched;
    s.truth_total = total;
    s.truth_overreported = runs.iter().map(|m| m.truth_overreported).sum();
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
    /// Whether the source tree contained uncommitted changes at eval start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_tree_dirty: Option<bool>,
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
            "| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | \
             mean time (s) | compactions | spills | reminders | recovery success |\n\
             |----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|\n",
        );
        for s in &self.scenarios {
            for v in &s.variants {
                out.push_str(&format!(
                    "| {} | {} | {} | {:.0}% | {:.0}% | {} | {:.0} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.0}%\n",
                    s.name,
                    v.variant,
                    v.runs,
                    v.success_rate * 100.0,
                    v.recall * 100.0,
                    v.truth_overreported,
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
        "| scenario | variant | success | recall | over-report | mean tokens | mean LLM calls | compactions | spills | recovery success |\n\
         |----------|---------|---------|--------|-------------|-------------|----------------|-------------|--------|------------------|\n",
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
                    let (od, oa) = delta(v.truth_overreported as f64, b.truth_overreported as f64);
                    out.push_str(&format!(
                        "| {} | {} | {}{:+.1}pp | {}{:+.1}pp | {}{:+.0} | {}{:+.0} | {}{:+.1} | {}{:+.1} | {}{:+.1} | {}{:+.1}pp\n",
                        s.name,
                        v.variant,
                        sa,
                        sd * 100.0,
                        ra,
                        rd * 100.0,
                        oa,
                        od,
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
                    "| {} | {} | — (new variant, no baseline) | | | | | | | |\n",
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
// Model matrix (multi-model evals)
// ---------------------------------------------------------------------------

/// A model matrix for `clausura eval --models models.yaml`: run the same
/// scenarios against several models and publish a comparable recall report.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelMatrix {
    /// Default vendor shorthand for entries that don't set one
    /// (e.g. `openrouter`).
    #[serde(default)]
    pub vendor: Option<String>,
    pub models: Vec<ModelEntry>,
}

/// One model in a matrix.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelEntry {
    /// Provider-native model id (e.g. `deepseek/deepseek-v4.1-flash`).
    pub model: String,
    /// Vendor shorthand override; falls back to the matrix-level `vendor`.
    #[serde(default)]
    pub vendor: Option<String>,
    /// Env var holding this entry's API key. Defaults to `CLAUSURA_API_KEY`
    /// (or `--api-key`), so a single gateway key can serve the whole matrix.
    #[serde(default)]
    pub api_key_env: Option<String>,
}

impl ModelEntry {
    pub fn effective_vendor<'a>(&'a self, matrix: &'a ModelMatrix) -> Option<&'a str> {
        self.vendor.as_deref().or(matrix.vendor.as_deref())
    }

    /// Filesystem-safe directory name for per-model artifacts.
    pub fn slug(&self) -> String {
        self.model
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect()
    }
}

/// Load and parse a model matrix file.
pub fn load_model_matrix(path: &Path) -> Result<ModelMatrix, EvalError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| EvalError::Config(format!("{}: {e}", path.display())))?;
    let matrix: ModelMatrix = serde_yaml::from_str(&content)
        .map_err(|e| EvalError::Config(format!("{}: {e}", path.display())))?;
    if matrix.models.is_empty() {
        return Err(EvalError::Config(format!(
            "{}: model matrix must list at least one model",
            path.display()
        )));
    }
    Ok(matrix)
}

/// Aggregated multi-model report: per-model eval reports plus a rendered
/// cross-model comparison suitable for publishing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MatrixReport {
    pub generated_at: String,
    pub commit: String,
    pub branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_tree_dirty: Option<bool>,
    /// Variant the matrix was restricted to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    pub models: Vec<MatrixModelReport>,
}

/// One model's outcome in a matrix. A model that failed to evaluate keeps
/// `error` and an empty report instead of aborting the whole matrix.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MatrixModelReport {
    pub model: String,
    pub vendor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub report: EvalReport,
}

impl MatrixModelReport {
    fn variants(&self) -> impl Iterator<Item = &VariantSummary> {
        self.report.scenarios.iter().flat_map(|s| s.variants.iter())
    }

    /// Micro recall across every scenario × variant (pooled ground truth).
    pub fn pooled_recall(&self) -> (u32, u32) {
        self.variants()
            .fold((0, 0), |(m, t), v| (m + v.truth_matched, t + v.truth_total))
    }

    fn total_runs(&self) -> u32 {
        self.variants().map(|v| v.runs).sum()
    }

    fn total_tokens(&self) -> f64 {
        self.variants()
            .map(|v| v.mean_total_tokens * v.runs as f64)
            .sum()
    }

    fn mean_success(&self) -> f64 {
        let runs = self.total_runs();
        if runs == 0 {
            0.0
        } else {
            self.variants()
                .map(|v| v.success_rate * v.runs as f64)
                .sum::<f64>()
                / runs as f64
        }
    }

    /// Recall for one scenario across its variants (micro, pooled).
    fn scenario_recall(&self, scenario: &str) -> Option<f64> {
        let s = self.report.scenarios.iter().find(|s| s.name == scenario)?;
        let (matched, total) = s
            .variants
            .iter()
            .fold((0, 0), |(m, t), v| (m + v.truth_matched, t + v.truth_total));
        if total == 0 {
            None
        } else {
            Some(matched as f64 / total as f64)
        }
    }
}

impl MatrixReport {
    /// Start a matrix report stamped with the current UTC time.
    pub fn new(variant: Option<String>) -> Self {
        MatrixReport {
            generated_at: chrono::Utc::now().to_rfc3339(),
            variant,
            ..Default::default()
        }
    }

    /// Render the matrix as publishable Markdown: a summary table plus a
    /// per-scenario recall table.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "# Model matrix — {} ({} @ {})\n\n",
            self.generated_at, self.branch, self.commit
        ));
        if let Some(dirty) = self.source_tree_dirty {
            out.push_str(&format!("source tree dirty: `{dirty}` · "));
        }
        if let Some(variant) = &self.variant {
            out.push_str(&format!("variant: `{variant}` · "));
        }
        out.push_str(
            "rule recall = fraction of ground-truth rule categories meeting their finding-count \
             threshold (micro-averaged over runs), not per-instance or location recall; \
             success = clean finish (gate exit codes are scenario properties).\n\n",
        );

        out.push_str(
            "| model | rule recall | success | tokens/run | LLM calls/run | time/run (s) | total tokens |\n\
             |-------|--------|---------|------------|---------------|--------------|--------------|\n",
        );
        for m in &self.models {
            if let Some(error) = &m.error {
                out.push_str(&format!(
                    "| `{}` | eval failed: {} | | | | | |\n",
                    m.model, error
                ));
                continue;
            }
            if m.total_runs() > 0 && m.total_tokens() == 0.0 {
                out.push_str(&format!(
                    "| `{}` | no tokens billed — provider/auth failure, not a recall result | | | | | |\n",
                    m.model
                ));
                continue;
            }
            let (matched, total) = m.pooled_recall();
            let recall = if total == 0 {
                0.0
            } else {
                matched as f64 / total as f64
            };
            let runs = m.total_runs().max(1) as f64;
            let llm_calls: f64 = m
                .variants()
                .map(|v| v.mean_llm_requests * v.runs as f64)
                .sum();
            let duration: f64 = m
                .variants()
                .map(|v| v.mean_duration_ms * v.runs as f64)
                .sum();
            out.push_str(&format!(
                "| `{}` | {:.0}% ({}/{}) | {:.0}% | {:.0} | {:.1} | {:.1} | {:.0} |\n",
                m.model,
                recall * 100.0,
                matched,
                total,
                m.mean_success() * 100.0,
                m.total_tokens() / runs,
                llm_calls / runs,
                duration / runs / 1000.0,
                m.total_tokens(),
            ));
        }

        let scenario_names: Vec<String> = self
            .models
            .iter()
            .flat_map(|m| m.report.scenarios.iter().map(|s| s.name.clone()))
            .fold(Vec::new(), |mut acc, name| {
                if !acc.contains(&name) {
                    acc.push(name);
                }
                acc
            });
        if !scenario_names.is_empty() {
            out.push_str("\n## Recall by scenario\n\n| model |");
            for name in &scenario_names {
                out.push_str(&format!(" {name} |"));
            }
            out.push_str("\n|-------|");
            for _ in &scenario_names {
                out.push_str("--------|");
            }
            out.push('\n');
            for m in &self.models {
                out.push_str(&format!("| `{}` |", m.model));
                let unavailable =
                    m.error.is_some() || (m.total_runs() > 0 && m.total_tokens() == 0.0);
                for name in &scenario_names {
                    match (unavailable, m.scenario_recall(name)) {
                        (false, Some(r)) => out.push_str(&format!(" {:.0}% |", r * 100.0)),
                        _ => out.push_str(" — |"),
                    }
                }
                out.push('\n');
            }
        }

        out.push_str(
            "\nLLM output is nondeterministic: treat small-run results as indicative, not as \
             stable rankings. Reproduce with `clausura eval --models <matrix.yaml>`; per-model \
             reports and event logs live alongside this file.\n",
        );
        out
    }
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
    /// Only run variants with this name; scenarios without it are skipped.
    /// Useful for model matrices, where only the `default` variant of each
    /// scenario is compared across models.
    pub variant_filter: Option<String>,
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
    #[error("Variant not found in any scenario: {0}")]
    VariantNotFound(String),
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

/// Initialize a throwaway git repo in `ws` so `git_diff` has a base to diff
/// against. The first commit is empty; the fixture files land in the second,
/// so `git_diff` with `base: HEAD~1` shows the whole fixture as the "diff".
/// Best-effort: fixtures also work when git is unavailable.
fn init_git_repo(ws: &Path) {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(ws)
            .args(args)
            .output()
            .is_ok()
    };
    if git(&["init", "-q"]) {
        let _ = git(&[
            "-c",
            "user.email=eval@clausura.invalid",
            "-c",
            "user.name=Clausura Eval",
            "commit",
            "--allow-empty",
            "-qm",
            "empty base",
        ]);
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
fn git_context() -> (String, String, Option<bool>) {
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
    static DIRTY: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();
    let dirty = *DIRTY.get_or_init(|| {
        let output = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .output()
            .ok()?;
        output.status.success().then_some(!output.stdout.is_empty())
    });
    (short, branch, dirty)
}

/// Run the full eval: every scenario × variant × run. Returns the report.
pub async fn run_eval(eval_path: &Path, opts: &EvalOptions) -> Result<EvalReport, EvalError> {
    let api_key = opts
        .api_key
        .clone()
        .or_else(|| std::env::var("CLAUSURA_API_KEY").ok())
        .ok_or(EvalError::MissingApiKey)?;

    let (eval_config, base_dir) = load_eval_config(eval_path)?;
    let (commit, branch, source_tree_dirty) = git_context();
    let mut report = EvalReport {
        generated_at: chrono::Utc::now().to_rfc3339(),
        commit,
        branch,
        source_tree_dirty,
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

        let mut variants = scenario.effective_variants();
        if let Some(filter) = &opts.variant_filter {
            variants.retain(|v| &v.name == filter);
            if variants.is_empty() {
                eprintln!(
                    "  skipping scenario '{}': no variant named '{}'",
                    scenario.name, filter
                );
                continue;
            }
        }
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
        if let Some(filter) = &opts.variant_filter {
            return Err(EvalError::VariantNotFound(filter.clone()));
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

    // Materialize the merged config inside the copy. A null override (the
    // default when a variant declares no config_overrides) is a no-op.
    let cfg_src = ws_src.join(&scenario.config);
    let mut merged: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(&cfg_src)
            .map_err(|e| EvalError::Config(format!("{}: {e}", cfg_src.display())))?,
    )
    .map_err(|e| EvalError::Config(format!("{}: {e}", cfg_src.display())))?;
    if !variant.config_overrides.is_null() {
        deep_merge(&mut merged, variant.config_overrides.clone());
    }
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
    let result = execute_task_with_options(
        &config,
        ExecutionOptions {
            preserve_archives: true,
        },
    )
    .await;

    // Preserve the run event log for debugging and mechanism metrics.
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
            status: crate::types::RunStatus::Complete,
            incomplete_reason: None,
            verification: None,
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
                max_findings: None,
            },
            GroundTruthRule {
                rule_id: "hardcoded-secret".into(),
                min_findings: 2,
                max_findings: Some(2),
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
        assert_eq!(m.truth_overreported, 0);
        assert_eq!(m.unexpected_rule_ids, vec!["unexpected-rule"]);

        // Over-reporting: 3 hardcoded-secret findings against max 2.
        report.findings = vec![
            finding("sql-injection"),
            finding("hardcoded-secret"),
            finding("hardcoded-secret"),
            finding("hardcoded-secret"),
        ];
        let m3 = metrics_from_run("s", "v", 0, &report, &[], &truth);
        assert_eq!(m3.truth_overreported, 1, "findings above max must count");

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
                incomplete_reason: None,
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
    fn test_metrics_error_without_run_end_is_not_success() {
        let mut report = empty_report();
        report.exit_code = 2;
        report.status = crate::types::RunStatus::Error;
        report.incomplete_reason = Some(crate::types::IncompleteReason::Timeout);
        report.errors = vec!["Task timeout exceeded".into()];

        let metrics = metrics_from_run("s", "v", 0, &report, &[], &[]);
        assert!(metrics.truncated);
        assert!(!metrics.success());
        assert_eq!(metrics.status, crate::types::RunStatus::Error);
        assert_eq!(metrics.errors, vec!["Task timeout exceeded"]);
    }

    #[test]
    fn test_recovery_rate_counts_attempted_runs_not_calls() {
        let summary = summarize_variant(&[RunMetrics {
            scenario: "s".into(),
            variant: "v".into(),
            recovery_attempts: 2,
            recovered: true,
            ..Default::default()
        }]);
        assert_eq!(summary.recovery_success_rate, 1.0);
        assert_eq!(summary.run_metrics.len(), 1);
        assert_eq!(summary.run_metrics[0].recovery_attempts, 2);
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
            source_tree_dirty: None,
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

    /// Committed eval scenarios must load: schema errors in fixture configs
    /// should fail CI, not a live eval run. Skipped when the eval corpus is
    /// not shipped with the crate (published tarballs).
    #[test]
    fn test_committed_scenario_configs_are_valid() {
        let eval_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/eval.yaml");
        if !eval_path.exists() {
            return;
        }
        let (cfg, base) = load_eval_config(&eval_path).expect("eval.yaml must parse");
        assert!(
            !cfg.scenarios.is_empty(),
            "eval.yaml must declare scenarios"
        );
        for s in &cfg.scenarios {
            let ws = base.join(&s.workspace);
            let cfg_path = ws.join(&s.config);
            Config::load(
                Some(&cfg_path),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                ws.clone(),
                PathBuf::from("/tmp/eval-output.sarif"),
                false,
                LogFormat::Json,
            )
            .unwrap_or_else(|e| panic!("scenario '{}' config invalid: {e}", s.name));
            for v in s.effective_variants() {
                if !v.config_overrides.is_null() {
                    // Merged overrides must still parse as a full config.
                    let mut merged: serde_yaml::Value = serde_yaml::from_str(
                        &std::fs::read_to_string(&cfg_path).expect("scenario config readable"),
                    )
                    .expect("scenario config is YAML");
                    deep_merge(&mut merged, v.config_overrides.clone());
                    let tmp = tempfile::NamedTempFile::new().unwrap();
                    std::fs::write(tmp.path(), serde_yaml::to_string(&merged).unwrap()).unwrap();
                    Config::load(
                        Some(tmp.path()),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        ws.clone(),
                        PathBuf::from("/tmp/eval-output.sarif"),
                        false,
                        LogFormat::Json,
                    )
                    .unwrap_or_else(|e| {
                        panic!(
                            "scenario '{}' variant '{}' overrides produce invalid config: {e}",
                            s.name, v.name
                        )
                    });
                }
            }
        }
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

    fn matrix_model(model: &str, scenario: &str, matched: u32, total: u32) -> MatrixModelReport {
        MatrixModelReport {
            model: model.into(),
            vendor: "openrouter".into(),
            error: None,
            report: EvalReport {
                generated_at: "x".into(),
                commit: "c".into(),
                branch: "main".into(),
                source_tree_dirty: None,
                model: model.into(),
                scenarios: vec![ScenarioSummary {
                    name: scenario.into(),
                    variants: vec![VariantSummary {
                        scenario: scenario.into(),
                        variant: "default".into(),
                        runs: 2,
                        success_rate: 1.0,
                        mean_total_tokens: 1000.0,
                        mean_llm_requests: 4.0,
                        mean_duration_ms: 5000.0,
                        recall: if total == 0 {
                            1.0
                        } else {
                            matched as f64 / total as f64
                        },
                        truth_matched: matched,
                        truth_total: total,
                        ..Default::default()
                    }],
                }],
            },
        }
    }

    #[test]
    fn test_model_matrix_parse_and_slug() {
        let yaml = r#"
vendor: openrouter
models:
  - model: deepseek/deepseek-v4.1-flash
  - model: anthropic/claude-haiku-5.5
    api_key_env: ANTHROPIC_KEY
"#;
        let matrix: ModelMatrix = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(matrix.models.len(), 2);
        assert_eq!(
            matrix.models[0].effective_vendor(&matrix),
            Some("openrouter")
        );
        assert_eq!(matrix.models[0].slug(), "deepseek-deepseek-v4-1-flash");
        assert_eq!(
            matrix.models[1].api_key_env.as_deref(),
            Some("ANTHROPIC_KEY")
        );
    }

    #[test]
    fn test_load_model_matrix_rejects_empty() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "vendor: openrouter\nmodels: []\n").unwrap();
        let err = load_model_matrix(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("at least one model"));
    }

    #[test]
    fn test_matrix_report_markdown_pools_recall() {
        let mut report = MatrixReport::new(Some("default".into()));
        report
            .models
            .push(matrix_model("a/a", "security-basics", 6, 6));
        report
            .models
            .push(matrix_model("b/b", "security-basics", 3, 6));
        report.models.push(MatrixModelReport {
            model: "c/c".into(),
            vendor: "openrouter".into(),
            error: Some("missing API key".into()),
            ..Default::default()
        });
        let mut zero_tokens = matrix_model("d/d", "security-basics", 0, 6);
        zero_tokens.report.scenarios[0].variants[0].mean_total_tokens = 0.0;
        report.models.push(zero_tokens);

        let md = report.to_markdown();
        assert!(md.contains("| `a/a` | 100% (6/6) |"), "got:\n{md}");
        assert!(md.contains("| `b/b` | 50% (3/6) |"), "got:\n{md}");
        assert!(md.contains("eval failed: missing API key"));
        assert!(md.contains("no tokens billed"), "got:\n{md}");
        assert!(md.contains("## Recall by scenario"));
        assert!(md.contains("security-basics"));
        let zero_token_scenario = md.lines().rfind(|line| line.starts_with("| `d/d`"));
        assert_eq!(zero_token_scenario, Some("| `d/d` | — |"));
        // JSON round-trip keeps the pooled counts for offline rendering.
        let json = serde_json::to_string(&report).unwrap();
        let back: MatrixReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.models[0].pooled_recall(), (6, 6));
    }

    #[tokio::test]
    async fn test_variant_filter_with_no_matches_is_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("ws")).unwrap();
        let eval_path = tmp.path().join("eval.yaml");
        std::fs::write(
            &eval_path,
            "scenarios:\n  - name: sec\n    workspace: ws\n    config: .clausura.yaml\n    variants:\n      - name: default\n",
        )
        .unwrap();
        let opts = EvalOptions {
            api_key: Some("not-used".into()),
            model: None,
            vendor: None,
            runs: Some(1),
            scenario_filter: None,
            variant_filter: Some("defualt".into()),
            out_dir: tmp.path().join("out"),
        };
        let err = run_eval(&eval_path, &opts).await.unwrap_err();
        assert!(
            matches!(err, EvalError::VariantNotFound(ref name) if name == "defualt"),
            "expected VariantNotFound, got {err}"
        );
    }
}
