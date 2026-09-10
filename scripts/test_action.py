"""Offline tests for the Action's shell orchestration; no API key or runner needed."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("run-action.sh").resolve()


class ActionTests(unittest.TestCase):
    def run_review(self, code, overrides=None):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binary = root / "clausura"
            binary.write_text(
                f"#!{sys.executable}\n"
                "import json, os, sys\n"
                "from pathlib import Path\n"
                "args = sys.argv[1:]\n"
                "print(json.dumps({'args': args, 'model': os.environ.get('CLAUSURA_MODEL'), "
                "'vendor': os.environ.get('CLAUSURA_VENDOR')}))\n"
                "code = int(os.environ['TEST_EXIT'])\n"
                "if code != 3:\n"
                "    Path(args[args.index('--output') + 1]).write_text('{}')\n"
                "    Path(args[args.index('--summary') + 1]).write_text('{}')\n"
                "sys.exit(code)\n"
            )
            binary.chmod(0o755)
            env = dict(os.environ, PATH=f"{root}:{os.environ['PATH']}",
                       RUNNER_TEMP=tmp, GITHUB_OUTPUT=str(root / "outputs"),
                       GITHUB_STEP_SUMMARY=str(root / "job-summary"),
                       INPUT_CONFIG="config with spaces; $(touch injected).yaml",
                       INPUT_BASE="base-ref", INPUT_MODEL="", INPUT_VENDOR="",
                       CLAUSURA_MODEL="caller-model", CLAUSURA_VENDOR="caller-vendor",
                       TEST_EXIT=str(code))
            env.update(overrides or {})
            result = subprocess.run(["bash", str(SCRIPT)], env=env, cwd=tmp,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            outputs = dict(line.split("=", 1) for line in (root / "outputs").read_text().splitlines())
            self.assertEqual(outputs["exit-code"], str(code))
            report = Path(outputs["report-dir"])
            self.assertEqual((report / "exit-code.txt").read_text().strip(), str(code))
            self.assertTrue((report / "run.log").exists())
            self.assertEqual((report / "output.sarif").exists(), code != 3)
            self.assertIn(f"exit code {code}", (root / "job-summary").read_text())
            self.assertFalse((root / "injected").exists())
            invocation = json.loads(result.stdout)
            self.assertEqual(invocation["args"][2], env["INPUT_CONFIG"])
            return invocation

    def test_all_verdicts_preserved_for_upload(self):
        for code in (0, 1, 2, 3):
            with self.subTest(code=code):
                call = self.run_review(code)
                self.assertEqual(call["model"], "caller-model")
                self.assertEqual(call["vendor"], "caller-vendor")
                self.assertEqual(call["args"][-2:], ["--base", "base-ref"])

    def test_explicit_override_and_non_pr_run(self):
        call = self.run_review(0, {"INPUT_MODEL": "override", "INPUT_BASE": ""})
        self.assertEqual(call["model"], "override")
        self.assertNotIn("--base", call["args"])


if __name__ == "__main__":
    unittest.main()
