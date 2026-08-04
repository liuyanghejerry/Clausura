# Clausura

CI-native agent CLI tool for deterministic pipeline gating.

[![Build](https://img.shields.io/github/actions/workflow/status/liuyanghejerry/Clausura/main.yml?branch=main)](https://github.com/liuyanghejerry/Clausura/actions)
[![Crates.io](https://img.shields.io/crates/v/clausura-cli)](https://crates.io/crates/clausura-cli)
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

The script detects your OS and architecture, downloads the latest release from GitHub, verifies the tarball against the release's `checksums.txt` (SHA256), and installs it to `/usr/local/bin` or `~/.local/bin`.

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
| 2    | Error         | Runtime error (provider, timeout, etc.), or an incomplete agent run with `on_incomplete: fail` |
| 3    | Config error  | Invalid configuration                    |

## Using Skills

Clausura 1.2.0+ can reuse community skill files (Markdown) as review prompts. Skills answer "what and how to review", while gating rules answer "how many findings is too many" — the two are cleanly separated.

### Skill file format

A skill is a Markdown file, optionally with YAML frontmatter:

```markdown
---
name: security-review
description: 检查 SQL 注入、XSS、硬编码密钥
---

# 安全代码审查

## SQL 注入
- 任何字符串拼接构造的 SQL 查询
- rule_id: `sql-injection`
- severity: `error`
```

The frontmatter is stripped automatically; only the Markdown body is injected into the agent's system prompt.

### Referencing skills

```yaml
task:
  skill_prompts:
    # Local file (relative to workspace or absolute)
    - ./skills/security-review.md

    # Named skill (looks in .clausura/skills/<name>/SKILL.md,
    # then ~/.clausura/skills/<name>/SKILL.md)
    - security-review
    - team/vue-best-practices

  # Optional: append your own extra instructions
  prompt_template: |
    另外检查：禁止 console.log

  gating:
    - rule: sql-injection
      max_findings: 0
      action: fail
```

### Installing community skills

```bash
# Project-level (only this repo)
mkdir -p .clausura/skills/security-review
cp ~/Downloads/security-review-SKILL.md .clausura/skills/security-review/SKILL.md

# User-level (available to all your projects)
mkdir -p ~/.clausura/skills/team/vue-check
cp ~/Downloads/vue-check-SKILL.md ~/.clausura/skills/team/vue-check/SKILL.md
```

See [`examples/`](examples/) for ready-to-use skill files and a sample configuration.

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
  skill_prompts: []                           # Optional. Reuse community skill files.
                                               # Supports local paths, named references,
                                               # and remote URLs.

  # Limits
  token_budget: 32000                # Default. Context-window budget: older messages are
                                     # truncated (and archived) when the conversation
                                     # approaches this size.
  max_total_tokens: 200000           # Optional. Cap on cumulative billed tokens across all
                                     # LLM calls in one run; the run stops (marked incomplete)
                                     # when reached. Unset means no cap.
  auto_compact: false                # Default. When true, dropped context is summarized with
                                     # an LLM call and injected back instead of a bare hint.
  max_compactions: 3                 # Default. Per-run cap on auto-compact calls. 0 disables.
  findings_ledger: true              # Default. Persist interim findings to a disk ledger and
                                     # merge them back before the final answer, so findings from
                                     # truncated iterations are never lost.
  timeout_secs: 300                  # Default. Max wall-clock time in seconds.
  max_iterations: 10                 # Default. Max agent loop iterations.
  shell_timeout_secs: 120            # Default. Per-command timeout for shell_exec.

  # Optional tool allowlist
  tool_allowlist:                    # Restrict shell commands to these argv prefixes.
    - git status                     # "git status" allows that subcommand tree only.
    - cargo test                     # A bare name (e.g. "git") allows all subcommands.
  shell_env_passthrough: []          # Default. Extra env vars forwarded to shell_exec
                                     # commands (exact names only; secret-looking names
                                     # like *_KEY / *_TOKEN are refused).

  # Safety
  ambiguity_policy: fail_closed      # "fail_closed" or "proceed_with_caution".
  on_incomplete: fail                # "fail" (exit 2, default) or "pass" (continue with
                                     # partial results, marked incomplete in logs and SARIF).
                                     # Applies when the agent loop ends without a clean stop
                                     # (context truncated or iteration limit reached).

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
| `CLAUSURA_ON_INCOMPLETE` | `task.on_incomplete` |
| `CLAUSURA_TOKEN_BUDGET` | `task.token_budget`  |
| `CLAUSURA_MAX_TOTAL_TOKENS` | `task.max_total_tokens` |
| `CLAUSURA_TIMEOUT`      | `task.timeout_secs`  |
| `CLAUSURA_SHELL_TIMEOUT` | `task.shell_timeout_secs` |
| `CLAUSURA_MAX_ITERATIONS` | `task.max_iterations` |

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
      --max-iterations <N>  Max agent loop iterations  [default: 10]
      --shell-timeout <SECS>  Per-command shell_exec timeout  [default: 120]
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
    version: latest        # optional: release version to install (e.g. "1.0.8", default "latest")
```

The action downloads the matching release binary for the runner's OS/arch (Linux and macOS, x86_64 and aarch64), verifies it against the release's SHA256 `checksums.txt`, and adds it to `PATH` before running.

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

### Supported platforms

Clausura supports Linux and macOS. **Windows is not supported** (a documented non-goal, not "not yet") — on Windows, run Clausura under WSL2 or use the Docker image instead.

## MCP Integration

Clausura can connect to external servers through the **Model Context Protocol (MCP)**, a standardized JSON-RPC-based protocol for LLM tool discovery and invocation.

### Configuration

Declare MCP servers in your `.clausura.yaml`:

```yaml
task:
  mcp_servers:
    - name: lsp
      command: agent-lsp
      args: []
    - name: github
      command: npx
      args: ["-y", "@anthropic/mcp-server-github"]
      env:
        GITHUB_TOKEN: "${GITHUB_TOKEN}"
```

Clausura spawns each server at task start, performs the MCP initialize handshake, discovers available tools via `tools/list`, and registers them as `mcp__<server>__<tool>` in the agent's tool list.

### Preflight checks

Before the agent loop runs, you can configure "preflight" MCP tool calls whose output is parsed into deterministic `Finding` objects and passed directly through the gating rule engine — bypassing the LLM entirely. This is ideal for LSP diagnostics:

```yaml
task:
  mcp_servers:
    - name: lsp
      command: agent-lsp

  preflight:
    - mcp_server: lsp
      tool: get_diagnostics
      args:
        path: "."
      rule_id_prefix: "lsp-"

  gating:
    - rule: lsp-error
      min_severity: error
      max_findings: 0
      action: fail
```

Preflight findings are also summarized in the agent's context so the LLM-aware review can build on them.

### LSP code intelligence

For semantic code analysis beyond diagnostics, pair MCP with a skill guide:

```yaml
task:
  skill_prompts:
    - examples/skills/code-review-with-lsp/SKILL.md
```

The skill tells the agent to use LSP tools — `hover`, `definition`, `references`, `symbols` — for type information, navigation, and impact analysis.

### CI environment setup

MCP servers need to be installed in your CI environment. For LSP support with `agent-lsp`:

```dockerfile
# Docker example
FROM rust:latest

# Install Clausura
RUN curl -fsSL https://raw.githubusercontent.com/liuyanghejerry/Clausura/main/install.sh | bash

# Install agent-lsp (MCP → LSP bridge)
RUN curl -fsSL https://raw.githubusercontent.com/blackwell-systems/agent-lsp/main/install.sh | sh

# Install language servers per project language
RUN rustup component add rust-analyzer
RUN npm i -g typescript-language-server
RUN pip install pyright
```

For GitHub Actions:

```yaml
- uses: liuyanghejerry/Clausura@v1
  with:
    config: .clausura.yaml
    api_key: ${{ secrets.LLM_API_KEY }}

- name: Install agent-lsp
  run: curl -fsSL https://raw.githubusercontent.com/blackwell-systems/agent-lsp/main/install.sh | sh

- name: Install language servers
  run: |
    rustup component add rust-analyzer
    npm i -g typescript-language-server
    pip install pyright

- name: Run review with LSP
  run: clausura run
  env:
    CLAUSURA_API_KEY: ${{ secrets.LLM_API_KEY }}
```

## Architecture

### Agent loop (reason -> act -> observe)

Clausura runs a bounded agent loop of up to `max_iterations` iterations (default 10, configurable via YAML, `--max-iterations`, or `CLAUSURA_MAX_ITERATIONS`). Each iteration:

1. Sends the conversation (system prompt + accumulated messages) to the LLM
2. If the LLM calls a tool, executes it and feeds the result back
3. If the LLM responds with structured findings and signals `stop`, the loop ends
4. The loop also ends on token budget exhaustion, timeout, or content filter

The system prompt is built from `prompt_template` plus available tool definitions. The LLM is instructed to respond in JSON with a `findings` array.

### Context truncation, archiving, and auto-compact

When the conversation exceeds the configured `token_budget`, Clausura automatically truncates older messages to stay within limits. Dropped messages are not silently discarded — they are archived to `.clausura/archives/context-dump-{task_id}-{seq}.log` inside the workspace as JSON lines. A hint message is injected into the conversation telling the LLM where to find the archived context, so it can retrieve earlier findings via the `read_file` tool if needed.

With `auto_compact: true`, the dropped messages are instead **summarized with a single LLM call** and the summary is injected at the truncation boundary, so the agent keeps its memory of earlier findings, files examined, and decisions across truncation. The full transcript is still archived. Auto-compact never changes the run's pass/fail semantics:

- Summary calls are billed and counted against `max_total_tokens`, but the call is **skipped** if it would not fit under the remaining quota.
- If the summarization call fails or times out, Clausura silently falls back to the bare truncation hint.
- Summaries are capped at 10% of `token_budget`; oversized output is trimmed.
- `max_compactions` bounds the number of summary calls per run (default 3) to prevent compaction loops on very long tasks.

### Findings ledger (disk-backed memory)

Compaction is lossy by design, so Clausura also keeps a **lossless, deterministic memory** on disk: whenever the agent emits findings in a response, they are appended to `{workspace}/.clausura/archives/findings-ledger-{task_id}.jsonl` (one JSON object per line). When the run finishes, the final findings are **merged with the ledger** — the final response wins on conflicts, and findings from iterations that were truncated out of context are appended back. No extra LLM calls are involved; the merge is plain deduplication keyed on `rule_id` + location + message. Disable with `findings_ledger: false` (env `CLAUSURA_FINDINGS_LEDGER=false`).

On successful completion (exit code 0), archive files are automatically cleaned up. On failure (exit code 1-3), they are preserved for debugging and audit.

### Incomplete runs fail closed

If the agent loop ends without a clean stop — the context could not be truncated further, the model hit a length limit, the `max_total_tokens` cap was reached, or `max_iterations` was exhausted — the extracted findings may be partial. By default (`on_incomplete: fail`) Clausura fails closed: the run exits with code 2 and a clear error message, so an incomplete review can never silently pass a `max_findings: 0` gate. With `on_incomplete: pass`, the previous behavior is kept (gating rules evaluate the partial findings), but a warning is logged and the SARIF output is annotated with `"properties": {"incomplete": true}` on the run.

### Deterministic rule engine

Findings from the agent are evaluated by the rule engine using pure counting:

- Match findings to rules by `rule_id`
- Filter by severity threshold
- Count violations above `max_findings`
- Apply action: `fail` exits 1, `warn` logs, `ignore` skips

No LLM calls, no heuristics. Just deterministic logic your pipeline can trust.

### LLM provider abstraction

Three vendor categories: OpenAI-compatible (works with OpenAI, DeepSeek, Groq, Ollama, vLLM, Mistral), Anthropic-compatible (native Claude Messages API), and Custom (configurable enterprise endpoints). Factory function `create_provider()` dispatches on vendor type.

Each LLM request has its own per-request timeout (default 120s), independent of the task's overall wall-clock `timeout_secs`. Failed requests are retried with exponential backoff (up to 3 retries by default) on HTTP 429, 5xx, and network errors — the `Retry-After` response header is honored when present. Other 4xx responses (e.g. 401, 400) fail immediately without retrying.

### Tool sandboxing

Five built-in tools:

| Tool          | Description                                      | Restrictions                           |
|---------------|--------------------------------------------------|----------------------------------------|
| `read_file`   | Read a file relative to workspace root, with optional `offset`/`limit` for line-range reading | Blocks absolute paths, `..` traversal, symlink escapes |
| `list_files`  | List directory contents, with recursive depth, glob filtering, and optional file sizes | Sandboxed to workspace; skips `.clausura/` |
| `grep`        | Search text patterns across files with literal or regex mode, extension filtering, and binary-skip | Auto-excludes `.git`, `target`, `.clausura`, `node_modules` |
| `git_diff`    | Run `git diff` with optional base ref or staged  | Operates inside workspace only         |
| `shell_exec`  | Execute an allowed command (argv form, no shell) | Restricted to `tool_allowlist` argv prefixes; dangerous flags denied; scrubbed env |

The `shell_exec` tool is locked by default (empty allowlist = no commands). Explicitly list allowed commands to enable it. It takes an `argv` array (e.g. `{"argv": ["git", "status"]}`) and executes `argv[0]` directly **without a shell** — shell metacharacters like `;`, `|`, `$()` or `>` are passed through as literal arguments and have no effect, so they cannot be used to bypass the allowlist. Commands run with `current_dir` set to the workspace root, but allowed commands can access paths outside the workspace via arguments.

**Allowlist prefix rules.** Each `tool_allowlist` entry is an argv prefix split on whitespace: an invocation is allowed when its leading tokens equal a rule's tokens. `"git status"` allows `["git", "status"]` and `["git", "status", "--short"]`, but not `["git", "log"]`. A bare program name (e.g. `"git"`) is a length-1 prefix and allows all subcommands of that program (backward compatible). The rule's first token also matches by basename, so `["/usr/bin/git", "status"]` satisfies a `"git status"` rule.

**Dangerous-flag denylist.** On top of the allowlist, known-risky flags are rejected regardless of position (scanning stops at the first `--`, whose following tokens are operands): `git -c`, `git --exec-path`, `git --git-dir`, `git --work-tree`, `tar --checkpoint-action`, `tar --to-command`. Both exact and attached forms are caught (`-c foo`, `-cfoo`, `--git-dir=/x`).

**Environment scrubbing.** Commands run with a minimal, deny-by-default environment: a fixed `PATH` (`/usr/local/bin:/usr/bin:/bin`, plus `$HOME/.cargo/bin` if it exists), `HOME`/`TERM`/`LANG`/`TMPDIR`/`CI` forwarded when present, and safety overrides (`GIT_PAGER=cat`, `PAGER=cat`, `GIT_CONFIG_NOSYSTEM=1`, `GIT_TERMINAL_PROMPT=0`, `GIT_EDITOR=:`, `EDITOR=:`). Everything else — including `CLAUSURA_API_KEY`, `LD_PRELOAD`, `GIT_SSH_COMMAND`, `SSH_AUTH_SOCK`, `BASH_ENV` — is dropped. `shell_env_passthrough` can forward additional variables by exact name; `CLAUSURA_API_KEY` and names ending in `_KEY`, `_TOKEN`, `_SECRET` or `_PASSWORD` are refused with a warning.

**Per-command timeout.** Each command is killed after `shell_timeout_secs` (default 120; override with `--shell-timeout` or `CLAUSURA_SHELL_TIMEOUT`) and the tool returns a `Command timed out after Ns and was killed` result.

Tool outputs are truncated at 32 KB or 1000 lines (whichever comes first) to stay within token budgets; a `[output truncated ...]` marker is appended when this happens — narrow the query or use `read_file`'s `offset`/`limit` to page through large outputs.

### Memory snapshots (SQLite checkpoints)

On every run, the agent's message history is serialized (MessagePack) and saved to `~/.clausura/checkpoints.db`. You can resume a truncated or interrupted run with `--resume`. Snapshots include a thread ID, version number, and truncation flag.

Note that checkpoints live under the user's home directory (`~/.clausura/`), not the workspace. In ephemeral CI containers without a persistent home/volume, checkpoints do not survive between runs, so `--resume` has nothing to restore from.

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

### Pre-commit hook

A `cargo fmt` pre-commit hook is included. Enable it after cloning:

```bash
git config core.hooksPath .githooks
```

Commits with misformatted Rust code will be rejected. Run `cargo fmt --all` to auto-fix.

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
