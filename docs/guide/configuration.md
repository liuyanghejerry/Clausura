# Configuration Reference

Clausura is configured via `.clausura.yaml` (or `.clausura.yml`) in your project root, with CLI flags and environment variables providing overrides.

## Loading Priority

Configuration is loaded in three layers, with later layers overriding earlier ones:

```
YAML file  <  CLI flags  <  Environment variables
```

For example, `task.model` from the YAML file is overridden by `--model` on the CLI, which is in turn overridden by `CLAUSURA_MODEL` in the environment.

## Complete Schema

```yaml
version: "1"                         # Required. Schema version (currently "1").

task:
  # ── Identity ──────────────────────────────────────
  name: my-task                      # Required. Task name, used in logs and SARIF.

  # ── LLM Provider ──────────────────────────────────
  model: gpt-4o                      # Required (or set CLAUSURA_MODEL).
  vendor: openai                     # Shorthand: openai, anthropic, deepseek, groq, ollama.
  # Or full config:
  vendor:
    type: openai_compatible          # openai_compatible | anthropic_compatible | custom
    base_url: "https://api.deepseek.com/v1"  # Optional. Override API endpoint.
    auth_header: "X-API-Key"         # Optional. Custom auth header (default: Authorization).
    api_key_env: "MY_SECRET_KEY"     # Optional. Env var for API key (default: CLAUSURA_API_KEY).

  # ── Prompt ────────────────────────────────────────
  prompt_template: |                 # The agent's system prompt. Can use {{template_vars}}.
    Review the diff for security issues.
  skill_prompts: []                  # Optional. Reuse community skill files (local paths
                                     # or named references).

  # ── Limits ────────────────────────────────────────
  token_budget: 32000                # Context-window budget. Drives message truncation.
  max_total_tokens: 200000           # Optional. Cumulative token cap across all LLM calls.
                                     # Unset = no cap. When reached, run stops (incomplete).
  auto_compact: false                # Optional. Summarize dropped context with an LLM call
                                     # instead of a bare truncation hint.
  max_compactions: 3                 # Per-run cap on auto-compact calls. 0 disables.
  findings_ledger: true              # Persist interim findings to a disk ledger and merge
                                     # them back before the final answer (lossless memory).
  timeout_secs: 300                  # Max wall-clock time for the entire run.
  max_iterations: 10                 # Max agent loop iterations (tool calls + LLM turns).
  shell_timeout_secs: 120            # Per-command timeout for shell_exec.

  # ── Shell Sandbox ─────────────────────────────────
  tool_allowlist:                    # Optional. Allowed shell_exec commands as argv prefixes.
    - git status                     # "git status" allows that subcommand tree only.
    - cargo test                     # Bare name (e.g. "git") allows all subcommands.
  shell_env_passthrough: []          # Optional. Extra env vars forwarded to shell_exec.

  # ── Safety ────────────────────────────────────────
  ambiguity_policy: fail_closed      # "fail_closed" or "proceed_with_caution".
  on_incomplete: fail                # "fail" (exit 2) or "pass" (continue with partial results).

  # ── Sharding (large PRs) ──────────────────────────
  # Setting `base` enables the sharded audit path: per-file diffs grouped
  # into bounded shards, one bounded agent run per shard, bisect-and-retry
  # on incomplete shards, aggregated gating. See docs/guide/sharding.md.
  sharding:
    base: origin/main                # Required. Diff base (resolved via merge-base).
    paths: []                        # Optional path filters.
    max_diff_bytes: 131072           # Per-shard diff byte budget (default 128 KiB).
    max_files_per_shard: 8           # File-count cap per shard.
    max_splits: 3                    # Bisect depth on incomplete retries.
    context_lines: 20                # --unified context per hunk in per-file diffs.
    on_shard_incomplete: bisect      # bisect | fail | pass.
    per_shard:                       # Budget overrides for each shard run.
      token_budget: 300000
      max_total_tokens: 300000
      max_iterations: 12
      timeout_secs: 300
    risk_patterns: {}                # Optional extra pre-scan patterns (tag: regex).

  # ── Gating Rules ──────────────────────────────────
  gating:                            # Optional. Evaluated in declaration order.
    - rule: sql-injection            # Rule ID. Matches findings by rule_id field.
      description: "No SQL injection"
      min_severity: error            # hint | info | warning | error
      max_findings: 0                # Maximum allowed findings at this severity or above.
      action: fail                   # fail | warn | ignore
    - rule: hardcoded-secret
      description: "No hardcoded credentials"
      min_severity: error
      max_findings: 0
      action: fail
    - rule: missing-validation
      description: "Warning on excessive missing validation"
      min_severity: warning
      max_findings: 5
      action: warn
```

