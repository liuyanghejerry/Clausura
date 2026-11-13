//! `clausura eval` — effect-oriented evaluation harness.
//!
//! Runs every scenario × variant × repeat in the eval config, distills
//! effect metrics (success rate, recall, tokens, compactions, spills,
//! recovery) from each run's event log, and writes a JSON + Markdown report.
//! With `--baseline`, the report is diffed against a previous run — use it
//! before/after an implementation switch to justify the change with numbers.
//!
//! With `--models <matrix.yaml>`, the whole eval runs once per model and the
//! reports are aggregated into a publishable cross-model recall matrix
//! (`eval-matrix.json` / `eval-matrix.md`). Combine with `--variant default`
//! to compare only the baseline variant of each scenario.

use clap::Args;
use clausura_core::eval::{
    compare_reports, load_model_matrix, run_eval, EvalOptions, EvalReport, MatrixModelReport,
    MatrixReport,
};
use colored::*;
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub struct EvalArgs {
    /// Path to the eval config file (eval.yaml)
    #[arg(long, default_value = "eval.yaml")]
    pub config: PathBuf,

    /// Override the model for every task
    #[arg(long)]
    pub model: Option<String>,

    /// Override the vendor for every task
    #[arg(long)]
    pub vendor: Option<String>,

    /// Model matrix YAML: run the eval once per model and publish a
    /// cross-model recall report (see eval/models.yaml)
    #[arg(long, conflicts_with_all = ["model", "vendor"])]
    pub models: Option<PathBuf>,

    /// API key (defaults to CLAUSURA_API_KEY)
    #[arg(long)]
    pub api_key: Option<String>,

    /// Only run the scenario with this name
    #[arg(long)]
    pub scenario: Option<String>,

    /// Only run variants with this name (e.g. `default`); scenarios without
    /// a matching variant are skipped
    #[arg(long)]
    pub variant: Option<String>,

    /// Override repeats per variant for every scenario
    #[arg(long)]
    pub runs: Option<u32>,

    /// Directory for reports, SARIF artifacts and per-run event logs
    #[arg(long, default_value = "eval-results")]
    pub out_dir: PathBuf,

    /// Previous eval report (eval-report.json) to diff against
    #[arg(long, conflicts_with = "models")]
    pub baseline: Option<PathBuf>,
}

pub async fn execute(args: EvalArgs) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(matrix_path) = args.models.clone() {
        return execute_matrix(&args, &matrix_path).await;
    }

    let out_dir = args.out_dir.clone();
    std::fs::create_dir_all(&out_dir)?;

    let options = EvalOptions {
        api_key: args.api_key.clone(),
        model: args.model.clone(),
        vendor: args.vendor.clone(),
        runs: args.runs,
        scenario_filter: args.scenario.clone(),
        variant_filter: args.variant.clone(),
        out_dir: out_dir.clone(),
    };

    let report = run_eval(&args.config, &options).await.map_err(|e| {
        eprintln!("{}: {e}", "Eval error".red().bold());
        e
    })?;

    write_report(&out_dir, &report)?;

    // Optional baseline comparison.
    if let Some(baseline_path) = args.baseline {
        let baseline: EvalReport = serde_json::from_str(&std::fs::read_to_string(&baseline_path)?)
            .map_err(|e| {
                format!(
                    "Could not parse baseline report {}: {e}",
                    baseline_path.display()
                )
            })?;
        let comparison = compare_reports(&baseline, &report);
        let cmp_path = out_dir.join("eval-comparison.md");
        std::fs::write(&cmp_path, &comparison)?;
        println!(
            "\n{}\nComparison written to {}",
            comparison,
            cmp_path.display()
        );
    }

    Ok(())
}

fn write_report(out_dir: &Path, report: &EvalReport) -> Result<(), Box<dyn std::error::Error>> {
    let json_path = out_dir.join("eval-report.json");
    let md_path = out_dir.join("eval-report.md");
    std::fs::write(&json_path, serde_json::to_string_pretty(report)?)?;
    std::fs::write(&md_path, report.to_markdown())?;
    println!("{}", report.to_markdown());
    println!(
        "Report written to {} and {}",
        json_path.display(),
        md_path.display()
    );
    Ok(())
}

/// Run the eval once per matrix entry, then aggregate a cross-model report.
/// A failing model is recorded in the matrix instead of aborting the others.
async fn execute_matrix(
    args: &EvalArgs,
    matrix_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = args.out_dir.clone();
    std::fs::create_dir_all(&out_dir)?;
    let matrix = load_model_matrix(matrix_path)?;

    let mut matrix_report = MatrixReport::new(args.variant.clone());

    for entry in &matrix.models {
        let vendor = entry.effective_vendor(&matrix);
        let vendor_display = vendor.unwrap_or("from config");
        println!(
            "{} `{}` (vendor: {})",
            "Evaluating model".cyan().bold(),
            entry.model,
            vendor_display
        );

        let api_key = match &entry.api_key_env {
            Some(env_name) => match std::env::var(env_name) {
                Ok(value) if !value.is_empty() => Some(value),
                _ => {
                    eprintln!(
                        "{}: model `{}` needs ${} (set it or drop api_key_env)",
                        "Skipping model".yellow().bold(),
                        entry.model,
                        env_name
                    );
                    matrix_report.models.push(MatrixModelReport {
                        model: entry.model.clone(),
                        vendor: vendor_display.into(),
                        error: Some(format!("missing API key in ${env_name}")),
                        ..Default::default()
                    });
                    continue;
                }
            },
            None => args.api_key.clone(),
        };

        let model_dir = out_dir.join(entry.slug());
        std::fs::create_dir_all(&model_dir)?;
        let options = EvalOptions {
            api_key,
            model: Some(entry.model.clone()),
            vendor: vendor.map(str::to_string),
            runs: args.runs,
            scenario_filter: args.scenario.clone(),
            variant_filter: args.variant.clone(),
            out_dir: model_dir.clone(),
        };

        match run_eval(&args.config, &options).await {
            Ok(report) => {
                if matrix_report.commit.is_empty() {
                    matrix_report.commit = report.commit.clone();
                    matrix_report.branch = report.branch.clone();
                    matrix_report.source_tree_dirty = report.source_tree_dirty;
                }
                write_report(&model_dir, &report)?;
                matrix_report.models.push(MatrixModelReport {
                    model: entry.model.clone(),
                    vendor: vendor_display.into(),
                    error: None,
                    report,
                });
            }
            Err(e) => {
                eprintln!(
                    "{}: model `{}`: {e}",
                    "Model eval failed".red().bold(),
                    entry.model
                );
                matrix_report.models.push(MatrixModelReport {
                    model: entry.model.clone(),
                    vendor: vendor_display.into(),
                    error: Some(e.to_string()),
                    ..Default::default()
                });
            }
        }
    }

    let json_path = out_dir.join("eval-matrix.json");
    let md_path = out_dir.join("eval-matrix.md");
    std::fs::write(&json_path, serde_json::to_string_pretty(&matrix_report)?)?;
    std::fs::write(&md_path, matrix_report.to_markdown())?;
    println!("{}", matrix_report.to_markdown());
    println!(
        "Matrix report written to {} and {}",
        json_path.display(),
        md_path.display()
    );

    if matrix_report.models.iter().all(|m| m.error.is_some()) {
        return Err("every model in the matrix failed to evaluate".into());
    }
    Ok(())
}
