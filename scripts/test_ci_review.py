"""Exercise --base through the real CLI and a local mock provider, without billing."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

BINARY = Path(__file__).resolve().parents[1] / "target/debug/clausura"


class ReviewTests(unittest.TestCase):
    def test_clean_pr_review_and_missing_base(self):
        requests = []

        class Provider(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                requests.append(body)
                if len(requests) == 1:
                    message = {"role": "assistant", "content": None, "tool_calls": [{
                        "id": "diff", "type": "function", "function": {
                            "name": "git_diff", "arguments": "{}"}}]}
                    reason = "tool_calls"
                else:
                    finding = {"id": "00000000-0000-0000-0000-000000000001",
                               "rule_id": "test-rule", "severity": "error",
                               "message": "Seeded PR issue", "evidence": "PR_MARKER"}
                    message = {"role": "assistant", "content": json.dumps({"findings": [finding]})}
                    reason = "stop"
                response = json.dumps({"id": "mock", "choices": [{"index": 0,
                                      "message": message, "finish_reason": reason}],
                                      "usage": {"prompt_tokens": 10, "completion_tokens": 10,
                                                "total_tokens": 20}}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(response)))
                self.end_headers()
                self.wfile.write(response)

        server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                def git(*args):
                    return subprocess.run(["git", *args], cwd=tmp, capture_output=True,
                                          text=True, check=True).stdout
                git("init")
                git("config", "user.email", "test@example.invalid")
                git("config", "user.name", "CI Test")
                (root / "code.txt").write_text("initial\n")
                git("add", ".")
                git("commit", "-m", "base")
                git("branch", "review-base")
                (root / "code.txt").write_text("initial\nPR_MARKER\n")
                git("add", ".")
                git("commit", "-m", "feature")
                self.assertEqual(git("diff"), "")
                config = root / "review.yaml"
                config.write_text('''version: "1"
task:
  name: ci-review-smoke
  model: mock
  vendor: openai
  prompt_template: Review the committed PR diff.
  timeout_secs: 10
  gating:
    - rule: test-rule
      description: Block seeded issue
      min_severity: error
      max_findings: 0
      action: fail
''')
                env = {k: v for k, v in os.environ.items() if not k.startswith("CLAUSURA_")}
                env["CLAUSURA_API_KEY"] = "mock-key"
                env["CLAUSURA_BASE_URL"] = f"http://127.0.0.1:{server.server_port}/v1"
                args = [str(BINARY), "run", "--config", str(config),
                        "--summary", str(root / "summary.json")]
                result = subprocess.run([*args, "--base", "review-base"], cwd=tmp, env=env,
                                        capture_output=True, text=True, timeout=20)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertEqual(len(requests), 2)
                tool_messages = [m for m in requests[1]["messages"] if m["role"] == "tool"]
                self.assertIn("PR_MARKER", tool_messages[0]["content"])
                summary = json.loads((root / "summary.json").read_text())
                self.assertEqual(summary["status"], "complete")
                self.assertEqual(summary["exit_code"], 1)
                self.assertEqual(summary["findings_count"], 1)
                sarif = json.loads((root / "clausura-output.sarif").read_text())
                self.assertEqual(sarif["runs"][0]["results"][0]["ruleId"], "test-rule")
                result = subprocess.run([*args, "--base", "missing-ref"], cwd=tmp, env=env,
                                        capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 2)
                self.assertIn("Fetch the base ref", result.stderr)
                self.assertEqual(len(requests), 2, "Invalid base must fail before any model call")
                # CLI base also overrides an unavailable YAML sharding base.
                with config.open("a") as out:
                    out.write("  sharding:\n    base: missing-yaml-base\n")
                result = subprocess.run([*args, "--base", "review-base", "--dry-run"],
                                        cwd=tmp, env=env, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("1 file(s)", result.stderr)
                result = subprocess.run([*args, "--dry-run"], cwd=tmp, env=env,
                                        capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 2)
                self.assertIn("plan unavailable", result.stderr)
                result = subprocess.run([*args, "--base", "missing-ref", "--dry-run"],
                                        cwd=tmp, env=env, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 2)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == "__main__":
    unittest.main()
