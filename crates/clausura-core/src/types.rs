use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Core type definitions for Clausura
// ---------------------------------------------------------------------------

/// Chat message role
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A chat message
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

/// Provider-agnostic finish reason
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Other(String),
}

/// A tool call from the LLM
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Result of executing a tool
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub call_id: String,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Tool definition for LLM tool calling
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Chat response from a provider
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ChatResponse {
    pub message: Message,
    pub usage: Usage,
    pub finish_reason: FinishReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// Token usage
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// Finding severity
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Severity {
    Hint,
    Info,
    Warning,
    Error,
}

/// Code location
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Location {
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub column_start: u32,
    pub column_end: u32,
}

/// Structured finding from agent output
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Finding {
    pub id: uuid::Uuid,
    pub rule_id: String,
    pub severity: Severity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    pub evidence: String,
}

/// Gate action for rule violations
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GateAction {
    Fail,
    Warn,
    Ignore,
}

/// A gating rule for deterministic pass/fail
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct GateRule {
    pub rule_id: String,
    pub description: String,
    pub min_severity: Severity,
    pub max_findings: u32,
    pub action: GateAction,
}

/// Rule evaluation violation
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct RuleViolation {
    pub rule_id: String,
    pub description: String,
    pub actual_count: u32,
    pub max_allowed: u32,
    pub action: GateAction,
}

/// Result from rule engine evaluation
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct GateResult {
    pub exit_code: u32,
    pub violations: Vec<RuleViolation>,
}

/// Ambiguity policy
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AmbiguityPolicy {
    FailClosed,
    ProceedWithCaution,
}

/// Task contract — defines what a task does, how it runs, and gating rules
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TaskContract {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub model: String,
    pub vendor: String,
    pub prompt_template: String,
    #[serde(default)]
    pub tool_allowlist: Vec<String>,
    pub token_budget: u64,
    pub timeout_secs: u64,
    #[serde(default = "default_ambiguity_policy")]
    pub ambiguity_policy: AmbiguityPolicy,
    #[serde(default)]
    pub gating_rules: Vec<GateRule>,
}

fn default_ambiguity_policy() -> AmbiguityPolicy {
    AmbiguityPolicy::FailClosed
}

/// Snapshot metadata for checkpoints
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SnapshotMeta {
    pub thread_id: String,
    pub checkpoint_id: uuid::Uuid,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub version: u32,
    pub truncated: bool,
}

/// A complete snapshot state
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub id: uuid::Uuid,
    pub messages: Vec<Message>,
    pub meta: SnapshotMeta,
}

/// Execution report — final output of a task run
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ExecutionReport {
    pub task_id: String,
    pub exit_code: u32,
    pub findings: Vec<Finding>,
    pub token_usage: Usage,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub errors: Vec<String>,
}

/// Context about the CI environment
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct CiContext {
    pub repo: Option<String>,
    pub pr_number: Option<String>,
    pub commit_sha: Option<String>,
    pub branch: Option<String>,
}

/// Error types for provider operations
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("Authentication failed: {0}")]
    AuthError(String),
    #[error("Rate limited: {0}")]
    RateLimited(String),
    #[error("Server error: {0}")]
    ServerError(String),
    #[error("Bad request: {0}")]
    BadRequest(String),
    #[error("Timeout: {0}")]
    Timeout(String),
    #[error("Network error: {0}")]
    NetworkError(#[from] reqwest::Error),
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),
}

/// Error types for tool operations
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Tool not found: {0}")]
    NotFound(String),
    #[error("Execution failed: {0}")]
    ExecutionFailed(String),
    #[error("Sandbox violation: {0}")]
    SandboxViolation(String),
    #[error("Timeout")]
    Timeout,
}

/// Error types for config operations
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("File not found: {0}")]
    FileNotFound(String),
    #[error("Parse error: {0}")]
    ParseError(String),
    #[error("Validation error: {0}")]
    ValidationError(String),
}

/// Error types for checkpoint operations
#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("Database error: {0}")]
    DbError(String),
    #[error("Serialization error: {0}")]
    SerializationError(String),
    #[error("Not found: {0}")]
    NotFound(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_round_trip() {
        let msg = Message {
            role: Role::User,
            content: "Hello".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_finding_round_trip_with_location() {
        let finding = Finding {
            id: uuid::Uuid::new_v4(),
            rule_id: "TEST".into(),
            severity: Severity::Error,
            message: "Test finding".into(),
            location: Some(Location {
                file: "src/main.rs".into(),
                line_start: 10,
                line_end: 12,
                column_start: 1,
                column_end: 5,
            }),
            evidence: "evidence text".into(),
        };
        let json = serde_json::to_string_pretty(&finding).unwrap();
        let deserialized: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(finding, deserialized);
    }

    #[test]
    fn test_finding_round_trip_without_location() {
        let finding = Finding {
            id: uuid::Uuid::new_v4(),
            rule_id: "TEST".into(),
            severity: Severity::Warning,
            message: "No location".into(),
            location: None,
            evidence: "".into(),
        };
        let json = serde_json::to_string(&finding).unwrap();
        let deserialized: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(finding, deserialized);
        assert!(deserialized.location.is_none());
    }

    #[test]
    fn test_task_contract_defaults() {
        let contract = TaskContract {
            id: "test".into(),
            name: "Test".into(),
            description: "".into(),
            model: "gpt-4o".into(),
            vendor: "openai".into(),
            prompt_template: "Review: {{diff}}".into(),
            tool_allowlist: vec![],
            token_budget: 32000,
            timeout_secs: 300,
            ambiguity_policy: AmbiguityPolicy::FailClosed,
            gating_rules: vec![],
        };
        assert_eq!(contract.ambiguity_policy, AmbiguityPolicy::FailClosed);
        assert!(contract.gating_rules.is_empty());
    }

    #[test]
    fn test_execution_report_round_trip() {
        let report = ExecutionReport {
            task_id: "task-1".into(),
            exit_code: 0,
            findings: vec![],
            token_usage: Usage {
                input_tokens: 100,
                output_tokens: 50,
                total_tokens: 150,
            },
            duration_ms: 5000,
            snapshot_id: None,
            errors: vec![],
        };
        let json = serde_json::to_string(&report).unwrap();
        let deserialized: ExecutionReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, deserialized);
    }

    #[test]
    fn test_usage_default() {
        let usage = Usage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.total_tokens, 0);
    }

    #[test]
    fn test_severity_ordering() {
        assert!(Severity::Error > Severity::Warning);
        assert!(Severity::Warning > Severity::Info);
        assert!(Severity::Info > Severity::Hint);
    }
}
