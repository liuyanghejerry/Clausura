# Eval report — 2026-11-11T16:21:52.678588024+00:00 (main @ 7653c87)

model: `anthropic/claude-sonnet-5.5`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 3 | 100% | 100% | 0 | 15531 | 3.3 | 11.7 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 3 | 0% | 0% | 0 | 39656 | 8.0 | 19.7 | 0.0 | 6.0 | 1.3 | 0%
| rust-service | default | 3 | 100% | 100% | 0 | 32978 | 4.0 | 33.9 | 0.0 | 0.0 | 0.0 | 0%
| web-monorepo | default | 3 | 100% | 100% | 0 | 20128 | 3.0 | 22.5 | 0.0 | 0.0 | 0.0 | 0%
| legacy-refactor | default | 3 | 100% | 100% | 0 | 33925 | 4.3 | 38.7 | 0.0 | 0.0 | 0.0 | 100%
| secret-sweep | default | 3 | 100% | 33% | 0 | 53926 | 4.3 | 23.5 | 0.0 | 1.0 | 0.0 | 0%
