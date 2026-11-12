# Model matrix — 2026-11-11T14:37:55.431293532+00:00 (main @ 7653c87)

variant: `default` · recall = fraction of ground-truth rules reported (micro-averaged over runs); success = clean finish (gate exit codes are scenario properties).

| model | recall | success | tokens/run | LLM calls/run | time/run (s) | total tokens |
|-------|--------|---------|------------|---------------|--------------|--------------|
| `qwen/qwen3.8-flash` | 87% (52/60) | 83% | 54204 | 7.2 | 99.9 | 975664 |
| `anthropic/claude-haiku-5.5` | 90% (54/60) | 83% | 36949 | 4.0 | 26.6 | 665089 |
| `deepseek/deepseek-v4.1-flash` | 83% (50/60) | 72% | 44399 | 6.4 | 41.5 | 799175 |
| `google/gemini-3.8-flash` | 87% (52/60) | 78% | 57954 | 8.7 | 54.6 | 1043179 |
| `openai/gpt-5.4-mini` | 68% (41/60) | 67% | 24055 | 5.2 | 16.4 | 432991 |
| `deepseek/deepseek-v4-pro` | 88% (53/60) | 67% | 33356 | 5.1 | 64.5 | 600408 |
| `moonshotai/kimi-k3` | 93% (56/60) | 89% | 25349 | 4.4 | 42.8 | 456281 |
| `anthropic/claude-sonnet-5.5` | 92% (55/60) | 83% | 32691 | 4.5 | 25.0 | 588434 |

## Recall by scenario

| model | security-basics | context-stress | rust-service | web-monorepo | legacy-refactor | secret-sweep |
|-------|--------|--------|--------|--------|--------|--------|
| `qwen/qwen3.8-flash` | 100% | 33% | 100% | 67% | 100% | 67% |
| `anthropic/claude-haiku-5.5` | 100% | 0% | 100% | 100% | 100% | 0% |
| `deepseek/deepseek-v4.1-flash` | 100% | 0% | 67% | 100% | 93% | 67% |
| `google/gemini-3.8-flash` | 100% | 0% | 100% | 100% | 73% | 67% |
| `openai/gpt-5.4-mini` | 100% | 0% | 60% | 100% | 33% | 100% |
| `deepseek/deepseek-v4-pro` | 100% | 0% | 93% | 100% | 100% | 0% |
| `moonshotai/kimi-k3` | 100% | 0% | 100% | 100% | 100% | 67% |
| `anthropic/claude-sonnet-5.5` | 100% | 0% | 100% | 100% | 100% | 33% |

LLM output is nondeterministic: treat small-run results as indicative, not as stable rankings. Reproduce with `clausura eval --models <matrix.yaml>`; per-model reports and event logs live alongside this file.
