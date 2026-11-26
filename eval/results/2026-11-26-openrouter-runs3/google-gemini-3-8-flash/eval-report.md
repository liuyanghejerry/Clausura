# Eval report — 2026-11-26T06:09:03.553477135+00:00 (main @ 43ec174)

model: `google/gemini-3.8-flash`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 3 | 100% | 100% | 0 | 13515 | 6.7 | 26.7 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 3 | 0% | 0% | 0 | 16055 | 10.0 | 36.1 | 0.0 | 1.0 | 0.0 | 0%
| rust-service | default | 3 | 0% | 0% | 0 | 78720 | 13.0 | 68.2 | 0.0 | 1.0 | 0.0 | 0%
| web-monorepo | default | 3 | 0% | 53% | 0 | 72953 | 13.0 | 56.6 | 0.0 | 1.0 | 0.0 | 0%
| legacy-refactor | default | 3 | 0% | 67% | 0 | 56818 | 13.0 | 60.5 | 0.0 | 0.0 | 0.0 | 0%
| secret-sweep | default | 3 | 0% | 0% | 0 | 76698 | 13.0 | 52.5 | 0.0 | 1.0 | 0.0 | 0%
