import pathlib
import tempfile
import unittest
from unittest import mock

import systemd_socket_rehearsal as harness

class HarnessGuardTests(unittest.TestCase):
    def test_unit_names_are_exact_and_reject_untrusted_tokens(self):
        service, sock = harness.unit_names('a' * 32)
        self.assertEqual(service, 'philotic-synthetic-socket-' + 'a' * 32 + '.service')
        self.assertEqual(sock, 'philotic-synthetic-socket-' + 'a' * 32 + '.socket')
        for token in ('../x', '', 'g' * 32, 'a' * 31, 'A' * 32):
            with self.assertRaises(harness.Stop):
                harness.unit_names(token)

    def test_runtime_refuses_before_any_fixture_when_not_root(self):
        with mock.patch.object(harness.sys, 'platform', 'linux'), mock.patch.object(harness.os, 'geteuid', return_value=1234), mock.patch.object(harness.tempfile, 'mkdtemp') as create:
            with self.assertRaises(harness.Stop):
                harness.run('/invalid', 'a' * 64)
            create.assert_not_called()

    def test_existing_unit_file_is_never_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / 'unit.service'
            path.write_bytes(b'unchanged')
            with self.assertRaises(FileExistsError):
                harness.create_unit(path, b'replacement')
            self.assertEqual(path.read_bytes(), b'unchanged')

    def test_manager_failures_do_not_become_success(self):
        with mock.patch.object(harness, 'command', return_value=subprocess_result(1, b'')):
            with self.assertRaises(harness.Stop):
                harness.must('start', 'synthetic.service')

    def test_masked_start_denial_requires_attributable_result(self):
        unit = 'philotic-synthetic-socket-' + 'a' * 32 + '.service'
        good = subprocess_result(1, b'')
        good.stderr = ('Failed to start: Unit ' + unit + ' is masked.').encode()
        harness.require_masked_start_denial(good, unit)
        for code, diagnostic in [(0, good.stderr), (1, b'permission denied'), (1, b'Unit another.service is masked')]:
            value = subprocess_result(code, b'')
            value.stderr = diagnostic
            with self.assertRaises(harness.Stop):
                harness.require_masked_start_denial(value, unit)


def subprocess_result(code, out):
    result = mock.Mock()
    result.returncode = code
    result.stdout = out
    return result

if __name__ == '__main__':
    unittest.main()
