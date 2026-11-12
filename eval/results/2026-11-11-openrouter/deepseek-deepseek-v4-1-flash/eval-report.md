# Eval report — 2026-11-11T06:11:29.536669614+00:00 (main @ 7653c87)

model: `deepseek/deepseek-v4.1-flash`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 2 | 100% | 100% | 0 | 15975 | 4.5 | 10.3 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 2 | 0% | 0% | 0 | 25978 | 8.5 | 17.1 | 0.0 | 4.0 | 1.5 | 0%
| rust-service | default | 2 | 50% | 50% | 0 | 62321 | 8.0 | 51.0 | 0.0 | 1.0 | 0.0 | 0%
| web-monorepo | default | 2 | 100% | 100% | 0 | 38070 | 5.0 | 44.9 | 0.0 | 0.0 | 0.0 | 100%
| legacy-refactor | default | 2 | 100% | 100% | 0 | 49418 | 6.5 | 33.2 | 0.0 | 0.0 | 0.0 | 0%
| secret-sweep | default | 2 | 100% | 100% | 0 | 96220 | 10.5 | 39.8 | 0.0 | 2.0 | 0.5 | 100%
