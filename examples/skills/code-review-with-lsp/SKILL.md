---
name: code-review-with-lsp
description: >
  Code review with LSP-powered code intelligence. Uses MCP tools
  (diagnostics, hover, references, definition, symbols) for semantic
  code understanding, not just text grep.
---

# Code Review with LSP

> This skill works with `mcp_servers` + `preflight` in `.clausura.yaml`.
> Preflight diagnostics are collected automatically; this skill guides
> the agent to use LSP tools for deeper semantic analysis.

## Review Workflow

For each file or diff hunk under review:

### Step 1. Check diagnostics

Call `mcp__<server>__get_diagnostics` (or equivalent) for the file.
Preflight results are already in context — but if diagnostics were not
automatically collected, run them now.

Record every diagnostic as a finding:
- `rule_id`: `lsp-<diagnostic-type>` (e.g. `lsp-type-mismatch`)
- `severity`: map 1:1 from diagnostic severity
- `message`: keep the original diagnostic message verbatim

### Step 2. Understand unfamiliar symbols

When you encounter a symbol, function, or type you need to understand:

1. **hover** — get type signature and docs: `mcp__<server>__hover`
2. **definition** — jump to the definition: `mcp__<server>__goto_definition`
3. **references** — check all callers: `mcp__<server>__find_references`

### Step 3. Check for cascading impact

If a change affects a public API or core type:
- Use `mcp__<server>__find_references` to find all callers
- Check if those callers handle the new signature correctly

## Finding Severity Mapping

| LSP Severity | Finding Severity | When |
|--------------|-----------------|------|
| Error | `error` | Compile error, type mismatch, undefined reference |
| Warning | `warning` | Deprecation, unused variable, non-fatal lints |
| Information | `info` | Style hints, auto-fix suggestions |
| Hint | `hint` | Minor suggestions, organizational hints |

## Example Rule IDs for Gating

```yaml
gating:
  # LSP diagnostics (rule_id_prefix: "lsp-")
  - rule: lsp-type-mismatch
    min_severity: error
    max_findings: 0
    action: fail
  - rule: lsp-compile-error
    min_severity: error
    max_findings: 0
    action: fail
```