## Field Reference

### `task.name`

**Required.** A human-readable name for this task. Appears in log output and the SARIF report.

```yaml
task:
  name: security-review
```

### `task.model`

**Required** (or set `CLAUSURA_MODEL`). The LLM model identifier.

```yaml
task:
  model: gpt-4o
```

### `task.vendor`

**Required** (or set `CLAUSURA_VENDOR`). Specifies which LLM provider to use.

Short form:

```yaml
vendor: openai      # Maps to built-in preset
vendor: anthropic
vendor: deepseek
vendor: groq
vendor: ollama
```

Full form:

```yaml
vendor:
  type: custom
  base_url: "https://llm.internal/v1"
  auth_header: "X-API-Key"
  api_key_env: "INTERNAL_LLM_KEY"
```

→ [Complete LLM provider guide](llm-providers.md)

### `task.prompt_template`

The system prompt sent to the LLM. This is where you define **what** the agent should look for.

```yaml
task:
  prompt_template: |
    Review the git diff for the following security issues:

    1. SQL injection — any string concatenation in SQL queries
       rule_id: "sql-injection", severity: "error"
    2. Hardcoded credentials — API keys, passwords, tokens in source
       rule_id: "hardcoded-secret", severity: "error"
    3. Missing input validation — API endpoints without type/range checks
       rule_id: "missing-validation", severity: "warning"

    Output your findings as a JSON array of objects with rule_id, severity, message, and evidence fields.
```

**Template variables.** Clausura injects CI context as template variables:

| Variable | Value |
|----------|-------|
| `{{task_name}}` | The task name |
| `{{repo}}` | Repository name (from CI detection) |
| `{{branch}}` | Current branch |
| `{{commit_sha}}` | Current commit SHA |
| `{{pr_number}}` | Pull request number |
| `{{ci_platform}}` | Detected CI platform (github_actions, gitlab_ci, etc.) |

### `task.skill_prompts`

References to reusable skill files. Only a catalog (name + description) is inlined into the system prompt; full bodies are loaded on demand by the `read_skill` tool (progressive disclosure).

```yaml
task:
  skill_prompts:
    - ./skills/security-review.md      # Local file (relative or absolute)
    - team/vue-best-practices          # Named reference (looked up in .clausura/skills/ and ~/.clausura/skills/)
    - community/i18n-check
  prompt_template: |                   # Your additions go after the skill catalog
    Also check: no console.log in production code.
```

→ [Skills guide](skills.md)

### `task.token_budget`

**Default: 32000.** The context-window budget. When the conversation (system prompt + message history) approaches this size, Clausura truncates older messages to make room. Truncated messages are archived to `.clausura/archives/` inside the workspace. A hint is injected into the conversation telling the agent where to find the archive.

This is separate from `max_total_tokens`:

- `token_budget` — how large the **active conversation** can be
- `max_total_tokens` — total **billed tokens** across all LLM calls in the run

### `task.max_total_tokens`

**Default: None (no cap).** Hard limit on cumulative billed tokens. When total usage across all LLM calls reaches this value, the agent loop stops and the run is marked incomplete. Use this for cost control — it has no effect on context truncation.

```yaml
task:
  token_budget: 32000        # context window budget
  max_total_tokens: 200000   # stop after $X worth of API calls
```

### `task.auto_compact`

**Default: false.** When the conversation exceeds `token_budget`, Clausura truncates older messages and injects a bare "context trimmed" hint. With `auto_compact: true`, the dropped messages are instead **summarized with one extra LLM call** and the summary is injected at the truncation boundary — the agent keeps remembering earlier findings, files examined, and decisions across truncation instead of relying on the `read_file`-the-archive hint.

Auto-compact never changes the run's pass/fail semantics:

- The summary call is billed and counts toward `max_total_tokens`, but is **skipped** when the remaining quota is too small to afford it (bare hint is used instead).
- If the summary call fails or times out, Clausura falls back to the bare hint silently.
- Summaries are sized to the headroom the retained context leaves under the truncation threshold (capped at 10% of `token_budget`); oversized output is trimmed to fit. A compacted context always stays within budget, so a successful compaction can never push the run into "incomplete".
- `max_compactions` (default 3) bounds summary calls per run; set it to `0` to disable compaction even with `auto_compact: true`.

```yaml
task:
  auto_compact: true       # summarize dropped context (adds one LLM call per compaction)
  max_compactions: 3       # at most 3 summary calls per run
```

Env override: `CLAUSURA_AUTO_COMPACT=true`, `CLAUSURA_MAX_COMPACTIONS=5`.

