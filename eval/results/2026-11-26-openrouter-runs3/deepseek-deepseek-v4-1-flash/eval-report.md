# Eval report — 2026-11-26T06:00:31.699378120+00:00 (main @ 43ec174)

model: `deepseek/deepseek-v4.1-flash`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 3 | 100% | 100% | 0 | 12641 | 4.0 | 13.0 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 3 | 33% | 67% | 0 | 21218 | 8.0 | 18.7 | 0.0 | 1.7 | 0.7 | 0%
| rust-service | default | 3 | 100% | 100% | 1 | 55550 | 7.0 | 24.6 | 0.0 | 1.0 | 0.0 | 0%
| web-monorepo | default | 3 | 67% | 80% | 0 | 65481 | 8.3 | 36.6 | 0.0 | 1.0 | 0.0 | 0%
| legacy-refactor | default | 3 | 100% | 93% | 0 | 41231 | 6.0 | 43.8 | 0.0 | 0.0 | 0.0 | 0%
| secret-sweep | default | 3 | 100% | 100% | 0 | 54574 | 6.7 | 32.5 | 0.0 | 1.0 | 0.0 | 0%
