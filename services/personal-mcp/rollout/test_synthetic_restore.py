import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

import synthetic_restore as rollback


class ColdStateRestoreTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='muninn-synthetic-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'quiescent.json').write_text(json.dumps(rollback.SEAL))
        for name in rollback.FILES:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            if name.endswith('.db'):
                with sqlite3.connect(path) as db:
                    db.execute('CREATE TABLE fixture_state(key TEXT PRIMARY KEY, value TEXT NOT NULL)')
                    db.execute('INSERT INTO fixture_state VALUES(?,?)', ('synthetic', 'baseline'))
            else:
                path.write_bytes(b'public synthetic baseline blob')

    def mutate(self):
        for name in rollback.FILES:
            path = self.root / name
            if name.endswith('.db'):
                with sqlite3.connect(path) as db:
                    db.execute('UPDATE fixture_state SET value=?', ('candidate',))
                    db.execute('CREATE TABLE candidate_only_schema(id INTEGER)')
            else:
                path.write_bytes(b'public synthetic candidate blob')

    def test_restore_returns_exact_cold_state_and_removes_candidate_schema(self):
        before = {n: (self.root / n).read_bytes() for n in rollback.FILES}
        rollback.snapshot(self.root)
        self.mutate()
        self.assertEqual(rollback.restore(self.root)['restored_files'], 4)
        for name, value in before.items():
            self.assertEqual((self.root / name).read_bytes(), value)
            if name.endswith('.db'):
                with sqlite3.connect(self.root / name) as db:
                    self.assertEqual(db.execute('PRAGMA integrity_check').fetchone()[0], 'ok')
                    self.assertEqual(db.execute('SELECT value FROM fixture_state').fetchone()[0], 'baseline')
                    self.assertIsNone(db.execute("SELECT name FROM sqlite_master WHERE name='candidate_only_schema'").fetchone())

    def test_running_fixture_denies_snapshot_and_restore(self):
        rollback.snapshot(self.root)
        (self.root / 'quiescent.json').write_text(json.dumps({**rollback.SEAL, 'active_processes': 1}))
        for operation in [rollback.snapshot, rollback.restore]:
            with self.assertRaises(ValueError):
                operation(self.root)

    def test_production_paths_refused_before_read(self):
        with self.assertRaises(ValueError):
            rollback.guard('/opt/philotic/data')

    def test_wal_sidecar_denies_cold_snapshot(self):
        (self.root / 'hotel.db-wal').write_bytes(b'synthetic pending writer')
        with self.assertRaises(ValueError):
            rollback.snapshot(self.root)

    def test_corrupt_snapshot_denies_before_any_target_mutation(self):
        rollback.snapshot(self.root)
        self.mutate()
        before = {n: (self.root / n).read_bytes() for n in rollback.FILES}
        (self.root / 'rollback' / rollback.FILES[-1]).write_bytes(b'corrupt')
        with self.assertRaises(ValueError):
            rollback.restore(self.root)
        self.assertEqual(before, {n: (self.root / n).read_bytes() for n in rollback.FILES})

    def test_snapshot_symlink_denies(self):
        rollback.snapshot(self.root)
        path = self.root / 'rollback' / 'hotel.db'
        path.unlink()
        path.symlink_to(self.root / 'hotel.db')
        with self.assertRaises(OSError):
            rollback.restore(self.root)

    def test_interrupted_restore_keeps_snapshot_and_can_be_repeated(self):
        before = {n: (self.root / n).read_bytes() for n in rollback.FILES}
        rollback.snapshot(self.root)
        self.mutate()
        import os
        real = os.replace
        count = 0
        def interruption(src, dst):
            nonlocal count
            count += 1
            if count == 2:
                raise OSError('synthetic process interruption')
            return real(src, dst)
        with patch.object(rollback.os, 'replace', interruption), self.assertRaises(OSError):
            rollback.restore(self.root)
        self.assertTrue((self.root / 'rollback' / 'manifest.json').is_file())
        rollback.restore(self.root)
        self.assertEqual(before, {n: (self.root / n).read_bytes() for n in rollback.FILES})

    def test_source_alias_denies(self):
        path = self.root / 'hotel.db'
        path.unlink()
        path.symlink_to(self.root / 'training.db')
        with self.assertRaises(OSError):
            rollback.snapshot(self.root)

    def test_crash_residue_and_alias_do_not_block_retry_or_get_touched(self):
        before = {n: (self.root / n).read_bytes() for n in rollback.FILES}
        rollback.snapshot(self.root)
        self.mutate()
        residue = self.root / 'hotel.db.restore.interrupted'
        residue.write_bytes(b'partial public fixture')
        old_fixed = self.root / 'hotel.db.restore.tmp'
        old_fixed.symlink_to(residue)
        rollback.restore(self.root)
        self.assertEqual(before, {n: (self.root / n).read_bytes() for n in rollback.FILES})
        self.assertEqual(residue.read_bytes(), b'partial public fixture')
        self.assertTrue(old_fixed.is_symlink())

    def test_invalid_later_target_denies_before_any_target_mutation(self):
        rollback.snapshot(self.root)
        self.mutate()
        before = (self.root / rollback.FILES[0]).read_bytes()
        target = self.root / rollback.FILES[-1]
        target.unlink()
        target.symlink_to(self.root / rollback.FILES[0])
        with self.assertRaises(ValueError):
            rollback.restore(self.root)
        self.assertEqual((self.root / rollback.FILES[0]).read_bytes(), before)


if __name__ == '__main__':
    unittest.main()
