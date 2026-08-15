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
clausura eval --config eval.yaml --model gpt-4o
```

See `eval/README.md` for the scenario format, metric definitions, and the
before/after comparison workflow (`--baseline`).

## Metric semantics

- `success` = the run finished cleanly (no truncation / iteration cap /
  overflow). The gate exit code is deliberately not part of it: a scenario
  whose ground truth is real issues correctly exits 1.
- `recall` = ground-truth rules whose expected findings were reported.
- `mean tokens` / `mean LLM calls` = cost and iteration pressure.
- `compactions` / `spills` / `reminders` / `recovery success` = mechanism
  effects, counted from the run event log (`run-*.events.jsonl`).

## When to run

For every change that switches how the agent works — context handling,
skills, tool outputs, findings extraction — run the eval before and after
and include the comparison in the PR description.
