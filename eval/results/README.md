# Published model-matrix results

Public, reproducible rule-recall data for Clausura's seeded eval corpus. Each
subdirectory is one matrix run, named `<date>-<provider>/`, and contains:

- `eval-matrix.md` — the table to link from talks/PRs/READMEs.
- `eval-matrix.json` — machine-readable source of the same numbers.
- `<model-slug>/eval-report.{json,md}` — per-model reports (baselines),
  including unaggregated `run_metrics` in new reports.
- `<model-slug>/<scenario>/<variant>/run-*/` — SARIF and event logs for
  audit. Older snapshots may have incomplete artifacts; see the errata below.

Rule recall means the fraction of ground-truth **rule categories** meeting
their configured finding-count threshold. It is not per-instance or
source-location recall, and unexpected rule IDs alone do not establish
precision. Use these matrices for regression analysis, not as definitive
model leaderboards.

## Published matrices

| Date | Provider | Models | Scenarios | Runs/model | Report |
|------|----------|--------|-----------|------------|--------|
| 2026-11-11 | OpenRouter | 8 | 6 (`default` variant) | 18 (3 repeats) | [eval-matrix.md](2026-11-11-openrouter-runs3/eval-matrix.md) **(historical, pre-hardening)** |
| 2026-11-11 | OpenRouter | 5 | 6 (`default` variant) | 12 (2 repeats) | [eval-matrix.md](2026-11-11-openrouter/eval-matrix.md) |

### 2026-11-11-openrouter-runs3 (historical)

- Binary: working tree on top of `7653c87` (multi-model eval changes), as
  recorded in the report. 144 runs, 5,561,221 billed tokens in total.
- Headline: `moonshotai/kimi-k3` 93% pooled rule recall,
  `anthropic/claude-sonnet-5.5` 92%, `anthropic/claude-haiku-5.5` 90%,
  `deepseek/deepseek-v4-pro` 88%, `qwen/qwen3.8-flash` and
  `google/gemini-3.8-flash` 87%, `deepseek/deepseek-v4.1-flash` 83%,
  `openai/gpt-5.4-mini` 68%.
- Three repeats are still not a stable ranking: Sonnet moved from 100% to
  33% on `secret-sweep` and from 50% to 0% on `context-stress` versus the
  runs=2 snapshot. Use the tables to compare failure modes and cost, not to
  crown a winner.
- `context-stress` remains the discriminator: only Qwen recovered the
  seeded secret once (1 of 3 runs); every other model, Sonnet included,
  ended incomplete on all three runs under the forced-truncation budget.
- `secret-sweep` exposes several distinct failure modes beyond "missed
  issues": incomplete sweeps (DeepSeek-v4-pro found nothing in three
  truncated runs), below-threshold finding counts (the ground truth
  requires ≥10), and contract drift — Haiku reported 12 genuine secrets as
  `hardcoded-credential` instead of the expected `hardcoded-secret`, which
  scores zero rule recall despite a successful sweep.
- Strict output validation also costs recall: models emitted findings
  without required fields (`message`, `line_start`) or with out-of-schema
  severities (`high`). Event logs preserve the rejected payloads.

### Known harness errata in the 2026-11-11 snapshots

These are historical limitations of the harness that produced the files,
not edits to the recorded model behavior:

- `moonshotai-kimi-k3/context-stress/default/run-1` has no `run_end`, but
  the old harness counted it as successful. Consequently that scenario's
  reported 33% success and Kimi's reported 89% overall success are
  overestimated. Regenerate rather than comparing these percentages with
  post-hardening reports.
- Successful exit-0 runs could delete their event log before eval copied
  it. The runs3 snapshot therefore has 142 logs for 144 runs: Haiku
  `secret-sweep/run-0` and Kimi `secret-sweep/run-2` are absent.
- The old recovery path could clear `iteration_limit`, `context_limit` or
  `length` after producing valid findings JSON. For example, Qwen
  `context-stress/run-1` is marked complete by the old semantics. New runs
  preserve the original incomplete reason even when findings are recovered.
- Per-run status and errors were not stored in the JSON summaries. New
  reports include `run_metrics`, `source_tree_dirty`, SARIF and complete
  event logs so these cases remain auditable.

The historical JSON and Markdown numbers remain unedited. Do not use the
old success rates as post-fix baselines.

### 2026-11-11-openrouter (runs=2 snapshot)

- 60 runs, 2,493,170 billed tokens. Superseded by the 8-model runs=3
  matrix above; kept as a historical snapshot and as evidence of run-to-run
  variance.
- Headline at the time: Sonnet 98%, Qwen 92%, Haiku 90%, DeepSeek-flash
  82%, GPT-5.4-mini 70% pooled rule recall.

## Reproduce

Requires Clausura built from the commit recorded in the report and an API
key for every matrix entry (one OpenRouter key covers `eval/models.yaml`):

```bash
export CLAUSURA_API_KEY="$OPENROUTER_API_KEY"
clausura eval --config eval/eval.yaml --models eval/models.yaml \
  --variant default --runs 3 --out-dir eval/results/$(date +%F)-openrouter
```

Full corpus cost scales with `models × scenarios × runs`; smoke-test first
with `--scenario security-basics --runs 1`. Token totals per model are in
the matrix summary, so cost is auditable before you publish.

## Publication rules

- Commit harness output unedited. Never hand-write or "repair" recall
  numbers; a surprising result is data, not a typo. Document harness errata
  separately instead of rewriting historical reports.
- Reports showing `no tokens billed` (provider/auth failure) or `eval
  failed` must be regenerated, not published as model results.
- Record the model list and date in `eval/models.yaml` when it changes;
  providers deprecate versions, so old reports are historical snapshots,
  not live rankings.
- Small run counts are indicative only. Prefer `--runs 3` for anything
  presented as a ranking.
- Publish from a clean tree (`source_tree_dirty: false`) whenever possible,
  and include `run_metrics`, per-run SARIF and event logs. SARIF remains
  ignored for local runs; use `git add -f` only for the reviewed publication
  directory, never to sweep up historical local artifacts.

