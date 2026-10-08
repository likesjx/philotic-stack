"""Synthetic role-module provenance tests: no systemctl calls or service changes."""
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest

SOURCE = Path(__file__).resolve().parents[3] / 'ansible/module_utils/percival_provenance.py'
spec = importlib.util.spec_from_file_location('percival_provenance', SOURCE)
provenance = importlib.util.module_from_spec(spec)
spec.loader.exec_module(provenance)


class ProvenanceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.approved = self.root / 'release-aiua'
        self.approved.write_bytes(b'public synthetic approved executable')
        self.approved.chmod(0o755)
        self.old = self.root / 'legacy-aiua'
        self.old.write_bytes(b'public synthetic legacy executable')
        self.old.chmod(0o755)
        self.sha = hashlib.sha256(self.approved.read_bytes()).hexdigest()
        self.proc = self.root / 'proc/42'
        self.proc.mkdir(parents=True)

    def properties(self, path):
        return {'ExecStart': '{ path=' + str(path) + ' ; argv[]=aiua ; ignore_errors=no ; }',
                'MainPID': '42', 'NeedDaemonReload': 'no'}

    def verify(self, props, running=False):
        return provenance.verify(props, str(self.approved), self.sha, running, str(self.root / 'proc'))

    def test_legacy_execstart_mismatch_rejected(self):
        with self.assertRaises(provenance.ProvenanceError):
            self.verify(self.properties(self.old))

    def test_approved_effective_path_admitted(self):
        self.assertEqual(self.verify(self.properties(self.approved))['sha256'], self.sha)

    def test_versioned_symlink_resolves_to_approved_artifact(self):
        current = self.root / 'current'
        current.symlink_to(self.approved)
        (self.proc / 'exe').symlink_to(self.approved)
        self.assertEqual(self.verify(self.properties(current), True)['main_pid'], 42)

    def test_stale_running_executable_rejected(self):
        (self.proc / 'exe').symlink_to(self.old)
        with self.assertRaises(provenance.ProvenanceError):
            self.verify(self.properties(self.approved), True)

    def test_incorrect_checksum_rejected(self):
        self.sha = '0' * 64
        with self.assertRaises(provenance.ProvenanceError):
            self.verify(self.properties(self.approved))

    def test_pending_daemon_reload_rejected(self):
        properties = self.properties(self.approved)
        properties['NeedDaemonReload'] = 'yes'
        with self.assertRaises(provenance.ProvenanceError):
            self.verify(properties)

    def test_missing_pid_or_deleted_executable_rejected(self):
        properties = self.properties(self.approved)
        properties['MainPID'] = '0'
        with self.assertRaises(provenance.ProvenanceError):
            self.verify(properties, True)
        (self.proc / 'exe').symlink_to(str(self.approved) + ' (deleted)')
        with self.assertRaises(provenance.ProvenanceError):
            self.verify(self.properties(self.approved), True)

    def test_multiple_commands_rejected(self):
        properties = self.properties(self.approved)
        properties['ExecStart'] += ' ' + properties['ExecStart']
        with self.assertRaises(provenance.ProvenanceError):
            self.verify(properties)


if __name__ == '__main__':
    unittest.main()
