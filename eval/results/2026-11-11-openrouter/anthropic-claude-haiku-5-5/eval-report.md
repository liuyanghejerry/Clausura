# Eval report — 2026-11-11T06:06:18.762937143+00:00 (main @ 7653c87)

model: `anthropic/claude-haiku-5.5`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 2 | 100% | 100% | 0 | 13032 | 3.0 | 9.3 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 2 | 0% | 0% | 0 | 29716 | 7.5 | 16.7 | 0.0 | 5.5 | 1.5 | 0%
| rust-service | default | 2 | 100% | 100% | 0 | 22986 | 3.0 | 26.0 | 0.0 | 0.0 | 0.0 | 0%
| web-monorepo | default | 2 | 100% | 100% | 0 | 39195 | 4.0 | 32.4 | 0.0 | 0.0 | 0.0 | 50%
| legacy-refactor | default | 2 | 100% | 100% | 0 | 52993 | 5.0 | 49.6 | 0.0 | 0.0 | 0.0 | 50%
| secret-sweep | default | 2 | 100% | 0% | 0 | 64552 | 5.0 | 21.1 | 0.0 | 1.0 | 0.0 | 0%
