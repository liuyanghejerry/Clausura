# Model matrix — 2026-11-26T05:27:45.725683544+00:00 (main @ 43ec174)

source tree dirty: `false` · variant: `default` · rule recall = fraction of ground-truth rule categories meeting their finding-count threshold (micro-averaged over runs), not per-instance or location recall; success = clean finish (gate exit codes are scenario properties).

| model | rule recall | success | tokens/run | LLM calls/run | time/run (s) | total tokens |
|-------|--------|---------|------------|---------------|--------------|--------------|
| `qwen/qwen3.8-flash` | 68% (41/60) | 33% | 74768 | 10.6 | 85.3 | 1345833 |
| `anthropic/claude-haiku-5.5` | 98% (59/60) | 94% | 30108 | 4.4 | 23.3 | 541952 |
| `deepseek/deepseek-v4.1-flash` | 92% (55/60) | 83% | 41783 | 6.7 | 28.2 | 752087 |
| `google/gemini-3.8-flash` | 45% (27/60) | 17% | 52460 | 11.4 | 50.1 | 944279 |
| `openai/gpt-5.4-mini` | 93% (56/60) | 94% | 21444 | 5.4 | 15.1 | 385990 |
| `deepseek/deepseek-v4-pro` | 92% (55/60) | 67% | 48084 | 7.0 | 47.5 | 865518 |
| `moonshotai/kimi-k3` | 100% (60/60) | 89% | 38603 | 6.8 | 39.1 | 694850 |
| `anthropic/claude-sonnet-5.5` | 100% (60/60) | 94% | 27658 | 4.7 | 22.0 | 497843 |

## Recall by scenario

| model | security-basics | context-stress | rust-service | web-monorepo | legacy-refactor | secret-sweep |
|-------|--------|--------|--------|--------|--------|--------|
| `qwen/qwen3.8-flash` | 100% | 33% | 67% | 60% | 67% | 67% |
| `anthropic/claude-haiku-5.5` | 100% | 100% | 100% | 93% | 100% | 100% |
| `deepseek/deepseek-v4.1-flash` | 100% | 67% | 100% | 80% | 93% | 100% |
| `google/gemini-3.8-flash` | 100% | 0% | 0% | 53% | 67% | 0% |
| `openai/gpt-5.4-mini` | 100% | 100% | 100% | 73% | 100% | 100% |
| `deepseek/deepseek-v4-pro` | 100% | 0% | 100% | 100% | 100% | 33% |
| `moonshotai/kimi-k3` | 100% | 100% | 100% | 100% | 100% | 100% |
| `anthropic/claude-sonnet-5.5` | 100% | 100% | 100% | 100% | 100% | 100% |

LLM output is nondeterministic: treat small-run results as indicative, not as stable rankings. Reproduce with `clausura eval --models <matrix.yaml>`; per-model reports and event logs live alongside this file.
