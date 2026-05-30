# Clausura

CI-native agent CLI tool for deterministic pipeline gating.

[![Build](https://img.shields.io/github/actions/workflow/status/liuyanghejerry/Clausura/main.yml?branch=main)](https://github.com/liuyanghejerry/Clausura/actions)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

## Overview

Clausura is a platform-agnostic agent CLI tool built for CI/CD pipelines. It runs bounded LLM agent tasks against your codebase, extracts structured findings, evaluates them against deterministic gating rules, and exits with a clear pass/fail signal. No mid-process questions, no human in the loop.

**Key philosophy: closed-loop execution with deterministic gating.** The LLM finds issues. The rule engine decides if they matter. Your pipeline gets a binary answer.

**Use cases**

- **Code review gating** -- flag violations in pull requests before merge
- **Cross-repo consistency checks** -- enforce conventions across multiple repositories
- **Smart gating** -- fail the pipeline only when findings exceed configured thresholds
- **i18n extraction and translation** -- scan source files and validate locale coverage
- **Architecture compliance** -- verify code structure against project conventions

## Installation

### Shell script (Linux, macOS, WSL)

```bash
curl -fsSL https://raw.githubusercontent.com/liuyanghejerry/Clausura/main/install.sh | bash
```

The script detects your OS and architecture, downloads the latest release from GitHub, and installs it to `/usr/local/bin` or `~/.local/bin`.

### Cargo

```bash
cargo install clausura-cli
```

### Docker

```bash
docker pull ghcr.io/liuyanghejerry/clausura
docker run --rm -v $(pwd):/workspace ghcr.io/liuyanghejerry/clausura run
```

### From source

```bash
git clone https://github.com/liuyanghejerry/Clausura.git
cd clausura
cargo build --release --package clausura-cli
# binary at target/release/clausura
```

### Verify

```bash
clausura --version
# clausura 1.0.0 (commit: abc1234, built: 2026-05-23)
```

## Supported LLM Providers

Clausura supports three vendor categories out of the box:

### 1. OpenAI-compatible

Any LLM that exposes an OpenAI-compatible `/chat/completions` endpoint:

| Shorthand | Base URL | Auth Header |
|-----------|----------|-------------|
| `openai` | `https://api.openai.com/v1` | `Authorization: Bearer` |
| `deepseek` | `https://api.deepseek.com/v1` | `Authorization: Bearer` |
| `groq` | `https://api.groq.com/openai/v1` | `Authorization: Bearer` |
| `ollama` | `http://localhost:11434/v1` | `Authorization: Bearer` |
| (custom) | User-defined | `Authorization: Bearer` |

```yaml
vendor: deepseek   # shorthand
# or full config:
vendor:
  type: openai_compatible
  base_url: "https://api.mistral.ai/v1"
```

### 2. Anthropic-compatible

Claude models via Anthropic's native Messages API:

| Shorthand | Base URL | Auth Header |
|-----------|----------|-------------|
| `anthropic` | `https://api.anthropic.com` | `x-api-key` |
| `claude` | `https://api.anthropic.com` | `x-api-key` |

```yaml
vendor: anthropic
model: claude-sonnet-4-20250514
```

Uses Anthropic's native Messages API (`/v1/messages`) with `x-api-key` auth and `anthropic-version: 2023-06-01`.

### 3. Custom

For enterprise-internal LLMs with non-standard authentication:

```yaml
vendor:
  type: custom
  base_url: "https://llm.internal.company.com/v1"
  auth_header: "X-API-Key"
  api_key_env: "INTERNAL_LLM_KEY"
```

Uses the OpenAI-compatible API format (`/chat/completions`) with configurable base URL and auth header. The `auth_header` defaults to `Authorization`; the `api_key_env` defaults to `CLAUSURA_API_KEY`.

## Quick Start

### 1. Create a configuration file

Create `.clausura.yaml` (or `.clausura.yml`) in your project root:

```yaml
version: "1"
task:
  name: code-review
  model: gpt-4o
  vendor: openai
  prompt_template: "Review the git diff and return findings as JSON."
  token_budget: 16000
  timeout_secs: 120
  ambiguity_policy: fail_closed
  gating:
    - rule: no-critical
      description: Block on any critical error
      min_severity: error
      max_findings: 0
      action: fail
    - rule: warn-on-warnings
      description: Warn on excessive warnings
      min_severity: warning
      max_findings: 10
      action: warn
```

### 2. Set your API key

```bash
export CLAUSURA_API_KEY=sk-...
```

The API key is never read from the YAML config file. It must come from this environment variable or the `--api-key` CLI flag.

### 3. Run the task

```bash
clausura run
```

### 4. Validate config without running

```bash
clausura run --validate-config
clausura run --dry-run  # show the execution plan
```

### Exit codes

| Code | Meaning       | Description                              |
|------|---------------|------------------------------------------|
| 0    | Pass          | All gating rules satisfied               |
| 1    | Fail          | A rule with `action: fail` was violated  |
| 2    | Error         | Runtime error (provider, timeout, etc.)  |
| 3    | Config error  | Invalid configuration                    |

## Configuration Reference

### YAML schema

All fields for `.clausura.yaml`:

```yaml
version: "1"                         # Required. Schema version.
task:
  name: my-task                      # Required. Task name.

  # LLM provider
  model: gpt-4o                      # Required (or set CLAUSURA_MODEL).
  vendor: openai                     # Shorthand (backward compatible).
  # Or with full config:
  vendor:
    type: openai_compatible          # openai_compatible | anthropic_compatible | custom
    base_url: "https://api.deepseek.com/v1"  # Optional. Override API endpoint.
    auth_header: "X-API-Key"         # Optional. For custom auth (default: Authorization).
    api_key_env: "MY_SECRET_KEY"     # Optional. Env var for API key (default: CLAUSURA_API_KEY).

  # Prompt
  prompt_template: "{{task_description}}"  # Default. The agent's system prompt.

  # Limits
  token_budget: 32000                # Default. Max total tokens across all LLM calls.
  timeout_secs: 300                  # Default. Max wall-clock time in seconds.

  # Safety
  ambiguity_policy: fail_closed      # "fail_closed" or "proceed_with_caution".

  # Optional tool allowlist
  tool_allowlist:                    # Restrict shell commands to these binaries.
    - git

  # Gating rules
  gating:                            # Optional. Evaluated in order.
    - rule: no-critical
      description: "No critical errors"
      min_severity: error
      max_findings: 0
      action: fail
```

**Gating rule fields:**

| Field         | Type   | Description                                      |
|---------------|--------|--------------------------------------------------|
| `rule`        | string | Rule ID. Findings with matching `rule_id` count. |
| `description` | string | Human-readable description.                      |
| `min_severity`| string | Minimum severity: `hint`, `info`, `warning`, `error`. |
| `max_findings`| number | Maximum allowed findings at or above this severity. |
| `action`      | string | `fail` (exit 1), `warn` (log only), `ignore` (skip). |

### Environment variables

| Variable                | Overrides            |
|-------------------------|----------------------|
| `CLAUSURA_API_KEY`      | API key (required)   |
| `CLAUSURA_MODEL`        | `task.model`         |
| `CLAUSURA_VENDOR`       | `task.vendor`        |
| `CLAUSURA_AMBIGUITY_POLICY` | `task.ambiguity_policy` |
| `CLAUSURA_TOKEN_BUDGET` | `task.token_budget`  |
| `CLAUSURA_TIMEOUT`      | `task.timeout_secs`  |

Config loading priority: YAML file < CLI flags < environment variables.

### CLI flags

```
clausura run [OPTIONS]

  -c, --config <PATH>       Config file path          [default: .clausura.yaml]
      --model <MODEL>       Override LLM model
      --vendor <VENDOR>     Override LLM vendor
      --api-key <KEY>       API key
      --token-budget <N>    Token budget override
      --timeout <SECS>      Timeout override
      --workspace <PATH>    Workspace root            [default: cwd]
      --output <PATH>       SARIF output path          [default: clausura-output.sarif]
      --resume              Resume from last checkpoint
      --log-format <FMT>    Log format (json|pretty)   [default: json]
      --dry-run             Validate config and print the execution plan
      --validate-config     Validate config and exit
```

Checkpoint management:

```
clausura snapshot list [--thread <ID>] [--limit <N>]    List checkpoints (default: all threads, 10 max)
clausura snapshot show [--thread <ID>]                  Show the latest checkpoint
clausura snapshot show --id <UUID> [--thread <ID>]     Show a specific checkpoint
clausura snapshot delete --thread <ID>                  Delete all checkpoints for a thread
```

## CI Integration

Clausura auto-detects your CI environment using well-known environment variables. It gathers repo, PR number, commit SHA, and branch context for template rendering and SARIF output.

### Detection order

1. `GITHUB_ACTIONS` -> GitHub Actions
2. `GITLAB_CI` -> GitLab CI
3. `JENKINS_URL` -> Jenkins
4. `CI=true` or `CI=1` -> Generic CI
5. Otherwise -> Local

### GitHub Actions

Use the composite action directly:

```yaml
- uses: liuyanghejerry/Clausura@v1
  with:
    config: .clausura.yaml
    api_key: ${{ secrets.LLM_API_KEY }}
    model: gpt-4o
    vendor: openai
    token_budget: 32000
    timeout: 300
```

Or run via the binary:

```yaml
- name: Run Clausura
  run: clausura run
  env:
    CLAUSURA_API_KEY: ${{ secrets.LLM_API_KEY }}
```

### GitLab CI

```yaml
clausura-review:
  image: ghcr.io/liuyanghejerry/clausura:latest
  script:
    - clausura run
  variables:
    CLAUSURA_API_KEY: $LLM_API_KEY
    CLAUSURA_MODEL: "gpt-4o"
```

### Jenkins

```groovy
stage('Code Review') {
    environment {
        CLAUSURA_API_KEY = credentials('llm-api-key')
    }
    steps {
        sh 'clausura run --model gpt-4o'
    }
}
```

### Generic CI

Set `CI=true` and the relevant `CI_*` environment variables:

```bash
export CI=true
export CLAUSURA_API_KEY=sk-...
clausura run
```

Custom context variables for Generic CI: `CI_REPO`, `CI_PR_NUMBER`, `CI_COMMIT_SHA`, `CI_BRANCH`.

## Architecture

### Agent loop (reason -> act -> observe)

Clausura runs a bounded agent loop of up to 10 iterations. Each iteration:

1. Sends the conversation (system prompt + accumulated messages) to the LLM
2. If the LLM calls a tool, executes it and feeds the result back
3. If the LLM responds with structured findings and signals `stop`, the loop ends
4. The loop also ends on token budget exhaustion, timeout, or content filter

The system prompt is built from `prompt_template` plus available tool definitions. The LLM is instructed to respond in JSON with a `findings` array.

### Context truncation and archiving

When the conversation exceeds the configured `token_budget`, Clausura automatically truncates older messages to stay within limits. Dropped messages are not silently discarded — they are archived to `.clausura/archives/context-dump-{task_id}-{seq}.log` inside the workspace as JSON lines. A hint message is injected into the conversation telling the LLM where to find the archived context, so it can retrieve earlier findings via the `read_file` tool if needed.

On successful completion (exit code 0), archive files are automatically cleaned up. On failure (exit code 1-3), they are preserved for debugging and audit.

### Deterministic rule engine

Findings from the agent are evaluated by the rule engine using pure counting:

- Match findings to rules by `rule_id`
- Filter by severity threshold
- Count violations above `max_findings`
- Apply action: `fail` exits 1, `warn` logs, `ignore` skips

No LLM calls, no heuristics. Just deterministic logic your pipeline can trust.

### LLM provider abstraction

Three vendor categories: OpenAI-compatible (works with OpenAI, DeepSeek, Groq, Ollama, vLLM, Mistral), Anthropic-compatible (native Claude Messages API), and Custom (configurable enterprise endpoints). Factory function `create_provider()` dispatches on vendor type.

### Tool sandboxing

Five built-in tools:

| Tool          | Description                                      | Restrictions                           |
|---------------|--------------------------------------------------|----------------------------------------|
| `read_file`   | Read a file relative to workspace root, with optional `offset`/`limit` for line-range reading | Blocks absolute paths, `..` traversal, symlink escapes |
| `list_files`  | List directory contents, with recursive depth, glob filtering, and optional file sizes | Sandboxed to workspace; skips `.clausura/` |
| `grep`        | Search text patterns across files with literal or regex mode, extension filtering, and binary-skip | Auto-excludes `.git`, `target`, `.clausura`, `node_modules` |
| `git_diff`    | Run `git diff` with optional base ref or staged  | Operates inside workspace only         |
| `shell_exec`  | Execute allowed shell commands                   | Restricted to `tool_allowlist` entries |

The `shell_exec` tool is locked by default (empty allowlist = no commands). Explicitly list allowed binaries to enable it. All file access is sandboxed to the workspace root.

### Memory snapshots (SQLite checkpoints)

On every run, the agent's message history is serialized (MessagePack) and saved to `~/.clausura/checkpoints.db`. You can resume a truncated or interrupted run with `--resume`. Snapshots include a thread ID, version number, and truncation flag.

Use `clausura snapshot list` and `clausura snapshot show` to inspect saved state.

### SARIF output

Findings are written to `clausura-output.sarif` (or the path from `--output`) in SARIF v2.1.0 format. This integrates with GitHub Advanced Security, CodeQL, and other SARIF-compatible tools.

## Development

### Build

```bash
# Debug build
cargo build

# Release build (recommended for production)
cargo build --release --package clausura-cli
```

### Run tests

```bash
cargo test --workspace
```

### Project structure

```
clausura/
  Cargo.toml                    # Workspace root
  crates/
    clausura-core/              # Core library
      src/
        lib.rs                  # Crate root (module re-exports)
        agent.rs                # Agent loop (reason -> act -> observe)
        build_info.rs           # Version and commit metadata
        checkpoint.rs           # SQLite checkpoint store
        ci.rs                   # CI environment detection
        config.rs               # Layered config loader (YAML + CLI + env)
        context.rs              # Token budget tracking, context truncation, and archiving
        executor.rs             # Task lifecycle orchestrator
        logging.rs              # Structured logging (JSON or pretty)
        provider.rs             # LLM provider (OpenAI/Anthropic/Custom + factory)
        rules.rs                # Deterministic rule engine for gating
        sarif.rs                # SARIF v2.1.0 output formatter
        snapshot.rs             # Snapshot manager (save/restore)
        tools.rs                # Tool sandbox (read_file, git_diff, shell_exec, list_files, grep)
        types.rs                # Core type definitions
    clausura-cli/               # CLI binary
      src/
        main.rs                 # CLI entry point (clap)
        commands/
          mod.rs                # Commands module
          run.rs                # clausura run command
          snapshot.rs           # clausura snapshot command
  action.yml                    # GitHub Action definition
  Dockerfile                    # Multi-stage Docker build (alpine, musl)
  install.sh                    # Release install script
```

### Dependencies

- **CLI**: clap (arg parsing), colored + atty (terminal output)
- **Core**: tokio (async), serde/serde_json/serde_yaml (serialization), reqwest (HTTP), tiktoken-rs (token counting), rusqlite (checkpoints), rmp-serde (binary serialization), regex-lite (grep pattern matching)
- **LLM**: OpenAI-compatible chat completions API (works with OpenAI, DeepSeek, Groq, Ollama, etc.)

## License

MIT. See [LICENSE](LICENSE).

## Links

- [GitHub](https://github.com/liuyanghejerry/Clausura)
- [Issues](https://github.com/liuyanghejerry/Clausura/issues)
- [Releases](https://github.com/liuyanghejerry/Clausura/releases)
- [Docker images](https://github.com/liuyanghejerry/Clausura/pkgs/container/clausura)
