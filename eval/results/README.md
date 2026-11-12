# Published model-matrix results

Public, reproducible recall data for Clausura's seeded eval corpus. Each
subdirectory is one matrix run, named `<date>-<provider>/`, and contains:

- `eval-matrix.md` — the table to link from talks/PRs/READMEs.
- `eval-matrix.json` — machine-readable source of the same numbers.
- `<model-slug>/eval-report.{json,md}` — per-model reports (baselines).
- `<model-slug>/<scenario>/<variant>/run-*/` — event logs for audit
  (`output.sarif` is also produced locally but excluded by the root
  `.gitignore`).

## Published matrices

| Date | Provider | Models | Scenarios | Runs/model | Report |
|------|----------|--------|-----------|------------|--------|
| 2026-11-11 | OpenRouter | 8 | 6 (`default` variant) | 18 (3 repeats) | [eval-matrix.md](2026-11-11-openrouter-runs3/eval-matrix.md) **(latest)** |
| 2026-11-11 | OpenRouter | 5 | 6 (`default` variant) | 12 (2 repeats) | [eval-matrix.md](2026-11-11-openrouter/eval-matrix.md) |

### 2026-11-11-openrouter-runs3 (latest)

- Binary: working tree on top of `7653c87` (multi-model eval changes), as
  recorded in the report. 144 runs, 5,561,221 billed tokens in total.
- Headline: `moonshotai/kimi-k3` 93% pooled recall,
  `anthropic/claude-sonnet-5.5` 92%, `deepseek/deepseek-v4-pro` 88%,
  `qwen/qwen3.8-flash` and `google/gemini-3.8-flash` 87%,
  `anthropic/claude-haiku-5.5` 90%, `deepseek/deepseek-v4.1-flash` 83%,
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
  scores zero recall despite a successful sweep.
- Strict output validation also costs recall: models emitted findings
  without required fields (`message`, `line_start`) or with out-of-schema
  severities (`high`). Event logs preserve the rejected payloads.

### 2026-11-11-openrouter (runs=2 snapshot)

- 60 runs, 2,493,170 billed tokens. Superseded by the 8-model runs=3
  matrix above; kept as a historical snapshot and as evidence of run-to-run
  variance.
- Headline at the time: Sonnet 98%, Qwen 92%, Haiku 90%, DeepSeek-flash
  82%, GPT-5.4-mini 70% pooled recall.

## Reproduce

Requires Clausura built from the commit recorded in the report and an API
key for every matrix entry (one OpenRouter key covers `eval/models.yaml`):

```bash
export CLAUSURA_API_KEY="$OPENROUTER_API_KEY"
clausura eval --config eval/eval.yaml --models eval/models.yaml \
  --variant default --runs 2 --out-dir eval/results/$(date +%F)-openrouter
```

Full corpus cost scales with `models × scenarios × runs`; smoke-test first
with `--scenario security-basics --runs 1`. Token totals per model are in
the matrix summary, so cost is auditable before you publish.

## Publication rules

- Commit harness output unedited. Never hand-write or "repair" recall
  numbers; a surprising result is data, not a typo.
- Reports showing `no tokens billed` (provider/auth failure) or `eval
  failed` must be regenerated, not published as model results.
- Record the model list and date in `eval/models.yaml` when it changes;
  providers deprecate versions, so old reports are historical snapshots,
  not live rankings.
- Small run counts are indicative only. Prefer `--runs 3` for anything
  presented as a ranking.

