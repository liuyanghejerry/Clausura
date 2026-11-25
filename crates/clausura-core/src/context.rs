use std::path::Path;
use std::path::PathBuf;

use crate::provider::Provider;
use crate::types::{Message, Role};

/// Manages conversation context with token budget enforcement.
pub struct ContextManager<'a> {
    provider: &'a dyn Provider,
    token_budget: u64,
    workspace_root: PathBuf,
    tool_defs_overhead: u64,
}

/// Create the archive directory at `{workspace_root}/.clausura/archives/`.
/// Returns the directory path.
pub fn create_archive_dir(workspace_root: &Path) -> Result<PathBuf, std::io::Error> {
    let archive_dir = workspace_root.join(".clausura").join("archives");
    std::fs::create_dir_all(&archive_dir)?;
    Ok(archive_dir)
}

impl<'a> ContextManager<'a> {
    pub fn new(provider: &'a dyn Provider, token_budget: u64, workspace_root: PathBuf) -> Self {
        Self {
            provider,
            token_budget,
            workspace_root,
            tool_defs_overhead: 0,
        }
    }

    /// Account for static API-level tool-definition schema tokens in budget
    /// checks so `should_truncate` reflects the real prompt payload size.
    pub fn with_tool_defs_overhead(mut self, tokens: u64) -> Self {
        self.tool_defs_overhead = tokens;
        self
    }

    /// Create the archive directory at `{workspace_root}/.clausura/archives/`.
    /// Returns the directory path.
    fn create_archive_dir_inner(&self) -> Result<PathBuf, std::io::Error> {
        create_archive_dir(&self.workspace_root)
    }

    /// Archive dropped messages to a JSON lines file.
    /// Returns the workspace-relative path to the archive file.
    /// Archive path: {workspace_root}/.clausura/archives/context-dump-{task_id}-{seq}.log
    pub async fn archive(
        &self,
        dropped_messages: &[Message],
        task_id: &str,
    ) -> Result<PathBuf, std::io::Error> {
        let archive_dir = self.create_archive_dir_inner()?;

        // Determine sequence number by counting existing files
        let prefix = format!("context-dump-{}-", task_id);
        let seq = {
            let mut max_seq = 0u32;
            if let Ok(entries) = std::fs::read_dir(&archive_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.starts_with(&prefix) && name_str.ends_with(".log") {
                        // Extract seq from filename: context-dump-{task_id}-{seq}.log
                        let rest = &name_str[prefix.len()..];
                        if let Some(seq_str) = rest.strip_suffix(".log") {
                            if let Ok(s) = seq_str.parse::<u32>() {
                                max_seq = max_seq.max(s);
                            }
                        }
                    }
                }
            }
            max_seq + 1
        };

        let filename = format!("context-dump-{}-{}.log", task_id, seq);
        let file_path = archive_dir.join(&filename);
        let relative_path = PathBuf::from(".clausura").join("archives").join(&filename);

        // Write each message as a JSON line
        let mut content = String::new();
        for msg in dropped_messages {
            if let Ok(line) = serde_json::to_string(msg) {
                content.push_str(&line);
                content.push('\n');
            }
        }

