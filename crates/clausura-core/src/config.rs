/// Layered configuration loader for Clausura.
///
/// Configuration is loaded from three sources, in increasing priority:
/// 1. YAML config file (`.clausura.yaml` or `.clausura.yml`)
/// 2. CLI flag overrides
/// 3. Environment variable overrides
///
/// The API key is NEVER read from the YAML file — it must come from
/// a CLI flag or the `CLAUSURA_API_KEY` environment variable.
use crate::types::{
    AmbiguityPolicy, ConfigError, GateAction, GateRule, OnIncompletePolicy, Severity, TaskContract,
    VendorConfig,
};
use serde::Deserialize;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Log output format.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum LogFormat {
    #[default]
    Json,
    Pretty,
}

/// Resolved Clausura configuration after applying all layers.
#[derive(Debug, Clone)]
pub struct Config {
    /// Path to the YAML config file that was loaded, if any.
    pub config_path: Option<PathBuf>,
    /// The fully resolved task contract.
    pub task: TaskContract,
    /// API key (from CLI or env var only, never from YAML).
    pub api_key: Option<String>,
    /// Workspace root directory.
    pub workspace: PathBuf,
    /// Output path for SARIF results.
    pub output: PathBuf,
    /// Whether to resume from a previous checkpoint.
    pub resume: bool,
    /// Log output format.
    pub log_format: LogFormat,
}

// ---------------------------------------------------------------------------
// Raw YAML structures (file format)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct YamlConfig {
    version: String,
    task: YamlTaskConfig,
}

#[derive(Debug, Deserialize)]
struct YamlTaskConfig {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    skill_prompts: Vec<String>,
    #[serde(default)]
    model: String,
    #[serde(default)]
    vendor: String,
    #[serde(default = "default_prompt")]
    prompt_template: String,
    #[serde(default)]
    tool_allowlist: Vec<String>,
    #[serde(default = "default_token_budget")]
    token_budget: u64,
    #[serde(default)]
    max_total_tokens: Option<u64>,
    #[serde(default)]
    auto_compact: bool,
    #[serde(default = "default_max_compactions")]
    max_compactions: u32,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
    #[serde(default = "default_shell_timeout")]
    shell_timeout_secs: u64,
    #[serde(default)]
    shell_env_passthrough: Vec<String>,
    #[serde(default = "default_ambiguity")]
    ambiguity_policy: String,
    #[serde(default)]
    gating: Vec<YamlGateRule>,
    #[serde(default = "default_max_iterations")]
    max_iterations: u32,
    #[serde(default = "default_on_incomplete")]
    on_incomplete: String,
    #[serde(default)]
    mcp_servers: Vec<YamlMcpServerConfig>,
    #[serde(default)]
    preflight: Vec<YamlPreflightCheck>,
}

#[derive(Debug, Deserialize)]
struct YamlGateRule {
    rule: String,
    description: String,
    min_severity: String,
    max_findings: u32,
    action: String,
}