### `task.findings_ledger`

**Default: true.** Whenever the agent emits findings in a response, Clausura appends them to `{workspace}/.clausura/archives/findings-ledger-{task_id}.jsonl` (one JSON object per line). When the run finishes, the final findings are merged with the ledger:

- The final response wins on conflicts (same `rule_id` + location + message).
- Findings from iterations that were truncated out of context are appended back — so context truncation/compaction can never silently drop an already-discovered finding.
- The merge is deterministic (plain deduplication, no LLM call) and zero-cost.

This is the **lossless complement to auto-compact**: compaction preserves continuity (summary in context), the ledger preserves completeness (findings on disk). It also applies when `auto_compact` is off — bare truncation drops just as much.

```yaml
task:
  findings_ledger: true   # default; set false to disable
```

Env override: `CLAUSURA_FINDINGS_LEDGER=false`.

### `task.timeout_secs`

**Default: 300.** Maximum wall-clock time for the entire run. If exceeded, the run terminates with exit code 2 (error).

```yaml
task:
  timeout_secs: 120   # Fail after 2 minutes
```

### `task.max_iterations`

**Default: 10.** Maximum number of agent loop iterations. Each iteration is one LLM call + tool execution round. After this many turns, the loop stops. If the agent hasn't produced a clean `Stop` response, the run is marked incomplete.

Typical runs complete in 1–3 iterations. Setting this too high risks long-running loops; too low risks incomplete reviews.

### `task.shell_timeout_secs`

**Default: 120.** Per-command timeout for `shell_exec`. Each individual command is killed after this duration.

### `task.tool_allowlist`

**Default: `[]` (empty = shell_exec disabled).** Commands the agent is allowed to execute via `shell_exec`. Each entry is an argv prefix:

```yaml
task:
  tool_allowlist:
    - git status      # Allows: ["git", "status"], ["git", "status", "--short"]
                      # Denies: ["git", "log"], ["git", "push"]
    - cargo test      # Allows: ["cargo", "test"], ["cargo", "test", "--lib"]
                      # Denies: ["cargo", "build"]
    - git             # Allows: ALL git subcommands (bare name = length-1 prefix)
```

Matching rules:
- Tokens are compared literally. `"git status"` matches `["git", "status"]`.
- The first token also matches by basename: `["/usr/bin/git", "status"]` matches a `"git status"` rule.
- Known-dangerous flags (`git -c`, `tar --checkpoint-action`, etc.) are rejected regardless of the allowlist.

→ See the [allowlist hardening research](../allowlist-hardening.md) for the full threat model.

### `task.shell_env_passthrough`

**Default: `[]`.** Extra environment variables forwarded to `shell_exec` commands. By default, commands run with a minimal environment (only `PATH`, `HOME`, `TERM`, `LANG`, `TMPDIR`, `CI`). Secret-shaped names (`*_KEY`, `*_TOKEN`, `*_SECRET`, `*_PASSWORD`) are refused even if listed.

```yaml
task:
  shell_env_passthrough:
    - HTTP_PROXY       # OK: forwarded
    - NODE_ENV         # OK: forwarded
    # - AWS_SECRET_KEY # REJECTED: secret-shaped name
```

### `task.ambiguity_policy`

**Default: `fail_closed`.** Controls behavior when the agent's output is ambiguous or malformed.

| Value | Behavior |
|-------|----------|
| `fail_closed` | Fail on ambiguity. Invalid JSON findings → error. Schema mismatch → error. |
| `proceed_with_caution` | Try best-effort extraction. Log a warning but continue. |

`fail_closed` is the safe default for CI — an unparseable answer should block the pipeline, not pass silently.

### `task.on_incomplete`

**Default: `fail`.** Controls what happens when the agent loop ends without a clean `Stop` (context exhausted, iteration limit reached, or `max_total_tokens` hit):

| Value | Behavior |
|-------|----------|
| `fail` | Exit code 2 (error). An incomplete review with zero findings must not pass gates. |
| `pass` | Evaluate what we have. Warning is logged. SARIF output is annotated with `incomplete: true`. |

The incomplete reason is always reported machine-readably: SARIF
`invocations[0].properties.incompleteReason` (e.g. `context_limit`,
`iteration_limit`, `token_cap`, `timeout`, `malformed_json`) and, with
`--summary <path>`, a standalone summary JSON. A final answer that never
becomes parseable JSON no longer discards the run: ledger-persisted findings
are kept and the run is reported `incomplete_reason=malformed_json`.

### `task.sharding`

