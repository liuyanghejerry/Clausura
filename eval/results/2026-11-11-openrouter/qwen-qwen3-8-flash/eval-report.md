# Eval report — 2026-11-11T05:43:04.940223513+00:00 (main @ 7653c87)

model: `qwen/qwen3.8-flash`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 2 | 100% | 100% | 0 | 21158 | 5.0 | 35.9 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 2 | 0% | 0% | 0 | 28026 | 8.5 | 56.1 | 0.0 | 4.5 | 0.0 | 0%
| rust-service | default | 2 | 100% | 100% | 0 | 110676 | 9.5 | 259.4 | 0.0 | 0.0 | 0.0 | 100%
| web-monorepo | default | 2 | 100% | 100% | 0 | 82708 | 8.0 | 153.7 | 0.0 | 0.0 | 0.0 | 67%
| legacy-refactor | default | 2 | 100% | 100% | 0 | 42524 | 6.0 | 99.2 | 0.0 | 0.0 | 0.0 | 0%
| secret-sweep | default | 2 | 50% | 50% | 0 | 64699 | 9.0 | 92.3 | 0.0 | 2.0 | 1.0 | 50%
