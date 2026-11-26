# Eval report — 2026-11-26T05:53:31.014005845+00:00 (main @ 43ec174)

model: `anthropic/claude-haiku-5.5`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 3 | 100% | 100% | 0 | 16721 | 4.0 | 11.9 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 3 | 67% | 100% | 0 | 24425 | 6.7 | 14.8 | 0.0 | 1.3 | 0.0 | 0%
| rust-service | default | 3 | 100% | 100% | 0 | 49203 | 4.7 | 38.6 | 0.0 | 1.0 | 0.0 | 0%
| web-monorepo | default | 3 | 100% | 93% | 0 | 27522 | 3.3 | 23.7 | 0.0 | 1.0 | 0.0 | 0%
| legacy-refactor | default | 3 | 100% | 100% | 0 | 17180 | 3.0 | 21.4 | 0.0 | 0.0 | 0.0 | 0%
| secret-sweep | default | 3 | 100% | 100% | 0 | 45600 | 5.0 | 29.4 | 0.0 | 1.0 | 0.0 | 0%
