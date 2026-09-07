//! Append-only JSON-lines event log for a single agent run.
//!
//! One file per task at
//! `{workspace}/.clausura/archives/run-{task_id}.events.jsonl`. The agent loop
//! records every model-visible exchange (full LLM request/response messages,
//! tool calls and results) plus context-truncation and checkpoint events, so
//! a failed CI run can be audited and partially replayed from this log alone.
//!
//! Checkpoints are recorded here as `Checkpoint` events carrying the complete
//! message state at save time; `last_checkpoint` replays the most recent one.
//! This makes `--resume` work even in ephemeral CI environments where the
//! SQLite checkpoint store under `~/.clausura` does not survive between runs.
//! The SQLite store remains the source for `clausura snapshot list/show`.
//!
//! All appends are best-effort: the log is a diagnostic and recovery aid,
//! never a dependency of the pass/fail verdict.

use crate::types::{FinishReason, Message, ToolCall, Usage};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// One entry in the run event log.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEvent {
    /// The agent loop started.
    RunStart {
        task_id: String,
        model: String,
        workspace: String,
    },
    /// A full LLM request (complete message state at dispatch time).
    LlmRequest { messages: Vec<Message> },
    /// An LLM response.
    LlmResponse {
        message: Message,
        usage: Usage,
        finish_reason: FinishReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCall>>,
    },
    /// A tool invocation requested by the model.
    ToolCall {
        call_id: String,
        name: String,
        arguments: serde_json::Value,
    },
    /// A tool invocation's result (error is the formatted error text, if any).
    ToolResult {
        call_id: String,
        name: String,
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Context truncation applied (with optional auto-compact summary text).
    ContextTruncated {
        dropped_count: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        archive_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compacted_summary: Option<String>,
    },
    /// An oversized tool result was spilled to disk (full output preserved
    /// at the locator path).
    ToolSpill { locator: String },
    /// An advisory repeat-call reminder was injected into the conversation.
    RepeatReminder { tool_name: String, count: u32 },
    /// A corrective findings-recovery prompt was sent (attempt is 1-based).
    FindingsRecoveryAttempt { attempt: u32, error: String },
    /// A Stop response's findings JSON failed to parse. `error_class` is one
    /// of `json_parse` / `schema` / `empty`; `raw_preview` is the first 2 KB
    /// of the raw assistant output (the full response is preserved in the
    /// preceding `llm_response` event).
    FindingsParseFailed {
        error_class: String,
        parse_error: String,
        raw_preview: String,
    },
    /// A message-state checkpoint (written alongside SQLite snapshot saves).
    Checkpoint {
        checkpoint_id: String,
        messages: Vec<Message>,
        truncated: bool,
    },
    /// The agent loop ended.
    RunEnd {
        truncated: bool,
        duration_ms: u64,
        /// Machine-readable `IncompleteReason` code when truncated.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        incomplete_reason: Option<String>,
    },
}

/// Writer for the run event log.
pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    /// Create the log handle for `task_id` under the workspace archives dir.
    pub fn new(workspace_root: &Path, task_id: &str) -> Self {
        Self {
            path: workspace_root
                .join(".clausura")
                .join("archives")
                .join(format!("run-{task_id}.events.jsonl")),
        }
    }

    /// Path of the underlying file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one event as a JSON line. Best-effort: failures are logged at
    /// debug level and never surface to the caller.
    pub fn append(&self, event: &RunEvent) {
        let Ok(mut line) = serde_json::to_string(event) else {
            return;
        };
        line.push('\n');
        let result = (|| -> std::io::Result<()> {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?
                .write_all(line.as_bytes())
        })();
        if let Err(e) = result {
            tracing::debug!(
                path = %self.path.display(),
                reason = %e,
                "event log append failed (ignored)"
            );
        }
    }

    /// Replay checkpoint events and return the message state of the most
    /// recent one. Returns `None` when the log is missing, unreadable, or
    /// contains no checkpoint.
    pub fn last_checkpoint(&self) -> Option<Vec<Message>> {
        let content = std::fs::read_to_string(&self.path).ok()?;
        let mut last: Option<Vec<Message>> = None;
        for line in content.lines() {
            if let Ok(RunEvent::Checkpoint { messages, .. }) = serde_json::from_str(line) {
                last = Some(messages);
            }
        }
        last
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Role;
    use tempfile::TempDir;

    fn sample_messages() -> Vec<Message> {
        vec![
            Message::new(Role::System, "system"),
            Message::new(Role::User, "user"),
        ]
    }

    #[test]
    fn test_append_and_last_checkpoint() {
        let tmp = TempDir::new().unwrap();
        let log = EventLog::new(tmp.path(), "task-1");

        assert!(log.last_checkpoint().is_none());

        log.append(&RunEvent::RunStart {
            task_id: "task-1".into(),
            model: "gpt-4o".into(),
            workspace: tmp.path().display().to_string(),
        });
        log.append(&RunEvent::Checkpoint {
            checkpoint_id: "c1".into(),
            messages: sample_messages(),
            truncated: false,
        });
        log.append(&RunEvent::Checkpoint {
            checkpoint_id: "c2".into(),
            messages: vec![Message::new(Role::User, "later")],
            truncated: true,
        });

        let restored = log.last_checkpoint().expect("checkpoint should exist");
        assert_eq!(restored, vec![Message::new(Role::User, "later")]);
    }

    #[test]
    fn test_log_is_valid_json_lines() {
        let tmp = TempDir::new().unwrap();
        let log = EventLog::new(tmp.path(), "task-2");
        log.append(&RunEvent::ToolCall {
            call_id: "call_1".into(),
            name: "git_diff".into(),
            arguments: serde_json::json!({}),
        });
        log.append(&RunEvent::ToolResult {
            call_id: "call_1".into(),
            name: "git_diff".into(),
            output: "diff".into(),
            error: None,
        });

        let content = std::fs::read_to_string(log.path()).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            let ty = value["type"].as_str().unwrap();
            assert!(
                ty == "tool_call" || ty == "tool_result",
                "unexpected event type: {ty}"
            );
        }
    }

    #[test]
    fn test_last_checkpoint_missing_file_is_none() {
        let tmp = TempDir::new().unwrap();
        let log = EventLog::new(tmp.path(), "ghost");
        assert!(log.last_checkpoint().is_none());
    }
}
