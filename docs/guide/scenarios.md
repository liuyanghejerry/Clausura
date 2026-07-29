# Common Scenarios

Real-world configurations for typical use cases. Each scenario is a complete `.clausura.yaml` you can adapt.

## Scenario 1: Security Code Review

The most common use case — catch SQL injection, XSS, hardcoded secrets, and input validation gaps.

```yaml
version: "1"
task:
  name: security-review
  model: gpt-4o
  vendor: openai
  prompt_template: |
    Review the git diff for security issues. For each finding use the specified rule_id.

    ### SQL Injection
    - String concatenation in SQL queries
    - Non-parameterized database calls
    - rule_id: "sql-injection", severity: "error"

    ### Hardcoded Credentials
    - API keys, passwords, tokens in source code
    - Config files with plaintext secrets (exclude example/docs files)
    - rule_id: "hardcoded-secret", severity: "error"

    ### XSS
    - Direct user input insertion into HTML (innerHTML, document.write)
    - Unescaped template variable output
    - rule_id: "xss", severity: "error"

    ### Missing Input Validation
    - API endpoints without type/range validation
    - File upload without type/size checks
    - rule_id: "missing-validation", severity: "warning"

    ### Insecure Dependencies
    - Known-vulnerable dependency versions
    - Scripts loaded from untrusted sources
    - rule_id: "insecure-dependency", severity: "warning"

    Output a JSON object with a "findings" array. Each finding: rule_id, severity, message, evidence, location (optional).

  token_budget: 16000
  timeout_secs: 120

  gating:
    - rule: sql-injection
      description: "SQL injection is a critical vulnerability"
      min_severity: error
      max_findings: 0
      action: fail
    - rule: hardcoded-secret
      description: "No hardcoded credentials in source"
      min_severity: error
      max_findings: 0
      action: fail
    - rule: xss
      description: "XSS is a critical vulnerability"
      min_severity: error
      max_findings: 0
      action: fail
    - rule: missing-validation
      description: "All user inputs should be validated"
      min_severity: warning
      max_findings: 3
      action: warn
    - rule: insecure-dependency
      description: "Dependencies should be vetted"
      min_severity: warning
      max_findings: 3
      action: warn
```

## Scenario 2: i18n Completeness Check

Ensure translation coverage stays consistent across locales.

```yaml
version: "1"
task:
  name: i18n-check
  model: gpt-4o-mini
  vendor: openai
  prompt_template: |
    Review the git diff for internationalization issues. Locale files are in locales/ or i18n/ directories.

    Use these rule_ids:
    - "hardcoded-text": UI strings not using i18n keys (exclude comments and logs), severity: "warning"
    - "missing-translation-key": Key referenced in code but missing from locale files, severity: "error"
    - "locale-mismatch": Inconsistent key counts across locale files, severity: "warning"
    - "unused-translation-key": Key defined in locale files but never referenced, severity: "info"

  token_budget: 8000
  timeout_secs: 60

  gating:
    - rule: missing-translation-key
      description: "All referenced keys must exist in locale files"
      min_severity: error
      max_findings: 0
      action: fail
    - rule: hardcoded-text
      description: "Warn on hardcoded UI strings"
      min_severity: warning
      max_findings: 3
      action: warn
    - rule: locale-mismatch
      description: "Locale files should be consistent"
      min_severity: warning
      max_findings: 1
      action: warn
```

Note the use of `gpt-4o-mini` — classification tasks like "is this string hardcoded?" don't need a frontier model.

## Scenario 3: Architecture Compliance

Enforce layer boundaries and import rules in a layered architecture.

```yaml
version: "1"
task:
  name: architecture-check
  model: gpt-4o
  vendor: openai
  prompt_template: |
    Review the git diff for architecture violations in our layered architecture:

    Layers (outer to inner):
    - controllers/   → depends on services/
    - services/      → depends on repositories/
    - repositories/  → depends on models/ and db/
    - models/        → no internal dependencies

    Rules:
    - rule_id: "layer-violation" severity: "error"
      A module importing from a more-inner layer than it should.
      Example: controllers/ importing from repositories/ (skip services/).

    - rule_id: "circular-dependency" severity: "error"
      Two modules importing from each other.

    - rule_id: "forbidden-import" severity: "error"
      Importing from deprecated or forbidden paths.

  token_budget: 16000
  timeout_secs: 120

  gating:
    - rule: layer-violation
      description: "Must follow layer dependency direction"
      min_severity: error
      max_findings: 0
      action: fail
    - rule: circular-dependency
      description: "No circular imports"
      min_severity: error
      max_findings: 0
      action: fail
    - rule: forbidden-import
      description: "No forbidden imports"
      min_severity: error
      max_findings: 0
      action: fail
```

