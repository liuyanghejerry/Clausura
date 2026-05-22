use clap::{Parser, Subcommand};
use commands::run::RunArgs;

mod commands;

#[derive(Parser)]
#[command(name = "clausura", about = "CI-native agent CLI tool")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a Clausura task
    Run(RunArgs),
    /// Manage checkpoints
    Snapshot {
        #[command(subcommand)]
        action: SnapshotAction,
    },
}

#[derive(Subcommand)]
enum SnapshotAction {
    /// List checkpoints
    List,
    /// Show checkpoint details
    Show {
        #[arg(long)]
        id: Option<uuid::Uuid>,
    },
    /// Delete checkpoints
    Delete {
        #[arg(long)]
        thread: String,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run(args) => {
            let exit_code = commands::run::execute(args).await;
            std::process::exit(exit_code);
        }
        Commands::Snapshot { action: _ } => {
            eprintln!("Snapshot commands not yet implemented (Task 15)");
            Ok(())
        }
    }
}
