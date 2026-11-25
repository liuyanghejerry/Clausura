#!/usr/bin/env bash
# Capture the verdict without failing this step, so report upload runs first.
set -euo pipefail

report_dir="$(mktemp -d "$RUNNER_TEMP/clausura-report.XXXXXX")"
printf 'report-dir=%s\n' "$report_dir" >> "$GITHUB_OUTPUT"
args=(run --config "$INPUT_CONFIG" --output "$report_dir/output.sarif" --summary "$report_dir/summary.json" --markdown "$report_dir/summary.md")
if [[ -n "${INPUT_BASE:-}" ]]; then args+=(--base "$INPUT_BASE"); fi
# Omitted Action inputs must not mask the caller's YAML or environment values.
if [[ -n "${INPUT_API_KEY:-}" ]]; then export CLAUSURA_API_KEY="$INPUT_API_KEY"; fi
if [[ -n "${INPUT_MODEL:-}" ]]; then export CLAUSURA_MODEL="$INPUT_MODEL"; fi
if [[ -n "${INPUT_VENDOR:-}" ]]; then export CLAUSURA_VENDOR="$INPUT_VENDOR"; fi
if [[ -n "${INPUT_TOKEN_BUDGET:-}" ]]; then export CLAUSURA_TOKEN_BUDGET="$INPUT_TOKEN_BUDGET"; fi
if [[ -n "${INPUT_TIMEOUT:-}" ]]; then export CLAUSURA_TIMEOUT="$INPUT_TIMEOUT"; fi

set +e
clausura "${args[@]}" 2>&1 | tee "$report_dir/run.log"
review_exit=${PIPESTATUS[0]}
set -e
printf '%s\n' "$review_exit" > "$report_dir/exit-code.txt"
printf 'exit-code=%s\n' "$review_exit" >> "$GITHUB_OUTPUT"
if [[ -f "$report_dir/output.sarif" ]]; then
  printf 'sarif-exists=true\n' >> "$GITHUB_OUTPUT"
fi

case "$review_exit" in
  0) verdict='Passed' ;;
  1) verdict='Gate violated' ;;
  2) verdict='Review incomplete or runtime error' ;;
  3) verdict='Invalid configuration' ;;
  *) verdict='Execution failed' ;;
esac

if [[ -s "$report_dir/summary.md" ]]; then
  cat "$report_dir/summary.md" >> "$GITHUB_STEP_SUMMARY"
else
  {
    printf '### Clausura review\n\n'
    printf '**%s** (exit code %s).\n\n' "$verdict" "$review_exit"
    printf 'Reports are available in the workflow artifacts when upload is enabled. '
    printf 'The JSON summary contains findings count, token usage and completion status. '
    printf 'Setup/configuration errors may only produce an execution log and exit code.\n'
  } >> "$GITHUB_STEP_SUMMARY"
fi

if [[ "${INPUT_POST_PR_COMMENT:-false}" == "true" && -s "$report_dir/summary.md" && -n "${INPUT_GITHUB_TOKEN:-}" && -n "${GITHUB_REPOSITORY:-}" && -n "${GITHUB_EVENT_PATH:-}" ]]; then
  python3 - "$report_dir/summary.md" <<'PY' || echo "Warning: failed to post PR comment" >&2
import json, os, sys, urllib.request

body_text = open(sys.argv[1], encoding="utf-8").read()
event = json.load(open(os.environ["GITHUB_EVENT_PATH"], encoding="utf-8"))
pr_num = (event.get("pull_request") or {}).get("number")
if not pr_num:
    sys.exit(0)

repo = os.environ["GITHUB_REPOSITORY"]
token = os.environ["INPUT_GITHUB_TOKEN"]
api = os.environ.get("GITHUB_API_URL", "https://api.github.com").rstrip("/")
marker = "<!-- clausura-review -->"
headers = {
    "Authorization": f"Bearer {token}",
    "Accept": "application/vnd.github+json",
    "Content-Type": "application/json",
    "User-Agent": "clausura-action",
}

def request(method, url, payload=None):
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    with urllib.request.urlopen(req, timeout=20) as resp:
        return json.loads(resp.read().decode())

comments = request("GET", f"{api}/repos/{repo}/issues/{pr_num}/comments?per_page=100")
existing = next((c["id"] for c in comments if marker in (c.get("body") or "")), None)
if existing:
    request("PATCH", f"{api}/repos/{repo}/issues/comments/{existing}", {"body": body_text})
else:
    request("POST", f"{api}/repos/{repo}/issues/{pr_num}/comments", {"body": body_text})
PY
fi
