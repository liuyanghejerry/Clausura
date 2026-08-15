# Eval fixture workspace

Seed data for the `web-monorepo` eval scenario. The seeded issues are
intentional — do not "fix" them.

- packages/shared — utilities (console.log, `any` casts)
- packages/api — Express API (SQL injection, hardcoded secrets)
- packages/web — React frontend (XSS, console.log, `any` casts)
