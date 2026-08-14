use std::path::Path;
use std::path::PathBuf;

use crate::provider::Provider;
use crate::types::{MemoryTier, Message, Role};

/// Manages conversation context with token budget enforcement.
pub struct ContextManager<'a> {
    provider: &'a dyn Provider,
    token_budget: u64,
    workspace_root: PathBuf,
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
        }
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

    /// Count total tokens in messages.
    pub fn count_tokens(&self, messages: &[Message]) -> u64 {
        messages
            .iter()
            .map(|m| self.provider.count_tokens(&m.content))
            .sum::<u64>()
            + (messages.len() as u64) // overhead per message
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
        if messages.is_empty() {
            return 0;
        }

        // Binary search for the maximum number of messages that fit
        let target = (self.token_budget as f64 * 0.75) as u64;

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

    /// Keep the system message (first), all explicitly pinned messages, and
    /// the last N-1-pinned messages. Returns the kept messages plus the
    /// index in the original slice where the contiguous kept tail begins
    /// (so callers can identify exactly which messages were dropped).
    ///
    /// Pinned messages are never dropped; they sit right after the system
    /// message at the head of the context. Assistant-tool pairing in the
    /// retained tail is preserved as in `keep_last_n`.
    fn keep_pinned_and_last_n(&self, messages: &[Message], n: usize) -> (Vec<Message>, usize) {
        if messages.is_empty() || n == 0 {
            return (Vec::new(), 0);
        }
        let system = messages[0].clone();

        // Pinned messages (excluding the system message, which is kept
        // separately). Pinning is only meaningful for head messages — a
        // pinned message that also falls inside the retained tail is not
        // duplicated.
        let pinned: Vec<&Message> = messages[1..]
            .iter()
            .filter(|m| m.tier == Some(MemoryTier::Pinned))
            .collect();
        if pinned.is_empty() || n >= messages.len() {
            let kept = self.keep_last_n(messages, n);
            let start = messages.len().saturating_sub(kept.len().saturating_sub(1));
            return (kept, start);
        }

        // Budget for the tail: n minus system minus pinned head.
        let tail_count = n.saturating_sub(1 + pinned.len());
        let mut tail_start = messages.len().saturating_sub(tail_count);

        // If the tail starts with a Tool message, walk back to its Assistant
        // to keep the pair intact.
        if messages[tail_start..].first().map(|m| &m.role) == Some(&Role::Tool) {
            for i in (1..tail_start).rev() {
                if messages[i].role == Role::Assistant && messages[i].tool_calls.is_some() {
                    tail_start = i;
                    break;
                }
            }
        }

        let mut result = vec![system];
        result.extend(pinned.into_iter().cloned());
        result.extend_from_slice(&messages[tail_start..]);
        (result, tail_start)
    }

    /// Elide ephemeral (tool-output) message bodies, oldest first, until the
    /// context no longer needs truncation or nothing elidable remains.
    ///
    /// Elision replaces the message *content* with a stub instead of dropping
    /// the message: the assistant-tool pairing invariant is untouched, so
    /// providers that validate it keep accepting the request. The most recent
    /// tool outputs are never elided (they are the agent's working context).
    ///
    /// Returns the elided originals as `(index, content)` pairs, oldest
    /// first, so the caller can archive them.
    pub fn elide_ephemeral_outputs(&self, messages: &mut Vec<Message>) -> Vec<(usize, String)> {
        /// Tool outputs within this many messages of the tail are kept
        /// verbatim — they are the agent's immediate working context.
        const KEEP_RECENT_MESSAGES: usize = 4;

        let mut elided = Vec::new();
        let len = messages.len();
        for i in 1..len.saturating_sub(KEEP_RECENT_MESSAGES) {
            if !self.should_truncate(messages) {
                break;
            }
            let m = &messages[i];
            if m.memory_tier() != MemoryTier::Ephemeral || m.content.is_empty() {
                continue;
            }
            let original = std::mem::replace(
                &mut messages[i].content,
                "[tool output elided by layered memory; the full transcript is archived]"
                    .to_string(),
            );
            elided.push((i, original));
        }
        elided
    }

    /// Layered truncation: like [`Self::truncate`], but explicitly pinned
    /// messages survive alongside the system message. Returns the dropped
    /// messages (oldest first, system message excluded) so the caller can
    /// archive exactly what left the context.
    pub fn truncate_preserving_pinned(&self, messages: &mut Vec<Message>) -> Vec<Message> {
        if messages.is_empty() {
            return Vec::new();
        }
        let target = (self.token_budget as f64 * 0.75) as u64;

        let mut low = 1usize;
        let mut high = messages.len();
        while low < high {
            let mid = (low + high).div_ceil(2);
            let (candidate, _) = self.keep_pinned_and_last_n(messages, mid);
            if self.count_tokens(&candidate) <= target {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        let (preserved, tail_start) = self.keep_pinned_and_last_n(messages, low);

        // Dropped = everything before the kept tail that is not pinned and
        // not the system message.
        let dropped: Vec<Message> = messages[1..tail_start.max(1)]
            .iter()
            .filter(|m| m.tier != Some(MemoryTier::Pinned))
            .cloned()
            .collect();
        *messages = preserved;
        dropped
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
    use crate::types::{MemoryTier, Role};
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

    // ---------------------------------------------------------------
    // Layered memory
    // ---------------------------------------------------------------

    fn layered_messages(big_tool_output: &str) -> Vec<Message> {
        let mut msgs =
            vec![Message::new(Role::System, "system prompt".to_string())
                .with_tier(MemoryTier::Pinned)];
        msgs.push(
            Message::new(Role::User, "task contract: review the diff".to_string())
                .with_tier(MemoryTier::Pinned),
        );
        for i in 0..6 {
            msgs.push(Message::new(Role::Assistant, format!("turn {i}")));
            msgs.push(Message::with_tool_call(
                Role::Tool,
                big_tool_output.to_string(),
                format!("call_{i}"),
            ));
        }
        msgs
    }

    #[test]
    fn test_elide_ephemeral_outputs_stubs_old_tool_outputs() {
        let mock = MockProvider::new("test");
        let root = TempDir::new().unwrap();
        // Budget small enough to require truncation until most tool outputs
        // are stubs, large enough that stubbing alone suffices.
        let manager = ContextManager::new(&mock, 120, root.path().to_path_buf());
        let mut msgs = layered_messages(&"x".repeat(200));
        assert!(manager.should_truncate(&msgs));

        let elided = manager.elide_ephemeral_outputs(&mut msgs);
        assert!(!elided.is_empty(), "expected some outputs to be elided");

        // Elided bodies became stubs; originals were returned oldest-first.
        for (idx, original) in &elided {
            assert_eq!(
                msgs[*idx].content,
                "[tool output elided by layered memory; the full transcript is archived]"
            );
            assert_eq!(original, &"x".repeat(200));
        }
        let indices: Vec<usize> = elided.iter().map(|(i, _)| *i).collect();
        let mut sorted = indices.clone();
        sorted.sort_unstable();
        assert_eq!(indices, sorted, "elision must proceed oldest-first");

        // Messages are never dropped: assistant-tool pairing is untouched.
        assert_eq!(msgs.len(), 14);
        assert!(msgs
            .iter()
            .all(|m| m.role != Role::Tool || m.tool_call_id.is_some()));

        // The most recent tool outputs survive verbatim.
        let last_tool = msgs.iter().rev().find(|m| m.role == Role::Tool).unwrap();
        assert_eq!(last_tool.content, "x".repeat(200));
    }

    #[test]
    fn test_elide_ephemeral_outputs_respects_explicit_conversation_tier() {
        let mock = MockProvider::new("test");
        let root = TempDir::new().unwrap();
        let manager = ContextManager::new(&mock, 120, root.path().to_path_buf());
        let mut msgs = layered_messages(&"x".repeat(200));
        // A tool output explicitly tiered as conversation must not be elided.
        msgs[3].tier = Some(MemoryTier::Conversation);
        let elided = manager.elide_ephemeral_outputs(&mut msgs);
        assert!(
            elided.iter().all(|(i, _)| *i != 3),
            "explicitly conversation-tiered tool output must not be elided"
        );
    }

    #[test]
    fn test_elide_ephemeral_outputs_noop_under_budget() {
        let mock = MockProvider::new("test");
        let root = TempDir::new().unwrap();
        let manager = ContextManager::new(&mock, 100000, root.path().to_path_buf());
        let mut msgs = layered_messages("small");
        let elided = manager.elide_ephemeral_outputs(&mut msgs);
        assert!(elided.is_empty());
        assert!(msgs.iter().all(|m| !m.content.contains("elided")));
    }

    #[test]
    fn test_truncate_preserving_pinned_keeps_contract() {
        let mock = MockProvider::new("test");
        let root = TempDir::new().unwrap();
        let manager = ContextManager::new(&mock, 60, root.path().to_path_buf());
        let mut msgs = layered_messages(&"x".repeat(120));
        let dropped = manager.truncate_preserving_pinned(&mut msgs);
        assert!(!dropped.is_empty());

        // Pinned head survived, in order, right after the system message.
        assert_eq!(msgs[0].role, Role::System);
        assert_eq!(msgs[1].content, "task contract: review the diff");
        assert_eq!(msgs[1].tier, Some(MemoryTier::Pinned));

        // The dropped set contains neither the system message nor the pin.
        assert!(dropped.iter().all(|m| m.role != Role::System));
        assert!(dropped
            .iter()
            .all(|m| m.content != "task contract: review the diff"));

        // Pairing invariant still holds in the retained tail.
        for i in 1..msgs.len() {
            if msgs[i].role == Role::Tool {
                assert_eq!(msgs[i - 1].role, Role::Assistant);
            }
        }
    }

    #[test]
    fn test_truncate_preserving_pinned_without_pins_matches_classic() {
        let mock = MockProvider::new("test");
        let root = TempDir::new().unwrap();
        let manager = ContextManager::new(&mock, 40, root.path().to_path_buf());
        let mut layered = make_messages(20);
        let mut classic = layered.clone();
        let dropped_l = manager.truncate_preserving_pinned(&mut layered);
        let dropped_c = manager.truncate(&mut classic);
        assert_eq!(layered, classic);
        assert_eq!(dropped_l.len(), dropped_c);
    }
}
