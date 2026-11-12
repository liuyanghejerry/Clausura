# Eval report — 2026-11-11T06:21:04.108481675+00:00 (main @ 7653c87)

model: `anthropic/claude-sonnet-5.5`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 2 | 100% | 100% | 0 | 15426 | 3.5 | 11.3 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 2 | 50% | 50% | 0 | 37822 | 8.5 | 20.5 | 0.0 | 5.5 | 1.0 | 33%
| rust-service | default | 2 | 100% | 100% | 0 | 20967 | 3.0 | 25.9 | 0.0 | 0.0 | 0.0 | 0%
| web-monorepo | default | 2 | 100% | 100% | 0 | 20138 | 3.0 | 23.1 | 0.0 | 0.0 | 0.0 | 0%
| legacy-refactor | default | 2 | 100% | 100% | 0 | 23000 | 3.0 | 24.3 | 0.0 | 0.0 | 0.0 | 0%
| secret-sweep | default | 2 | 100% | 100% | 0 | 86558 | 7.0 | 34.3 | 0.0 | 1.0 | 0.0 | 100%
