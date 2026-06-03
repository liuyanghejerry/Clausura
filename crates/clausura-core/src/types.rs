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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_call_id: None,
        }
    }

    pub fn with_tool_call(role: Role, content: impl Into<String>, tool_call_id: String) -> Self {
        Self {
            role,
            content: content.into(),
            tool_call_id: Some(tool_call_id),
        }
    }
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

/// Supported LLM vendor API types.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VendorType {
    /// OpenAI-compatible API (OpenAI, DeepSeek, Groq, Ollama, vLLM, etc.)
    #[serde(alias = "openai_compatible", alias = "openai")]
    #[default]
    OpenAiCompatible,
    /// Anthropic Messages API (Claude)
    #[serde(alias = "anthropic_compatible", alias = "anthropic", alias = "claude")]
    AnthropicCompatible,
    /// User-defined enterprise LLM with custom auth
    Custom,
}

/// Vendor-specific configuration.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct VendorConfig {
    /// Vendor API type
    #[serde(rename = "type", default)]
    pub vendor_type: VendorType,
    /// Base URL override (for Anthropic, defaults to https://api.anthropic.com)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Auth header name (for Custom, defaults to "Authorization")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<String>,
    /// Environment variable name for the API key (defaults to CLAUSURA_API_KEY)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
}

impl Default for VendorConfig {
    fn default() -> Self {
        VendorConfig {
            vendor_type: VendorType::OpenAiCompatible,
            base_url: None,
            auth_header: None,
            api_key_env: None,
        }
    }
}

impl VendorConfig {
    /// Create a VendorConfig from a vendor name string (e.g., "openai", "anthropic", "ollama").
    /// This is used by the config loader to convert YAML/CLI string values into VendorConfig.
    pub fn from_name(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "" => VendorConfig::default(),
            "openai" | "openai_compatible" => VendorConfig::openai(),
            "ollama" => VendorConfig::ollama(),
            "anthropic" | "claude" | "anthropic_compatible" => VendorConfig::anthropic(),
            "deepseek" => VendorConfig {
                vendor_type: VendorType::OpenAiCompatible,
                base_url: Some("https://api.deepseek.com/v1".into()),
                ..Default::default()
            },
            "groq" => VendorConfig {
                vendor_type: VendorType::OpenAiCompatible,
                base_url: Some("https://api.groq.com/openai/v1".into()),
                ..Default::default()
            },
            other => VendorConfig {
                vendor_type: VendorType::OpenAiCompatible,
                base_url: Some(format!("https://api.{}.com/v1", other)),
                ..Default::default()
            },
        }
    }

    /// Create an OpenAI-compatible vendor config.
    pub fn openai() -> Self {
        VendorConfig {
            vendor_type: VendorType::OpenAiCompatible,
            base_url: Some("https://api.openai.com/v1".into()),
            ..Default::default()
        }
    }

    /// Create an Anthropic-compatible vendor config.
    pub fn anthropic() -> Self {
        VendorConfig {
            vendor_type: VendorType::AnthropicCompatible,
            base_url: Some("https://api.anthropic.com".into()),
            auth_header: Some("x-api-key".into()),
            ..Default::default()
        }
    }

    /// Create an Ollama vendor config (OpenAI-compatible).
    pub fn ollama() -> Self {
        VendorConfig {
            vendor_type: VendorType::OpenAiCompatible,
            base_url: Some("http://localhost:11434/v1".into()),
            ..Default::default()
        }
    }

    /// Get the effective base URL.
    pub fn effective_base_url(&self) -> &str {
        self.base_url.as_deref().unwrap_or(match self.vendor_type {
            VendorType::OpenAiCompatible => "https://api.openai.com/v1",
            VendorType::AnthropicCompatible => "https://api.anthropic.com",
            VendorType::Custom => "",
        })
    }

    /// Get the effective auth header name.
    pub fn auth_header_name(&self) -> &str {
        self.auth_header
            .as_deref()
            .unwrap_or(match self.vendor_type {
                VendorType::AnthropicCompatible => "x-api-key",
                _ => "Authorization",
            })
    }
}

