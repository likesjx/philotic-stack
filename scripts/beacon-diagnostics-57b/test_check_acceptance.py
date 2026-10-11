import contextlib
import io
from pathlib import Path
import runpy
import subprocess
import unittest
from unittest.mock import patch


class ExecutionGuard(unittest.TestCase):
    def invoke(self, stdout, code=0):
        result = subprocess.CompletedProcess([], code, stdout, "")
        with patch("subprocess.run", return_value=result), contextlib.redirect_stdout(io.StringIO()):
            return runpy.run_path(str(Path(__file__).with_name("check_acceptance.py")))

    def test_one_executed_pass(self):
        self.invoke("test result: ok. 1 passed; 0 failed; 0 ignored;")

    def test_zero_ignored_failed_and_extra_tests_are_denied(self):
        for summary, code in [
            ("test result: ok. 0 passed; 0 failed; 0 ignored;", 0),
            ("test result: ok. 0 passed; 0 failed; 1 ignored;", 0),
            ("test result: FAILED. 0 passed; 1 failed; 0 ignored;", 101),
            ("test result: ok. 2 passed; 0 failed; 0 ignored;", 0),
            ("test result: ok. 1 passed; 0 failed; 0 ignored;", 101),
        ]:
            with self.subTest(summary=summary, code=code), self.assertRaises(SystemExit):
                self.invoke(summary, code)


if __name__ == "__main__":
    unittest.main()
