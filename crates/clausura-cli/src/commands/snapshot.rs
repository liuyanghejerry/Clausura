use clausura_core::checkpoint::CheckpointStore;
use clausura_core::snapshot::SnapshotManager;
use clausura_core::types::CheckpointError;

/// Manage checkpoints
#[derive(clap::Args, Debug)]
pub struct SnapshotArgs {
    #[command(subcommand)]
    pub action: SnapshotAction,
}

#[derive(clap::Subcommand, Debug)]
pub enum SnapshotAction {
    /// List checkpoints
    List {
        /// Thread ID to list (defaults to "default")
        #[arg(long)]
        thread: Option<String>,
        /// Max results
        #[arg(long, default_value = "10")]
        limit: u32,
    },
    /// Show checkpoint details
    Show {
        /// Thread ID
        #[arg(long)]
        thread: Option<String>,
        /// Checkpoint ID
        #[arg(long)]
        id: Option<uuid::Uuid>,
    },
    /// Delete checkpoints
    Delete {
        /// Thread ID to delete
        #[arg(long)]
        thread: String,
        /// Specific checkpoint ID (otherwise deletes all for thread)
        #[arg(long)]
        id: Option<uuid::Uuid>,
    },
}

fn get_manager() -> Result<SnapshotManager, CheckpointError> {
    let store = CheckpointStore::new()?;
    Ok(SnapshotManager::new(store))
}

pub fn execute(args: SnapshotArgs) -> Result<(), Box<dyn std::error::Error>> {
    let manager = get_manager()?;

    match args.action {
        SnapshotAction::List { thread, limit } => {
            let thread_id = thread.unwrap_or_else(|| "default".to_string());
            let snapshots = manager.list_snapshots(&thread_id, limit)?;
            if snapshots.is_empty() {
                println!("No checkpoints found for thread '{}'", thread_id);
                return Ok(());
            }
            println!(
                "{:<36} {:<28} {:<8} {:<10}",
                "Checkpoint ID", "Created At", "Version", "Truncated"
            );
            println!("{}", "-".repeat(82));
            for meta in &snapshots {
                println!(
                    "{:<36} {:<28} {:<8} {:<10}",
                    meta.checkpoint_id.to_string(),
                    meta.created_at.format("%Y-%m-%d %H:%M:%S"),
                    meta.version,
                    if meta.truncated { "yes" } else { "no" },
                );
            }
        }
        SnapshotAction::Show { thread, id } => {
            let thread_id = thread.unwrap_or_else(|| "default".to_string());
            if let Some(checkpoint_id) = id {
                let snapshot = manager.restore_at(&thread_id, &checkpoint_id)?;
                match snapshot {
                    Some(snap) => {
                        println!("Checkpoint ID: {}", snap.id);
                        println!("Thread ID: {}", snap.meta.thread_id);
                        println!(
                            "Created: {}",
                            snap.meta.created_at.format("%Y-%m-%d %H:%M:%S")
                        );
                        println!("Version: {}", snap.meta.version);
                        println!(
                            "Truncated: {}",
                            if snap.meta.truncated { "yes" } else { "no" }
                        );
                        println!("Messages: {}", snap.messages.len());
                        for (i, msg) in snap.messages.iter().enumerate() {
                            let preview: String = msg.content.chars().take(80).collect();
                            println!("  [{}] {:?}: {}", i, msg.role, preview);
                        }
                    }
                    None => {
                        println!(
                            "Checkpoint '{}' not found in thread '{}'",
                            checkpoint_id, thread_id
                        );
                    }
                }
            } else {
                // Show latest
                let snapshot = manager.restore_snapshot(&thread_id, false)?;
                match snapshot {
                    Some(snap) => {
                        println!("Latest checkpoint for thread '{}':", thread_id);
                        println!("  ID: {}", snap.id);
                        println!("  Messages: {}", snap.messages.len());
                        println!(
                            "  Truncated: {}",
                            if snap.meta.truncated { "yes" } else { "no" }
                        );
                    }
                    None => {
                        println!("No checkpoints found for thread '{}'", thread_id);
                    }
                }
            }
        }
        SnapshotAction::Delete { thread, id } => {
            if id.is_some() {
                eprintln!(
                    "Note: Per-checkpoint deletion not yet supported; deleting all checkpoints for thread '{}'",
                    thread
                );
            }
            manager.delete_thread(&thread)?;
            println!("Deleted all checkpoints for thread '{}'", thread);
        }
    }
    Ok(())
}
