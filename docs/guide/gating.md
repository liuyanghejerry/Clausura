# Gating Rules

Gating rules are the deterministic pass/fail criteria that make Clausura trustworthy in CI. After the LLM produces findings, the rule engine evaluates them with pure counting logic — no AI, no heuristics, no ambiguity.

## How Rules Work

Each rule defines a threshold: "for findings matching this ID at this severity or above, allow at most N occurrences." When the count exceeds the threshold, the rule's action is triggered.

```
For each gating rule:
  1. Collect findings where finding.rule_id == rule.rule
  2. Filter to those where finding.severity >= rule.min_severity
  3. Count remaining → N
  4. If N > rule.max_findings → apply rule.action
```

## Rule Schema

```yaml
gating:
  - rule: sql-injection            # Rule ID (matches finding.rule_id)
    description: "No SQL injection" # Human-readable description
    min_severity: error            # Severity threshold: hint | info | warning | error
    max_findings: 0                # Maximum allowed count
    action: fail                   # fail | warn | ignore
```

### `rule`

The rule ID. This must match the `rule_id` in the LLM's findings output. If no findings have a matching `rule_id`, the count is 0 (rule passes).

### `min_severity`

The minimum severity level to count. Severity levels, from lowest to highest:

| Level | Typical use |
|-------|-------------|
| `hint` | Style suggestions, minor improvements |
| `info` | Informational notes, things to be aware of |
| `warning` | Best practice violations, potential issues |
| `error` | Definite problems that should block merge |

A rule with `min_severity: warning` counts findings at `warning` and `error` levels, but not `info` or `hint`.

### `max_findings`

The maximum number of matching findings allowed. `0` means "zero tolerance" — any finding with this rule_id at this severity or above triggers the action.

### `action`

| Action | Exit code | Effect |
|--------|-----------|--------|
| `fail` | 1 | Pipeline fails. Block merge. |
| `warn` | 0 | Warning printed to log. Pipeline passes. |
| `ignore` | 0 | No effect. Useful for temporarily disabling a rule. |

## Designing Effective Rules

### Pattern 1: Zero-Tolerance Critical Issues

For security vulnerabilities and other issues you never want to ship:

```yaml
gating:
  - rule: sql-injection
    min_severity: error
    max_findings: 0
    action: fail
  - rule: hardcoded-secret
    min_severity: error
    max_findings: 0
    action: fail
  - rule: xss
    min_severity: error
    max_findings: 0
    action: fail
```

### Pattern 2: Threshold-Based Warnings

For issues that are concerning in aggregate but acceptable in small numbers:

```yaml
gating:
  - rule: missing-validation
    min_severity: warning
    max_findings: 3
    action: warn
  - rule: complex-function
    min_severity: warning
    max_findings: 5
    action: warn
```

### Pattern 3: Fail on Excessive Warnings

Warn on a few, fail when it gets out of hand:

```yaml
gating:
  - rule: no-as-any
    min_severity: warning
    max_findings: 2
    action: warn
  - rule: no-as-any
    min_severity: warning
    max_findings: 10
    action: fail
```

### Pattern 4: Gradual Adoption

When introducing Clausura to an existing codebase, start permissive and tighten over time:

```yaml
# Week 1: Log everything, fail nothing
gating:
  - rule: sql-injection
    min_severity: error
    max_findings: 0
    action: warn

# Week 3: After fixing existing issues
gating:
  - rule: sql-injection
    min_severity: error
    max_findings: 0
    action: fail

# Week 5: Add more rules
gating:
  - rule: sql-injection
    min_severity: error
    max_findings: 0
    action: fail
  - rule: hardcoded-secret
    min_severity: error
    max_findings: 0
    action: fail
```

## Rule Evaluation Order

Rules are evaluated in the order they appear in the YAML file. **All rules are evaluated** — Clausura does not stop at the first violation. The final exit code is the maximum across all rules:

- If any rule produces `fail` → exit code 1
- If no `fail` but at least one `warn` → exit code 0 (with warnings logged)
- If all rules pass → exit code 0

This means you can have multiple `fail` rules and multiple `warn` rules in the same config, and all will be reported.

## Matching `rule_id` to Findings

The `rule` field in your gating config must match the `rule_id` in the LLM's findings. This is the contract between your prompt and your gates.

**Prompt tells the LLM what `rule_id` to use:**

```yaml
prompt_template: |
  For SQL injection issues, use rule_id: "sql-injection"
  For hardcoded secrets, use rule_id: "hardcoded-secret"
```

**Gating tells Clausura what to do with those findings:**

```yaml
gating:
  - rule: sql-injection
    max_findings: 0
    action: fail
  - rule: hardcoded-secret
    max_findings: 0
    action: fail
```

If the LLM produces a finding with a `rule_id` that has no matching gating rule, that finding is **not counted against any gate** — it appears in the SARIF output but doesn't affect the pass/fail decision.

## Severity Mapping

The LLM assigns a severity to each finding. Your gating rules filter by severity threshold. Make sure your prompt teaches the LLM a consistent severity scheme:

```yaml
prompt_template: |
  Severity guidelines:
  - error:   Exploitable vulnerabilities, will cause data loss or security breach
  - warning: Best practice violations that could lead to problems
  - info:    Style inconsistencies, minor improvements
  - hint:    Suggestions, nice-to-haves
```

## Tips

1. **Start with `max_findings: 0` and `action: warn`.** Run Clausura for a week in warn-only mode. Review the SARIF output to calibrate your thresholds. Then switch to `action: fail`.

2. **Use `ignore` instead of deleting rules.** When you need to temporarily disable a rule (e.g., during a refactor), change `action` to `ignore` rather than removing the rule. This keeps the rule visible in config and makes it easy to re-enable.

3. **Keep rule_ids stable.** If you rename a `rule_id` in your prompt, update the corresponding gating rule at the same time. Mismatched `rule_id`s silently produce zero-count matches.

4. **One rule per concern.** Avoid compound rules like "check-for-sql-injection-and-xss". The rule engine matches by exact `rule_id`, so each concern needs its own rule.

5. **Review SARIF for un-gated findings.** Findings with `rule_id`s that have no matching gating rule appear in the SARIF output but don't affect the verdict. Regularly review SARIF to see if you're missing gates for important issue types.

## Next

→ [Reuse community skills](skills.md)
→ [See complete scenario configs](scenarios.md)
