# Eval report — 2026-11-11T15:49:40.478349689+00:00 (main @ 7653c87)

model: `deepseek/deepseek-v4-pro`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 3 | 100% | 100% | 0 | 9577 | 2.7 | 15.4 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 3 | 0% | 0% | 0 | 18619 | 7.0 | 24.6 | 0.0 | 6.0 | 2.0 | 0%
| rust-service | default | 3 | 100% | 93% | 0 | 44314 | 5.0 | 138.7 | 0.0 | 0.0 | 0.0 | 100%
| web-monorepo | default | 3 | 100% | 100% | 0 | 23744 | 3.3 | 43.8 | 0.0 | 0.0 | 0.0 | 0%
| legacy-refactor | default | 3 | 100% | 100% | 0 | 24103 | 3.7 | 110.9 | 0.0 | 0.0 | 0.0 | 0%
| secret-sweep | default | 3 | 0% | 0% | 0 | 79778 | 8.7 | 53.5 | 0.0 | 2.3 | 0.7 | 0%
