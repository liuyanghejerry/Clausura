# Eval report — 2026-11-11T15:44:43.948773168+00:00 (main @ 7653c87)

model: `openai/gpt-5.4-mini`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 3 | 100% | 100% | 0 | 6961 | 3.0 | 9.1 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 3 | 0% | 0% | 0 | 16449 | 8.0 | 16.7 | 0.0 | 3.0 | 1.7 | 0%
| rust-service | default | 3 | 67% | 60% | 0 | 33279 | 6.0 | 19.2 | 0.0 | 0.0 | 0.0 | 67%
| web-monorepo | default | 3 | 100% | 100% | 0 | 19652 | 4.0 | 18.1 | 0.0 | 0.0 | 0.0 | 100%
| legacy-refactor | default | 3 | 33% | 33% | 0 | 17848 | 4.0 | 16.7 | 0.0 | 0.0 | 0.0 | 33%
| secret-sweep | default | 3 | 100% | 100% | 0 | 50141 | 6.3 | 18.8 | 0.0 | 0.7 | 0.0 | 0%
