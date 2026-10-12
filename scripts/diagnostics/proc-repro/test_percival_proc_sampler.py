import copy
import errno
import os
import sys
import time
import unittest
from unittest import mock

import percival_proc_sampler as s


def sample(state="R", failed=True):
    record = {"pid": 200, "ppid": 100, "starttime": 1234, "state": state, "threads": 1}
    value = {"pid": 200, "before": record, "after": dict(record),
             "authority": {"Uid": [999]*4, "Gid": [987]*4, "Groups": [987], "CapEff": 0}}
    if failed:
        value["failure"] = {"syscall_stage": "stat_exe", "errno": errno.ENOENT}
        value["threads_after"] = {"states": [{"tid": 200, "state": state, "starttime": 1234}], "omitted": 0}
        value["closing"] = dict(record)
    return value


class SamplerTrace(unittest.TestCase):
    def test_same_identity_non_z_to_all_z_observed(self):
        self.assertEqual(s.classify([sample(), sample("Z")]), "non_z_exe_loss_then_same_identity_all_z_observed")

    def test_persistent_window_does_not_claim_permanent_defect(self):
        self.assertEqual(s.classify([sample()]*3), "unavailable_through_bounded_observation_not_proven_permanent")

    def test_permission_denial_separate(self):
        value = sample()
        value["failure"]["errno"] = errno.EACCES
        self.assertEqual(s.classify([value]), "permission_denial_observed")

    def test_identity_parent_and_authority_drift_cannot_certify_exit(self):
        for field in ("ppid", "starttime"):
            value = sample("Z")
            value["after"][field] += 1
            self.assertEqual(s.classify([sample(), value]), "identity_or_parent_drift_observed")
        value = sample("Z")
        value["authority"]["Uid"] = [1001]*4
        self.assertNotEqual(s.classify([sample(), value]), "non_z_exe_loss_then_same_identity_all_z_observed")

    def test_live_thread_omitted_thread_or_error_cannot_certify_exit(self):
        value = sample("Z")
        value["threads_after"]["states"].append({"tid": 201, "state": "S", "starttime": 999})
        self.assertEqual(s.classify([sample(), value]), "z_leader_with_live_threads_observed")
        for key, number in (("omitted", 1), ("errno", errno.ENOENT)):
            value = sample("Z")
            value["threads_after"][key] = number
            self.assertNotEqual(s.classify([sample(), value]), "non_z_exe_loss_then_same_identity_all_z_observed")

    def test_only_two_followups_and_no_acceptance_decision(self):
        reader = mock.Mock(return_value=sample())
        sleeper = mock.Mock()
        result = s.diagnose(sample(), 100, time.monotonic()+10, lambda anchor: None, reader, sleeper)
        self.assertEqual(reader.call_count, 2)
        self.assertEqual(sleeper.call_args_list, [mock.call(0.1), mock.call(0.1)])
        self.assertEqual(result["parent_relation"], "direct_child")
        self.assertTrue(result["diagnosis_only_no_member_discarded"])

    def test_deadline_prevents_followups(self):
        reader = mock.Mock()
        s.diagnose(sample(), 100, time.monotonic(), lambda anchor: None, reader, mock.Mock())
        reader.assert_not_called()

    def test_no_stale_authority_or_missing_closing_identity_exit_proof(self):
        for key in ("authority", "closing"):
            value = sample("Z")
            del value[key]
            self.assertNotEqual(s.classify([sample(), value]), "non_z_exe_loss_then_same_identity_all_z_observed")
        value = sample("Z")
        value["closing"]["starttime"] += 1
        self.assertEqual(s.classify([sample(), value]), "identity_or_parent_drift_observed")

    def test_followup_membership_or_identity_loss_prevents_recapture(self):
        for reason in ("target_no_longer_in_hotel_cgroup", "target_identity_or_parent_changed"):
            reader = mock.Mock()
            result = s.diagnose(sample(), 100, time.monotonic()+10, lambda anchor: reason, reader, mock.Mock())
            reader.assert_not_called()
            self.assertEqual(result["classification"], "diagnostic_scope_or_identity_fence_lost")
            self.assertEqual(result["followup_fence_failure"], reason)

    def test_capture_reuse_after_fence_does_not_read_new_authority_or_exe(self):
        changed = {**sample()["before"], "starttime": 9999}
        with mock.patch.object(s, "proc_stat", return_value=changed), \
             mock.patch.object(s, "authority") as authority, \
             mock.patch.object(s.Path, "stat") as exe:
            value = s.capture(200, expected=sample()["before"])
        authority.assert_not_called()
        exe.assert_not_called()
        self.assertEqual(value["failure"]["syscall_stage"], "identity_guard")

    def test_initial_membership_and_anchor_fenced_before_capture(self):
        reader = mock.Mock(return_value=sample())
        stat_reader = mock.Mock(return_value=sample()["before"])
        fence = mock.Mock(return_value=None)
        s.scoped_capture(200, lambda pid: True, fence, reader, stat_reader)
        fence.assert_called_once_with(sample()["before"])
        reader.assert_called_once_with(200, expected=sample()["before"])
        reader.reset_mock(); stat_reader.reset_mock()
        value=s.scoped_capture(200,lambda pid:False,fence,reader,stat_reader)
        stat_reader.assert_not_called();reader.assert_not_called()
        self.assertIn("scope_failure",value)
        value=s.scoped_capture(200,lambda pid:True,lambda anchor:"target_identity_or_parent_changed",reader,stat_reader)
        reader.assert_not_called()
        self.assertIn("scope_failure",value)

    def test_exact_syscall_and_errno_captured(self):
        with mock.patch.object(s, "proc_stat", return_value=sample()["before"]), \
             mock.patch.object(s, "authority", return_value=sample()["authority"]), \
             mock.patch.object(s, "threads", return_value=sample()["threads_after"]), \
             mock.patch.object(s.Path, "stat", side_effect=FileNotFoundError(errno.ENOENT,"synthetic")):
            value = s.capture(200)
        self.assertEqual(value["failure"], {"syscall_stage":"stat_exe", "errno":errno.ENOENT})
        with mock.patch.object(s, "proc_stat", return_value=sample()["before"]), \
             mock.patch.object(s, "authority", return_value=sample()["authority"]), \
             mock.patch.object(s, "threads", return_value=sample()["threads_after"]), \
             mock.patch.object(s.Path, "stat", return_value=mock.Mock(st_dev=1,st_ino=2)), \
             mock.patch.object(s.os,"readlink",side_effect=PermissionError(errno.EACCES,"synthetic")):
            value = s.capture(200)
        self.assertEqual(value["failure"], {"syscall_stage":"readlink_exe", "errno":errno.EACCES})

    @unittest.skipUnless(sys.platform.startswith("linux") and hasattr(os,"fork"), "No local Linux runtime")
    def test_local_linux_short_lived_child_metadata(self):
        pid = os.fork()
        if pid == 0:
            os._exit(0)
        try:
            deadline = time.monotonic()+1
            samples=[]
            while time.monotonic()<deadline:
                value=s.capture(pid);samples.append(value)
                if value.get("after",{}).get("state")=="Z":break
                time.sleep(.01)
            self.assertTrue(any(v.get("after",{}).get("state")=="Z" for v in samples))
        finally:
            os.waitpid(pid,0)


if __name__ == "__main__":
    unittest.main()
