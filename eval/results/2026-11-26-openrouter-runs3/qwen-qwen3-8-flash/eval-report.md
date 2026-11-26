# Eval report — 2026-11-26T05:27:45.733201299+00:00 (main @ 43ec174)

model: `qwen/qwen3.8-flash`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 3 | 100% | 100% | 0 | 21144 | 6.0 | 34.1 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 3 | 0% | 33% | 0 | 24483 | 8.7 | 24.7 | 0.0 | 1.0 | 0.0 | 0%
| rust-service | default | 3 | 33% | 67% | 0 | 103182 | 12.0 | 148.1 | 0.0 | 1.0 | 0.0 | 0%
| web-monorepo | default | 3 | 0% | 60% | 0 | 113889 | 12.7 | 84.0 | 0.0 | 1.0 | 0.0 | 0%
| legacy-refactor | default | 3 | 33% | 67% | 0 | 83933 | 12.0 | 117.0 | 0.0 | 0.0 | 0.0 | 0%
| secret-sweep | default | 3 | 33% | 67% | 0 | 101980 | 12.0 | 103.7 | 0.0 | 0.7 | 0.0 | 0%
