# Effect-Oriented Evaluation (`clausura eval`)

Behavior tests assert what code *does*. `clausura eval` measures what a
change *achieves*: success, recall, token cost, and the effect of each
mechanism (compaction, spill, repeat-call reminders, findings recovery).

## Concepts

- **Scenario** — a fixture workspace + task config + ground-truth rules.
- **Variant** — a partial YAML override merged over the scenario config
  (e.g. `auto_compact: true`), for A/B comparison.
- **Run** — one execution of one variant against a real provider.
  LLM output is nondeterministic, so every variant runs `runs` times
  (default 3) and metrics are aggregated.

## Usage

```bash
export CLAUSURA_API_KEY=sk-...
clausura eval --config eval.yaml --model gpt-5.4-mini
```

To compare several models on the same corpus, use a model matrix:

```bash
clausura eval --config eval/eval.yaml --models eval/models.yaml \
  --variant default --runs 3 --out-dir eval-results/matrix
```

This writes `eval-matrix.{json,md}` (cross-model rule-recall summary and
per-scenario table) plus one full report per model. See `eval/README.md`
for the matrix format and `eval/results/` for published data.

See `eval/README.md` for the scenario format, metric definitions, and the
before/after comparison workflow (`--baseline`).

## Metric semantics

- `success` = the executor reports a clean finish (no truncation / iteration
  cap / overflow). The gate exit code is deliberately not part of it: a
  scenario whose ground truth is real issues correctly exits 1.
- `rule recall` = ground-truth rule categories whose configured finding-count
  thresholds were met. This is not per-instance or source-location recall.
- `mean tokens` / `mean LLM calls` = cost and iteration pressure.
- `compactions` / `spills` / `reminders` / `recovery success` = mechanism
  effects, counted from preserved run event logs (`run-*.events.jsonl`).
- `run_metrics` = unaggregated per-run status, errors and mechanism metrics;
  aggregate tables alone are not the audit record.

## When to run

For every change that switches how the agent works — context handling,
skills, tool outputs, findings extraction — run the eval before and after
and include the comparison in the PR description.
