"""Bounded metadata-only acceptance. No guest registrations or broker requests."""
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import time
import uuid

SOURCE = "5a08d74d944ece80940513a197c5ffa9f6f5c255"
PROC_ROOT = Path("/proc")
CGROUP_ROOT = Path("/sys/fs/cgroup")
# Baseline last child:23.29s; full certification:25.806s (+1s timestamp uncertainty).
# Rounded bound includes the unchanged three final 0.25s stability samples.
GUEST_READINESS_SECONDS = 30
EXECUTABLE_CLASSES = frozenset((
    "aiua", "philote", "heal-dispatcher", "model-controller-openrouter", "model-router",
    "agent-datasource", "agent-graph-runner", "data-memorygraphrag", "egress-http-runner",
    "graph-datasource", "graph-intelligence", "membrane-discord", "membrane-mcp-client",
    "membrane-mcp", "membrane-telegram", "router-listener", "table-datasource", "tool-runner",
    "node", "python3", "bash", "sh", "sleep", "ps",
))
NAMESPACE_PROPERTIES = (
    "User", "Group", "SupplementaryGroups", "DynamicUser", "UMask",
    "PrivateTmp", "PrivateDevices", "PrivateUsers", "PrivateMounts", "MountAPIVFS",
    "ProtectSystem", "ProtectHome", "ProtectProc", "ProcSubset", "ProtectKernelTunables",
    "ProtectKernelModules", "ProtectKernelLogs", "ProtectControlGroups", "NoNewPrivileges",
    "CapabilityBoundingSet", "AmbientCapabilities", "RestrictSUIDSGID", "RemoveIPC",
    "RestrictNamespaces", "RestrictAddressFamilies", "SystemCallFilter", "SystemCallArchitectures",
    "RootDirectory", "RootImage", "RootDirectoryStartOnly", "RootImageOptions", "RootHash",
    "BindPaths", "BindReadOnlyPaths", "InaccessiblePaths", "ReadOnlyPaths", "ReadWritePaths",
    "TemporaryFileSystem", "MountImages", "ExtensionImages", "ExtensionDirectories",
    "RuntimeDirectory", "StateDirectory", "CacheDirectory", "LogsDirectory", "ConfigurationDirectory",
    "WorkingDirectory",
)
FORBIDDEN_NONEMPTY = (
    "RootDirectory", "RootImage", "RootHash", "BindPaths", "BindReadOnlyPaths",
    "TemporaryFileSystem", "MountImages", "ExtensionImages", "ExtensionDirectories",
    "RuntimeDirectory", "StateDirectory", "CacheDirectory", "LogsDirectory", "ConfigurationDirectory",
)
MANDATORY = {
    "User": "proc-repro-gateway", "Group": "proc-repro-gateway", "SupplementaryGroups": "",
    "DynamicUser": "no", "NoNewPrivileges": "yes", "ProtectSystem": "strict", "ProtectHome": "yes",
    "CapabilityBoundingSet": "", "AmbientCapabilities": "",
}


def require(ok, reason):
    if not ok:
        raise RuntimeError(reason)


def canonical_digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def parse_proc_stat(raw):
    end = raw.rfind(")")
    require(end >= 0, "process_stat_format")
    fields = raw[end + 2:].split()
    require(len(fields) >= 20, "process_stat_format")
    result = {"ppid": int(fields[1]), "starttime": int(fields[19])}
    if fields[0] == "Z":
        result["terminal_state"] = "Z"
    return result


class SnapshotChildExited(ProcessLookupError):
    def __init__(self, pid, metadata):
        super().__init__("snapshot_child_exited")
        self.pid, self.metadata = pid, metadata


def numeric_authority(record):
    require(record.get("Uid") == [999] * 4 and record.get("Gid") == [987] * 4 and
            record.get("Groups") == [987] and record.get("CapEff") == 0, "hotel_process_authority")


def proc_authority(base):
    result = {}
    with open(base / "status") as stream:
        for line in stream:
            key, _, value = line.partition(":")
            if key in ("Uid", "Gid", "Groups"):
                result[key] = [int(n) for n in value.split()]
            elif key == "CapEff":
                result[key] = int(value.strip(), 16)
    return result


def terminal_snapshot(pid, metadata, record):
    base = PROC_ROOT / str(pid)
    numeric_authority(proc_authority(base))
    require(parse_proc_stat((base / "stat").read_text()) == metadata, "terminal_process_identity_changed")
    tasks = list((base / "task").iterdir())
    require(1 <= len(tasks) <= 256, "terminal_process_thread_bounds")
    require(all(parse_proc_stat((task / "stat").read_text()).get("terminal_state") == "Z"
                for task in tasks), "terminal_process_has_live_threads")
    raise SnapshotChildExited(pid, metadata)


