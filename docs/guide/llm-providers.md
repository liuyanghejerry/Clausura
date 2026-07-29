# LLM Providers

Clausura works with any LLM that exposes a chat completions API. Three vendor categories are supported out of the box, plus a custom option for enterprise endpoints.

## Configuration: The `vendor` Field

The `vendor` field in `.clausura.yaml` accepts two forms:

**Shorthand (string):**

```yaml
vendor: openai
```

**Full config (object):**

```yaml
vendor:
  type: openai_compatible
  base_url: "https://api.mistral.ai/v1"
  auth_header: "Authorization"
  api_key_env: "MY_CUSTOM_KEY"
```

The shorthand maps to built-in presets. The object form gives you full control.

## Provider Presets

### 1. OpenAI

```yaml
task:
  model: gpt-4o
  vendor: openai
```

Uses `https://api.openai.com/v1` with `Authorization: Bearer` header.

| Model | Recommended for |
|-------|-----------------|
| `gpt-4o` | General-purpose code review |
| `gpt-4o-mini` | Faster, cheaper, good for lint-like checks |
| `o3-mini` | Reasoning-heavy analysis |

Set the API key:

```bash
export CLAUSURA_API_KEY=sk-proj-...
```

### 2. Anthropic (Claude)

```yaml
task:
  model: claude-sonnet-4-20250514
  vendor: anthropic
```

Uses Anthropic's native [Messages API](https://docs.anthropic.com/en/api/messages) at `https://api.anthropic.com` with `x-api-key` header and `anthropic-version: 2023-06-01`.

| Model | Recommended for |
|-------|-----------------|
| `claude-sonnet-4-20250514` | Balanced speed and quality |
| `claude-opus-4-20250514` | Complex analysis, large diffs |
| `claude-haiku-3-5-20241022` | Fast, cheap, simple checks |

The shorthand `claude` is an alias for `anthropic`:

```yaml
vendor: claude  # same as vendor: anthropic
```

Set the API key:

```bash
export CLAUSURA_API_KEY=sk-ant-...
```

### 3. DeepSeek

```yaml
task:
  model: deepseek-chat
  vendor: deepseek
```

Uses DeepSeek's OpenAI-compatible endpoint at `https://api.deepseek.com/v1`.

| Model | Notes |
|-------|-------|
| `deepseek-chat` | General purpose (V3) |
| `deepseek-reasoner` | Reasoning model (R1) |

Set the API key:

```bash
export CLAUSURA_API_KEY=sk-...
```

### 4. Groq

```yaml
task:
  model: llama-3.3-70b-versatile
  vendor: groq
```

Uses Groq's OpenAI-compatible endpoint at `https://api.groq.com/openai/v1`. Groq is known for very fast inference.

Set the API key:

```bash
export CLAUSURA_API_KEY=gsk_...
```

### 5. Ollama (Local)

```yaml
task:
  model: llama3.2
  vendor: ollama
```

Uses your local Ollama instance at `http://localhost:11434/v1`. No API key required — set a dummy value:

```bash
export CLAUSURA_API_KEY=ollama
```

Ollama must be running locally:

```bash
ollama serve
ollama pull llama3.2
```

## Custom Providers

Any endpoint that speaks the OpenAI chat completions API format can be used:

```yaml
task:
  model: my-model
  vendor:
    type: custom
    base_url: "https://llm.internal.company.com/v1"
    auth_header: "X-API-Key"
    api_key_env: "INTERNAL_LLM_KEY"
```

| Field | Default | Description |
|-------|---------|-------------|
| `type` | — | Must be `custom` |
| `base_url` | — | Your endpoint's base URL (required) |
| `auth_header` | `Authorization` | Custom auth header name |
| `api_key_env` | `CLAUSURA_API_KEY` | Env var to read the key from |

For `type: openai_compatible`, you can also override the base URL while keeping `Authorization: Bearer` auth:

```yaml
vendor:
  type: openai_compatible
  base_url: "https://api.mistral.ai/v1"
```

This is useful for Mistral, Together AI, Fireworks, vLLM, and any other OpenAI-compatible provider.

## API Key

The API key is **never** read from the YAML config file. It must come from one of:

1. `CLAUSURA_API_KEY` environment variable (default)
2. `--api-key` CLI flag
3. A custom env var specified via `api_key_env` in the vendor config

```bash
# Environment variable (recommended for CI)
export CLAUSURA_API_KEY=sk-...

# CLI flag
clausura run --api-key sk-...

# Custom env var (with vendor.api_key_env: "LLM_KEY")
export LLM_KEY=sk-...
clausura run
```

## Model Selection Tips

### For Code Review Tasks

Code review is the primary use case. Key considerations:

| Priority | Concern | Recommendation |
|----------|---------|----------------|
| Quality | False negatives (missed issues) | Use a strong model: `gpt-4o`, `claude-sonnet-4` |
| Speed | CI pipeline latency | `gpt-4o-mini` or `claude-haiku` for lint-like checks |
| Cost | API spend per PR | Smaller models + low `token_budget`; skip large diffs |

### For Classification Tasks

If your task is classification rather than reasoning (e.g., "does this file use Options API or Composition API?"), cheaper models like `gpt-4o-mini` or `claude-haiku-3-5` work well.

### Conditional Model Selection

Clausura uses a single model per run. For different review dimensions that benefit from different models, run multiple `clausura run` commands with different configs in parallel CI jobs.

## Retry Behavior

Clausura retries failed LLM requests with exponential backoff:

- Retries on: HTTP 429 (rate limit), 5xx (server error), network errors
- No retry on: HTTP 4xx (bad request, auth failure) — these fail immediately
- Backoff: 1s, 2s, 4s with jitter
- Honors `Retry-After` response header when present
- Maximum 3 retry attempts per request

## Token Budget vs. Cumulative Token Cap

Two separate limits control token usage:

| Field | Purpose | Default |
|-------|---------|---------|
| `token_budget` | Context window limit — drives message truncation | 32,000 |
| `max_total_tokens` | Cumulative spend cap across all LLM calls in a run | None (no cap) |

`token_budget` keeps the conversation from exceeding the model's context window. Older messages are truncated and archived. `max_total_tokens` stops the run entirely when cumulative billed tokens reach the cap — useful for cost control.

```yaml
task:
  token_budget: 32000       # trim conversation at 32K tokens
  max_total_tokens: 200000  # stop entire run at 200K tokens billed
```

## Next

→ [Understand the full configuration](configuration.md)
