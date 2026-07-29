# Troubleshooting

Common issues, error codes, and debugging strategies for Clausura.

## Exit Codes

| Code | Meaning | Typical causes |
|------|---------|---------------|
| 0 | Pass | All gating rules satisfied. |
| 1 | Fail | One or more rules with `action: fail` were violated. Check the log output for violation details. |
| 2 | Error | Runtime failure: LLM provider error, timeout, incomplete run (`on_incomplete: fail`), max total tokens reached. |
| 3 | Config | Invalid `.clausura.yaml`. Run `clausura run --validate-config` to get details. |

## Common Issues

### "Authentication failed" (exit code 2)

**Symptom:** Run fails immediately with an auth error.

**Causes and fixes:**

1. **Missing API key.**
   ```bash
   # Ensure the key is set
   export CLAUSURA_API_KEY=sk-...
   clausura run
   ```

2. **Wrong vendor.** If you're using Anthropic with an OpenAI key (or vice versa):
   ```yaml
   vendor: anthropic    # Make sure this matches your key type
   ```

3. **Custom auth header.** If your enterprise LLM uses a non-standard header:
   ```yaml
   vendor:
     type: custom
     base_url: "https://llm.internal/v1"
     auth_header: "X-API-Key"    # Match what your endpoint expects
   ```

4. **Custom API key env var.** If you set `api_key_env`:
   ```yaml
   vendor:
     api_key_env: "INTERNAL_LLM_KEY"
   ```
   Then export that variable, not `CLAUSURA_API_KEY`:
   ```bash
   export INTERNAL_LLM_KEY=sk-...
   ```

### "Agent run incomplete" (exit code 2 with `on_incomplete: fail`)

**Symptom:** Run ends with exit code 2 and the error message includes "Agent run incomplete."

**Causes and fixes:**

1. **Context exhausted.** The conversation grew beyond `token_budget` and couldn't be truncated further.
   - Increase `token_budget` (e.g., 32000 → 64000)
   - Reduce the size of the input (narrower diff, shorter prompt)
   - Increase `max_iterations` if truncation keeps happening

2. **Iteration limit reached.** The agent used all `max_iterations` without producing a final answer.
   - Increase `max_iterations` (e.g., 10 → 15)
   - Simplify the task — fewer, more focused checks per run
   - Check if the LLM is stuck in a tool-call loop (see below)

3. **`max_total_tokens` reached.** Cumulative billing exceeded the cap.
   - Increase `max_total_tokens` or remove it
   - Use a cheaper model
   - Reduce `token_budget` to limit per-request cost

### "Task timeout exceeded" (exit code 2)

**Symptom:** Run fails with a timeout error.

- Increase `timeout_secs` (e.g., 300 → 600)
- Use a faster model
- Reduce the scope of the review
- Check if `shell_exec` commands are hanging — adjust `shell_timeout_secs`

### Agent stuck in a tool-call loop

**Symptom:** The run uses all `max_iterations` repeatedly calling tools without producing findings.

This happens when the LLM calls a tool, isn't satisfied with the result, calls it again with slightly different arguments, and repeats.

**Fixes:**
- Make your prompt more directive: "After running tools, output your findings. Do not re-run tools."
- Reduce `max_iterations` to force earlier termination
- Check if the tool output is confusing the LLM (e.g., truncated output, unexpected format)

### "No findings" but I expected some

**Symptom:** Clausura passes (exit 0) but you know there should be findings.

**Debug steps:**

1. **Check the SARIF output.** Even empty runs produce valid SARIF. The file may contain partial output even in "clean" runs.

2. **Verify the prompt is clear.** If the LLM doesn't understand what to look for, it produces nothing. Test your prompt interactively (e.g., in ChatGPT/Claude) before putting it in Clausura.

3. **Check `rule_id` matching.** If the LLM uses a different `rule_id` than your gating rules expect, findings are produced but not counted. Review `clausura-output.sarif` for unmatched findings.

4. **Run with more iterations.** The agent might need more tool calls to gather context. Increase `max_iterations`.

5. **Check if context is being truncated.** If `token_budget` is too low for your diff size, the agent might lose important context.

### Schema mismatch errors

**Symptom:** Error message says "X of Y finding(s) failed to match the Finding schema."

The LLM produced JSON that doesn't match Clausura's expected finding format. Required fields: `rule_id`, `severity`, `message`, `evidence`. Optional: `location`.

**Fixes:**
- Include an example finding in your `prompt_template` showing the exact format
- Use a more capable model that follows JSON schemas reliably
- Check if the LLM is wrapping findings in extra nesting or using wrong field names

### "Command not in allowlist" (shell_exec error)

**Symptom:** The agent tries to run a command but gets "Command not in allowlist."

- Check your `tool_allowlist` — the command must be explicitly listed
- Remember that allowlist entries are argv prefixes: `"git status"` allows `git status --short` but NOT `git log`
- The first token matches by basename: `"/usr/bin/git"` satisfies a `"git"` rule

### "Config error" (exit code 3)

**Symptom:** Clausura exits immediately with exit code 3.

Run the config validator for detailed error messages:

```bash
clausura run --validate-config
```

Or use dry-run to see how Clausura interprets your config:

```bash
clausura run --dry-run
```

## Debugging Strategies

### 1. Use `--log-format pretty`

By default, logs are JSON (designed for CI log processors). For local debugging:

```bash
clausura run --log-format pretty
```

### 2. Validate config without running

```bash
clausura run --validate-config  # Check config syntax
clausura run --dry-run          # Show the execution plan
```

### 3. Inspect the SARIF output

Even failed runs produce (partial) SARIF:

```bash
cat clausura-output.sarif | jq '.runs[0].results[] | {ruleId, level, message: .message.text}'
```

### 4. Check the checkpoint

If a run was interrupted, inspect what the agent had done:

```bash
clausura snapshot list
clausura snapshot show
```

### 5. Test with a small config first

Before running a full review pipeline, test with minimal settings:

```yaml
task:
  name: test
  model: gpt-4o-mini
  vendor: openai
  prompt_template: "Look at the diff and tell me if you see any issues. Output {'findings': []}."
  token_budget: 4000
  timeout_secs: 30
  max_iterations: 2
```

This verifies your LLM connection, API key, and basic agent loop work.

### 6. Check context archives

When runs are truncated, archived messages land in `.clausura/archives/`. These can help debug what the agent saw:

```bash
ls -la .clausura/archives/
cat .clausura/archives/context-dump-<task-id>-1.log
```

### 7. Test prompts interactively

Before deploying a prompt to CI, test it against your favorite LLM chat interface. Copy the prompt, paste it into ChatGPT or Claude, and see if it produces the findings format you expect.

## Getting Help

If you're stuck:

- Check the [GitHub Issues](https://github.com/liuyanghejerry/Clausura/issues) for similar problems
- Enable verbose logging: `clausura run --log-format pretty`
- Run with `--dry-run` to confirm config interpretation
- Ensure your LLM provider is accessible from the CI environment (some corp networks block API endpoints)