**Default: absent (single-task path).** Setting `base` enables the sharded
audit path for large PRs. Each shard runs in its own bounded agent loop with
the `per_shard` budget overrides; incomplete shards are bisected and retried
(`on_shard_incomplete: bisect`), and the aggregated findings go through the
normal gating rules.

| Field | Default | Description |
|-------|---------|-------------|
| `base` | (required) | Git ref to diff against (resolved via merge-base). |
| `paths` | `[]` | Optional path filters passed to `git diff --`. |
| `max_diff_bytes` | `131072` | Per-shard diff byte budget. |
| `max_files_per_shard` | `8` | File-count cap per shard. |
| `max_splits` | `3` | Bisect depth for incomplete shards. |
| `context_lines` | `20` | `--unified` context per hunk (max 100). |
| `on_shard_incomplete` | `bisect` | `bisect`, `fail`, or `pass`. |
| `per_shard` | (all unset) | `token_budget`, `max_total_tokens`, `max_iterations`, `timeout_secs` overrides per shard. |
| `risk_patterns` | `{}` | Extra deterministic pre-scan patterns (tag → regex on added lines). |

Sharded runs always write a summary JSON (`<output>.summary.json`, or
`--summary`) listing per-shard statuses; a shard that never completes yields
exit 2 with `status: incomplete` / `reason: shard_incomplete`, separate from
gate violations (exit 1).

→ [Sharded audits deep dive](sharding.md)

### `task.gating`

An array of gating rules, evaluated in order. Each rule:

| Field | Type | Description |
|-------|------|-------------|
| `rule` | string | Rule ID. Matches `finding.rule_id`. |
| `description` | string | Human-readable description. |
| `min_severity` | string | Severity threshold: `hint`, `info`, `warning`, `error`. |
| `max_findings` | number | Maximum allowed findings at or above this severity. |
| `action` | string | What to do when exceeded: `fail` (exit 1), `warn` (log only), `ignore` (skip). |

→ [Gating rules deep dive](gating.md)

## CLI Flags

```
clausura run [OPTIONS]

  -c, --config <PATH>       Config file path              [default: .clausura.yaml]
      --model <MODEL>       Override LLM model
      --vendor <VENDOR>     Override LLM vendor
      --api-key <KEY>       API key
      --token-budget <N>    Token budget override
      --timeout <SECS>      Timeout override
      --summary <PATH>      Write a machine-readable run summary JSON
                            (status, incomplete reason, findings count, usage)
      --max-iterations <N>  Max agent loop iterations      [default: 10]
      --shell-timeout <SECS> Per-command shell_exec timeout [default: 120]
      --workspace <PATH>    Workspace root                 [default: cwd]
      --output <PATH>       SARIF output path              [default: clausura-output.sarif]
      --resume              Resume from last checkpoint
      --log-format <FMT>    Log format (json|pretty)        [default: json]
      --dry-run             Validate config and print execution plan
      --validate-config     Validate config and exit
```

### Snapshot management

```
clausura snapshot list [--thread <ID>] [--limit <N>]     List checkpoints
clausura snapshot show [--thread <ID>]                   Show latest checkpoint
clausura snapshot show --id <UUID> [--thread <ID>]      Show specific checkpoint
clausura snapshot delete --thread <ID>                   Delete all checkpoints for a thread
```

## Environment Variables

| Variable | Overrides | Example |
|----------|-----------|---------|
| `CLAUSURA_API_KEY` | API key (required) | `sk-...` |
| `CLAUSURA_MODEL` | `task.model` | `gpt-4o` |
| `CLAUSURA_VENDOR` | `task.vendor` | `openai` |
| `CLAUSURA_BASE_URL` | Provider base URL — points any OpenAI-compatible endpoint (e.g. `https://open.bigmodel.cn/api/paas/v4`) at any vendor shorthand | `https://api.deepseek.com/v1` |
| `CLAUSURA_AMBIGUITY_POLICY` | `task.ambiguity_policy` | `fail_closed` |
| `CLAUSURA_ON_INCOMPLETE` | `task.on_incomplete` | `fail` |
| `CLAUSURA_TOKEN_BUDGET` | `task.token_budget` | `32000` |
| `CLAUSURA_MAX_TOTAL_TOKENS` | `task.max_total_tokens` | `200000` |
| `CLAUSURA_TIMEOUT` | `task.timeout_secs` | `300` |
| `CLAUSURA_SHELL_TIMEOUT` | `task.shell_timeout_secs` | `120` |
| `CLAUSURA_MAX_ITERATIONS` | `task.max_iterations` | `10` |

## Next

→ [Design your gating rules](gating.md)
→ [See common scenarios](scenarios.md)
