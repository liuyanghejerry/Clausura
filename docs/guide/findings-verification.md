# Findings Verification (System One Decision Model)

Findings verification is an optional, fail-open filter that runs **after the
review agent produces findings and before the gating rules evaluate them**.
Each finding is scored by a System One decision model — [TypeSafe
Jev](https://typesafe.ai) — and findings whose calibrated probability of being
genuine falls below a threshold are excluded from the gate.

The goal is fewer false-positive CI blocks: the review LLM still does all
open-ended analysis, and the decision model audits its output.

## Concepts

**System One models return decisions, not text.** Jev does not chat or
generate prose. You send it a `state` (a JSON object) and typed `questions`;
it returns calibrated probabilities. Clausura asks one yes/no question (a
*noul*, in Jev's vocabulary) per finding:

> Is this code-review finding genuine and supported by its evidence?

The answer is a probability `p ∈ [0, 1]`. Your `threshold` decides the cutoff:
`p >= threshold` keeps the finding, `p < threshold` filters it out of gating.

**What the decision model sees.** Only the finding itself — `rule_id`,
`severity`, `message`, `evidence`, and `location` — not the full diff. This
makes verification a *consistency check*: it catches vague, generic,
self-contradictory, or unsupported findings (classic LLM padding), which is
where most gate false positives come from. It cannot catch a finding that is
plausible and well-written but wrong about the code — that still requires a
good review prompt and sane gating rules.

**Fail-open by design.** Verification is an enhancement, never a dependency:

| Situation | Behavior |
|-----------|----------|
| `verify.enabled: false` (default) | Findings pass through untouched |
| `$TYPESAFE_API_KEY` unset/empty | Warning on stderr; verification skipped |
| Request error / timeout / HTTP 5xx | Finding is **kept**, counted as `failed` |
| Unparseable response | Finding is **kept**, counted as `failed` |
| Findings > `max_findings` | Excess findings pass through unverified (`skipped`) |

A decision-API outage can never block your pipeline.

## Setup

### 1. Get an API key

Sign up at [console.typesafe.ai](https://console.typesafe.ai) and create a
key (Jev is in early access). Put it in your environment:

```bash
# .env (git-ignored) — see .env.example
export TYPESAFE_API_KEY=apikey_...
```

In CI, add `TYPESAFE_API_KEY` as a secret and pass it to the job:

```yaml
# GitHub Actions example
- name: Run Clausura
  run: clausura run --config .clausura.yaml
  env:
    CLAUSURA_API_KEY: ${{ secrets.CLAUSURA_API_KEY }}
    TYPESAFE_API_KEY: ${{ secrets.TYPESAFE_API_KEY }}
```

### 2. Enable verification

```yaml
# .clausura.yaml
task:
  verify:
    enabled: true
    threshold: 0.5
```

Or toggle it per run without touching the config:

```bash
CLAUSURA_VERIFY=1 clausura run
```

### 3. Read the results

Filtered findings are announced on stderr:

```
Verification filtered 1 finding(s) below threshold 0.50:
  - [missing-validation] Input is not validated (p=0.21)
```

And recorded machine-readably in the run summary (`--summary <path>` or the
sharded `<output>.summary.json`) and the report JSON:

```json
"verification": {
  "model": "jev-latest",
  "threshold": 0.5,
  "verified": 3,
  "filtered": 1,
  "failed": 0,
  "skipped": 0,
  "filtered_findings": [
    {
      "finding_id": "7c9e…",
      "rule_id": "missing-validation",
      "probability": 0.21,
      "message": "Input is not validated"
    }
  ]
}
```

| Field | Meaning |
|-------|---------|
| `verified` | Findings at or above the threshold — these were gated |
| `filtered` | Findings below the threshold — excluded from gating, SARIF, and the report's finding list |
| `failed` | Findings kept because their verification call errored (fail-open) |
| `skipped` | Findings kept unverified (over the `max_findings` cap) |
| `filtered_findings` | Audit trail: id, rule, probability, and message of each filtered finding |

Filtered findings disappear from SARIF and the gate on purpose — SARIF should
reflect what was actually enforced. The audit trail above is where you review
the filter's decisions.

## Configuration Reference

All fields under `task.verify`, every one optional:

| Field | Default | Description |
|-------|---------|-------------|
| `enabled` | `false` | Master switch. `CLAUSURA_VERIFY=1` overrides. |
| `threshold` | `0.5` | Keep-threshold on P(genuine), clamped to [0, 1]. |
| `model` | `jev-latest` | Decision model id. |
| `base_url` | `https://api.typesafe.ai` | API base URL; `/v1/systemone` is appended. Point at a gateway/proxy here. |
| `api_key_env` | `TYPESAFE_API_KEY` | Name of the env var holding the key. |
| `timeout_secs` | `30` | Per-request timeout. |
| `max_findings` | `100` | Cost guard: at most this many findings are verified per run; the rest pass through unverified. |

## Tuning the Threshold

**Start at 0.5 and observe.** Run a few PRs and inspect
`verification.filtered_findings` in the summary JSON. The question to ask for
each filtered finding: *would a human reviewer agree this was noise?*

| Posture | Threshold | Effect |
|---------|-----------|--------|
| Conservative | `0.3` – `0.5` | Filters only obvious noise; recommended starting band |
| Aggressive | `0.7` – `0.9` | Filters anything the model is unsure about; expect some true positives to be dropped |

Because Jev returns *calibrated* probabilities (a `0.9` should be right ~90%
of the time), the threshold has a clean interpretation — but Jev is in early
access, so validate calibration on your own review history before going
aggressive. The eval harness (`clausura eval`) is a good way to A/B a
threshold: run your scenarios with and without `verify` and compare recall
and findings counts.

**Raising the threshold is a gating decision, like editing rules.** Treat a
threshold change like a rule change: review what it filtered before merging
the config.

## Cost and Latency

- One API call per finding (up to `max_findings`), 8 concurrent at most.
- Jev pricing is input-only (~$0.042 / 1M tokens, output free) — a finding is
  a few hundred input tokens, so verification typically costs **fractions of
  a cent per run** and adds roughly a second to wall-clock time.
- Verification does not consume the task's `token_budget` or
  `max_total_tokens`; those only govern the review LLM.

## Troubleshooting

**`Warning: verify.enabled is true but $TYPESAFE_API_KEY is not set`** — the
key variable is missing or empty in the environment where `clausura` runs
(remember: CI jobs need the secret passed explicitly). Verification was
skipped; findings were gated unverified.

**`Warning: verification call failed for finding …`** — the decision API
errored (network, 5xx, timeout). The finding was kept. If *every* call fails,
check `base_url`, key validity, and whether the API is reachable from your CI
runner.

**Everything got filtered.** A very low set of probabilities usually means
the review prompt produces thin findings — no `evidence`, generic messages.
Verification can only judge what it sees; improve the prompt (require
concrete evidence and locations) rather than lowering the threshold.

**`skipped` is non-zero.** More than `max_findings` findings were produced.
Either raise the cap or treat it as a signal the review is over-reporting.

## Limitations

- Jev is early-access (launched 2026-09-15): single vendor, and its
  calibration on *your* data is unverified until you measure it. Keep the
  audit trail habit.
- Verification judges the finding's internal consistency, not the code. It is
  a complement to good prompts and gating rules, not a replacement.
- The feature is off by default and can be removed at any time by deleting
  the `verify` block — findings simply flow straight to the gate again.

## Next

→ [Design your gating rules](gating.md)
→ [Configuration reference](configuration.md)
