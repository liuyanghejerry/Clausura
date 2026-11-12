# Eval report — 2026-11-11T15:15:54.112946010+00:00 (main @ 7653c87)

model: `deepseek/deepseek-v4.1-flash`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 3 | 100% | 100% | 0 | 11096 | 3.0 | 12.2 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 3 | 0% | 0% | 0 | 22360 | 8.0 | 17.6 | 0.0 | 4.3 | 1.0 | 0%
| rust-service | default | 3 | 67% | 67% | 0 | 84958 | 9.0 | 78.3 | 0.0 | 0.7 | 0.0 | 50%
| web-monorepo | default | 3 | 100% | 100% | 0 | 38050 | 4.7 | 52.2 | 0.0 | 0.0 | 0.0 | 100%
| legacy-refactor | default | 3 | 100% | 93% | 0 | 41833 | 5.7 | 42.6 | 0.0 | 0.0 | 0.0 | 100%
| secret-sweep | default | 3 | 67% | 67% | 0 | 68095 | 8.0 | 45.7 | 0.0 | 2.3 | 0.7 | 33%
