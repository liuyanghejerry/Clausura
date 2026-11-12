# Eval report — 2026-11-11T14:37:55.434577696+00:00 (main @ 7653c87)

model: `qwen/qwen3.8-flash`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 3 | 100% | 100% | 0 | 17374 | 4.3 | 25.5 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 3 | 33% | 33% | 0 | 27686 | 8.3 | 45.1 | 0.0 | 3.3 | 0.3 | 33%
| rust-service | default | 3 | 100% | 100% | 0 | 69738 | 7.0 | 166.1 | 0.0 | 0.0 | 0.0 | 100%
| web-monorepo | default | 3 | 67% | 67% | 0 | 68221 | 6.7 | 144.6 | 0.0 | 0.0 | 0.0 | 67%
| legacy-refactor | default | 3 | 100% | 100% | 0 | 44057 | 6.0 | 118.6 | 0.0 | 0.0 | 0.0 | 100%
| secret-sweep | default | 3 | 100% | 67% | 1 | 98145 | 11.0 | 99.7 | 0.0 | 1.3 | 0.0 | 100%
