"""Small end-to-end checks for the real subprocess fallback runner."""
import hashlib
import importlib.util
import json
import pathlib
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

HERE = pathlib.Path(__file__).parent
SPEC = importlib.util.spec_from_file_location("fallback_runner", HERE / "fallback.py")
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class FallbackTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.base = pathlib.Path(self.tmp.name)
        self.root = self.base / "source"
        self.root.mkdir()
        self.script = self.root / "gate.py"
        self.script.write_text("print('Ran 2 tests in 0.001s')\nprint('OK')\n")
        self.sha = "a" * 40
        self.identity()
        self.profile = {"id": "test", "project": "fixture", "coverage": [], "not_covered": ["full CI"],
                        "stages": [{"id": "gate", "argv": ["{python}", "-B", "gate.py"], "test_kind": "unittest"}]}

    def identity(self):
        (self.root / "SOURCE_IDENTITY.json").write_text(json.dumps({"project": "fixture", "source_sha": self.sha,
            "source_tree": "b" * 40, "reference": "fixture", "files": {"gate.py": runner.digest(self.script)}}))

    def run_profile(self):
        return runner.run(self.profile, self.root, self.sha, self.base / "evidence")

    def test_real_subprocess_pass_and_durable_report(self):
        result = self.run_profile()
        self.assertEqual(result["status"], "passed")
        self.assertEqual(result["stages"][0]["test_count"], 2)
        self.assertFalse(result["full_ci_passed"])
        self.assertEqual(json.loads((self.base / "evidence/report.json").read_text())["status"], "passed")

    def test_tampered_source_and_wrong_sha_refuse_before_execution(self):
        self.script.write_text("raise RuntimeError('must not execute')\n")
        with self.assertRaises(ValueError):
            self.run_profile()
        self.assertFalse((self.base / "evidence").exists())
        self.identity()
        with self.assertRaises(ValueError):
            runner.run(self.profile, self.root, "c" * 40, self.base / "evidence")

    def test_no_tests_or_nonzero_exit_is_not_green(self):
        for content in ("print('OK')\n", "raise SystemExit(7)\n"):
            with self.subTest(content=content):
                self.script.write_text(content)
                self.identity()
                result = runner.run(self.profile, self.root, self.sha, self.base / ("run" + str(len(content))))
                self.assertEqual(result["status"], "failed")

    def test_skips_and_missing_tools_are_incomplete(self):
        self.script.write_text("print('Ran 2 tests in 0.01s')\nprint('OK (skipped=1)')\n")
        self.identity()
        result = self.run_profile()
        self.assertEqual(result["status"], "incomplete")
        self.assertEqual(result["stages"][0]["skipped"], 1)

    def test_missing_tool_is_never_executed(self):
        self.profile["stages"][0]["tools"] = ["no-such-fallback-tool"]
        result = self.run_profile()
        self.assertEqual(result["stages"][0]["status"], "unavailable")

    def test_timeout_stops_child_and_records_interruption(self):
        self.script.write_text("import time\ntime.sleep(30)\n")
        self.identity()
        self.profile["stages"][0]["timeout_seconds"] = 0.05
        result = self.run_profile()
        self.assertEqual(result["stages"][0]["status"], "interrupted")
        self.assertEqual(result["status"], "incomplete")

    def test_child_environment_excludes_ambient_secrets(self):
        self.script.write_text("import os\nassert 'SYNTHETIC_SECRET_CANARY' not in os.environ\nprint('Ran 1 test in 0.01s')\nprint('OK')\n")
        self.identity()
        with patch.dict(runner.os.environ, {"SYNTHETIC_SECRET_CANARY": "private"}):
            self.assertEqual(self.run_profile()["status"], "passed")

    def test_failed_dependency_is_not_run(self):
        self.script.write_text("raise SystemExit(1)\n")
        self.identity()
        self.profile["stages"].append({"id": "later", "needs": ["gate"], "argv": ["must-not-execute"]})
        result = self.run_profile()
        self.assertEqual(result["stages"][1]["status"], "not-run")

    def test_source_mutation_is_a_failure(self):
        self.script.write_text("from pathlib import Path\nPath('extra').write_text('unexpected')\nprint('Ran 1 test in 0.01s')\nprint('OK')\n")
        self.identity()
        result = self.run_profile()
        self.assertEqual(result["status"], "failed")
        self.assertIn("source_changed", result["stages"][0])

    def test_existing_evidence_or_evidence_inside_source_is_refused(self):
        (self.base / "evidence").mkdir()
        with self.assertRaises(FileExistsError):
            self.run_profile()
        with self.assertRaises(ValueError):
            runner.run(self.profile, self.root, self.sha, self.root / "evidence")

    def test_tap_counts_are_not_invented(self):
        self.assertEqual(runner.counts("tap", "# tests 10\n# skipped 2\n"), (10, 2))
        self.assertEqual(runner.counts("tap", "TAP version 13\n"), (0, 0))

    def test_failed_unittest_summary_retains_skips(self):
        self.assertEqual(runner.counts("unittest", "Ran 141 tests in 3.1s\nFAILED (errors=4, skipped=2)\n"), (141, 2))
        self.assertEqual(runner.counts("unittest", "Ran 2 tests in 1.0s\nOK (skipped=1)\n"), (2, 1))

    def test_deleted_manifest_finalizes_failed_report(self):
        self.script.write_text("from pathlib import Path\nPath('SOURCE_IDENTITY.json').unlink()\n")
        self.identity()
        result = self.run_profile()
        self.assertEqual(result["status"], "failed")
        self.assertEqual(json.loads((self.base / "evidence/report.json").read_text())["status"], "failed")

    def test_sigterm_during_spawn_is_deferred_until_child_owned(self):
        self.script.write_text("import time\ntime.sleep(30)\n")
        self.identity()
        child = Mock(pid=12345)
        def spawn(*args, **kwargs):
            # Fully synthetic delivery and child. Immediate process cleanup hit
            # EPERM on the selected host; do not retry that OS operation.
            signal.getsignal(signal.SIGTERM)(signal.SIGTERM, None)
            return child
        with patch.object(runner.subprocess, "Popen", side_effect=spawn), patch.object(runner, "stop_group") as cleanup:
            result = self.run_profile()
        cleanup.assert_called_once_with(child)
        self.assertEqual(result["stages"][0]["status"], "interrupted", result)
        self.assertEqual(result["stages"][0]["signal"], signal.SIGTERM)
        self.assertEqual(json.loads((self.base / "evidence/report.json").read_text())["status"], "incomplete")

    def test_timeout_kills_descendant_even_when_leader_exits_on_term(self):
        heartbeat = self.base / "heartbeat"
        child = ("import signal,time,pathlib\n"
                 "signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
                 "p=pathlib.Path(" + repr(str(heartbeat)) + ")\n"
                 "while True:\n p.write_text(str(time.monotonic()))\n time.sleep(.02)\n")
        self.script.write_text("import subprocess,sys,time\nsubprocess.Popen([sys.executable,'-c'," + repr(child) + "])\ntime.sleep(30)\n")
        self.identity()
        self.profile["stages"][0]["timeout_seconds"] = 1
        result = self.run_profile()
        self.assertEqual(result["stages"][0]["status"], "interrupted")
        before = heartbeat.read_text()
        time.sleep(.2)
        self.assertEqual(heartbeat.read_text(), before)

    def test_cleanup_denial_stops_profile_and_retains_process_identity(self):
        self.profile["stages"].append({"id": "later", "argv": ["must-not-execute"]})
        child = Mock(pid=12345)
        child.wait.return_value = 0
        with patch.object(runner.subprocess, "Popen", return_value=child), patch.object(runner, "stop_group", side_effect=PermissionError("synthetic EPERM")):
            result = self.run_profile()
        self.assertEqual(len(result["stages"]), 1)
        self.assertEqual(result["stages"][0]["status"], "cleanup-unresolved")
        self.assertEqual(result["stages"][0]["pgid"], 12345)
        self.assertIn("log_sha256", result["stages"][0])
        self.assertEqual(result["status"], "incomplete")

    def test_late_cancellation_cannot_report_passed(self):
        original = runner.source_identity
        calls = []
        def identify(*args):
            result = original(*args)
            calls.append(True)
            if len(calls) == 2:
                signal.getsignal(signal.SIGTERM)(signal.SIGTERM, None)
            return result
        with patch.object(runner, "source_identity", side_effect=identify):
            result = self.run_profile()
        self.assertEqual(result["status"], "incomplete")
        self.assertEqual(result["stages"][0]["status"], "interrupted")


    def test_partial_or_contradictory_summaries_never_pass(self):
        cases = [
            ("tap", "# tests 2\n# pass 1\n# fail 0\n# cancelled 0\n# skipped 0\n# todo 1\n", "incomplete"),
            ("tap", "# tests 2\n# pass 1\n# fail 0\n# cancelled 1\n# skipped 0\n# todo 0\n", "incomplete"),
            ("tap", "# tests 2\n# pass 1\n# fail 1\n# cancelled 0\n# skipped 0\n# todo 0\n", "failed"),
            ("tap", "# tests 2\n# pass 2\n# fail 0\n# cancelled 0\n# skipped 0\n# todo 0\n", "passed"),
            ("tap", "# tests 2\n", "incomplete"),
            ("tap", "# tests 2\n# pass 3\n# fail 0\n# cancelled 0\n# skipped 0\n# todo 0\n", "incomplete"),
            ("unittest", "Ran 2 tests in 0.1s\nOK (expected failures=1)\n", "incomplete"),
            ("unittest", "Ran 2 tests in 0.1s\nFAILED (unexpected successes=1)\n", "failed"),
            ("unittest", "Ran 2 tests in 0.1s\nFAILED (errors=1)\n", "failed"),
            ("unittest", "Ran 2 tests in 0.1s\n", "incomplete"),
            ("unittest", "Ran 2 tests in 0.1s\nOK (skipped=3)\n", "incomplete"),
            ("unittest", "Ran 2 tests in 0.1s\nOK (unknown=1)\n", "incomplete"),
        ]
        for kind, output, status in cases:
            with self.subTest(kind=kind, output=output):
                self.assertEqual(runner.outcome(0, kind, runner.test_summary(kind, output))[0], status)

    def test_summary_retains_all_coverage_counters(self):
        summary = runner.test_summary("unittest", "Ran 141 tests in 3.1s\nFAILED (errors=4, skipped=2)\n")
        self.assertEqual((summary["test_count"], summary["passed"], summary["errors"], summary["skipped"]),
                         (141, 135, 4, 2))
        self.assertEqual(summary["summary_issues"], [])
        summary = runner.test_summary("tap", "# tests 3\n# pass 1\n# fail 0\n# cancelled 1\n# skipped 0\n# todo 1\n")
        self.assertEqual((summary["passed"], summary["todo"], summary["cancelled"]), (1, 1, 1))

    def test_executable_evidence_hashes_without_launching(self):
        with patch.object(runner.subprocess, "run") as run, patch.object(runner.subprocess, "Popen") as popen:
            identities = runner.executable_identity({}, [sys.executable], {"PATH": os.environ["PATH"]})
        self.assertEqual(identities[sys.executable]["sha256"], runner.digest(pathlib.Path(sys.executable).resolve()))
        run.assert_not_called()
        popen.assert_not_called()


    def test_portable_profile_is_explicitly_partial_without_weakening_full_gates(self):
        profiles = json.loads((HERE / "fallback-profiles.json").read_text())
        portable = profiles["native-portable-verification"]
        expected = ["egress-inventory", "egress-negative-tests", "release-manifest-tests", "worktree-retirement-tests"]
        self.assertEqual([stage["id"] for stage in portable["stages"]], expected)
        self.assertTrue(portable["not_covered"])
        self.assertEqual(portable["stages"], profiles["native-core"]["stages"][:4])
        self.assertIn("gateway-integration", [stage["id"] for stage in profiles["native-core"]["stages"]])
        self.assertIn("gateway-integration", profiles["native-linux-build"]["stages"][-1]["needs"])


if __name__ == "__main__":
    unittest.main()
