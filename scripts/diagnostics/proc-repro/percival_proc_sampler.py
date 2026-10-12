"""Separate bounded read-only proc sampler. Never used for acceptance decisions."""
import argparse
from collections import Counter
import errno
from itertools import islice
import json
import os
from pathlib import Path
import subprocess
import time

PROC = Path("/proc")
GROUP = "/system.slice/proc-repro-hotel.service"
UNIT = "proc-repro-hotel.service"
LIMIT = 8192


def proc_stat(pid):
    raw = (PROC / str(pid) / "stat").read_text()
    fields = raw[raw.rfind(")") + 2:].split()
    if len(fields) < 20:
        raise ValueError("invalid_numeric_metadata")
    return {"pid": pid, "state": fields[0], "ppid": int(fields[1]),
            "threads": int(fields[17]), "starttime": int(fields[19])}


def authority(pid):
    result = {}
    with open(PROC / str(pid) / "status") as stream:
        for line in stream:
            key, _, value = line.partition(":")
            if key in ("Uid", "Gid", "Groups"):
                result[key] = [int(x) for x in value.split()]
            elif key == "CapEff":
                result[key] = int(value.strip(), 16)
    return result


def threads(pid):
    result = {"states": [], "omitted": 0}
    try:
        tasks = list(islice((PROC / str(pid) / "task").iterdir(), 33))
        result["omitted"] = max(0, len(tasks) - 32)
        for task in tasks[:32]:
            raw = (task / "stat").read_text()
            fields = raw[raw.rfind(")") + 2:].split()
            result["states"].append({"tid": int(task.name), "state": fields[0],
                                     "starttime": int(fields[19])})
    except OSError as error:
        result["errno"] = error.errno
    return result


def capture(pid, expected=None):
    result = {"pid": pid, "at_monotonic_ns": time.monotonic_ns()}
    stage = "stat_before"
    try:
        result["before"] = proc_stat(pid)
        if expected is not None and identity(result["before"]) != identity(expected):
            result["failure"] = {"syscall_stage": "identity_guard", "errno": None}
            return result
        stage = "authority_status"
        result["authority"] = authority(pid)
        stage = "stat_exe"
        info = (PROC / str(pid) / "exe").stat()
        result["executable"] = [info.st_dev, info.st_ino]
        stage = "readlink_exe"
        # No executable pathname is returned or retained in diagnostic output.
        os.readlink(PROC / str(pid) / "exe")
        stage = "stat_after"
        result["after"] = proc_stat(pid)
    except OSError as error:
        result["failure"] = {"syscall_stage": stage, "errno": error.errno}
        try:
            result["after"] = proc_stat(pid)
        except OSError as after_error:
            result["after_errno"] = after_error.errno
        result["threads_after"] = threads(pid)
        try:
            result["closing"] = proc_stat(pid)
        except OSError as closing_error:
            result["closing_errno"] = closing_error.errno
    return result


def identity(record):
    return (record["pid"], record["starttime"], record["ppid"])


def scoped_capture(pid, member, fence, reader=capture, stat_reader=proc_stat):
    result = {"pid": pid}
    if not member(pid):
        result["scope_failure"] = "initial_target_not_in_hotel_cgroup"
        return result
    try:
        anchor = stat_reader(pid)
    except OSError as error:
        result["failure"] = {"syscall_stage": "stat_initial_anchor", "errno": error.errno}
        return result
    result["before"] = anchor
    failure = fence(anchor)
    if failure:
        result["scope_failure"] = failure
        return result
    return reader(pid, expected=anchor)


def classify(samples):
    initial = samples[0]
    if any(s.get("failure", {}).get("errno") in (errno.EACCES, errno.EPERM) for s in samples):
        return "permission_denial_observed"
    anchor = initial.get("before")
    if not anchor:
        return "initial_process_metadata_unavailable"
    records = [s[k] for s in samples for k in ("before", "after", "closing") if k in s]
    if any(identity(r) != identity(anchor) for r in records):
        return "identity_or_parent_drift_observed"
    if initial.get("failure", {}).get("syscall_stage") not in ("stat_exe", "readlink_exe"):
        return "not_an_executable_lookup_failure"
    if initial["failure"]["errno"] != errno.ENOENT:
        return "other_executable_lookup_error"
    for sample in samples:
        states = sample.get("threads_after", {})
        after = sample.get("after", {})
        if after.get("state") == "Z" and not states.get("errno") and not states.get("omitted"):
            if any(t["state"] != "Z" for t in states.get("states", [])):
                return "z_leader_with_live_threads_observed"
            auth = sample.get("authority")
            closing = sample.get("closing", {})
            if (states.get("states") and closing.get("state") == "Z" and
                    closing.get("threads") == len(states["states"]) and
                    any(t["tid"] == anchor["pid"] and t["starttime"] == anchor["starttime"]
                        for t in states["states"]) and auth == initial.get("authority") and auth == {
                    "Uid": [999] * 4, "Gid": [987] * 4, "Groups": [987], "CapEff": 0}):
                return ("non_z_exe_loss_then_same_identity_all_z_observed" if anchor["state"] != "Z"
                        else "already_z_executable_unavailable")
    if any(s.get("after_errno") in (errno.ENOENT, errno.ESRCH) for s in samples):
        return "missing_exe_then_process_metadata_vanished"
    if all(s.get("failure", {}).get("errno") == errno.ENOENT for s in samples):
        return "unavailable_through_bounded_observation_not_proven_permanent"
    return "lookup_recovered_without_exit_proof"


