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
    AmbiguityPolicy, ConfigError, GateAction, GateRule, Severity, TaskContract, VendorConfig,
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
    model: String,
    #[serde(default)]
    vendor: String,
    #[serde(default = "default_prompt")]
    prompt_template: String,
    #[serde(default)]
    tool_allowlist: Vec<String>,
    #[serde(default = "default_token_budget")]
    token_budget: u64,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
    #[serde(default = "default_ambiguity")]
    ambiguity_policy: String,
    #[serde(default)]
    gating: Vec<YamlGateRule>,
}

#[derive(Debug, Deserialize)]
struct YamlGateRule {
    rule: String,
    description: String,
    min_severity: String,
    max_findings: u32,
    action: String,
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

fn default_ambiguity() -> String {
    "fail_closed".to_string()
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

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn validate_yaml(yaml: &YamlConfig) -> Result<(), ConfigError> {
    if yaml.version.is_empty() {
        return Err(ConfigError::ValidationError("version is required".into()));
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
    if yaml.task.timeout_secs == 0 {
        return Err(ConfigError::ValidationError(
            "task.timeout_secs must be > 0".into(),
        ));
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
                    model: String::new(),
                    vendor: String::new(),
                    prompt_template: default_prompt(),
                    tool_allowlist: vec![],
                    token_budget: default_token_budget(),
                    timeout_secs: default_timeout(),
                    ambiguity_policy: default_ambiguity(),
                    gating: vec![],
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

        let timeout = std::env::var("CLAUSURA_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(cli_timeout)
            .unwrap_or(yaml_task.timeout_secs);

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

        Ok(Config {
            config_path,
            task: TaskContract {
                id: format!("task-{}", yaml_task.name.replace(' ', "-")),
                name: yaml_task.name,
                description: yaml_task.description,
                model,
                vendor,
                prompt_template: yaml_task.prompt_template,
                tool_allowlist: yaml_task.tool_allowlist,
                token_budget,
                timeout_secs: timeout,
                ambiguity_policy,
                gating_rules,
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
            Some(120),
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
            std::env::remove_var("CLAUSURA_TIMEOUT");
            std::env::remove_var("CLAUSURA_AMBIGUITY_POLICY");
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
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_env_api_key_takes_precedence_over_cli() {
        unsafe {
            std::env::remove_var("CLAUSURA_API_KEY");
            std::env::set_var("CLAUSURA_API_KEY", "sk-env-key");
        };
        let config = Config::load(
            None,
            None,
            None,
            Some("sk-cli-key"),
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
            std::env::current_dir().unwrap(),
            "output.sarif".into(),
            false,
            LogFormat::Json,
        )
        .unwrap();
        assert_eq!(config.config_path, Some(file.path().to_path_buf()));
    }
}