        tokio::fs::write(&file_path, content).await?;
        Ok(relative_path)
    }

    /// Count total tokens in messages, including tool call names/arguments and
    /// any configured static tool-definition overhead.
    pub fn count_tokens(&self, messages: &[Message]) -> u64 {
        let msg_tokens: u64 = messages
            .iter()
            .map(|m| {
                let mut total = if m.content.is_empty() {
                    0
                } else {
                    self.provider.count_tokens(&m.content)
                };
                if let Some(tcid) = &m.tool_call_id {
                    total += self.provider.count_tokens(tcid);
                }
                if let Some(calls) = &m.tool_calls {
                    for tc in calls {
                        total += self.provider.count_tokens(&tc.name);
                        let args_str = tc.arguments.to_string();
                        total += self.provider.count_tokens(&args_str);
                    }
                }
                total + 1 // per-message overhead
            })
            .sum();
        self.tool_defs_overhead + msg_tokens
    }

    /// Prune oversized older `Role::Tool` outputs in place (leaving the latest
    /// contiguous batch of tool results intact) until total tokens fit within
    /// `target_tokens`. Preserves a short head preview and any spill/resume
    /// locator line so the agent can re-read if needed.
    ///
    /// Returns the number of tool messages pruned.
    pub fn prune_old_tool_outputs(&self, messages: &mut [Message], target_tokens: u64) -> usize {
        if self.count_tokens(messages) <= target_tokens {
            return 0;
        }

        // Find the start of the most recent tool-result turn so we never prune
        // what the agent literally just fetched.
        let mut protected_from = messages.len();
        if let Some(last_tool) = messages.iter().rposition(|m| m.role == Role::Tool) {
            let mut batch_start = last_tool;
            while batch_start > 0 && messages[batch_start - 1].role == Role::Tool {
                batch_start -= 1;
            }
            protected_from = batch_start;
        }

        const MIN_PRUNE_CHARS: usize = 400;
        const PREVIEW_CHARS: usize = 160;
        const PRUNED_MARKER: &str = "[earlier tool output pruned to stay within token budget]";

        let mut pruned = 0usize;
        for idx in 1..protected_from {
            if self.count_tokens(messages) <= target_tokens {
                break;
            }
            if messages[idx].role != Role::Tool {
                continue;
            }
            let content = &messages[idx].content;
            if content.contains(PRUNED_MARKER) || content.chars().count() < MIN_PRUNE_CHARS {
                continue;
            }

            let preview: String = content.chars().take(PREVIEW_CHARS).collect();
            let locator_line = content
                .lines()
                .rev()
                .find(|l| l.contains("saved to ") || l.contains("resume from "))
                .map(|l| format!("\n{l}"))
                .unwrap_or_default();

            messages[idx].content = format!(
                "{preview}...\n{PRUNED_MARKER} ({} bytes originally){locator_line}",
                content.len()
            );
            pruned += 1;
        }
        pruned
    }

    /// Estimate remaining token budget.
    pub fn estimate_remaining(&self, messages: &[Message]) -> u64 {
        self.token_budget
            .saturating_sub(self.count_tokens(messages))
    }

    /// Check if truncation is needed (> 80% of budget used).
    pub fn should_truncate(&self, messages: &[Message]) -> bool {
        let used = self.count_tokens(messages);
        used > (self.token_budget as f64 * 0.8) as u64
    }

    /// Truncate messages to fit within 75% of budget.
    /// Returns the number of messages dropped.
    /// Preserves system message (index 0) and assistant-tool pairs.
    pub fn truncate(&self, messages: &mut Vec<Message>) -> usize {
        self.truncate_to(messages, 0.75)
    }

    /// Truncate messages to fit within `ratio` of the budget (e.g. 0.5 for
    /// aggressive reactive compaction). Returns the number of messages
    /// dropped. Preserves the system message (index 0) and assistant-tool
    /// pairs.
    pub fn truncate_to(&self, messages: &mut Vec<Message>, ratio: f64) -> usize {
        if messages.is_empty() {
            return 0;
        }

        // Binary search for the maximum number of messages that fit
        let target = (self.token_budget as f64 * ratio) as u64;

        let mut low = 1usize; // At least keep system message
        let mut high = messages.len();

        while low < high {
            let mid = (low + high).div_ceil(2);
            let candidate = self.keep_last_n(messages, mid);
            let tokens = self.count_tokens(&candidate);

            if tokens <= target {
                low = mid;
            } else {
                high = mid - 1;
            }
        }

        // Keep `low` messages, preserving system message
        let preserved = self.keep_last_n(messages, low);
        let dropped = messages.len() - preserved.len();

        *messages = preserved;
        dropped
    }

    /// Keep the system message (first) and the last N-1 messages.
    /// Preserves assistant-tool pairs (never splits them).
    fn keep_last_n(&self, messages: &[Message], n: usize) -> Vec<Message> {
        if messages.is_empty() || n == 0 {
            return Vec::new();
        }
        if n >= messages.len() {
            return messages.to_vec();
        }

        let system = messages[0].clone();

        // Take the last n-1 messages (excluding system)
        let tail_count = n - 1;
        let tail_start = messages.len().saturating_sub(tail_count);
        let tail: Vec<Message> = messages[tail_start..].to_vec();

        // If tail starts with a Tool message, ensure its Assistant is included
        if !tail.is_empty() && tail[0].role == Role::Tool {
            // Walk backwards from tail_start to find the Assistant with tool_calls
            for i in (1..tail_start).rev() {
                if messages[i].role == Role::Assistant && messages[i].tool_calls.is_some() {
                    // Include everything from i to the end
                    let mut result = vec![system];
                    result.extend_from_slice(&messages[i..]);
                    return result;
                }
            }
        }

        let mut result = vec![system];
        result.extend(tail);
        result
    }

    /// Truncate to fit budget, returning whether truncation occurred and the count dropped.
    pub fn truncate_to_budget(&self, messages: &mut Vec<Message>) -> (bool, usize) {
        if !self.should_truncate(messages) {
            return (false, 0);
        }
        let dropped = self.truncate(messages);
        (dropped > 0, dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::tests::MockProvider;
    use crate::types::Role;
    use tempfile::TempDir;

    fn make_messages(count: usize) -> Vec<Message> {
        let mut msgs = vec![Message::new(
            Role::System,
            "You are a helpful assistant.".to_string(),
        )];
        for i in 0..count - 1 {
            msgs.push(Message::new(
                if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                format!("Message {}", i),
            ));
        }
        msgs
    }

    #[test]
    fn test_under_budget_no_truncation() {
        let mock = MockProvider::new("test");
        let root = TempDir::new().unwrap();
        let manager = ContextManager::new(&mock, 100000, root.path().to_path_buf());
        let msgs = make_messages(5);
        assert!(!manager.should_truncate(&msgs));
    }

    #[test]
    fn test_over_budget_triggers_truncation() {
        let mock = MockProvider::new("test");
        let root = TempDir::new().unwrap();
        let manager = ContextManager::new(&mock, 35, root.path().to_path_buf());
        let msgs = make_messages(10);
        assert!(manager.should_truncate(&msgs));
    }

    #[test]
    fn test_truncation_preserves_system_message() {
        let mock = MockProvider::new("test");
        let root = TempDir::new().unwrap();
        let manager = ContextManager::new(&mock, 40, root.path().to_path_buf());
        let mut msgs = make_messages(20);
        let dropped = manager.truncate(&mut msgs);
        assert!(dropped > 0);
        assert_eq!(msgs[0].role, Role::System);
        assert_eq!(msgs[0].content, "You are a helpful assistant.");
    }

    #[test]
    fn test_estimate_remaining() {
        let mock = MockProvider::new("test");
        let root = TempDir::new().unwrap();
        let manager = ContextManager::new(&mock, 1000, root.path().to_path_buf());
        let msgs = make_messages(5);
        let remaining = manager.estimate_remaining(&msgs);
        assert!(remaining > 0);
        assert!(remaining <= 1000);
    }

    #[test]
    fn test_truncate_to_budget_noop_when_under() {
        let mock = MockProvider::new("test");
        let root = TempDir::new().unwrap();
        let manager = ContextManager::new(&mock, 100000, root.path().to_path_buf());
        let mut msgs = make_messages(5);
        let (truncated, dropped) = manager.truncate_to_budget(&mut msgs);
        assert!(!truncated);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn test_empty_messages() {
        let mock = MockProvider::new("test");
        let root = TempDir::new().unwrap();
        let manager = ContextManager::new(&mock, 1000, root.path().to_path_buf());
        let mut msgs: Vec<Message> = vec![];
        assert!(!manager.should_truncate(&msgs));
        assert_eq!(manager.truncate(&mut msgs), 0);
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_assistant_tool_pair_preserved() {
        let mock = MockProvider::new("test");
        let root = TempDir::new().unwrap();
        let manager = ContextManager::new(&mock, 50, root.path().to_path_buf());
        let msgs = vec![
            Message::new(Role::System, "System prompt".to_string()),
            Message::new(Role::User, "Run git diff".to_string()),
            Message::new(Role::Assistant, "calling tool".to_string()),
            Message::new(Role::Tool, "diff output".to_string()),
            Message::new(Role::User, "What does that mean?".to_string()),
        ];
        let mut msgs = msgs;
        let _dropped = manager.truncate(&mut msgs);
        for i in 1..msgs.len() {
            if msgs[i].role == Role::Tool {
                assert_eq!(
                    msgs[i - 1].role,
                    Role::Assistant,
                    "tool message at index {} has no preceding assistant",
                    i
                );
            }
        }
    }

    #[tokio::test]
    async fn test_archive_writes_valid_json() {
        let mock = MockProvider::new("test");
        let root = TempDir::new().unwrap();
        let cm = ContextManager::new(&mock, 1000, root.path().to_path_buf());
        let messages = vec![
            Message::new(Role::User, "Hello".to_string()),
            Message::new(Role::Assistant, "Hi there".to_string()),
            Message::new(Role::Tool, "tool result".to_string()),
        ];
        let path = cm.archive(&messages, "test-task").await.unwrap();
        assert_eq!(
            path,
            PathBuf::from(".clausura/archives/context-dump-test-task-1.log")
        );

        let full_path = root.path().join(&path);
        assert!(full_path.exists());

        let content = tokio::fs::read_to_string(&full_path).await.unwrap();
        let lines: Vec<&str> = content.trim().split('\n').collect();
        assert_eq!(lines.len(), 3);

        for (i, line) in lines.iter().enumerate() {
            let msg: Message = serde_json::from_str(line).unwrap();
            assert_eq!(msg.content, messages[i].content);
            assert_eq!(msg.role, messages[i].role);
        }
    }

    #[test]
    fn test_archive_creates_directory() {
        let root = TempDir::new().unwrap();
        let dir = create_archive_dir(root.path()).unwrap();
        let expected = root.path().join(".clausura").join("archives");
        assert_eq!(dir, expected);
        assert!(dir.exists());
    }

    #[tokio::test]
    async fn test_archive_sequential_naming() {
        let mock = MockProvider::new("test");
        let root = TempDir::new().unwrap();
        let cm = ContextManager::new(&mock, 1000, root.path().to_path_buf());
        let messages = vec![Message::new(Role::User, "test".to_string())];

        let path1 = cm.archive(&messages, "seq-test").await.unwrap();
        assert_eq!(
            path1,
            PathBuf::from(".clausura/archives/context-dump-seq-test-1.log")
        );

        let path2 = cm.archive(&messages, "seq-test").await.unwrap();
        assert_eq!(
            path2,
            PathBuf::from(".clausura/archives/context-dump-seq-test-2.log")
        );

        let full1 = root.path().join(&path1);
        let full2 = root.path().join(&path2);
        assert!(full1.exists());
        assert!(full2.exists());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_archive_failure_returns_io_error() {
        use std::os::unix::fs::PermissionsExt;

        let mock = MockProvider::new("test");
        let root = TempDir::new().unwrap();
        // Create a read-only directory to use as workspace_root
        let readonly = root.path().join("readonly");
        std::fs::create_dir(&readonly).unwrap();
        std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o444)).unwrap();
        let readonly_for_cleanup = readonly.clone();

        let cm = ContextManager::new(&mock, 1000, readonly);
        let messages = vec![Message::new(Role::User, "test".to_string())];
        let result = cm.archive(&messages, "fail-test").await;
        assert!(result.is_err());
        // Restore permissions so TempDir can clean up
        let _ = std::fs::set_permissions(
            &readonly_for_cleanup,
            std::fs::Permissions::from_mode(0o755),
        );
    }

    #[test]
    fn test_count_tokens_includes_tool_calls_and_overhead() {
        let mock = MockProvider::new("test");
        let root = TempDir::new().unwrap();
        let cm_plain = ContextManager::new(&mock, 10_000, root.path().to_path_buf());
        let cm_with_defs = ContextManager::new(&mock, 10_000, root.path().to_path_buf())
            .with_tool_defs_overhead(200);

        let msgs = vec![Message {
            role: Role::Assistant,
            content: String::new(),
            tool_call_id: None,
            tool_calls: Some(vec![crate::types::ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "src/very/long/path/example.rs", "offset": 0, "limit": 500}),
            }]),
        }];

        let plain_tokens = cm_plain.count_tokens(&msgs);
        assert!(
            plain_tokens > 10,
            "tool_calls name and arguments must contribute tokens, got {plain_tokens}"
        );
        assert_eq!(cm_with_defs.count_tokens(&msgs), plain_tokens + 200);
    }

    #[test]
    fn test_prune_old_tool_outputs_preserves_latest_batch_and_locator() {
        let mock = MockProvider::new("test");
        let root = TempDir::new().unwrap();
        let cm = ContextManager::new(&mock, 500, root.path().to_path_buf());

        let old_big = format!(
            "{}\n... [output truncated: saved to .clausura/archives/tool-output-t-1.txt — read it with read_file]",
            "line of diff content\n".repeat(120)
        );
        let recent_big = "recent diff line\n".repeat(40);

        let mut msgs = vec![
            Message::new(Role::System, "sys"),
            Message::new(Role::User, "review"),
            Message::new(Role::Assistant, "step 1"),
            Message::with_tool_call(Role::Tool, old_big.clone(), "c1".into()),
            Message::new(Role::Assistant, "step 2"),
            Message::with_tool_call(Role::Tool, recent_big.clone(), "c2".into()),
        ];

        let pruned = cm.prune_old_tool_outputs(&mut msgs, 350);
        assert_eq!(pruned, 1);
        assert!(msgs[3].content.contains("earlier tool output pruned"));
        assert!(msgs[3]
            .content
            .contains(".clausura/archives/tool-output-t-1.txt"));
        assert_eq!(
            msgs[5].content, recent_big,
            "most recent tool-result batch must remain untouched"
        );
    }
}
