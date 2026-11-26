# Eval report — 2026-11-26T06:28:38.709810411+00:00 (main @ 43ec174)

model: `deepseek/deepseek-v4-pro`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 3 | 100% | 100% | 0 | 16974 | 5.0 | 13.0 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 3 | 0% | 0% | 0 | 22157 | 7.7 | 13.3 | 0.0 | 2.0 | 0.3 | 0%
| rust-service | default | 3 | 67% | 100% | 1 | 73866 | 8.0 | 84.8 | 0.0 | 1.0 | 0.0 | 0%
| web-monorepo | default | 3 | 100% | 100% | 0 | 44808 | 5.7 | 50.8 | 0.0 | 1.0 | 0.0 | 0%
| legacy-refactor | default | 3 | 100% | 100% | 0 | 46074 | 6.0 | 71.3 | 0.0 | 0.0 | 0.0 | 0%
| secret-sweep | default | 3 | 33% | 33% | 0 | 84627 | 9.7 | 51.7 | 0.0 | 1.0 | 0.0 | 0%
