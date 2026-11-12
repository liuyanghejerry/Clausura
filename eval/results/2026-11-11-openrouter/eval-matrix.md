# Model matrix — 2026-11-11T05:43:04.937374897+00:00 (main @ 7653c87)

variant: `default` · recall = fraction of ground-truth rules reported (micro-averaged over runs); success = clean finish (gate exit codes are scenario properties).

| model | recall | success | tokens/run | LLM calls/run | time/run (s) | total tokens |
|-------|--------|---------|------------|---------------|--------------|--------------|
| `qwen/qwen3.8-flash` | 92% (37/40) | 75% | 58298 | 7.7 | 116.1 | 699579 |
| `anthropic/claude-haiku-5.5` | 90% (36/40) | 83% | 37079 | 4.6 | 25.9 | 444948 |
| `deepseek/deepseek-v4.1-flash` | 82% (33/40) | 75% | 47997 | 7.2 | 32.7 | 575962 |
| `openai/gpt-5.4-mini` | 70% (28/40) | 67% | 30405 | 6.0 | 15.1 | 364860 |
| `anthropic/claude-sonnet-5.5` | 98% (39/40) | 92% | 33985 | 4.7 | 23.2 | 407821 |

## Recall by scenario

| model | security-basics | context-stress | rust-service | web-monorepo | legacy-refactor | secret-sweep |
|-------|--------|--------|--------|--------|--------|--------|
| `qwen/qwen3.8-flash` | 100% | 0% | 100% | 100% | 100% | 50% |
| `anthropic/claude-haiku-5.5` | 100% | 0% | 100% | 100% | 100% | 0% |
| `deepseek/deepseek-v4.1-flash` | 100% | 0% | 50% | 100% | 100% | 100% |
| `openai/gpt-5.4-mini` | 100% | 0% | 50% | 50% | 100% | 100% |
| `anthropic/claude-sonnet-5.5` | 100% | 50% | 100% | 100% | 100% | 100% |

LLM output is nondeterministic: treat small-run results as indicative, not as stable rankings. Reproduce with `clausura eval --models <matrix.yaml>`; per-model reports and event logs live alongside this file.
