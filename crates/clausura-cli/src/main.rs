use clap::{Parser, Subcommand};
use colored::*;
use commands::run::RunArgs;
use commands::snapshot::SnapshotArgs;

mod commands;

#[derive(Parser)]
#[command(
    name = "clausura",
    version = clausura_core::build_info::VERSION_FULL,
    about = "CI-native agent CLI tool",
    long_about = "Clausura runs deterministic agent tasks in CI pipelines.\n\
                   \n\
                   EXAMPLES:\n  \
                   clausura run --config .clausura.yaml    Run a task\n  \
                   clausura run --dry-run                   Validate config and print plan\n  \
                   clausura snapshot list                   List checkpoints\n  \
                   clausura --version                       Show version"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a Clausura task
    Run(RunArgs),
    /// Manage checkpoints
    Snapshot(SnapshotArgs),
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    colored::control::set_override(atty::is(atty::Stream::Stderr));

    let cli = Cli::parse();

    match cli.command {
        Commands::Run(args) => {
            let exit_code = commands::run::execute(args).await;
            std::process::exit(exit_code);
        }
        Commands::Snapshot(args) => {
            commands::snapshot::execute(args).map_err(|e| {
                eprintln!("{}: {}", "Error".red().bold(), e);
                e
            })?;
            Ok(())
        }
    }
}
