"""Synthetic proc syscall traces only; no process launches or production changes."""
import errno
from pathlib import Path
import unittest
from unittest import mock

import percival_acceptance as a


def proc_stat(state, ppid=100, start=12345):
    fields = [state, str(ppid)] + ["0"] * 17 + [str(start)]
    return "200 (fixture) " + " ".join(fields)


AUTHORITY = {"Uid": [999] * 4, "Gid": [987] * 4,
             "Groups": [987], "CapEff": 0}


class ExecutableTrace(unittest.TestCase):
    def trace(self, states, executable_error):
        with mock.patch.object(Path, "read_text", side_effect=states), \
             mock.patch.object(Path, "stat", side_effect=executable_error), \
             mock.patch.object(a, "proc_authority", return_value=AUTHORITY):
            return a.process_identity(200)

    def test_non_z_state_is_discarded_by_existing_parser(self):
        self.assertEqual(a.parse_proc_stat(proc_stat("R")), a.parse_proc_stat(proc_stat("S")))

    def test_enoent_with_same_identity_non_z_fails_closed(self):
        # Compatible with both an exit-mm window and a persistently missing exe.
        # This trace alone cannot identify which actually happened on the VPS.
        for before, after in (("R", "R"), ("R", "S"), ("S", "S")):
            with self.subTest(before=before, after=after), \
                 mock.patch.object(a.time, "sleep"), \
                 self.assertRaisesRegex(RuntimeError, "live_process_executable_unavailable"):
                self.trace([proc_stat(before), proc_stat(after), proc_stat(after), proc_stat(after)],
                           FileNotFoundError(errno.ENOENT, "synthetic"))

    def test_enoent_then_same_identity_terminal_z_is_validated_child_exit(self):
        leader = Path("/proc/200/task/200")
        with mock.patch.object(a, "proc_authority", return_value=AUTHORITY), \
             mock.patch.object(Path, "iterdir", return_value=iter([leader])), \
             mock.patch.object(a.time, "sleep"), \
             mock.patch.object(Path, "read_text", side_effect=[
                 proc_stat("R"), proc_stat("R"), proc_stat("Z"), proc_stat("Z"), proc_stat("Z")
             ]), \
             mock.patch.object(Path, "stat", side_effect=FileNotFoundError(errno.ENOENT, "synthetic")):
            with self.assertRaises(a.SnapshotChildExited):
                a.process_identity(200)

    def test_permission_denial_is_not_missing_executable(self):
        with self.assertRaises(PermissionError):
            self.trace([proc_stat("R")], PermissionError(errno.EACCES, "synthetic"))

    def test_ancestry_or_pid_reuse_is_rejected(self):
        for after in (proc_stat("R", ppid=101), proc_stat("R", start=12346)):
            with self.subTest(after=after), self.assertRaisesRegex(RuntimeError, "process_identity_race"):
                self.trace([proc_stat("R"), after], FileNotFoundError(errno.ENOENT, "synthetic"))

    def test_z_leader_with_live_thread_is_not_discarded(self):
        leader = Path("/proc/200/task/200")
        worker = Path("/proc/200/task/201")
        with mock.patch.object(a, "proc_authority", return_value=AUTHORITY), \
             mock.patch.object(Path, "iterdir", return_value=iter([leader, worker])), \
             mock.patch.object(Path, "read_text", side_effect=[proc_stat("Z"), proc_stat("Z"), proc_stat("S")]):
            with self.assertRaisesRegex(RuntimeError, "terminal_process_has_live_threads"):
                a.terminal_snapshot(200, a.parse_proc_stat(proc_stat("Z")), AUTHORITY)

    def test_exit_classification_still_requires_direct_parent(self):
        error = a.SnapshotChildExited(200, {"ppid": 101, "starttime": 12345, "terminal_state": "Z"})
        with self.assertRaisesRegex(RuntimeError, "exited_process_unverified_ancestry"):
            a.validate_child_exit(error, 100)


if __name__ == "__main__":
    unittest.main()