/// A helper for deserializing VendorConfig from either a string or an object.
/// String form: "openai", "ollama", "anthropic" → maps to the corresponding VendorConfig preset.
/// Object form: { type: "openai_compatible", base_url: "...", auth_header: "..." }
impl<'de> Deserialize<'de> for VendorConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;

        struct VendorConfigVisitor;

        impl<'de> de::Visitor<'de> for VendorConfigVisitor {
            type Value = VendorConfig;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a vendor name string or a vendor config object")
            }

            // Handle string form: "openai", "ollama", "anthropic" — delegates to from_name
            fn visit_str<E>(self, value: &str) -> Result<VendorConfig, E>
            where
                E: de::Error,
            {
                Ok(VendorConfig::from_name(value))
            }

            // Handle map form: { type: "...", base_url: "...", ... }
            fn visit_map<M>(self, map: M) -> Result<VendorConfig, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                #[derive(Deserialize)]
                struct VendorConfigRaw {
                    #[serde(rename = "type", default)]
                    vendor_type: VendorType,
                    #[serde(default)]
                    base_url: Option<String>,
                    #[serde(default)]
                    auth_header: Option<String>,
                    #[serde(default)]
                    api_key_env: Option<String>,
                }

                let raw = VendorConfigRaw::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(VendorConfig {
                    vendor_type: raw.vendor_type,
                    base_url: raw.base_url,
                    auth_header: raw.auth_header,
                    api_key_env: raw.api_key_env,
                })
            }
        }

        deserializer.deserialize_any(VendorConfigVisitor)
    }
}

/// Task contract — defines what a task does, how it runs, and gating rules
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TaskContract {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub model: String,
    #[serde(default)]
    pub vendor: VendorConfig,
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
            tool_call_id: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_message_new_sets_tool_call_id_none() {
        let msg = Message::new(Role::System, "prompt");
        assert_eq!(msg.role, Role::System);
        assert_eq!(msg.content, "prompt");
        assert!(msg.tool_call_id.is_none());
    }

    #[test]
    fn test_message_with_tool_call_sets_id() {
        let msg = Message::with_tool_call(Role::Tool, "result", "call_123".into());
        assert_eq!(msg.role, Role::Tool);
        assert_eq!(msg.content, "result");
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_123"));
    }

    #[test]
    fn test_tool_message_serializes_tool_call_id() {
        let msg = Message::with_tool_call(Role::Tool, "tool output", "call_abc".into());
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("tool_call_id"));
        assert!(json.contains("call_abc"));
    }

    #[test]
    fn test_non_tool_message_omits_tool_call_id() {
        let msg = Message::new(Role::User, "hello");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("tool_call_id"));
    }

    #[test]
    fn test_deserialize_tool_message_with_tool_call_id() {
        let json = r#"{"role":"tool","content":"result","tool_call_id":"call_xyz"}"#;
        let msg: Message = serde_json::from_str(json).unwrap();
        assert_eq!(msg.role, Role::Tool);
        assert_eq!(msg.content, "result");
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_xyz"));
    }

    #[test]
    fn test_deserialize_message_without_tool_call_id() {
        let json = r#"{"role":"user","content":"hello"}"#;
        let msg: Message = serde_json::from_str(json).unwrap();
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.content, "hello");
        assert!(msg.tool_call_id.is_none());
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
            vendor: VendorConfig::openai(),
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

    #[test]
    fn test_vendor_config_openai() {
        let cfg = VendorConfig::openai();
        assert_eq!(cfg.vendor_type, VendorType::OpenAiCompatible);
        assert_eq!(cfg.effective_base_url(), "https://api.openai.com/v1");
        assert_eq!(cfg.auth_header_name(), "Authorization");
    }

    #[test]
    fn test_vendor_config_anthropic() {
        let cfg = VendorConfig::anthropic();
        assert_eq!(cfg.vendor_type, VendorType::AnthropicCompatible);
        assert_eq!(cfg.effective_base_url(), "https://api.anthropic.com");
        assert_eq!(cfg.auth_header_name(), "x-api-key");
    }

    #[test]
    fn test_vendor_config_ollama() {
        let cfg = VendorConfig::ollama();
        assert_eq!(cfg.vendor_type, VendorType::OpenAiCompatible);
        assert_eq!(cfg.effective_base_url(), "http://localhost:11434/v1");
    }

    #[test]
    fn test_vendor_config_deser_from_string() {
        let cfg: VendorConfig = serde_json::from_str(r#""openai""#).unwrap();
        assert_eq!(cfg.vendor_type, VendorType::OpenAiCompatible);

        let cfg: VendorConfig = serde_json::from_str(r#""anthropic""#).unwrap();
        assert_eq!(cfg.vendor_type, VendorType::AnthropicCompatible);

        let cfg: VendorConfig = serde_json::from_str(r#""ollama""#).unwrap();
        assert_eq!(cfg.vendor_type, VendorType::OpenAiCompatible);
    }

    #[test]
    fn test_vendor_config_deser_from_object() {
        let cfg: VendorConfig = serde_json::from_str(
            r#"{"type": "custom", "base_url": "https://llm.internal/v1", "auth_header": "X-API-Key"}"#
        ).unwrap();
        assert_eq!(cfg.vendor_type, VendorType::Custom);
        assert_eq!(cfg.base_url.as_deref(), Some("https://llm.internal/v1"));
        assert_eq!(cfg.auth_header.as_deref(), Some("X-API-Key"));
    }
}
