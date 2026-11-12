# Eval report — 2026-11-11T15:28:21.085064437+00:00 (main @ 7653c87)

model: `google/gemini-3.8-flash`

| scenario | variant | runs | success | recall | over-report | mean tokens | mean LLM calls | mean time (s) | compactions | spills | reminders | recovery success |
|----------|---------|------|---------|--------|-------------|-------------|----------------|--------------|-------------|--------|-----------|-------------------|
| security-basics | default | 3 | 100% | 100% | 0 | 10328 | 3.7 | 19.1 | 0.0 | 0.0 | 0.0 | 0%
| context-stress | default | 3 | 0% | 0% | 0 | 19029 | 8.3 | 55.2 | 0.0 | 5.0 | 0.7 | 0%
| rust-service | default | 3 | 100% | 100% | 0 | 73740 | 10.3 | 70.5 | 0.0 | 0.0 | 0.0 | 75%
| web-monorepo | default | 3 | 100% | 100% | 0 | 41491 | 6.7 | 51.9 | 0.0 | 0.0 | 0.0 | 50%
| legacy-refactor | default | 3 | 100% | 73% | 0 | 55135 | 9.7 | 66.6 | 0.0 | 0.0 | 0.0 | 67%
| secret-sweep | default | 3 | 67% | 67% | 0 | 148003 | 13.3 | 64.2 | 0.0 | 1.0 | 0.0 | 50%
