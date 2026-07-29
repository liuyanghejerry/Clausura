# Overview & Philosophy

## What Clausura Is

Clausura is a **CI-native agent CLI** that runs bounded LLM tasks against your codebase and produces a deterministic pass/fail verdict. It was built for a single purpose: **automated code review gating in CI/CD pipelines.**

Think of it as a code reviewer that:

- Never gets tired or distracted
- Always follows the same rules
- Produces machine-readable output (SARIF) that integrates with your existing tools
- Fails the pipeline when it should — and only when it should

## What Clausura Is Not

- **Not an interactive coding assistant.** There is no chat, no follow-up questions, no human in the loop. Every run is fully autonomous.
- **Not a general-purpose agent framework.** The tool set is deliberately limited. The iteration count is capped. The output format is fixed.
- **Not a replacement for linting or static analysis.** Clausura complements ESLint, Semgrep, and CodeQL — it catches semantic and architectural issues those tools can't express.

## Core Design Principles

### 1. Closed-Loop Execution

Every Clausura run is **bounded** by three hard limits:

| Limit | Default | What happens when exceeded |
|-------|---------|---------------------------|
| `token_budget` | 32,000 | Older messages are truncated and archived; agent is notified to inspect archives if needed |
| `timeout_secs` | 300 | Run terminates with exit code 2 |
| `max_iterations` | 10 | Run ends; if incomplete → exit code 2 (`on_incomplete: fail`) |

The agent loop follows a simple **reason → act → observe** cycle:

```
1. Send conversation to LLM (system prompt + message history)
2. LLM responds with either:
   a. Tool calls → execute them, feed results back, repeat
   b. Stop signal → extract findings, exit loop
3. If loop ends without clean Stop → mark as incomplete, fail closed
```

There is no branching, no sub-tasking, no delegation. One agent. One loop. One result.

### 2. Deterministic Gating

After the LLM produces findings, the **rule engine** takes over. This is pure counting logic — no AI, no heuristics:

```
For each gating rule:
  1. Filter findings matching rule.rule_id
  2. Filter by severity >= rule.min_severity
  3. Count remaining
  4. If count > rule.max_findings → apply rule.action
```

The rule engine is why Clausura can be trusted in CI. The LLM might hallucinate, miss things, or produce inconsistent output — but the gating decision is always deterministic and auditable.

### 3. CI-Native

Clausura is designed to run unattended in CI environments:

- **Auto-detection**: recognizes GitHub Actions, GitLab CI, Jenkins, generic CI via environment variables
- **Exit codes map to pipeline status**: 0 = pass, 1 = fail (rule violation), 2 = error (runtime), 3 = error (config)
- **SARIF output**: `clausura-output.sarif` in SARIF v2.1.0 format, compatible with GitHub Advanced Security, CodeQL dashboard, and any SARIF viewer
- **No interactive prompts**: the run either completes or fails — there is no "ask the user what to do next"

## Key Concepts

### Task

A **task** is a single Clausura run defined by a `.clausura.yaml` config file. It specifies:

- Which LLM to use (model + vendor)
- What to check (prompt template or skills)
- How long to run (budget, timeout, iterations)
- What counts as failure (gating rules)

### Findings

**Findings** are structured issues discovered by the LLM. Each finding has:

| Field | Type | Description |
|-------|------|-------------|
| `rule_id` | string | Identifier matching a gating rule (e.g. `"sql-injection"`) |
| `severity` | enum | `hint` < `info` < `warning` < `error` |
| `message` | string | Human-readable description of the issue |
| `evidence` | string | The code or text that triggered the finding |
| `location` | object? | Optional file/line/column range |

The LLM is instructed to output findings as a JSON array. Clausura validates every finding against its schema — schema mismatches fail loudly, not silently.

### Gating Rules

**Gating rules** are the deterministic pass/fail criteria. Each rule says: "for findings with `rule_id` X at severity Y or higher, allow at most Z occurrences, and if exceeded, do W."

→ [Full gating guide](gating.md)

### Skills

**Skills** are reusable Markdown files that encode review knowledge. They tell the LLM **what** to look for, while gating rules tell Clausura **how many** is too many. Skills come from the community or your team's internal repository.

→ [Skills guide](skills.md)

## Architecture at a Glance

```
┌─────────────────────────────────────────────────┐
│                    .clausura.yaml                 │
│  task definition + gating rules                  │
└──────────────────┬──────────────────────────────┘
                   │
                   ▼
┌─────────────────────────────────────────────────┐
│                 Config Loader                     │
│  YAML → CLI flags → env vars (layered)           │
└──────────────────┬──────────────────────────────┘
                   │
                   ▼
┌─────────────────────────────────────────────────┐
│                 Executor                          │
│  provider → agent loop → rule engine → SARIF     │
└──┬───────────┬───────────┬───────────┬──────────┘
   │           │           │           │
   ▼           ▼           ▼           ▼
┌──────┐ ┌──────────┐ ┌────────┐ ┌──────────┐
│LLM   │ │Agent Loop│ │Rule    │ │SARIF     │
│Provider│ │(≤N iter)│ │Engine  │ │Formatter │
└──────┘ └────┬─────┘ └────────┘ └──────────┘
              │
              ▼
┌────────────────────────────┐
│  Tools (sandboxed)         │
│  read_file | list_files    │
│  grep | git_diff           │
│  shell_exec (allowlisted)  │
└────────────────────────────┘
```

### Components

- **Provider**: Abstracts over LLM APIs (OpenAI-compatible, Anthropic Messages, Custom). Handles auth, retries, and response parsing.
- **Agent Loop**: The bounded reason→act→observe cycle. Manages context window truncation, archive hints, and checkpoint saves.
- **Context Manager**: Tracks token usage, truncates older messages to stay within `token_budget`, archives dropped context.
- **Tool Registry**: Five built-in tools with sandboxing — path confinement, command allowlisting, environment scrubbing.
- **Rule Engine**: Deterministic counting engine. No LLM. Pure logic.
- **SARIF Formatter**: Writes findings to `clausura-output.sarif` in SARIF v2.1.0 format.
- **Snapshot Manager**: SQLite-based checkpoint store for crash recovery (`--resume`).

## Design Decisions

Here are a few explicit choices that shape Clausura's design:

**Why no sub-agents?**
Clausura's value is bounded, deterministic execution. Sub-agents make the system less predictable — harder to budget, harder to gate, harder to debug. Parallel review is better done at the CI orchestration level (multiple `clausura run` calls in parallel jobs).

**Why JSON findings instead of free-text?**
Structured output is the contract between the LLM and the rule engine. Free-text findings would require another LLM call to classify — adding cost, latency, and non-determinism between the reviewer and the gate.

**Why `on_incomplete: fail` by default?**
An incomplete review (context exhausted, iteration limit hit) that finds zero issues must not silently pass. Fail closed is the safe default for CI gating.

**Why no interactive mode?**
Clausura is not an assistant. It's a pipeline gate. Every interactive prompt is a CI timeout waiting to happen.

## Next Steps

- [Quick start with a minimal config →](../README.md#quick-start)
- [Set up your LLM provider →](llm-providers.md)
- [Understand the configuration →](configuration.md)
- [Design your gating rules →](gating.md)
- [See real-world scenarios →](scenarios.md)