def validate_child_exit(error, main_pid, members=None):
    require(error.pid != main_pid, "hotel_not_live_during_guest_readiness")
    require(error.metadata.get("terminal_state") == "Z" and error.metadata["ppid"] == main_pid,
            "exited_process_unverified_ancestry")
    if members is not None and error.pid in members:
        require(all(members[error.pid][key] == error.metadata[key] for key in ("ppid", "starttime")),
                "exited_process_pid_reused")
    path = PROC_ROOT / str(error.pid) / "stat"
    try:
        require(parse_proc_stat(path.read_text()) == error.metadata, "exited_process_pid_reused")
    except FileNotFoundError:
        require(not path.exists(), "exited_process_metadata_unavailable")


def process_identity(pid):
    base = PROC_ROOT / str(pid)
    start = parse_proc_stat((base / "stat").read_text())
    result = {"pid": pid, **start}
    result.update(proc_authority(base))
    if start.get("terminal_state") == "Z":
        terminal_snapshot(pid, start, result)
    try:
        executable = (base / "exe").stat()
        name = Path(os.readlink(base / "exe").removesuffix(" (deleted)")).name
    except FileNotFoundError:
        try:
            after = parse_proc_stat((base / "stat").read_text())
        except FileNotFoundError:
            require(not base.exists(), "live_process_executable_unavailable")
            raise ProcessLookupError("process_disappeared") from None
        require(all(after[key] == start[key] for key in ("ppid", "starttime")), "process_identity_race")
        if after.get("terminal_state") == "Z":
            terminal_snapshot(pid, after, result)
        raise RuntimeError("live_process_executable_unavailable") from None
    result["executable"] = [executable.st_dev, executable.st_ino]
    result["executable_class"] = name if name in EXECUTABLE_CLASSES else "unknown"
    after = parse_proc_stat((base / "stat").read_text())
    if after.get("terminal_state") == "Z":
        require(all(after[key] == start[key] for key in ("ppid", "starttime")), "process_identity_race")
        terminal_snapshot(pid, after, result)
    require(after == start, "process_identity_race")
    return result


def capture(main_pid, group, pids):
    require(group.startswith("/system.slice/proc-repro-hotel.service") and ".." not in group,
            "hotel_cgroup_identity")
    cgroup = CGROUP_ROOT / group.lstrip("/")
    info = cgroup.lstat()
    require(stat.S_ISDIR(info.st_mode) and info.st_uid == 0 and not info.st_mode & 0o022,
            "hotel_cgroup_not_root_controlled")
    require(1 <= len(pids) <= 256 and main_pid in pids, "bounded_hotel_process_set")
    members = {pid: process_identity(pid) for pid in sorted(pids)}
    for record in members.values():
        numeric_authority(record)
    return members


def guest_roots(members, main_pid):
    require(main_pid in members, "hotel_missing")
    roots = {pid for pid, record in members.items() if record["ppid"] == main_pid}
    require(roots or len(members) == 1, "unattributed_hotel_descendant")
    require(roots, "no_materialized_os_children")
    # The reviewed guest manager's TokioCommand::spawn materializes direct OS
    # children. Inspect their entire cgroup subtree as a conservative superset.
    for pid in members:
        if pid == main_pid:
            continue
        visited = set()
        cursor = pid
        while cursor not in roots:
            require(cursor in members and cursor not in visited, "unattributed_hotel_descendant")
            visited.add(cursor)
            cursor = members[cursor]["ppid"]
    return roots


def signature(members, main_pid):
    roots = guest_roots(members, main_pid)
    counts = Counter((tuple(members[pid]["executable"]), tuple(members[pid]["Uid"]),
                      tuple(members[pid]["Gid"])) for pid in roots)
    return sorted([[list(exe), list(uid), list(gid), count]
                   for (exe, uid, gid), count in counts.items()])


