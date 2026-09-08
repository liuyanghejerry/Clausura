# Sharded Audits (Large PRs)

A pull request with a multi-thousand-line diff cannot be reviewed by a single
agent run: the model spends its context reading, the budget evaporates, and
the run ends *incomplete* with zero findings. The sharding path splits the
same review into bounded per-file shards so every agent run can actually
finish.

## How it works

Adding a `sharding:` section to `.clausura.yaml` switches `clausura run` to
the sharded path:

1. **Collect** — changed files are listed via `git diff --name-only` between
   `git merge-base <base> HEAD` and `HEAD`, then each file's diff is taken
   with `--unified=<context_lines>`.
2. **Pre-scan** — a deterministic regex scanner (`risk.rs`) flags candidate
   hotspots on added lines: new routes, SQL statements, string-built SQL,
   request-body entry points, filesystem access, auth checks, logging, and
   process execution. No LLM calls.
3. **Plan** — files are grouped into shards under `max_diff_bytes` /
   `max_files_per_shard` (default 128 KiB / 8 files).
4. **Review** — each shard runs its own bounded agent loop. The initial
   message contains the shard manifest (file, changed lines, diff bytes,
   risk tags, candidate hotspots) plus the diffs themselves — inline, so no
   tool round-trips are needed just to read the input. The prompt instructs
   the agent to read surrounding source only to validate a potential
   finding.
5. **Retry** — a shard that ends incomplete (`context_limit`,
   `iteration_limit`, `token_cap`, `length`, `timeout`) is bisected — by file
   list, or by hunk boundaries for a single oversized file — and both halves
   are re-run, up to `max_splits` times.
6. **Aggregate** — findings from all shards are deduplicated
   (rule + location + message) and evaluated against the normal gating
   rules; SARIF and a run summary are written.

## Configuration

```yaml
task:
  sharding:
    base: origin/main          # required — enables the sharded path
    paths: ["packages/server/src"]   # optional path filters
    max_diff_bytes: 131072     # per-shard diff budget (default 128 KiB)
    max_files_per_shard: 8     # file-count cap per shard
    max_splits: 3              # bisect depth on incomplete retries
    context_lines: 20          # --unified context per hunk
    on_shard_incomplete: bisect   # bisect | fail | pass
    per_shard:                 # budget overrides for each shard run
      token_budget: 300000
      max_total_tokens: 300000
      max_iterations: 12
      timeout_secs: 300
    risk_patterns:             # optional extra pre-scan patterns (tag: regex)
      internal-endpoint: "/internal/[a-z]+"
```

See [`examples/sharded-security-audit.yaml`](../../examples/sharded-security-audit.yaml)
for a complete security-audit configuration.

A starting point for shard sizing:

- 50–150 KB of diff per shard
- 100k–300k token budget per shard
- 8–15 iterations
- 2–5 minutes timeout

Use `clausura run --dry-run` to see the planned shard count for your PR
before spending tokens. `--dry-run` also warns when a *non-sharded* task
configures `token_budget >= 1M` or `max_iterations >= 40` — the shape that
lets an agent burn budget reading instead of reviewing.

## Shard manifest

Each shard's initial message starts with a manifest the agent (and you, in
the archives) can read at a glance:

```text
SHARD: 2 file(s), 30412 diff bytes total
FILE: packages/server/src/services/data-manager.ts
CHANGED_LINES: 461-520
DIFF_BYTES: 18420
RISK_TAGS: filesystem, sql
CANDIDATE HOTSPOTS (deterministic pre-scan of added lines — start here):
- services/data-manager.ts:472 [filesystem] fs.writeFile(path, data)
- services/data-manager.ts:498 [sql] const q = "SELECT * FROM items WHERE ...
```

## Exit semantics: findings vs. infrastructure

The sharded run keeps security findings and audit-infrastructure failures
separate:

| Outcome | Exit | Summary JSON |
|---|---|---|
| Gate violation (real findings) | 1 | `status: complete` |
| All shards complete, no violation | 0 | `status: complete` |
| Some shard never completed | 2 | `status: incomplete`, `reason: shard_incomplete` |

A sharded run always writes a summary JSON next to the SARIF output
(`<output>.summary.json`, or `--summary <path>`) with per-shard statuses:

```json
{
  "task_id": "task-security-audit",
  "status": "incomplete",
  "reason": "shard_incomplete",
  "findings_count": 4,
  "exit_code": 2,
  "shards": [
    { "shard": 1, "files": ["src/a.ts", "src/b.ts"], "status": "complete",
      "findings": 3, "tokens": 41200, "attempts": 1 },
    { "shard": 2, "files": ["src/big.ts"], "status": "incomplete",
      "reason": "context_limit", "findings": 1, "tokens": 298000, "attempts": 4 }
  ]
}
```

Typical CI policy:

- `exit_code == 1` → block the PR (real findings, gate decided)
- `exit_code == 2` with `status == incomplete` → audit did not finish; mark
  the check as "audit incomplete" (infrastructure), re-run or investigate
  the failing shard listed in the summary
- `exit_code == 0` → pass

### `on_shard_incomplete` policies

- `bisect` (default) — split the failing shard and retry the halves, up to
  `max_splits`; shards that still fail are recorded as incomplete.
- `fail` — the first incomplete shard fails the whole run immediately with
  exit 2 (strict compliance mode).
- `pass` — keep the shard's partial findings and continue; the run is still
  marked incomplete (exit 2 when no gate fails).

## Limitations (v1)

- Shards run serially (determinism, rate limits). A `concurrency` knob may
  follow.
- The pre-scan is regex-based by design; for semantic scanning, pair
  sharding with MCP preflight checks (Semgrep, CodeQL, LSP).
- MCP tools/preflight are not available *inside* shard runs; configure
  them at the task level for the non-sharded path.
- The findings ledger, checkpoints and `--resume` apply per shard run; the
  summary JSON is the aggregate record.
