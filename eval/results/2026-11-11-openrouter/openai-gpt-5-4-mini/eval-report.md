# Eval report — 2026-11-11T06:18:02.717491259+00:00 (main @ 7653c87)

model: `openai/gpt-5.4-mini`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 2 | 100% | 100% | 0 | 7026 | 3.0 | 7.3 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 2 | 0% | 0% | 0 | 16536 | 8.0 | 13.3 | 0.0 | 3.0 | 2.0 | 0%
| rust-service | default | 2 | 50% | 50% | 0 | 44734 | 7.5 | 21.4 | 0.0 | 0.0 | 0.0 | 50%
| web-monorepo | default | 2 | 50% | 50% | 0 | 37880 | 6.5 | 16.2 | 0.0 | 0.0 | 0.0 | 50%
| legacy-refactor | default | 2 | 100% | 100% | 0 | 16836 | 4.0 | 15.1 | 0.0 | 0.0 | 0.0 | 100%
| secret-sweep | default | 2 | 100% | 100% | 0 | 59418 | 7.0 | 17.2 | 0.0 | 1.0 | 0.0 | 100%