def diagnose(first, main_pid, deadline, fence, reader=capture, sleeper=time.sleep):
    samples = [first]
    fence_failure = None
    anchor = first.get("before")
    for unused in range(2):
        if anchor is None or time.monotonic() + 0.1 >= deadline:
            break
        sleeper(0.1)
        fence_failure = fence(anchor)
        if fence_failure:
            break
        value = reader(first["pid"], expected=anchor)
        samples.append(value)
        fence_failure = fence(anchor)
        if fence_failure:
            break
    parent = first.get("before", {}).get("ppid")
    relation = "main" if first["pid"] == main_pid else "direct_child" if parent == main_pid else "other_cgroup_member"
    return {"pid": first["pid"], "parent_relation": relation,
            "classification": "diagnostic_scope_or_identity_fence_lost" if fence_failure else classify(samples),
            "followup_fence_failure": fence_failure, "samples": samples,
            "diagnosis_only_no_member_discarded": True}


def unit():
    raw = subprocess.check_output(["systemctl", "show", UNIT,
                                  "--property=ActiveState,SubState,MainPID,ControlGroup,ExecMainStartTimestampMonotonic"],
                                 text=True, timeout=3)
    value = dict(x.split("=", 1) for x in raw.splitlines())
    if value.get("ActiveState") != "active" or value.get("ControlGroup") != GROUP:
        raise RuntimeError("hotel_identity_not_ready")
    return value


def run(seconds):
    if not 1 <= seconds <= 60:
        raise ValueError("duration_out_of_bounds")
    initial = unit()
    main_pid = int(initial["MainPID"])
    group = Path("/sys/fs/cgroup") / GROUP.lstrip("/")
    begin = time.monotonic()
    deadline = begin + seconds
    summary = {"schema": 1, "read_only": True, "kernel": os.uname().release,
               "main_pid": main_pid, "seconds_requested": seconds, "poll_interval_seconds": 0.25,
               "observations": 0, "member_snapshots": 0, "failure_events": 0,
               "events_omitted": 0, "events": [], "classifications": {},
               "acceptance_unchanged": True, "unrelated_processes_scanned": False}
    counts = Counter()
    seen = set()
    def membership():
        members = set()
        for file in [group / "cgroup.procs", *group.glob("**/cgroup.procs")]:
            members.update(int(x) for x in file.read_text().split())
        return members
    def fence(anchor):
        if unit() != initial:
            return "hotel_unit_identity_changed"
        if anchor["pid"] not in membership():
            return "target_no_longer_in_hotel_cgroup"
        try:
            current = proc_stat(anchor["pid"])
        except OSError:
            return "target_numeric_metadata_unavailable"
        return None if identity(current) == identity(anchor) else "target_identity_or_parent_changed"
    while time.monotonic() < deadline:
        if unit() != initial:
            summary["stop_reason"] = "hotel_identity_changed"
            break
        pids = membership()
        if main_pid not in pids or not 1 <= len(pids) <= 256:
            summary["stop_reason"] = "cgroup_bounds_or_main_changed"
            break
        summary["observations"] += 1
        for pid in sorted(pids):
            if time.monotonic() >= deadline:
                break
            value = scoped_capture(pid, lambda pid: pid in membership(), fence)
            summary["member_snapshots"] += 1
            if "scope_failure" in value:
                counts["initial_scope_fence_lost"] += 1
                continue
            if "failure" not in value:
                continue
            summary["failure_events"] += 1
            event = diagnose(value, main_pid, deadline, fence)
            counts[event["classification"]] += 1
            key = (pid, value.get("before", {}).get("starttime"), event["classification"])
            if key in seen:
                summary["events_omitted"] += 1
                continue
            seen.add(key)
            tentative = {**summary, "events": [*summary["events"], event]}
            if len(json.dumps(tentative)) <= LIMIT - 1024:
                summary["events"].append(event)
            else:
                summary["events_omitted"] += 1
        if time.monotonic() + 0.25 < deadline:
            time.sleep(0.25)
        else:
            break
    summary["classifications"] = dict(counts)
    summary["elapsed_seconds"] = round(time.monotonic() - begin, 3)
    summary["hotel_unchanged_at_end"] = unit() == initial
    output = json.dumps(summary, sort_keys=True)
    if len(output.encode()) > LIMIT:
        summary["events_omitted"] += len(summary["events"])
        summary["events"] = []
    return summary


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--seconds", type=int, default=30)
    args = parser.parse_args()
    try:
        print(json.dumps(run(args.seconds), sort_keys=True))
    except Exception as error:
        print(json.dumps({"read_only": True, "status": "sampler_stopped",
                          "error_type": type(error).__name__}))
        raise SystemExit(1)
