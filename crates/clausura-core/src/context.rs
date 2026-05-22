use crate::provider::Provider;
use crate::types::{Message, Role};

/// Manages conversation context with token budget enforcement.
pub struct ContextManager<'a> {
    provider: &'a dyn Provider,
    token_budget: u64,
}

impl<'a> ContextManager<'a> {
    pub fn new(provider: &'a dyn Provider, token_budget: u64) -> Self {
        Self {
            provider,
            token_budget,
        }
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

        let system = messages[0].clone(); // System message is always first

        // Collect the last n-1 messages (don't count system)
        let mut tail: Vec<Message> = messages[1..]
            .iter()
            .rev()
            .take(n - 1)
            .cloned()
            .collect::<Vec<_>>();
        tail.reverse();

        // Ensure we don't orphan tool calls — if the tail starts with a Tool
        // message without its paired Assistant, expand to include the Assistant.
        if !tail.is_empty() && tail[0].role == Role::Tool {
            // Find the index in the original messages of this tool message,
            // then walk back to include the preceding Assistant.
            let first_tail_content = &tail[0].content;
            if let Some(pos) = messages[1..]
                .iter()
                .position(|m| m.role == Role::Tool && &m.content == first_tail_content)
            {
                let orig_idx = pos + 1;
                if orig_idx > 0 && messages[orig_idx - 1].role == Role::Assistant {
                    // The Assistant is already right before the Tool in the slice before tail.
                    // Check if it would have been omitted.
                    let tail_count = n - 1;
                    let before_tail_start = messages.len().saturating_sub(tail_count);
                    if orig_idx < before_tail_start {
                        // The Assistant was omitted; include one more message
                        let expanded: Vec<Message> = messages[1..]
                            .iter()
                            .rev()
                            .take(tail_count + 1)
                            .cloned()
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect();
                        let mut result = vec![system];
                        result.extend(expanded);
                        return result;
                    }
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

    fn make_messages(count: usize) -> Vec<Message> {
        let mut msgs = vec![Message {
            role: Role::System,
            content: "You are a helpful assistant.".to_string(),
        }];
        for i in 0..count - 1 {
            msgs.push(Message {
                role: if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: format!("Message {}", i),
            });
        }
        msgs
    }

    #[test]
    fn test_under_budget_no_truncation() {
        let mock = MockProvider::new("test");
        let manager = ContextManager::new(&mock, 100000);
        let msgs = make_messages(5);
        assert!(!manager.should_truncate(&msgs));
    }

    #[test]
    fn test_over_budget_triggers_truncation() {
        let mock = MockProvider::new("test");
        let manager = ContextManager::new(&mock, 35);
        let msgs = make_messages(10);
        assert!(manager.should_truncate(&msgs));
    }

    #[test]
    fn test_truncation_preserves_system_message() {
        let mock = MockProvider::new("test");
        let manager = ContextManager::new(&mock, 40);
        let mut msgs = make_messages(20);
        let dropped = manager.truncate(&mut msgs);
        assert!(dropped > 0);
        assert_eq!(msgs[0].role, Role::System);
        assert_eq!(msgs[0].content, "You are a helpful assistant.");
    }

    #[test]
    fn test_estimate_remaining() {
        let mock = MockProvider::new("test");
        let manager = ContextManager::new(&mock, 1000);
        let msgs = make_messages(5);
        let remaining = manager.estimate_remaining(&msgs);
        assert!(remaining > 0);
        assert!(remaining <= 1000);
    }

    #[test]
    fn test_truncate_to_budget_noop_when_under() {
        let mock = MockProvider::new("test");
        let manager = ContextManager::new(&mock, 100000);
        let mut msgs = make_messages(5);
        let (truncated, dropped) = manager.truncate_to_budget(&mut msgs);
        assert!(!truncated);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn test_empty_messages() {
        let mock = MockProvider::new("test");
        let manager = ContextManager::new(&mock, 1000);
        let mut msgs: Vec<Message> = vec![];
        assert!(!manager.should_truncate(&msgs));
        assert_eq!(manager.truncate(&mut msgs), 0);
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_assistant_tool_pair_preserved() {
        let mock = MockProvider::new("test");
        let manager = ContextManager::new(&mock, 50);
        let msgs = vec![
            Message {
                role: Role::System,
                content: "System prompt".to_string(),
            },
            Message {
                role: Role::User,
                content: "Run git diff".to_string(),
            },
            Message {
                role: Role::Assistant,
                content: "calling tool".to_string(),
            },
            Message {
                role: Role::Tool,
                content: "diff output".to_string(),
            },
            Message {
                role: Role::User,
                content: "What does that mean?".to_string(),
            },
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
}
