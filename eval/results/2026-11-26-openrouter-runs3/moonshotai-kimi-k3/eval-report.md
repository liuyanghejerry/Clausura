# Eval report — 2026-11-26T06:42:54.089241353+00:00 (main @ 43ec174)

model: `moonshotai/kimi-k3`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 3 | 100% | 100% | 0 | 13630 | 4.7 | 20.8 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 3 | 67% | 100% | 0 | 20865 | 8.0 | 19.9 | 0.0 | 1.0 | 0.0 | 0%
| rust-service | default | 3 | 100% | 100% | 0 | 38245 | 5.7 | 61.8 | 0.0 | 1.0 | 0.0 | 0%
| web-monorepo | default | 3 | 100% | 100% | 0 | 39804 | 6.0 | 47.3 | 0.0 | 1.0 | 0.0 | 0%
| legacy-refactor | default | 3 | 100% | 100% | 0 | 46547 | 7.3 | 38.3 | 0.0 | 0.0 | 0.0 | 0%
| secret-sweep | default | 3 | 67% | 100% | 0 | 72525 | 9.3 | 46.6 | 0.0 | 1.0 | 0.0 | 0%
