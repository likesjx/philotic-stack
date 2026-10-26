"""Actual SQLite writer barriers/backups on disposable public state only."""
from contextlib import closing
import json
import os
from pathlib import Path
import sqlite3
import stat
import tempfile
import unittest
from unittest.mock import patch
from provision import Refused
import runtime_state as state


class Backup(unittest.TestCase):
    def fixture(self, training=True, wal=False):
        td = tempfile.TemporaryDirectory()
        self.addCleanup(td.cleanup)
        root = Path(td.name)
        source = root / 'state'
        source.mkdir()
        parent = root / 'private-backups'
        parent.mkdir(mode=0o700)
        paths = [source / 'aiua_context.db', source / 'whisper_training.db']
        for n, path in enumerate(paths):
            if n and not training:
                continue
            db = sqlite3.connect(path)
            if wal:
                db.execute('PRAGMA journal_mode=WAL')
            db.execute('CREATE TABLE public_fixture(value INTEGER)')
            db.execute('INSERT INTO public_fixture VALUES(?)', (n + 42,))
            db.commit()
            path.chmod(0o600 if n == 0 else 0o644)
            if wal:
                self.addCleanup(db.close)  # Keep committed data in WAL during backup.
            else:
                db.close()
        return paths, parent / 'snapshot'

    def snapshot(self, paths, target, check=lambda: None, **kw):
        return state.snapshot(paths, target, check, data_identity=(os.getuid(), os.getgid()), **kw)

    def test_private_consistent_pair_preserves_original_metadata_and_no_row_output(self):
        paths, target = self.fixture()
        before = [state.identity(path) for path in paths]
        result = self.snapshot(paths, target)
        self.assertEqual(result, {'backupComplete': True, 'databaseCount': 2, 'trainingAbsentPreserved': False})
        self.assertEqual([state.identity(path) for path in paths], before)
        manifest = json.loads((target / 'state.json').read_text())
        self.assertTrue(manifest['completed'])
        for n, path in enumerate(paths):
            with closing(sqlite3.connect(target / path.name)) as db:
                self.assertEqual(db.execute('SELECT value FROM public_fixture').fetchall(), [(n + 42,)])
            self.assertEqual(stat.S_IMODE((target / path.name).stat().st_mode), 0o600)
            self.assertEqual(manifest['files'][path.name]['original']['mode'], before[n]['mode'])
        self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE((target / 'state.json').stat().st_mode), 0o600)

    def test_wal_pages_included_and_both_external_writers_blocked(self):
        paths, target = self.fixture(wal=True)
        observations = []
        count = [0]
        def quiescent():
            count[0] += 1
            if count[0] == 2:
                for path in paths:
                    with closing(sqlite3.connect(path, timeout=.01)) as writer:
                        with self.assertRaises(sqlite3.OperationalError):
                            writer.execute('INSERT INTO public_fixture VALUES(99)')
                        observations.append('writer_blocked')
        self.assertTrue(self.snapshot(paths, target, quiescent)['backupComplete'])
        self.assertEqual(observations, ['writer_blocked', 'writer_blocked'])
        for n, path in enumerate(paths):
            with closing(sqlite3.connect(target / path.name)) as db:
                self.assertEqual(db.execute('SELECT value FROM public_fixture').fetchall(), [(n + 42,)])
            with closing(sqlite3.connect(path, timeout=.01)) as db:
                db.execute('BEGIN IMMEDIATE')  # Barriers were closed/released.
                db.rollback()

    def test_absent_training_is_recorded_not_initialized(self):
        paths, target = self.fixture(training=False)
        self.assertTrue(self.snapshot(paths, target)['trainingAbsentPreserved'])
        self.assertFalse(paths[1].exists())
        self.assertFalse((target / paths[1].name).exists())

    def test_occupied_snapshot_is_never_reused_or_deleted(self):
        paths, target = self.fixture()
        target.mkdir()
        (target / 'sentinel').write_text('public evidence')
        self.assertRaises(FileExistsError, self.snapshot, paths, target)
        self.assertEqual((target / 'sentinel').read_text(), 'public evidence')

    def test_service_refusal_and_bad_mode_precede_snapshot_creation(self):
        paths, target = self.fixture()
        def refuse():
            raise Refused('public fixture writer still active')
        self.assertRaises(Refused, self.snapshot, paths, target, refuse)
        self.assertFalse(target.exists())
        paths[0].chmod(0o666)
        self.assertRaises(Refused, self.snapshot, paths, target)
        self.assertFalse(target.exists())

    def test_timeout_retains_partial_directory_without_completed_manifest(self):
        paths, target = self.fixture()
        with patch.object(state.time, 'monotonic', side_effect=[0, 2]):
            self.assertRaises(Refused, self.snapshot, paths, target, timeout=1)
        self.assertTrue(target.is_dir())
        self.assertFalse((target / 'state.json').exists())

    def test_source_drift_after_backups_refuses_completion_and_preserves_evidence(self):
        paths, target = self.fixture()
        count = [0]
        def check():
            count[0] += 1
            if count[0] == 3:
                info = paths[0].stat()
                os.utime(paths[0], ns=(info.st_atime_ns, info.st_mtime_ns + 1000000))
        self.assertRaises(Refused, self.snapshot, paths, target, check)
        self.assertTrue((target / paths[0].name).exists())
        self.assertFalse((target / 'state.json').exists())

    def test_parent_directory_entry_is_fsynced(self):
        paths, target = self.fixture()
        parent = target.parent.stat()
        observed = []
        sync = os.fsync
        def fsync(fd):
            info = os.fstat(fd)
            observed.append((info.st_dev, info.st_ino))
            sync(fd)
        with patch.object(state.os, 'fsync', side_effect=fsync):
            self.assertTrue(self.snapshot(paths, target)['backupComplete'])
        self.assertIn((parent.st_dev, parent.st_ino), observed)

    def test_logical_wal_growth_is_bounded_even_when_main_file_is_small(self):
        paths, target = self.fixture(training=False, wal=True)
        with closing(sqlite3.connect(paths[0])) as db:
            db.execute('INSERT INTO public_fixture VALUES(?)', ('public' * 4000,))
            db.commit()
            logical = db.execute('PRAGMA page_count').fetchone()[0] * db.execute('PRAGMA page_size').fetchone()[0]
        self.assertLess(paths[0].stat().st_size, logical)
        with patch.object(state, 'MAX_BYTES', paths[0].stat().st_size):
            self.assertRaises(Refused, self.snapshot, paths, target)
        self.assertFalse((target / 'state.json').exists())

    def test_slow_hash_and_final_quiescence_cannot_report_success(self):
        for phase in ('hash', 'final_check'):
            paths, target = self.fixture()
            clock = [0]
            count = [0]
            original_digest = state.digest_file
            def digest(path, check):
                clock[0] = 2
                return original_digest(path, check)
            def quiescent():
                count[0] += 1
                if phase == 'final_check' and count[0] == 3:
                    clock[0] = 2
            with patch.object(state.time, 'monotonic', side_effect=lambda: clock[0]):
                if phase == 'hash':
                    with patch.object(state, 'digest_file', side_effect=digest):
                        self.assertRaises(Refused, self.snapshot, paths, target, quiescent, timeout=1)
                else:
                    self.assertRaises(Refused, self.snapshot, paths, target, quiescent, timeout=1)
            self.assertFalse((target / 'state.json').exists())

    def restore(self, paths, baseline, failed, check=lambda: None):
        return state.restore(paths, baseline, failed, check,
                             manifest_sha256=state.digest_file(baseline / 'state.json'),
                             data_identity=(os.getuid(), os.getgid()))

    def test_restore_preserves_failed_candidate_rows_then_restores_pair_and_modes(self):
        paths, baseline = self.fixture()
        self.snapshot(paths, baseline)
        for path in paths:
            with closing(sqlite3.connect(path)) as db:
                db.execute('INSERT INTO public_fixture VALUES(99)');db.commit()
        failed = baseline.with_name('failed-candidate')
        result = self.restore(paths, baseline, failed)
        self.assertTrue(result['stateRestored'])
        self.assertTrue(result['failedCandidateStatePreserved'])
        for n, path in enumerate(paths):
            with closing(sqlite3.connect(path)) as db:
                self.assertEqual(db.execute('SELECT value FROM public_fixture').fetchall(), [(n + 42,)])
            with closing(sqlite3.connect(failed / path.name)) as db:
                self.assertEqual(db.execute('SELECT value FROM public_fixture').fetchall(), [(n + 42,), (99,)])
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600 if n == 0 else 0o644)

    def test_restore_preserves_candidate_training_before_restoring_absence(self):
        paths, baseline = self.fixture(training=False)
        self.snapshot(paths, baseline)
        with closing(sqlite3.connect(paths[1])) as db:
            db.execute('CREATE TABLE public_training(value TEXT)')
            db.execute("INSERT INTO public_training VALUES('public candidate state')");db.commit()
        paths[1].chmod(0o644)
        failed = baseline.with_name('failed-candidate')
        self.assertTrue(self.restore(paths, baseline, failed)['trainingAbsenceRestored'])
        self.assertFalse(paths[1].exists())
        self.assertTrue((failed / paths[1].name).exists())

    def test_changed_baseline_cannot_overwrite_live_state(self):
        paths, baseline = self.fixture()
        self.snapshot(paths, baseline)
        original = paths[0].read_bytes()
        (baseline / paths[0].name).write_bytes(b'public corrupted baseline')
        failed = baseline.with_name('failed-candidate')
        self.assertRaises(Refused, self.restore, paths, baseline, failed)
        self.assertEqual(paths[0].read_bytes(), original)
        self.assertFalse(failed.exists())


if __name__ == '__main__':
    unittest.main()