#[derive(Debug, Deserialize)]
struct YamlMcpServerConfig {
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: std::collections::HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct YamlPreflightCheck {
    mcp_server: String,
    tool: String,
    #[serde(default)]
    args: serde_json::Value,
    #[serde(default)]
    rule_id_prefix: Option<String>,
    #[serde(default)]
    severity_field: Option<String>,
    #[serde(default)]
    message_field: Option<String>,
    #[serde(default)]
    file_field: Option<String>,
    #[serde(default)]
    line_field: Option<String>,
    #[serde(default)]
    column_field: Option<String>,
    #[serde(default)]
    default_severity: Option<String>,
}

// ---------------------------------------------------------------------------
// Default helpers
// ---------------------------------------------------------------------------

fn default_prompt() -> String {
    "{{task_description}}".to_string()
}

fn default_token_budget() -> u64 {
    32000
}

fn default_timeout() -> u64 {
    300
}

fn default_shell_timeout() -> u64 {
    120
}

fn default_max_iterations() -> u32 {
    10
}

fn default_max_compactions() -> u32 {
    3
}

fn default_ambiguity() -> String {
    "fail_closed".to_string()
}

fn default_on_incomplete() -> String {
    "fail".to_string()
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn parse_severity(s: &str) -> Severity {
    match s.to_lowercase().as_str() {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        "info" => Severity::Info,
        "hint" => Severity::Hint,
        _ => Severity::Warning,
    }
}

fn parse_gate_action(s: &str) -> GateAction {
    match s.to_lowercase().as_str() {
        "fail" => GateAction::Fail,
        "warn" => GateAction::Warn,
        "ignore" => GateAction::Ignore,
        _ => GateAction::Warn,
    }
}

fn parse_on_incomplete(s: &str) -> OnIncompletePolicy {
    match s.to_lowercase().as_str() {
        "pass" => OnIncompletePolicy::Pass,
        // Unknown values fail closed, matching the default.
        _ => OnIncompletePolicy::Fail,
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn validate_yaml(yaml: &YamlConfig) -> Result<(), ConfigError> {
    if yaml.version.is_empty() {
        return Err(ConfigError::ValidationError("version is required".into()));
    }
    if yaml.version != "1" {
        return Err(ConfigError::ValidationError(format!(
            "Unsupported schema version '{}'. Expected '1'",
            yaml.version
        )));
    }
    if yaml.task.model.is_empty() && std::env::var("CLAUSURA_MODEL").is_err() {
        return Err(ConfigError::ValidationError(
            "task.model is required (or set CLAUSURA_MODEL)".into(),
        ));
    }
    if yaml.task.token_budget == 0 {
        return Err(ConfigError::ValidationError(
            "task.token_budget must be > 0".into(),
        ));
    }
    if yaml.task.max_total_tokens == Some(0) {
        return Err(ConfigError::ValidationError(
            "task.max_total_tokens must be > 0 when set".into(),
        ));
    }
    if yaml.task.timeout_secs == 0 {
        return Err(ConfigError::ValidationError(
            "task.timeout_secs must be > 0".into(),
        ));
    }
    // Validate MCP server names are non-empty and unique.
    {
        let mut seen = std::collections::HashSet::new();
        for s in &yaml.task.mcp_servers {
            if s.name.is_empty() {
                return Err(ConfigError::ValidationError(
                    "mcp_servers: name must be non-empty".into(),
                ));
            }
            if !seen.insert(&s.name) {
                return Err(ConfigError::ValidationError(format!(
                    "Duplicate MCP server name: '{}'",
                    s.name
                )));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Config file discovery
// ---------------------------------------------------------------------------

fn find_config_in_cwd() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    for name in &[".clausura.yaml", ".clausura.yml"] {
        let path = cwd.join(name);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

impl Config {
    /// Load configuration from a layered pipeline:
    ///
    /// 1. YAML file (auto-discovered or explicit path)
    /// 2. CLI flag overrides
    /// 3. Environment variable overrides
    ///
    /// Each subsequent layer overrides the previous one.
    #[allow(clippy::too_many_arguments)]
    pub fn load(
        config_path: Option<&Path>,
        cli_model: Option<&str>,
        cli_vendor: Option<&str>,
        cli_api_key: Option<&str>,
        cli_token_budget: Option<u64>,
        cli_timeout: Option<u64>,
        cli_max_iterations: Option<u32>,
        cli_shell_timeout: Option<u64>,
        workspace: PathBuf,
        output: PathBuf,
        resume: bool,
        log_format: LogFormat,
    ) -> Result<Self, ConfigError> {
        // ---- Layer 1: YAML file ----
        let yaml_path = config_path
            .map(|p| p.to_path_buf())
            .or_else(find_config_in_cwd);

        let (yaml_task, config_path) = if let Some(ref path) = yaml_path {
            let content = std::fs::read_to_string(path)
                .map_err(|e| ConfigError::FileNotFound(format!("{}: {}", path.display(), e)))?;
            let yaml: YamlConfig = serde_yaml::from_str(&content)
                .map_err(|e| ConfigError::ParseError(format!("YAML error: {}", e)))?;
            validate_yaml(&yaml)?;
            (yaml.task, Some(path.clone()))
        } else {
            // No config file — use defaults; CLI / env vars will fill in.
            (
                YamlTaskConfig {
                    name: "default".into(),
                    description: String::new(),
                    skill_prompts: vec![],
                    model: String::new(),
                    vendor: String::new(),
                    prompt_template: default_prompt(),
                    tool_allowlist: vec![],
                    token_budget: default_token_budget(),
                    max_total_tokens: None,
                    auto_compact: false,
                    max_compactions: default_max_compactions(),
                    timeout_secs: default_timeout(),
                    shell_timeout_secs: default_shell_timeout(),
                    shell_env_passthrough: vec![],
                    ambiguity_policy: default_ambiguity(),
                    gating: vec![],
                    max_iterations: default_max_iterations(),
                    on_incomplete: default_on_incomplete(),
                    mcp_servers: vec![],
                    preflight: vec![],
                },
                None,
            )
        };

        // ---- Layer 2: Environment variable + CLI overrides ----
        let model = std::env::var("CLAUSURA_MODEL")
            .ok()
            .or_else(|| cli_model.map(|m| m.to_string()))
            .unwrap_or_else(|| yaml_task.model.clone());

        let vendor_input = std::env::var("CLAUSURA_VENDOR")
            .ok()
            .or_else(|| cli_vendor.map(|v| v.to_string()))
            .unwrap_or_else(|| yaml_task.vendor.clone());
        let vendor = VendorConfig::from_name(&vendor_input);

        let token_budget = std::env::var("CLAUSURA_TOKEN_BUDGET")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(cli_token_budget)
            .unwrap_or(yaml_task.token_budget);

        let max_total_tokens = std::env::var("CLAUSURA_MAX_TOTAL_TOKENS")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(yaml_task.max_total_tokens);

        // Auto-compact: summarize dropped messages instead of bare truncation.
        let auto_compact = match std::env::var("CLAUSURA_AUTO_COMPACT") {
            Ok(v) => matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"),
            Err(_) => yaml_task.auto_compact,
        };
        let max_compactions = std::env::var("CLAUSURA_MAX_COMPACTIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(yaml_task.max_compactions);

        let timeout = std::env::var("CLAUSURA_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(cli_timeout)
            .unwrap_or(yaml_task.timeout_secs);

        let max_iterations = std::env::var("CLAUSURA_MAX_ITERATIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(cli_max_iterations)
            .unwrap_or(yaml_task.max_iterations);

        let shell_timeout = std::env::var("CLAUSURA_SHELL_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(cli_shell_timeout)
            .unwrap_or(yaml_task.shell_timeout_secs);

        // ---- Layer 3: Environment variable overrides ----
        let api_key = std::env::var("CLAUSURA_API_KEY")
            .ok()
            .or_else(|| cli_api_key.map(|s| s.to_string()));

        let ambiguity_str =
            std::env::var("CLAUSURA_AMBIGUITY_POLICY").unwrap_or(yaml_task.ambiguity_policy);

        let ambiguity_policy = match ambiguity_str.as_str() {
            "proceed_with_caution" => AmbiguityPolicy::ProceedWithCaution,
            _ => AmbiguityPolicy::FailClosed,
        };

        let on_incomplete_str =
            std::env::var("CLAUSURA_ON_INCOMPLETE").unwrap_or(yaml_task.on_incomplete);
        let on_incomplete = parse_on_incomplete(&on_incomplete_str);

        let gating_rules = yaml_task
            .gating
            .iter()
            .map(|g| GateRule {
                rule_id: g.rule.clone(),
                description: g.description.clone(),
                min_severity: parse_severity(&g.min_severity),
                max_findings: g.max_findings,
                action: parse_gate_action(&g.action),
            })
            .collect();

        // ---- Resolve skill prompts ----
        let prompt_template = if yaml_task.skill_prompts.is_empty() {
            yaml_task.prompt_template
        } else {
            let mut skill_contents: Vec<(String, String)> = Vec::new();
            for skill_ref in &yaml_task.skill_prompts {
                let content = crate::skills::resolve_skill(skill_ref, &workspace)?;
                skill_contents.push((skill_ref.clone(), content));
            }
            crate::skills::merge_prompts(&skill_contents, &yaml_task.prompt_template)
        };

        Ok(Config {
            config_path,
            task: TaskContract {
                id: format!("task-{}", yaml_task.name.replace(' ', "-")),
                name: yaml_task.name,
                description: yaml_task.description,
                model,
                vendor,
                prompt_template,
                tool_allowlist: yaml_task.tool_allowlist,
                token_budget,
                max_total_tokens,
                auto_compact,
                max_compactions,
                timeout_secs: timeout,
                shell_timeout_secs: shell_timeout,
                shell_env_passthrough: yaml_task.shell_env_passthrough,
                ambiguity_policy,
                gating_rules,
                max_iterations,
                on_incomplete,
                mcp_servers: yaml_task
                    .mcp_servers
                    .into_iter()
                    .map(|s| crate::types::McpServerConfig {
                        name: s.name,
                        command: s.command,
                        args: s.args,
                        env: s.env,
                    })
                    .collect(),
                preflight: yaml_task
                    .preflight
                    .into_iter()
                    .map(|p| {
                        let d = crate::types::PreflightCheck::default();
                        crate::types::PreflightCheck {
                            mcp_server: p.mcp_server,
                            tool: p.tool,
                            args: p.args,
                            rule_id_prefix: p.rule_id_prefix.unwrap_or(d.rule_id_prefix),
                            severity_field: p.severity_field.unwrap_or(d.severity_field),
                            message_field: p.message_field.unwrap_or(d.message_field),
                            file_field: p.file_field.unwrap_or(d.file_field),
                            line_field: p.line_field.unwrap_or(d.line_field),
                            column_field: p.column_field.unwrap_or(d.column_field),
                            default_severity: p.default_severity.unwrap_or(d.default_severity),
                        }
                    })
                    .collect(),
            },
            api_key,
            workspace,
            output,
            resume,
            log_format,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::VendorType;
    use std::io::Write;
    use std::sync::Mutex;
    use tempfile::NamedTempFile;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn write_yaml(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", content).unwrap();
        file
    }

    #[test]
    fn test_valid_config_with_gating() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        let yaml = r#"
version: "1"
task:
  name: code-review
  model: gpt-4o
  vendor: openai
  prompt_template: "Review this diff: {{diff}}"
  token_budget: 16000
  timeout_secs: 120
  ambiguity_policy: fail_closed
  gating:
    - rule: no-critical
      description: No critical errors
      min_severity: error
      max_findings: 0
      action: fail
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.name, "code-review");
        assert_eq!(config.task.model, "gpt-4o");
        assert_eq!(config.task.vendor, VendorConfig::openai());
        assert_eq!(config.task.token_budget, 16000);
        assert_eq!(config.task.timeout_secs, 120);
        assert_eq!(config.task.gating_rules.len(), 1);
        assert_eq!(config.task.gating_rules[0].rule_id, "no-critical");
        assert_eq!(config.task.gating_rules[0].min_severity, Severity::Error);
        assert_eq!(config.task.gating_rules[0].max_findings, 0);
        assert_eq!(config.task.gating_rules[0].action, GateAction::Fail);
    }

    #[test]
    fn test_cli_overrides_model() {
        let _guard = ENV_LOCK.lock().unwrap();
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-3.5-turbo
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            Some("gpt-4o"), // CLI overrides model
            None,
            None,
            Some(32000), // CLI overrides token budget
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.model, "gpt-4o");
        assert_eq!(config.task.token_budget, 32000);
        // These should still come from YAML
        assert_eq!(config.task.vendor, VendorConfig::openai());
        assert_eq!(config.task.timeout_secs, 60);
    }

    #[test]
    fn test_env_overrides_cli_model() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        unsafe { std::env::set_var("CLAUSURA_MODEL", "claude-sonnet") };
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-3.5-turbo
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            Some("gpt-4o"), // CLI model — env should override this
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.model, "claude-sonnet"); // env wins over CLI
        unsafe { std::env::remove_var("CLAUSURA_MODEL") };
    }

    #[test]
    fn test_env_overrides_cli_all_fields() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        unsafe {
            std::env::set_var("CLAUSURA_MODEL", "env-model");
            std::env::set_var("CLAUSURA_VENDOR", "deepseek");
            std::env::set_var("CLAUSURA_TOKEN_BUDGET", "99000");
            std::env::set_var("CLAUSURA_TIMEOUT", "600");
            std::env::set_var("CLAUSURA_API_KEY", "sk-env-key");
        }
        let yaml = r#"
version: "1"
task:
  name: test
  model: yaml-model
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            Some("cli-model"),
            Some("ollama"),
            Some("sk-cli-key"),
            Some(16000),
            None,
            Some(120),
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.model, "env-model");
        assert!(matches!(
            config.task.vendor.vendor_type,
            VendorType::OpenAiCompatible
        ));
        assert_eq!(config.task.token_budget, 99000);
        assert_eq!(config.task.timeout_secs, 600);
        assert_eq!(config.api_key, Some("sk-env-key".to_string()));
        unsafe {
            std::env::remove_var("CLAUSURA_MODEL");
            std::env::remove_var("CLAUSURA_VENDOR");
            std::env::remove_var("CLAUSURA_TOKEN_BUDGET");
            std::env::remove_var("CLAUSURA_TIMEOUT");
            std::env::remove_var("CLAUSURA_API_KEY");
        }
    }

    fn clean_env_vars() {
        unsafe {
            std::env::remove_var("CLAUSURA_API_KEY");
            std::env::remove_var("CLAUSURA_MODEL");
            std::env::remove_var("CLAUSURA_VENDOR");
            std::env::remove_var("CLAUSURA_TOKEN_BUDGET");
            std::env::remove_var("CLAUSURA_MAX_TOTAL_TOKENS");
            std::env::remove_var("CLAUSURA_TIMEOUT");
            std::env::remove_var("CLAUSURA_AMBIGUITY_POLICY");
            std::env::remove_var("CLAUSURA_ON_INCOMPLETE");
            std::env::remove_var("CLAUSURA_SHELL_TIMEOUT");
            std::env::remove_var("CLAUSURA_AUTO_COMPACT");
            std::env::remove_var("CLAUSURA_MAX_COMPACTIONS");
        }
    }

    #[test]
    fn test_env_override_api_key() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        unsafe { std::env::set_var("CLAUSURA_API_KEY", "sk-test-key") };
        let config = Config::load(
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.api_key, Some("sk-test-key".to_string()));
        unsafe { std::env::remove_var("CLAUSURA_API_KEY") };
    }

    #[test]
    fn test_valid_config_minimal() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        let yaml = r#"
version: "1"
task:
  name: quick-scan
  model: claude-3-5-sonnet
  vendor: anthropic
  token_budget: 64000
  timeout_secs: 600
  ambiguity_policy: proceed_with_caution
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "out.sarif".into(),
            true,
            LogFormat::Pretty,
        )
        .unwrap();
        assert_eq!(config.task.name, "quick-scan");
        assert_eq!(config.task.model, "claude-3-5-sonnet");
        assert_eq!(
            config.task.ambiguity_policy,
            AmbiguityPolicy::ProceedWithCaution
        );
        assert!(config.resume);
        assert_eq!(config.log_format, LogFormat::Pretty);
        assert_eq!(config.output, PathBuf::from("out.sarif"));
    }

    #[test]
    fn test_missing_model_is_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        let yaml = r#"
version: "1"
task:
  name: test
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        // CLAUSURA_MODEL is also not set
        let result = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            ConfigError::ValidationError(msg) => {
                assert!(msg.contains("model"));
            }
            _ => panic!("expected ValidationError, got {:?}", err),
        }
    }

    #[test]
    fn test_zero_token_budget_is_error() {
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 0
  timeout_secs: 60
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let result = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_zero_timeout_is_error() {
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 0
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let result = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_env_api_key_takes_precedence_over_cli() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        unsafe {
            std::env::set_var("CLAUSURA_API_KEY", "sk-env-key");
        };
        let config = Config::load(
            None,
            None,
            None,
            Some("sk-cli-key"),
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.api_key, Some("sk-env-key".to_string()));
        unsafe { std::env::remove_var("CLAUSURA_API_KEY") };
    }

    #[test]
    fn test_empty_version_is_error() {
        let yaml = r#"
version: ""
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let result = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_max_iterations_from_yaml() {
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  max_iterations: 5
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.max_iterations, 5);
    }

    #[test]
    fn test_max_iterations_default_is_10() {
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.max_iterations, 10);
    }

    #[test]
    fn test_on_incomplete_default_is_fail() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.on_incomplete, OnIncompletePolicy::Fail);
    }

    #[test]
    fn test_on_incomplete_pass_from_yaml() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
  on_incomplete: pass
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.on_incomplete, OnIncompletePolicy::Pass);
    }