def signature_projection(expected, members, main_pid):
    observed = [] if set(members) == {main_pid} else signature(members, main_pid)
    def counts(rows):
        return Counter({(tuple(exe), tuple(uid), tuple(gid)): count for exe, uid, gid, count in rows})
    before, after = counts(expected), counts(observed)
    missing, extra = before - after, after - before
    def differences(values):
        return [{"identity_digest": canonical_digest(key), "executable": list(key[0]),
                 "uid": list(key[1]), "gid": list(key[2]), "count": count}
                for key, count in sorted(values.items())[:16]]
    roots = {pid for pid, value in members.items() if value["ppid"] == main_pid}
    classes = Counter(members[pid].get("executable_class", "unknown") for pid in roots)
    depths = Counter()
    for pid in members:
        if pid == main_pid:
            continue
        cursor, depth = pid, 0
        while cursor != main_pid:
            cursor = members[cursor]["ppid"]
            depth += 1
        depths[str(depth)] += 1
    classification = ("match" if before == after else "empty" if not after else
                      "partial" if not extra else "extra" if not missing else "identity_or_topology_mismatch")
    return {"classification": classification, "expected_root_count": sum(before.values()),
            "observed_root_count": sum(after.values()), "descendant_count": len(members) - 1,
            "expected_signature_digest": canonical_digest(expected),
            "observed_signature_digest": canonical_digest(observed),
            "missing_count": sum(missing.values()), "extra_count": sum(extra.values()),
            "missing": differences(missing), "extra": differences(extra),
            "observed_root_class_counts": dict(sorted(classes.items())), "descendant_depth_counts": dict(depths),
            "difference_groups_omitted": max(0, len(missing) - 16) + max(0, len(extra) - 16)}


def listener_inodes(path):
    # Read only the kernel socket metadata table, never socket contents.
    inodes = set()
    for line in (PROC_ROOT / "net/unix").read_text().splitlines():
        fields = line.split()
        if len(fields) == 8 and fields[7] == str(path):
            inodes.add(fields[6])
    require(len(inodes) == 1, "listener_inode_ambiguous")
    return inodes


def scan_fds(members, main_pid, inodes):
    targets = {"socket:[" + inode + "]" for inode in inodes}
    hotel_fds = 0
    for pid, identity in members.items():
        require(process_identity(pid) == identity, "fd_scan_pid_reused")
        base = PROC_ROOT / str(pid)
        descriptors = list((base / "fd").iterdir())
        require(len(descriptors) <= 4096, "descriptor_scan_limit")
        for fd in descriptors:
            try:
                target = os.readlink(fd)
            except FileNotFoundError:
                require(process_identity(pid) == identity, "fd_scan_pid_disappeared")
                continue
            if target not in targets:
                continue
            require(pid == main_pid, "materialized_child_inherited_scoped_fd")
            flags = next(line.split()[1] for line in (base / "fdinfo" / fd.name).read_text().splitlines()
                         if line.startswith("flags:"))
            require(int(flags, 8) & os.O_CLOEXEC, "listener_not_cloexec")
            hotel_fds += 1
        require(process_identity(pid) == identity, "fd_scan_process_changed")
    require(1 <= hotel_fds <= 2, "scoped_descriptor_count")
    return hotel_fds


def wait_guest_members(before_signature, hotel, cgroup_reader, unit_reader, binary_validator, deadline,
                       anchor=None, member_validator=None, observe=None):
    main_pid = int(hotel["MainPID"])
    if anchor is None:
        anchor = process_identity(main_pid)
    began, poll = time.monotonic(), 0
    while True:
        require(time.monotonic() < deadline, "materialized_guest_acceptance_timeout")
        require(unit_reader() == hotel and binary_validator() == hotel, "hotel_changed_during_guest_readiness")
        try:
            current_main = process_identity(main_pid)
        except (FileNotFoundError, ProcessLookupError):
            raise RuntimeError("hotel_not_live_during_guest_readiness") from None
        require(current_main == anchor, "hotel_identity_changed_during_guest_readiness")
        members = None
        try:
            members = capture(main_pid, hotel["ControlGroup"], cgroup_reader(hotel["ControlGroup"]))
            if member_validator is not None:
                member_validator(members)
            if observe is not None:
                poll += 1
                projected = signature_projection(before_signature, members, main_pid)
                projected.update(poll=poll, wait_elapsed_seconds=round(time.monotonic() - began, 3))
                stamp = hotel.get("ExecMainStartTimestampMonotonic", "")
                if stamp.isdigit() and int(stamp) > 0:
                    projected["hotel_age_seconds"] = round(time.monotonic() - int(stamp) / 1000000, 3)
                observe(projected)
            if signature(members, main_pid) == before_signature:
                require(cgroup_reader(hotel["ControlGroup"]) == set(members) and unit_reader() == hotel and
                        binary_validator() == hotel and process_identity(main_pid) == anchor,
                        "materialized_guest_set_unstable")
                return members
        except SnapshotChildExited as error:
            validate_child_exit(error, main_pid, members)
            if observe is not None:
                observe({"event": "validated_direct_child_exit", "pid": error.pid,
                         "starttime": error.metadata["starttime"], "terminal_state": "Z"})
        except (FileNotFoundError, ProcessLookupError):
            pass
        except RuntimeError as error:
            if str(error) != "no_materialized_os_children":
                raise
        time.sleep(0.25)


