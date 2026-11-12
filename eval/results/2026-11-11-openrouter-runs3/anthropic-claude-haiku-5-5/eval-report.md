# Eval report — 2026-11-11T15:07:54.984497692+00:00 (main @ 7653c87)

model: `anthropic/claude-haiku-5.5`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 3 | 100% | 100% | 0 | 13142 | 3.0 | 10.5 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 3 | 0% | 0% | 0 | 39962 | 8.0 | 26.3 | 0.0 | 6.3 | 2.0 | 0%
| rust-service | default | 3 | 100% | 100% | 0 | 31583 | 3.3 | 31.1 | 0.0 | 0.0 | 0.0 | 100%
| web-monorepo | default | 3 | 100% | 100% | 0 | 27480 | 3.3 | 27.5 | 0.0 | 0.0 | 0.0 | 100%
| legacy-refactor | default | 3 | 100% | 100% | 0 | 19735 | 3.0 | 25.0 | 0.0 | 0.0 | 0.0 | 0%
| secret-sweep | default | 3 | 100% | 0% | 0 | 89794 | 3.3 | 39.1 | 0.0 | 0.7 | 0.0 | 0%
