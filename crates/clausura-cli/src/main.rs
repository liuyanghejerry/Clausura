use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "clausura", about = "CI-native agent CLI tool")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a Clausura task
    Run {
        #[arg(short, long, default_value = ".clausura.yaml")]
        config: std::path::PathBuf,
    },
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
    let _cli = Cli::parse();
    // TODO: wire up in Task 14
    Ok(())
}
