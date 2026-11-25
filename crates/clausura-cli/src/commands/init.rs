use colored::*;
use std::fs;
use std::path::{Path, PathBuf};

/// Scaffold `.clausura.yaml`, a starter skill, and optionally a GitHub Actions workflow.
#[derive(clap::Args, Debug)]
pub struct InitArgs {
    /// Target workspace directory
    #[arg(long)]
    pub workspace: Option<PathBuf>,

    /// Also generate `.github/workflows/clausura.yml`
    #[arg(long)]
    pub with_workflow: bool,

    /// Overwrite existing files if present
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectStack {
    Rust,
    TypeScript,
    Python,
    Go,
    Generic,
}

impl ProjectStack {
    fn detect(workspace: &Path) -> Self {
        if workspace.join("Cargo.toml").exists() {
            Self::Rust
        } else if workspace.join("tsconfig.json").exists()
            || workspace.join("package.json").exists()
        {
            Self::TypeScript
        } else if workspace.join("pyproject.toml").exists()
            || workspace.join("requirements.txt").exists()
            || workspace.join("setup.py").exists()
        {
            Self::Python
        } else if workspace.join("go.mod").exists() {
            Self::Go
        } else {
            Self::Generic
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::TypeScript => "TypeScript/JavaScript",
            Self::Python => "Python",
            Self::Go => "Go",
            Self::Generic => "Generic",
        }
    }

    fn skill_name(self) -> &'static str {
        match self {
            Self::Rust => "rust-review",
            Self::TypeScript => "ts-review",
            Self::Python => "python-review",
            Self::Go => "go-review",
            Self::Generic => "code-review",
        }
    }

    fn config_yaml(self) -> String {
        let skill = self.skill_name();
        let extra_rule = match self {
            Self::Rust => {
                "    - rule: panic-unwrap\n      description: Avoid panicking unwrap/expect in production code\n      min_severity: warning\n      max_findings: 3\n      action: warn\n"
            }
            Self::TypeScript => {
                "    - rule: xss\n      description: Block cross-site scripting sinks\n      min_severity: error\n      max_findings: 0\n      action: fail\n"
            }
            Self::Python => {
                "    - rule: unsafe-deserialization\n      description: Block unsafe pickle/yaml deserialization\n      min_severity: error\n      max_findings: 0\n      action: fail\n"
            }
            Self::Go => {
                "    - rule: unchecked-error\n      description: Warn on ignored error return values\n      min_severity: warning\n      max_findings: 3\n      action: warn\n"
            }
            Self::Generic => "",
        };

        format!(
            r#"version: "1"
task:
  name: {skill}
  model: gpt-5.4-mini
  vendor: openai
  skill_prompts:
    - {skill}
  token_budget: 32000
  timeout_secs: 300
  max_iterations: 10
  temperature: 0.0
  ambiguity_policy: fail_closed
  on_incomplete: fail
  gating:
    - rule: sql-injection
      description: Block SQL injection vulnerabilities
      min_severity: error
      max_findings: 0
      action: fail
    - rule: hardcoded-secret
      description: Block hardcoded API keys, tokens, and credentials
      min_severity: error
      max_findings: 0
      action: fail
{extra_rule}"#
        )
    }

    fn skill_markdown(self) -> String {
        let skill = self.skill_name();
        let stack_checks = match self {
            Self::Rust => {
                "## Panicking unwraps\n- Flag `.unwrap()` / `.expect()` on untrusted input or fallible I/O outside tests.\n- rule_id: `panic-unwrap`, severity: `warning`\n"
            }
            Self::TypeScript => {
                "## XSS sinks\n- Flag unsanitized `innerHTML`, `dangerouslySetInnerHTML`, or `document.write`.\n- rule_id: `xss`, severity: `error`\n"
            }
            Self::Python => {
                "## Unsafe deserialization\n- Flag `pickle.loads` / `yaml.load` (without SafeLoader) on untrusted data.\n- rule_id: `unsafe-deserialization`, severity: `error`\n"
            }
            Self::Go => {
                "## Ignored errors\n- Flag ignored error returns from I/O, DB, or auth calls.\n- rule_id: `unchecked-error`, severity: `warning`\n"
            }
            Self::Generic => "",
        };

        format!(
            r#"---
name: {skill}
description: Security and reliability review checklist for {label} projects
---

# {label} Review Checklist

Inspect the git diff carefully. Use `report_finding` as soon as you verify an issue, and return all findings in the final JSON response.

## SQL injection
- Flag string concatenation or interpolation used to construct SQL queries.
- rule_id: `sql-injection`, severity: `error`

## Hardcoded secrets
- Flag live API keys, tokens, private keys, or database passwords committed in source code (ignore obvious placeholders like `REPLACE_ME` or `example-*`).
- rule_id: `hardcoded-secret`, severity: `error`

{stack_checks}"#,
            label = self.label()
        )
    }
}

const WORKFLOW_YML: &str = r#"name: Clausura Review
on: [pull_request]

permissions:
  contents: read
  pull-requests: write

jobs:
  review:
    if: github.event.pull_request.head.repo.full_name == github.repository && github.actor != 'dependabot[bot]'
    runs-on: ubuntu-latest
    timeout-minutes: 15
    steps:
      - uses: actions/checkout@v7
        with:
          ref: ${{ github.event.pull_request.head.sha }}
          fetch-depth: 0
          persist-credentials: false

      - name: Run Clausura review
        uses: liuyanghejerry/Clausura@v1
        with:
          config: .clausura.yaml
          api_key: ${{ secrets.LLM_API_KEY }}
          base: ${{ github.event.pull_request.base.sha }}
          post_pr_comment: 'true'
          github_token: ${{ secrets.GITHUB_TOKEN }}
"#;

fn write_checked(path: &Path, content: &str, force: bool) -> Result<(), String> {
    if path.exists() && !force {
        return Err(format!(
            "{} already exists (re-run with --force to overwrite)",
            path.display()
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
    }
    fs::write(path, content).map_err(|e| format!("Failed to write {}: {e}", path.display()))
}

pub fn execute(args: InitArgs) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = args
        .workspace
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let stack = ProjectStack::detect(&workspace);

    let config_path = workspace.join(".clausura.yaml");
    let skill_path = workspace
        .join(".clausura")
        .join("skills")
        .join(stack.skill_name())
        .join("SKILL.md");

    write_checked(&config_path, &stack.config_yaml(), args.force)?;
    write_checked(&skill_path, &stack.skill_markdown(), args.force)?;

    let mut created = vec![config_path, skill_path];
    if args.with_workflow {
        let wf_path = workspace
            .join(".github")
            .join("workflows")
            .join("clausura.yml");
        write_checked(&wf_path, WORKFLOW_YML, args.force)?;
        created.push(wf_path);
    }

    eprintln!(
        "{} Initialized Clausura for {} project:",
        "OK:".green().bold(),
        stack.label()
    );
    for p in &created {
        eprintln!("  - {}", p.display());
    }
    eprintln!(
        "{} Set CLAUSURA_API_KEY and run `clausura run --dry-run` to validate.",
        "Next:".cyan().bold()
    );
    Ok(())
}
