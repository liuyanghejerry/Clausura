# Eval report — 2026-11-26T06:24:06.099117824+00:00 (main @ 43ec174)

model: `openai/gpt-5.4-mini`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 3 | 100% | 100% | 0 | 7905 | 4.0 | 8.4 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 3 | 100% | 100% | 0 | 9631 | 5.7 | 9.7 | 0.0 | 1.0 | 0.0 | 0%
| rust-service | default | 3 | 100% | 100% | 0 | 26156 | 5.0 | 17.9 | 0.0 | 1.0 | 0.0 | 0%
| web-monorepo | default | 3 | 67% | 73% | 0 | 42059 | 7.7 | 21.3 | 0.0 | 1.0 | 0.0 | 0%
| legacy-refactor | default | 3 | 100% | 100% | 0 | 24277 | 5.7 | 21.0 | 0.0 | 0.0 | 0.0 | 0%
| secret-sweep | default | 3 | 100% | 100% | 0 | 18635 | 4.3 | 12.3 | 0.0 | 1.0 | 0.0 | 0%