    #[test]
    fn test_on_incomplete_unknown_value_fails_closed() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
  on_incomplete: banana
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.on_incomplete, OnIncompletePolicy::Fail);
    }

    #[test]
    fn test_on_incomplete_env_overrides_yaml() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        unsafe { std::env::set_var("CLAUSURA_ON_INCOMPLETE", "pass") };
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
  on_incomplete: fail
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.on_incomplete, OnIncompletePolicy::Pass);
        unsafe { std::env::remove_var("CLAUSURA_ON_INCOMPLETE") };
    }

    #[test]
    fn test_auto_compact_defaults_off() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert!(
            !config.task.auto_compact,
            "auto_compact must default to off"
        );
        assert_eq!(config.task.max_compactions, 3);
    }

    #[test]
    fn test_auto_compact_from_yaml() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
  auto_compact: true
  max_compactions: 5
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert!(config.task.auto_compact);
        assert_eq!(config.task.max_compactions, 5);
    }

    #[test]
    fn test_auto_compact_env_overrides_yaml() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        unsafe {
            std::env::set_var("CLAUSURA_AUTO_COMPACT", "true");
            std::env::set_var("CLAUSURA_MAX_COMPACTIONS", "7");
        };
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
  auto_compact: false
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert!(config.task.auto_compact, "env must override YAML");
        assert_eq!(config.task.max_compactions, 7);
        unsafe {
            std::env::remove_var("CLAUSURA_AUTO_COMPACT");
            std::env::remove_var("CLAUSURA_MAX_COMPACTIONS");
        }
    }

    #[test]
    fn test_auto_compact_env_false_strings() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        for val in ["0", "false", "no", "off"] {
            unsafe { std::env::set_var("CLAUSURA_AUTO_COMPACT", val) };
            let config = Config::load(
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                std::env::current_dir().unwrap(),
                "output.sarif".into(),
                false,
                LogFormat::Json,
            )
            .unwrap();
            assert!(!config.task.auto_compact, "{val} must parse as false");
        }
        unsafe { std::env::remove_var("CLAUSURA_AUTO_COMPACT") };
    }

    #[test]
    fn test_max_total_tokens_from_yaml() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  max_total_tokens: 200000
  timeout_secs: 60
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.max_total_tokens, Some(200000));
    }

    #[test]
    fn test_max_total_tokens_defaults_to_none() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.max_total_tokens, None);
    }

    #[test]
    fn test_max_total_tokens_env_overrides_yaml() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        unsafe { std::env::set_var("CLAUSURA_MAX_TOTAL_TOKENS", "500000") };
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  max_total_tokens: 200000
  timeout_secs: 60
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.max_total_tokens, Some(500000));
        unsafe { std::env::remove_var("CLAUSURA_MAX_TOTAL_TOKENS") };
    }

    #[test]
    fn test_zero_max_total_tokens_is_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  max_total_tokens: 0
  timeout_secs: 60
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let result = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_defaults_when_no_config_file() {
        let config = Config::load(
            None,
            Some("gpt-4o"),
            Some("openai"),
            Some("sk-test"),
            Some(16000),
            Some(120),
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.name, "default");
        assert_eq!(config.task.model, "gpt-4o");
        assert_eq!(config.task.vendor, VendorConfig::openai());
        assert_eq!(config.task.token_budget, 16000);
        assert_eq!(config.task.timeout_secs, 120);
        assert_eq!(config.task.prompt_template, "{{task_description}}");
        assert!(config.task.tool_allowlist.is_empty());
    }

    #[test]
    fn test_gate_rule_parsing() {
        let yaml = r#"
version: "1"
task:
  name: gate-test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
  gating:
    - rule: no-errors
      description: Block on any error
      min_severity: error
      max_findings: 0
      action: fail
    - rule: warn-on-warnings
      description: Warn on warnings
      min_severity: warning
      max_findings: 5
      action: warn
    - rule: ignore-hints
      description: Ignore hints
      min_severity: hint
      max_findings: 100
      action: ignore
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.gating_rules.len(), 3);

        assert_eq!(config.task.gating_rules[0].rule_id, "no-errors");
        assert_eq!(config.task.gating_rules[0].min_severity, Severity::Error);
        assert_eq!(config.task.gating_rules[0].max_findings, 0);
        assert_eq!(config.task.gating_rules[0].action, GateAction::Fail);

        assert_eq!(config.task.gating_rules[1].rule_id, "warn-on-warnings");
        assert_eq!(config.task.gating_rules[1].min_severity, Severity::Warning);
        assert_eq!(config.task.gating_rules[1].max_findings, 5);
        assert_eq!(config.task.gating_rules[1].action, GateAction::Warn);

        assert_eq!(config.task.gating_rules[2].rule_id, "ignore-hints");
        assert_eq!(config.task.gating_rules[2].min_severity, Severity::Hint);
        assert_eq!(config.task.gating_rules[2].max_findings, 100);
        assert_eq!(config.task.gating_rules[2].action, GateAction::Ignore);
    }

    #[test]
    fn test_config_path_is_recorded() {
        let yaml = r#"
version: "1"
task:
  name: path-test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.config_path, Some(file.path().to_path_buf()));
    }

    #[test]
    fn test_shell_config_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.shell_timeout_secs, 120);
        assert!(config.task.shell_env_passthrough.is_empty());
    }

    #[test]
    fn test_shell_config_from_yaml() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  shell_timeout_secs: 30
  shell_env_passthrough:
    - HTTP_PROXY
    - CARGO_NET_GIT_FETCH_WITH_CLI
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.shell_timeout_secs, 30);
        assert_eq!(
            config.task.shell_env_passthrough,
            vec![
                "HTTP_PROXY".to_string(),
                "CARGO_NET_GIT_FETCH_WITH_CLI".to_string()
            ]
        );
    }

    #[test]
    fn test_shell_timeout_env_overrides_cli_and_yaml() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        unsafe { std::env::set_var("CLAUSURA_SHELL_TIMEOUT", "45") };
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  shell_timeout_secs: 30
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            Some(90), // CLI flag — env should override this
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.shell_timeout_secs, 45); // env wins over CLI
        unsafe { std::env::remove_var("CLAUSURA_SHELL_TIMEOUT") };
    }

    #[test]
    fn test_shell_timeout_cli_overrides_yaml() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  shell_timeout_secs: 30
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            Some(90),
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.shell_timeout_secs, 90); // CLI wins over YAML
    }

    // -- skill_prompts tests ----------------------------------------------

    #[test]
    fn test_skill_prompts_empty_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        // No skill_prompts in YAML → prompt_template unchanged.
        assert_eq!(config.task.prompt_template, "{{task_description}}");
    }

    #[test]
    fn test_skill_prompts_local_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();

        let tmp = tempfile::TempDir::new().unwrap();
        let skill_path = tmp.path().join("my-check.md");
        std::fs::write(&skill_path, "# Check for bugs").unwrap();

        let yaml = format!(
            r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
  skill_prompts:
    - '{}'
"#,
            skill_path.to_string_lossy()
        );
        let file = write_yaml(&yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            tmp.path().to_path_buf(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert!(config.task.prompt_template.contains("[Skill:"));
        assert!(config.task.prompt_template.contains("# Check for bugs"));
        assert!(!config.task.prompt_template.contains("{{task_description}}"));
    }

    #[test]
    fn test_skill_prompts_named_skill() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();

        let tmp = tempfile::TempDir::new().unwrap();
        let skill_dir = tmp
            .path()
            .join(".clausura")
            .join("skills")
            .join("team")
            .join("vue-check");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: vue-check\n---\n# Vue best practices",
        )
        .unwrap();

        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
  skill_prompts:
    - team/vue-check
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            tmp.path().to_path_buf(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert!(config
            .task
            .prompt_template
            .contains("[Skill: team/vue-check]"));
        assert!(config.task.prompt_template.contains("# Vue best practices"));
    }

    #[test]
    fn test_skill_prompts_with_user_template() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();

        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("check.md"), "Skill body").unwrap();

        let yaml = format!(
            r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
  skill_prompts:
    - '{}'
  prompt_template: "User extra check."
"#,
            tmp.path().join("check.md").to_string_lossy()
        );
        let file = write_yaml(&yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            tmp.path().to_path_buf(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert!(config.task.prompt_template.contains("[Skill:"));
        assert!(config.task.prompt_template.contains("Skill body"));
        assert!(config.task.prompt_template.contains("User extra check."));
    }

    #[test]
    fn test_skill_prompts_not_found_is_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        clean_env_vars();

        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
  skill_prompts:
    - nonexistent/skill
"#;
        let file = write_yaml(yaml);
        let result = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        );
        assert!(result.is_err());
    }

    // ── MCP config tests ────────────────────────────────────────────────────

    #[test]
    fn test_mcp_config_parsing() {
        let yaml = r#"
version: "1"
task:
  name: mcp-test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
  mcp_servers:
    - name: github
      command: npx
      args: ["-y", "@anthropic/mcp-server-github"]
      env:
        GITHUB_TOKEN: "${GITHUB_TOKEN}"
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.mcp_servers.len(), 1);
        assert_eq!(config.task.mcp_servers[0].name, "github");
        assert_eq!(config.task.mcp_servers[0].command, "npx");
        assert_eq!(
            config.task.mcp_servers[0].args,
            vec!["-y", "@anthropic/mcp-server-github"]
        );
        assert_eq!(
            config.task.mcp_servers[0].env.get("GITHUB_TOKEN"),
            Some(&"${GITHUB_TOKEN}".to_string())
        );
    }

    #[test]
    fn test_mcp_empty_name_is_error() {
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
  mcp_servers:
    - name: ""
      command: npx
"#;
        let file = write_yaml(yaml);
        let result = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        );
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("name must be non-empty"), "got: {err_msg}");
    }

    #[test]
    fn test_mcp_duplicate_name_is_error() {
        let yaml = r#"
version: "1"
task:
  name: test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
  mcp_servers:
    - name: dup
      command: npx
    - name: dup
      command: node
"#;
        let file = write_yaml(yaml);
        let result = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        );
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("Duplicate"), "got: {err_msg}");
    }

    #[test]
    fn test_preflight_config_parsing() {
        let yaml = r#"
version: "1"
task:
  name: preflight-test
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
  mcp_servers:
    - name: diag
      command: agent-lsp
  preflight:
    - mcp_server: diag
      tool: get_diagnostics
      args:
        path: "."
      rule_id_prefix: "lsp-"
      severity_field: "severity"
      message_field: "message"
      file_field: "file"
      line_field: "line"
      default_severity: "warning"
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.preflight.len(), 1);
        let check = &config.task.preflight[0];
        assert_eq!(check.mcp_server, "diag");
        assert_eq!(check.tool, "get_diagnostics");
        assert_eq!(check.rule_id_prefix, "lsp-");
        assert_eq!(check.severity_field, "severity");
        assert_eq!(check.message_field, "message");
        assert_eq!(check.file_field, "file");
        assert_eq!(check.line_field, "line");
        assert_eq!(check.default_severity, "warning");
    }

    #[test]
    fn test_preflight_config_defaults() {
        let yaml = r#"
version: "1"
task:
  name: preflight-min
  model: gpt-4o
  vendor: openai
  token_budget: 8000
  timeout_secs: 60
  ambiguity_policy: fail_closed
  mcp_servers:
    - name: diag
      command: agent-lsp
  preflight:
    - mcp_server: diag
      tool: get_diagnostics
"#;
        let file = write_yaml(yaml);
        let config = Config::load(
            Some(file.path()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.task.preflight.len(), 1);
        let check = &config.task.preflight[0];
        assert_eq!(check.mcp_server, "diag");
        assert_eq!(check.tool, "get_diagnostics");
        // All field mappings should use defaults
        assert_eq!(check.rule_id_prefix, "preflight-");
        assert_eq!(check.severity_field, "severity");
        assert_eq!(check.message_field, "message");
        assert_eq!(check.file_field, "file");
        assert_eq!(check.default_severity, "warning");
    }
}
