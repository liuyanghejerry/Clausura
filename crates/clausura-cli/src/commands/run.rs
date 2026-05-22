use clausura_core::config::{Config, LogFormat};
use clausura_core::executor::execute_task;
use clausura_core::logging;
use colored::*;
use std::path::PathBuf;

/// Run a Clausura task
#[derive(clap::Args, Debug)]
pub struct RunArgs {
    /// Path to config file
    #[arg(short, long, default_value = ".clausura.yaml")]
    pub config: PathBuf,

    /// Override the model
    #[arg(long)]
    pub model: Option<String>,

    /// Override the vendor
    #[arg(long)]
    pub vendor: Option<String>,

    /// API key
    #[arg(long)]
    pub api_key: Option<String>,

    /// Token budget override
    #[arg(long)]
    pub token_budget: Option<u64>,

    /// Timeout in seconds
    #[arg(long)]
    pub timeout: Option<u64>,

    /// Workspace root
    #[arg(long)]
    pub workspace: Option<PathBuf>,

    /// Output path for SARIF
    #[arg(long)]
    pub output: Option<PathBuf>,

    /// Resume from last checkpoint
    #[arg(long)]
    pub resume: bool,

    /// Log format (json or pretty)
    #[arg(long, default_value = "json")]
    pub log_format: String,

    /// Only validate config, don't run
    #[arg(long)]
    pub dry_run: bool,

    /// Validate config and exit
    #[arg(long)]
    pub validate_config: bool,
}

fn step(current: usize, total: usize, message: &str) {
    let progress = format!("[{}/{}]", current, total).bold();
    eprintln!("{} {}", progress, message);
}

pub async fn execute(args: RunArgs) -> i32 {
    let log_fmt = match args.log_format.as_str() {
        "pretty" => LogFormat::Pretty,
        _ => LogFormat::Json,
    };
    logging::init(&log_fmt);

    let total_steps: usize = if args.validate_config || args.dry_run {
        2
    } else {
        4
    };

    step(1, total_steps, "Loading configuration...");

    let workspace = args
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| workspace.join("clausura-output.sarif"));

    let config = match Config::load(
        Some(args.config.as_path()),
        args.model.as_deref(),
        args.vendor.as_deref(),
        args.api_key.as_deref(),
        args.token_budget,
        args.timeout,
        workspace,
        output,
        args.resume,
        log_fmt,
    ) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("{} Config error: {}", "Error:".red().bold(), e);
            eprintln!(
                "{} Check your config file and try again.",
                "Hint:".yellow().bold()
            );
            return 3;
        }
    };

    if args.validate_config {
        step(2, total_steps, "Validating configuration...");
        eprintln!("{} Configuration is valid", "OK:".green().bold());
        return 0;
    }

    if args.dry_run {
        step(2, total_steps, "Planning execution...");
        eprintln!();
        eprintln!("{}", "Task Plan:".bold().underline());
        eprintln!("  {} {}", "Name:".bold(), config.task.name);
        eprintln!("  {} {}", "ID:".bold(), config.task.id);
        eprintln!("  {} {}", "Model:".bold(), config.task.model);
        eprintln!("  {} {}", "Vendor:".bold(), config.task.vendor);
        eprintln!("  {} {}", "Token budget:".bold(), config.task.token_budget);
        eprintln!("  {} {}s", "Timeout:".bold(), config.task.timeout_secs);
        eprintln!(
            "  {} {}",
            "Gating rules:".bold(),
            config.task.gating_rules.len()
        );
        return 0;
    }

    step(2, total_steps, "Initializing agent...");
    step(3, total_steps, "Executing task...");
    eprintln!("  {}", config.task.name);

    let report = execute_task(&config).await;

    step(4, total_steps, "Processing results...");

    if report.exit_code == 0 {
        eprintln!("{} Task completed successfully", "Success:".green().bold());
    } else {
        eprintln!("{} Task failed", "Error:".red().bold());
    }

    eprintln!(
        "  {} {} | {} {} | {} {} | {} {}ms",
        "Findings:".bold(),
        report.findings.len(),
        "Exit:".bold(),
        report.exit_code,
        "Tokens:".bold(),
        report.token_usage.total_tokens,
        "Duration:".bold(),
        report.duration_ms,
    );

    if !report.errors.is_empty() {
        for err in &report.errors {
            eprintln!("  {} {}", "Error:".red(), err);
        }
    }

    report.exit_code as i32
}
