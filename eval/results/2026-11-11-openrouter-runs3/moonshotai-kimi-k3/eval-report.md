# Eval report — 2026-11-11T16:09:01.489795279+00:00 (main @ 7653c87)

model: `moonshotai/kimi-k3`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 3 | 100% | 100% | 0 | 9879 | 3.3 | 11.4 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 3 | 33% | 0% | 0 | 12746 | 6.3 | 112.2 | 0.0 | 4.3 | 0.3 | 0%
| rust-service | default | 3 | 100% | 100% | 0 | 26486 | 4.0 | 33.8 | 0.0 | 0.0 | 0.0 | 100%
| web-monorepo | default | 3 | 100% | 100% | 0 | 16368 | 3.0 | 31.1 | 0.0 | 0.0 | 0.0 | 0%
| legacy-refactor | default | 3 | 100% | 100% | 0 | 19146 | 3.7 | 32.5 | 0.0 | 0.0 | 0.0 | 0%
| secret-sweep | default | 3 | 100% | 67% | 0 | 67469 | 6.0 | 35.8 | 0.0 | 0.7 | 0.3 | 50%
