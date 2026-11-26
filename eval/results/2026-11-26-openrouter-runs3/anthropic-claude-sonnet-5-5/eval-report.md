# Eval report — 2026-11-26T06:54:39.043477055+00:00 (main @ 43ec174)

model: `anthropic/claude-sonnet-5.5`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 3 | 100% | 100% | 0 | 18076 | 4.3 | 13.2 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 3 | 67% | 100% | 0 | 26931 | 7.3 | 15.4 | 0.0 | 1.0 | 0.0 | 0%
| rust-service | default | 3 | 100% | 100% | 0 | 26555 | 3.7 | 19.8 | 0.0 | 1.0 | 0.0 | 0%
| web-monorepo | default | 3 | 100% | 100% | 0 | 39825 | 5.0 | 31.8 | 0.0 | 1.0 | 0.0 | 0%
| legacy-refactor | default | 3 | 100% | 100% | 0 | 21467 | 3.3 | 25.9 | 0.0 | 0.0 | 0.0 | 0%
| secret-sweep | default | 3 | 100% | 100% | 1 | 33094 | 4.7 | 26.0 | 0.0 | 0.3 | 0.0 | 0%