def verify_materialized_guests(before_signature, hotel, cgroup_reader, unit_reader, socket_path, binary_validator,
                              observe=None):
    main_pid = int(hotel["MainPID"])
    require(hotel.get("Delegate") == "no", "hotel_cgroup_delegated")
    anchor = process_identity(main_pid)
    deadline = time.monotonic() + GUEST_READINESS_SECONDS
    while True:
        require(time.monotonic() < deadline, "materialized_guest_acceptance_timeout")
        try:
            members = wait_guest_members(before_signature, hotel, cgroup_reader, unit_reader, binary_validator,
                deadline, anchor, lambda current: scan_fds(current, main_pid, listener_inodes(socket_path)), observe)
            roots = guest_roots(members, main_pid)
            inodes = listener_inodes(socket_path)
            for _ in range(3):
                require(time.monotonic() < deadline, "materialized_guest_acceptance_timeout")
                scan_fds(members, main_pid, inodes)
                require(cgroup_reader(hotel["ControlGroup"]) == set(members) and
                        unit_reader() == hotel and binary_validator() == hotel,
                        "materialized_guest_set_unstable")
                time.sleep(0.25)
            return {"materialized_root_count": len(roots), "descendant_count": len(members) - 1,
                    "metadata_digest": canonical_digest(members), "stable_samples": 3,
                    "existing_guest_set_restored": True, "observed_guest_fd_inheritance": False,
                    "reviewed_exec_boundary_cloexec_verified": True, "future_guest_registrations_tested": False}
        except SnapshotChildExited as error:
            validate_child_exit(error, main_pid, members)
            if observe is not None:
                observe({"event": "validated_direct_child_exit", "pid": error.pid,
                         "starttime": error.metadata["starttime"], "terminal_state": "Z"})
            time.sleep(0.25)
        except (FileNotFoundError, ProcessLookupError):
            time.sleep(0.25)


def namespace_values(raw, denied):
    values = dict(line.split("=", 1) for line in raw.splitlines() if "=" in line)
    require(set(NAMESPACE_PROPERTIES) - {"RootImageOptions"} <= set(values), "namespace_properties_missing")
    require(set(values) <= set(NAMESPACE_PROPERTIES), "namespace_property_unexpected")
    for key, expected in MANDATORY.items():
        require(values[key] == expected, "namespace_policy_" + key)
    for key in FORBIDDEN_NONEMPTY:
        require(not values.get(key, ""), "namespace_unsupported_or_alias_" + key)
    # systemctl prints this typed value as [unprintable] even with no RootImage.
    # It has no effect without an image; image namespaces are refused above.
    if values.get("RootImageOptions") == "[unprintable]":
        require(not values.get("RootImage"), "opaque_active_image_options")
        del values["RootImageOptions"]
    for key, value in values.items():
        require("\n" not in value and "\x00" not in value and len(value) <= 4096, "namespace_value_limit")
    paths = values["InaccessiblePaths"].split()
    require(set(denied) <= set(paths), "namespace_denied_roots_missing")
    require(all(re.fullmatch(r"/[a-zA-Z0-9_./-]+", path) for path in paths), "namespace_path_format")
    return values


INERT_TRANSIENT_OMISSIONS = ("RootHash", "BindPaths", "BindReadOnlyPaths")


