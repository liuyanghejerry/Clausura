# Clausura Eval — effect-oriented evaluation

Unit tests verify behavior; **eval measures effect**. Each scenario couples a
fixture workspace with a task config and ground-truth expectations, runs it
against a real provider, and distills every run's outcome from the
append-only event log into comparable metrics.

## Run

```bash
export CLAUSURA_API_KEY=sk-...
clausura eval --config eval.yaml --model gpt-4o --runs 3
```

Outputs (under `eval-results/` by default):

- `eval-report.json` — machine-readable full report
- `eval-report.md` — summary table
- `<scenario>/<variant>/run-<i>.events.jsonl` — per-run event logs (audit)
- `<scenario>/<variant>/run-<i>/output.sarif` — per-run SARIF
- `eval-comparison.md` — when `--baseline` is given

## Metrics

| Metric | Meaning |
|---|---|
| success | clean finish (no truncation / iteration cap / overflow) |
| recall | ground-truth findings actually reported |
| mean tokens | billed tokens per run (cost) |
| mean LLM calls | agent iterations |
| compactions | auto-compact summaries applied |
| spills | oversized tool outputs preserved to disk |
| reminders | repeat-call advisories injected |
| recovery success | findings-recovery attempts that succeeded |

Note: **success ignores the gate exit code**. Scenarios whose ground truth is
real issues correctly exit 1 — the verdict is a scenario property; recall is
the quality metric.

## Comparing implementations

The intended workflow for an implementation switch (e.g. inline skills →
progressive disclosure, or a compaction change):

1. On the old commit: `clausura eval --config eval.yaml` → keep `eval-report.json`.
2. On the new commit: `clausura eval --config eval.yaml --baseline eval-results/eval-report.json`.
3. Read `eval-comparison.md` — deltas across success / recall / tokens /
   compactions / spills — and put the numbers in the PR description.

## Scenarios

- `security-basics` — three seeded security issues (SQL injection, hardcoded
  key, XSS); recall is the metric.
- `context-stress` — one secret near the tail of a ~330KB diff under a small
  token budget; measures whether findings survive truncation/spill, with and
  without `auto_compact`.

Adding a scenario = fixture workspace + `.clausura.yaml` + ground-truth entry
in `eval.yaml`. Workspaces are copied to a temp dir per run (with a throwaway
`git init` so `git_diff` has a HEAD) — fixtures stay pristine.

## CI

Eval costs real LLM calls and is nondeterministic, so it does not run in CI by
default. Run it locally per release/PR that changes agent behavior, and paste
the comparison into the PR.
