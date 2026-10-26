"""Synthetic runner failure receipts; no containers, accounts or executables."""
import contextlib
import json
from pathlib import Path
import subprocess
import types
import unittest
from unittest.mock import patch
import run_known_source as runner


class Reporting(unittest.TestCase):
    def exercise(self, result):
        receipts = []
        with contextlib.ExitStack() as stack:
            stack.enter_context(patch.object(runner.sys, 'platform', 'linux'))
            stack.enter_context(patch.object(runner.os, 'geteuid', return_value=0))
            stack.enter_context(patch.object(runner.os, 'listdir', return_value=['lo']))
            stack.enter_context(patch.object(runner.pwd, 'getpwuid', return_value=types.SimpleNamespace(pw_gid=987)))
            stack.enter_context(patch.object(Path, 'is_file', return_value=True))
            stack.enter_context(patch.object(Path, 'exists', return_value=False))
            stack.enter_context(patch.object(Path, 'read_bytes', return_value=b'public'))
            stack.enter_context(patch.object(Path, 'read_text', return_value='/public/unit-test'))
            stack.enter_context(patch.object(Path, 'mkdir'))
            stack.enter_context(patch.object(Path, 'write_text', side_effect=receipts.append))
            stack.enter_context(patch.dict(runner.os.environ, {'PERCIVAL_ISOLATED_LINUX_FIXTURE': '1'}))
            stack.enter_context(patch.object(runner.subprocess, 'run', side_effect=result))
            try:
                runner.main()
            except (OSError, subprocess.TimeoutExpired, AssertionError):
                pass
        self.assertEqual(len(receipts), 1)
        self.assertNotIn('PRIVATE_EXCEPTION_DETAIL', receipts[0])
        return json.loads(receipts[0])

    def test_timeout_is_reported_as_incomplete(self):
        report = self.exercise(subprocess.TimeoutExpired('PRIVATE_EXCEPTION_DETAIL', 1))
        self.assertFalse(report['completed'])
        self.assertEqual(report['cases'][0]['outcome'], 'timeout')

    def test_start_failure_is_reported_as_incomplete(self):
        report = self.exercise(OSError('PRIVATE_EXCEPTION_DETAIL'))
        self.assertFalse(report['completed'])
        self.assertEqual(report['cases'][0]['outcome'], 'start_failed')

    def test_zero_selected_tests_cannot_pass(self):
        report = self.exercise([types.SimpleNamespace(returncode=0, stdout='test result: ok. 0 passed; 0 failed; 0 ignored;')])
        self.assertFalse(report['completed'])
        self.assertEqual(report['cases'][0]['outcome'], 'test_count_mismatch')

    def test_all_four_contracts_required(self):
        report = self.exercise([types.SimpleNamespace(returncode=0, stdout='test result: ok. 3 passed; 0 failed; 0 ignored;') for _ in range(4)])
        self.assertTrue(report['completed'])
        self.assertEqual(len(report['cases']), 4)
        self.assertFalse(report['allInstalledGuestsQualified'])
        self.assertFalse(report['installedEquivalence'])


if __name__ == '__main__':
    unittest.main()