def namespace_command(values, denied, public_files=None):
    namespace_values("\n".join(key + "=" + value for key, value in values.items()), denied)
    public_files = public_files or {}
    require(len(public_files) <= 3 and all(re.fullmatch(r"/opt/proc-repro/[a-zA-Z0-9_-]+/(?:server|gateway)\.mjs|/etc/proc-repro/gateway-discovery\.json", path)
        and re.fullmatch(r"[0-9a-f]{64}", digest) for path, digest in public_files.items()), "namespace_public_asset_scope")
    require(all(not values.get(key) for key in INERT_TRANSIENT_OMISSIONS), "namespace_omission_not_inert")
    probe = "import os,json,sys,hashlib; result=[]; paths=" + repr(denied) + "; assets=" + repr(public_files) + "\n"
    probe += "if sys.stdin.readline().strip()!='ALLOW_METADATA_PROBE': raise SystemExit(1)\n"
    probe += "for path in paths:\n try:\n  fd=os.open(path,os.O_RDONLY|os.O_DIRECTORY);os.close(fd);result.append(False)\n except PermissionError:result.append(True)\n"
    probe += "verified={}\nfor path,digest in assets.items():\n with open(path,'rb') as stream: data=stream.read(131073)\n verified[path]=len(data)<=131072 and hashlib.sha256(data).hexdigest()==digest\n"
    probe += "print(json.dumps({'uid':os.getuid(),'gid':os.getgid(),'groups':os.getgroups(),'denied':result,'public_files':verified,'cwd_expected':os.getcwd()==" + repr(values["WorkingDirectory"]) + "}),flush=True)"
    argv = ["systemd-run", "--quiet", "--wait", "--pipe", "--collect",
            "--unit=proc-repro-namespace-" + SOURCE[:12] + "-" + uuid.uuid4().hex[:16],
            "--property=Type=oneshot", "--property=TimeoutStartSec=15",
            "--property=RuntimeMaxSec=15", "--property=LoadCredential=", "--property=SetCredential=",
            "--property=PassEnvironment=", "--property=Environment="]
    # Clone every returned property in the allowlisted namespace/security set.
    # Image/bind aliases and directory-creating properties are rejected above.
    argv.extend("--property=" + key + "=" + value for key, value in sorted(values.items())
                if key not in INERT_TRANSIENT_OMISSIONS)
    return argv + ["/usr/bin/python3", "-I", "-S", "-B", "-c", probe]


def replica_run(argv, expected, denied):
    name = next(value.split("=", 1)[1] for value in argv if value.startswith("--unit=")) + ".service"
    process = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    deadline = time.monotonic() + 20
    owned = False
    try:
        while True:
            require(time.monotonic() < deadline and process.poll() is None, "namespace_replica_not_live")
            readiness = subprocess.run(["systemctl", "show", name, "--property=LoadState,MainPID"],
                capture_output=True, text=True, check=True, timeout=5)
            ready = dict(line.split("=", 1) for line in readiness.stdout.splitlines())
            if ready.get("LoadState") == "loaded" and int(ready.get("MainPID", "0")) > 0:
                owned = True
                break
            time.sleep(0.05)
        raw = subprocess.check_output(["systemctl", "show", name, "--property=" + ",".join(NAMESPACE_PROPERTIES)],
                                      text=True, timeout=5)
        actual = namespace_values(raw, denied)
        require(actual == expected, "namespace_replica_properties_differ")
        output, unused_error = process.communicate("ALLOW_METADATA_PROBE\n", timeout=max(1, deadline - time.monotonic()))
        require(process.returncode == 0, "namespace_replica_process_failed")
        return json.loads(output)
    finally:
        try:
            if process.poll() is None:
                process.kill()
            process.communicate(timeout=5)
        finally:
            if owned:
                subprocess.run(["systemctl", "stop", name], stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL, timeout=5)


def verify_namespace(command, gateway, denied, public_files=None, runner=None):
    show = ["systemctl", "show", gateway, "--property=" + ",".join(NAMESPACE_PROPERTIES)]
    raw = command(show)
    values = namespace_values(raw, denied)
    result = (runner or replica_run)(namespace_command(values, denied, public_files), values, denied)
    require(result["uid"] == result["gid"] == 1003 and set(result["groups"]) <= {1003} and
            result["denied"] == [True] * len(denied), "effective_namespace_denial_failed")
    require(result.get("cwd_expected") is True and result.get("public_files") ==
            {path: True for path in (public_files or {})}, "namespace_public_layout_unreadable")
    require(command(show) == raw, "gateway_namespace_properties_changed")
    return {"effective_namespace_digest": canonical_digest(values), "properties_read": len(NAMESPACE_PROPERTIES),
            "replicated_property_count": len(values) - len(INERT_TRANSIENT_OMISSIONS),
            "inert_empty_properties_omitted": list(INERT_TRANSIENT_OMISSIONS),
            "public_file_count_verified": len(public_files or {}), "actual_replica_properties_verified": True,
            "denied_root_count": len(denied), "actual_numeric_identity_verified": True,
            "effective_mount_namespace_replica_verified": True, "gateway_started": False,
            "credential_delivery": False}
