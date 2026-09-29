"""Regression tests for CI verdicts through the real CLI; only local mocks are used."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import uuid
from collections import deque
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

REPO = Path(__file__).resolve().parents[1]
BINARY = REPO / "target/debug/clausura"

MCP_SERVER = r'''
import json, pathlib, sys, time
mode, marker = sys.argv[1:]
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        if mode == "hang" or (mode == "hang-call" and pathlib.Path(marker).exists()):
            time.sleep(30)
        pathlib.Path(marker).touch()
        result = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                  "serverInfo": {"name": "mock", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "diagnostics", "description": "test diagnostics",
                            "inputSchema": {"type": "object"}}]}
    else:
        diagnostics = [{"severity": 1, "message": "type mismatch", "file": "a.rs", "line": 1}]
        if mode == "empty":
            diagnostics = []
        if mode == "bad-item":
            diagnostics.append({"severity": "error"})
        text = "invalid json" if mode == "malformed" else json.dumps(diagnostics)
        result = {"content": [{"type": "text", "text": text}]}
        if mode == "tool-error":
            result = {"isError": True, "content": [{"type": "text", "text": "[]"}]}
        if mode == "rpc-error":
            print(json.dumps({"jsonrpc": "2.0", "id": request["id"],
                              "error": {"code": -32603, "message": "diagnostics failed"}}), flush=True)
            continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
'''


class GatingTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.requests = []
        self.request_headers = []
        self.responses = deque()
        owner = self

        class Provider(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                owner.requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
                owner.request_headers.append(dict(self.headers))
                status, body, headers = owner.responses.popleft() if owner.responses else (500, {}, {})
                data = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                for key, value in headers.items():
                    self.send_header(key, value)
                self.end_headers()
                self.wfile.write(data)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=0.01), daemon=True)
        self.thread.start()
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("CLAUSURA_")}
        self.env["CLAUSURA_API_KEY"] = "mock-key"
        self.task = {
            "name": "gating-test-" + uuid.uuid4().hex,
            "model": "mock-model",
            "vendor": {"type": "openai_compatible", "base_url": f"http://127.0.0.1:{self.server.server_port}/v1"},
            "prompt_template": "Review the code.",
            "timeout_secs": 10,
            "gating": [{"rule": "lsp-error", "min_severity": "error", "max_findings": 0, "action": "fail"}],
        }

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        # Remove only this test's checkpoints, keeping the user's real state.
        subprocess.run([str(BINARY), "snapshot", "delete", "--thread", "task-" + self.task["name"]],
                       env=self.env, capture_output=True, check=True)
        self.tmp.cleanup()

    def respond(self, findings=None, reason="stop", text=None):
        message = {"role": "assistant", "content": text if text is not None else json.dumps({"findings": findings or []})}
        if reason == "tool_calls":
            message["tool_calls"] = [{"id": "call1", "type": "function",
                                      "function": {"name": "list_files", "arguments": "{}"}}]
        self.responses.append((200, {"choices": [{"message": message, "finish_reason": reason}],
                                     "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}}, {}))

    def run_cli(self, *args, expected, env=None):
        config = self.root / "review.yaml"
        config.write_text(json.dumps({"version": "1", "task": self.task}))
        result = subprocess.run([str(BINARY), "run", "--config", str(config),
                                 "--summary", str(self.root / "summary.json"), *args],
                                cwd=self.root, env=env or self.env, capture_output=True, text=True, timeout=8)
        self.assertEqual(result.returncode, expected, result.stderr)
        return result

    def summary(self):
        return json.loads((self.root / "summary.json").read_text())

    def sarif(self):
        return json.loads((self.root / "clausura-output.sarif").read_text())["runs"][0]

    def mcp(self, mode):
        script = self.root / "mcp.py"
        script.write_text(MCP_SERVER)
        self.task["mcp_servers"] = [{"name": "lsp", "command": sys.executable,
                                     "args": [str(script), mode, str(self.root / "connected")]}]
        self.task["preflight"] = [{"mcp_server": "lsp", "tool": "diagnostics", "rule_id_prefix": "lsp-"}]

    def test_fresh_run_isolates_ledger_but_resume_preserves_it(self):
        finding = {"rule_id": "lsp-error", "severity": "error", "message": "fixed later", "evidence": "test"}
        self.respond([finding])
        self.run_cli(expected=1)
        self.respond()
        self.run_cli("--resume", expected=1)
        self.assertEqual(self.summary()["findings_count"], 1)
        self.respond()
        self.run_cli(expected=0)
        self.assertEqual(self.summary()["findings_count"], 0)
        previous = list((self.root / ".clausura/archives").glob("previous-findings-*.jsonl"))
        self.assertEqual(len(previous), 1)
        self.assertIn("fixed later", previous[0].read_text())

    def test_preflight_error_matches_gate_and_sarif(self):
        self.mcp("normal")
        self.respond()
        self.run_cli(expected=1)
        result = self.sarif()["results"][0]
        self.assertEqual(result["ruleId"], "lsp-error")
        self.assertEqual(result["level"], "error")
        self.assertEqual(self.summary()["findings_count"], 1)

    def test_empty_preflight_passes(self):
        self.mcp("empty")
        self.respond()
        self.run_cli(expected=0)
        self.assertEqual(self.summary()["status"], "complete")

    def test_preflight_failures_never_call_model(self):
        for mode in ["missing", "rpc-error", "tool-error", "malformed", "bad-item"]:
            with self.subTest(mode=mode):
                self.mcp(mode)
                if mode == "missing":
                    self.task["mcp_servers"][0]["command"] = str(self.root / "nonexistent-mcp")
                self.run_cli(expected=2)
                self.assertFalse(self.requests)
                self.assertEqual(self.summary()["status"], "error")
                self.assertTrue(self.sarif()["properties"]["incomplete"])

    def test_missing_required_server_with_another_healthy_server(self):
        self.mcp("empty")
        self.task["preflight"][0]["mcp_server"] = "missing"
        self.run_cli(expected=2)
        self.assertFalse(self.requests)

    def test_sharding_cannot_skip_required_preflight(self):
        self.mcp("empty")
        self.task["sharding"] = {"base": "HEAD~1"}
        result = self.run_cli(expected=2)
        self.assertIn("Preflight checks are not supported", result.stderr)
        self.assertFalse(self.requests)

    def setup_sharded_repo(self):
        def git(*args):
            return subprocess.run(["git", *args], cwd=self.root, env=self.env,
                                  capture_output=True, text=True, check=True).stdout.strip()
        git("init")
        git("config", "user.email", "test@example.invalid")
        git("config", "user.name", "Gating Test")
        source = self.root / "code.txt"
        source.write_text("before\n")
        git("add", "code.txt")
        git("commit", "-m", "base")
        base = git("rev-parse", "HEAD")
        source.write_text("after\n")
        git("add", "code.txt")
        git("commit", "-m", "change")
        self.task["sharding"] = {"base": base, "on_shard_incomplete": "fail"}

    def test_new_sharded_run_does_not_merge_incomplete_run_ledger(self):
        self.setup_sharded_repo()
        finding = {"rule_id": "lsp-error", "severity": "error", "message": "old finding", "evidence": "test"}
        self.respond([finding], reason="tool_calls")
        self.respond(reason="length")
        self.run_cli(expected=2)
        self.respond()
        self.run_cli(expected=0)
        self.assertEqual(self.summary()["findings_count"], 0)

    def test_sharded_run_obeys_overall_deadline(self):
        self.setup_sharded_repo()
        self.task["timeout_secs"] = 1
        self.task["sharding"]["per_shard"] = {"timeout_secs": 10}
        self.responses.append((503, {}, {"Retry-After": "5"}))
        self.respond()
        self.run_cli(expected=2)
        self.assertEqual(self.summary()["reason"], "timeout")
        self.assertEqual(len(self.requests), 1)

    def test_recovery_requires_clean_stop(self):
        for reason in ["length", "content_filter", "tool_calls"]:
            with self.subTest(reason=reason):
                self.respond(text="not JSON")
                self.respond(reason=reason)
                self.run_cli(expected=2)
                self.assertEqual(self.summary()["status"], "incomplete")
                self.assertTrue(self.sarif()["properties"]["incomplete"])
        self.assertEqual(len(self.requests), 6)

    def test_recovery_clean_stop_can_pass(self):
        self.respond(text="not JSON")
        self.respond()
        self.run_cli(expected=0)
        self.assertEqual(len(self.requests), 2)
        self.assertEqual(self.summary()["status"], "complete")

    def test_task_deadline_includes_provider_backoff(self):
        self.task["timeout_secs"] = 1
        self.responses.append((503, {}, {"Retry-After": "5"}))
        self.respond()
        start = time.monotonic()
        self.run_cli(expected=2)
        self.assertLess(time.monotonic() - start, 4)
        self.assertEqual(len(self.requests), 1)
        self.assertEqual(self.summary()["reason"], "timeout")
        self.assertTrue(self.sarif()["properties"]["incomplete"])

    def test_task_deadline_includes_mcp_startup(self):
        self.mcp("hang")
        self.task["timeout_secs"] = 1
        start = time.monotonic()
        self.run_cli(expected=2)
        self.assertLess(time.monotonic() - start, 4)
        self.assertEqual(self.summary()["reason"], "timeout")
        self.assertFalse(self.requests)

    def test_mcp_tool_deadline_includes_repeated_handshake(self):
        self.mcp("hang-call")
        self.task["shell_timeout_secs"] = 1
        result = self.run_cli(expected=2)
        self.assertIn("timed out after 1s", result.stderr)
        self.assertFalse(self.requests)

    def test_empty_optional_env_preserves_yaml_vendor_and_model(self):
        self.env.update(CLAUSURA_MODEL="", CLAUSURA_VENDOR="", CLAUSURA_API_KEY="")
        self.respond()
        self.run_cli(expected=0)
        self.assertEqual(self.requests[0]["model"], "mock-model")

    def test_custom_vendor_uses_configured_auth_header_and_key_env(self):
        self.task["vendor"].update(type="custom", auth_header="X-Test-Key", api_key_env="GATING_MOCK_API_KEY")
        self.env["GATING_MOCK_API_KEY"] = "custom-mock-key"
        self.respond()
        self.run_cli(expected=0)
        headers = {k.lower(): v for k, v in self.request_headers[0].items()}
        self.assertEqual(headers["x-test-key"], "custom-mock-key")

    def test_cli_model_can_fill_missing_yaml_model(self):
        del self.task["model"]
        self.respond()
        self.run_cli("--model", "cli-model", expected=0)
        self.assertEqual(self.requests[0]["model"], "cli-model")

    def test_documented_examples_validate(self):
        for name in ["mcp-lsp-review.yaml", "skill-based-review.yaml"]:
            result = subprocess.run([str(BINARY), "run", "--config", str(REPO / "examples" / name),
                                     "--workspace", str(REPO), "--validate-config"], env=self.env,
                                    capture_output=True, text=True, timeout=8)
            self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
