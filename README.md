# Clausura

CI-native agent CLI for deterministic pipeline gating.

[![Build](https://img.shields.io/github/actions/workflow/status/liuyanghejerry/Clausura/main.yml?branch=main)](https://github.com/liuyanghejerry/Clausura/actions)
[![Crates.io](https://img.shields.io/crates/v/clausura-cli)](https://crates.io/crates/clausura-cli)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

## What is Clausura?

Clausura runs bounded LLM agent tasks against your codebase in CI/CD pipelines. It extracts structured findings, evaluates them against **deterministic gating rules**, and exits with a clear pass/fail signal — no mid-process questions, no human in the loop.

**The LLM finds issues. The rule engine decides if they matter. Your pipeline gets a binary answer.**

## Design Philosophy

Clausura is built on three principles:

1. **Closed-loop execution.** Every run is bounded by time, token budget, and iteration count. No interactive prompts. No indefinite loops. The agent either finishes cleanly or fails closed.

2. **Deterministic gating.** Findings are evaluated by a pure counting engine — match by rule ID, filter by severity, compare against thresholds. No LLM in the decision path. No heuristics. Result is reproducible.

3. **CI-native.** Auto-detects GitHub Actions, GitLab CI, Jenkins. Outputs SARIF v2.1.0. Exit codes 0/1/2/3 map directly to pipeline status. Designed to run unattended.

→ [Read more about the philosophy and architecture](docs/guide/overview.md)

## Quick Start

### 1. Install

```bash
# macOS / Linux / WSL
curl -fsSL https://raw.githubusercontent.com/liuyanghejerry/Clausura/main/install.sh | bash

# Or via Cargo
cargo install clausura-cli
```

→ [All installation options](docs/guide/installation.md)

### 2. Create a config

Drop `.clausura.yaml` in your project root:

```yaml
version: "1"
task:
  name: code-review
  model: gpt-4o
  vendor: openai
  prompt_template: |
    Review the git diff for:
    1. SQL injection — any string concatenation in SQL queries
    2. Hardcoded credentials or secrets
    3. Missing input validation

    For each finding use:
    - rule_id: "sql-injection" / "hardcoded-secret" / "missing-validation"
    - severity: "error" or "warning"
  token_budget: 16000
  gating:
    - rule: sql-injection
      min_severity: error
      max_findings: 0
      action: fail
    - rule: hardcoded-secret
      min_severity: error
      max_findings: 0
      action: fail
```

### 3. Run

```bash
export CLAUSURA_API_KEY=sk-...
clausura run
```

That's it. Clausura calls the LLM, the LLM reviews your diff, the rule engine evaluates the findings, and you get a pass/fail exit code + SARIF report.

## Documentation

| Guide | Content |
|-------|---------|
| [Overview & Philosophy](docs/guide/overview.md) | How Clausura works, core concepts, design rationale |
| [Installation](docs/guide/installation.md) | All install methods: script, Cargo, Docker, from source |
| [Configuration Reference](docs/guide/configuration.md) | Every YAML field, CLI flag, and environment variable |
| [LLM Providers](docs/guide/llm-providers.md) | Setting up OpenAI, Anthropic, DeepSeek, Ollama, custom endpoints |
| [Gating Rules](docs/guide/gating.md) | How rules work, severity levels, designing effective gates |
| [Skills](docs/guide/skills.md) | Reusing community review skills, composing multiple checks |
| [Common Scenarios](docs/guide/scenarios.md) | Recipes: security review, i18n check, architecture compliance, Vue/React best practices |
| [CI Integration](docs/guide/ci-integration.md) | GitHub Actions, GitLab CI, Jenkins, generic CI setup |
| [Troubleshooting](docs/guide/troubleshooting.md) | Exit codes, common errors, debugging tips |

## Use Cases

- **Code review gating** — flag SQL injection, hardcoded secrets, missing validation before merge
- **Cross-repo consistency** — enforce naming conventions, directory structure, dependency policies
- **i18n completeness** — scan for hardcoded strings, missing translation keys, locale drift
- **Architecture compliance** — verify layering, import direction, forbidden patterns
- **Smart gating** — fail only when findings exceed configurable thresholds

→ [See all scenarios with complete configs](docs/guide/scenarios.md)

## Exit Codes

| Code | Meaning | When |
|------|---------|------|
| 0 | Pass | All gating rules satisfied |
| 1 | Fail | A rule with `action: fail` was violated |
| 2 | Error | Runtime error (timeout, provider failure, incomplete run) |
| 3 | Config | Invalid configuration |

## Supported LLM Providers

OpenAI · Anthropic (Claude) · DeepSeek · Groq · Ollama · Custom (any OpenAI-compatible endpoint)

→ [Full provider setup guide](docs/guide/llm-providers.md)

## CI Integration

```yaml
# GitHub Actions
- uses: liuyanghejerry/Clausura@v1
  with:
    config: .clausura.yaml
    api_key: ${{ secrets.LLM_API_KEY }}
```

Also supports GitLab CI, Jenkins, and generic CI via environment variables.

→ [All CI platforms](docs/guide/ci-integration.md)

## Development

```bash
# Build
cargo build --release --package clausura-cli

# Run tests
cargo test --workspace

# Format (pre-commit hook enforces this)
cargo fmt --all

# Lint
cargo clippy --workspace -- -D warnings
```

See [RELEASE.md](RELEASE.md) for release procedures.

## License

MIT. See [LICENSE](LICENSE).