## Scenario 4: Vue Best Practices

Enforce Vue-specific conventions in a frontend project.

```yaml
version: "1"
task:
  name: vue-review
  model: gpt-4o-mini
  vendor: openai
  skill_prompts:
    - vue-best-practices
  prompt_template: |
    Additionally check:
    - No `any` type in TypeScript files (rule_id: "no-as-any", severity: "warning")

  token_budget: 8000

  gating:
    - rule: vue-component-name
      description: "Components must use multi-word PascalCase names"
      min_severity: warning
      max_findings: 2
      action: warn
    - rule: vue-missing-prop-validation
      description: "Props must have type declarations"
      min_severity: warning
      max_findings: 5
      action: warn
    - rule: no-as-any
      description: "Avoid TypeScript `as any`"
      min_severity: warning
      max_findings: 0
      action: fail
```

This scenario shows combining a skill (`vue-best-practices`) with a custom `prompt_template` addition. See the [Skills guide](skills.md) for more.

## Scenario 5: Multi-Dimensional Review via Parallel CI Jobs

For projects that need different review dimensions (security, i18n, architecture), run multiple Clausura tasks in parallel CI jobs:

```yaml
# .github/workflows/review.yml
name: Code Review

on: [pull_request]

jobs:
  security:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 2
      - name: Security Review
        run: clausura run -c .clausura/security.yaml
        env:
          CLAUSURA_API_KEY: ${{ secrets.LLM_API_KEY }}

  i18n:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 2
      - name: i18n Check
        run: clausura run -c .clausura/i18n.yaml
        env:
          CLAUSURA_API_KEY: ${{ secrets.LLM_API_KEY }}

  architecture:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 2
      - name: Architecture Check
        run: clausura run -c .clausura/architecture.yaml
        env:
          CLAUSURA_API_KEY: ${{ secrets.LLM_API_KEY }}
```

Each job uses its own config file. This gives you:
- Independent gating rules per dimension
- Different models per job (fast model for i18n, strong model for security)
- Parallel execution for speed
- Separate SARIF outputs per dimension

## Scenario 6: Smart Gating with Custom Tool Execution

For reviews that need to run project-specific tools (linters, test suites) before analysis:

```yaml
version: "1"
task:
  name: smart-gate
  model: gpt-4o
  vendor: openai
  prompt_template: |
    Run `cargo clippy --message-format=json` first using shell_exec to gather lint results.
    Then review the git diff and clippy output together.
    Focus on issues that clippy can't catch: logic errors, unsafe patterns, missing error handling.

    rule_ids:
    - "unsafe-block", severity: "error"
    - "missing-error-handling", severity: "warning"
    - "panic-in-library", severity: "error"

  tool_allowlist:
    - cargo clippy
    - cargo check
  token_budget: 16000
  shell_timeout_secs: 180

  gating:
    - rule: unsafe-block
      max_findings: 0
      action: fail
    - rule: panic-in-library
      max_findings: 0
      action: fail
    - rule: missing-error-handling
      max_findings: 5
      action: warn
```

This lets the agent gather data from project tooling before making its analysis, combining traditional static analysis with LLM reasoning.

## Selecting the Right Model

| Task Type | Recommended Model | Why |
|-----------|------------------|-----|
| Security review | `gpt-4o` / `claude-sonnet-4` | Needs deep reasoning to spot subtle vulnerabilities |
| Style/lint checks | `gpt-4o-mini` / `claude-haiku` | Pattern matching, not reasoning |
| Architecture analysis | `gpt-4o` / `claude-sonnet-4` | Requires understanding project structure |
| i18n coverage | `gpt-4o-mini` / `claude-haiku` | Classification task, fast and cheap |
| Local dev / offline | Ollama + `llama3.2` or `qwen2.5-coder` | No API cost, suitable for pre-commit hooks |

→ [LLM provider setup](llm-providers.md)

## Next

→ [Set up CI integration](ci-integration.md)
→ [Learn about skills](skills.md)
