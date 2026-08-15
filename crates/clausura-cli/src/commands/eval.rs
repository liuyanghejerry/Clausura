//! `clausura eval` — effect-oriented evaluation harness.
//!
//! Runs every scenario × variant × repeat in the eval config, distills
//! effect metrics (success rate, recall, tokens, compactions, spills,
//! recovery) from each run's event log, and writes a JSON + Markdown report.
//! With `--baseline`, the report is diffed against a previous run — use it
//! before/after an implementation switch to justify the change with numbers.

use clap::Args;
use clausura_core::eval::{compare_reports, run_eval, EvalOptions, EvalReport};
use colored::*;
use std::path::PathBuf;

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

    /// API key (defaults to CLAUSURA_API_KEY)
    #[arg(long)]
    pub api_key: Option<String>,

    /// Only run the scenario with this name
    #[arg(long)]
    pub scenario: Option<String>,

    /// Override repeats per variant for every scenario
    #[arg(long)]
    pub runs: Option<u32>,

    /// Directory for reports, SARIF artifacts and per-run event logs
    #[arg(long, default_value = "eval-results")]
    pub out_dir: PathBuf,

    /// Previous eval report (eval-report.json) to diff against
    #[arg(long)]
    pub baseline: Option<PathBuf>,
}

pub async fn execute(args: EvalArgs) -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = args.out_dir.clone();
    std::fs::create_dir_all(&out_dir)?;

    let options = EvalOptions {
        api_key: args.api_key.clone(),
        model: args.model.clone(),
        vendor: args.vendor.clone(),
        runs: args.runs,
        scenario_filter: args.scenario.clone(),
        out_dir: out_dir.clone(),
    };

    let report = run_eval(&args.config, &options).await.map_err(|e| {
        eprintln!("{}: {e}", "Eval error".red().bold());
        e
    })?;

    // Persist the report as JSON + Markdown.
    let json_path = out_dir.join("eval-report.json");
    let md_path = out_dir.join("eval-report.md");
    std::fs::write(&json_path, serde_json::to_string_pretty(&report)?)?;
    std::fs::write(&md_path, report.to_markdown())?;

    println!("{}", report.to_markdown());
    println!(
        "Report written to {} and {}",
        json_path.display(),
        md_path.display()
    );

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
