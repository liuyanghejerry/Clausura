use clausura_core::config::{Config, LogFormat};
use clausura_core::executor::execute_task;
use clausura_core::logging;
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

pub async fn execute(args: RunArgs) -> i32 {
    let log_fmt = match args.log_format.as_str() {
        "pretty" => LogFormat::Pretty,
        _ => LogFormat::Json,
    };
    logging::init(&log_fmt);

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
            eprintln!("Error: Config error: {}", e);
            return 3;
        }
    };

    if args.validate_config || args.dry_run {
        if args.dry_run {
            eprintln!("Would run task: {} ({})", config.task.name, config.task.id);
            eprintln!("  Model: {}", config.task.model);
            eprintln!("  Vendor: {}", config.task.vendor);
            eprintln!("  Token budget: {}", config.task.token_budget);
            eprintln!("  Timeout: {}s", config.task.timeout_secs);
            eprintln!("  Gating rules: {}", config.task.gating_rules.len());
        }
        return 0;
    }

    eprintln!("Running task: {}...", config.task.name);
    let report = execute_task(&config).await;

    eprintln!(
        "Task: {} | Findings: {} | Exit: {} | Tokens: {} | Duration: {}ms",
        config.task.name,
        report.findings.len(),
        report.exit_code,
        report.token_usage.total_tokens,
        report.duration_ms,
    );

    if !report.errors.is_empty() {
        for err in &report.errors {
            eprintln!("  Error: {}", err);
        }
    }

    report.exit_code as i32
}
