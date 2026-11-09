#!/usr/bin/env python3
"""Run bounded, ordinary fallback CI against an explicitly identified source snapshot.

No GitHub calls, Docker, installation, deployment or synthetic receipt-capture job
is provided. This is a trusted-source runner, not a sandbox.
"""
import argparse
import hashlib
import json
import os
import pathlib
import platform
import re
import shutil
import signal
import subprocess
import sys
import time
from contextlib import contextmanager

HERE = pathlib.Path(__file__).resolve().parent


def digest(path):
    value = hashlib.sha256()
    with open(path, "rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def source_identity(root, expected_sha, project):
    manifest = json.loads((root / "SOURCE_IDENTITY.json").read_text())
    if manifest.get("project") != project or manifest.get("source_sha") != expected_sha or not re.fullmatch(r"[0-9a-f]{40}", expected_sha):
        raise ValueError("source project or exact SHA mismatch")
    declared = manifest.get("files")
    if not isinstance(declared, dict) or not declared:
        raise ValueError("source file identities missing")
    for relative, expected in declared.items():
        path = pathlib.PurePosixPath(relative)
        if path.is_absolute() or ".." in path.parts:
            raise ValueError("invalid source identity path")
        actual = root / relative
        if actual.is_symlink() or not actual.is_file() or digest(actual) != expected:
            raise ValueError("source content drift: " + relative)
    actual_names = {path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file() or path.is_symlink()}
    if actual_names != set(declared) | {"SOURCE_IDENTITY.json"}:
        raise ValueError("unexpected source files")
    return {**{key: manifest[key] for key in ("project", "source_sha", "source_tree", "reference")},
            "manifest_sha256": digest(root / "SOURCE_IDENTITY.json")}


def command(stage, root):
    result = []
    for argument in stage["argv"]:
        if argument == "{python}":
            result.append(sys.executable)
        elif argument.startswith("{glob:") and argument.endswith("}"):
            matches = sorted(root.glob(argument[6:-1]))
            if not matches:
                raise ValueError("test glob has no matches")
            result.extend(path.relative_to(root).as_posix() for path in matches)
        else:
            result.append(argument)
    return result


def prerequisites(stage, root):
    problems = []
    if stage.get("system") and stage["system"] != platform.system():
        problems.append("requires " + stage["system"])
    for tool in stage.get("tools", []):
        if shutil.which(tool) is None:
            problems.append("missing tool: " + tool)
    for relative in stage.get("inputs", []):
        if not (root / relative).exists():
            problems.append("missing input: " + relative)
    return problems


def test_summary(kind, output):
    """Preserve incomplete coverage and contradictory summaries, even on exit 0."""
    result = dict(test_count=None, passed=None, failed=0, errors=0, skipped=0,
                  todo=0, cancelled=0, expected_failures=0, unexpected_successes=0)
    issues = []
    if kind == "tap":
        names = {"tests": "test_count", "pass": "passed", "fail": "failed",
                 "skipped": "skipped", "todo": "todo", "cancelled": "cancelled"}
        for label, key in names.items():
            values = re.findall(r"^# " + label + r" (\d+)\s*$", output, re.M)
            if len(values) != 1:
                issues.append("missing or ambiguous TAP counter: " + label)
            result[key] = int(values[-1]) if values else 0
        if result["test_count"] != sum(result[key] for key in
                                       ("passed", "failed", "skipped", "todo", "cancelled")):
            issues.append("TAP counters do not sum to total")
    elif kind == "unittest":
        totals = re.findall(r"^Ran (\d+) tests?(?: in [^\n]+)?$", output, re.M)
        summaries = re.findall(r"^(OK|FAILED)(?: \(([^\n)]*)\))?\s*$", output, re.M)
        result["test_count"] = sum(map(int, totals))
        if len(totals) != 1 or len(summaries) != 1:
            issues.append("missing or ambiguous unittest summary")
        names = {"failures": "failed", "errors": "errors", "skipped": "skipped",
                 "expected failures": "expected_failures", "unexpected successes": "unexpected_successes"}
        for status, counters in summaries:
            seen = set()
            for counter in filter(None, counters.split(", ")):
                match = re.fullmatch(r"([a-z ]+)=(\d+)", counter)
                if not match or match[1] not in names or match[1] in seen:
                    issues.append("unrecognized unittest counter: " + counter)
                    continue
                seen.add(match[1])
                result[names[match[1]]] += int(match[2])
            if status == "FAILED" and not any(result[key] for key in
                                               ("failed", "errors", "unexpected_successes")):
                issues.append("FAILED summary without failure count")
        result["passed"] = result["test_count"] - sum(result[key] for key in names.values())
        if result["passed"] < 0:
            issues.append("unittest counters exceed total")
            result["passed"] = None
    elif kind:
        issues.append("unsupported test summary: " + str(kind))
    result["summary_issues"] = issues
    return result


def counts(kind, output):
    """Compatibility accessor; stage decisions use the full summary."""
    result = test_summary(kind, output)
    return result["test_count"], result["skipped"]


def outcome(exit_code, kind, summary):
    if exit_code != 0 or any(summary[key] for key in ("failed", "errors", "unexpected_successes")):
        return "failed", "nonzero exit or reported test failure"
    if kind and not summary["test_count"]:
        return "failed", "zero tests discovered"
    if summary["summary_issues"]:
        return "incomplete", "test summary is incomplete or inconsistent"
    if any(summary[key] for key in ("skipped", "todo", "cancelled", "expected_failures")):
        return "incomplete", "tests skipped, pending, cancelled or expected to fail"
    return "passed", None


def executable_identity(stage, argv, env):
    """Read binary identities only; do not run version probes or auto-install tools."""
    names = sorted(set(stage.get("tools", [])) | {argv[0]})
    identities = {}
    for name in names:
        located = shutil.which(name, path=env["PATH"])
        if located is None:
            raise ValueError("executable no longer available: " + name)
        resolved = pathlib.Path(located).resolve()
        identities[name] = {"path": located, "resolved_path": str(resolved), "sha256": digest(resolved)}
    return identities


def test_environment(evidence):
    home, temp = evidence / "home", evidence / "tmp"
    home.mkdir(mode=0o700)
    temp.mkdir(mode=0o700)
    return {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(home), "TMPDIR": str(temp), "TMP": str(temp), "TEMP": str(temp),
        "XDG_CONFIG_HOME": str(home / ".config"), "CI": "true",
        "LANG": "C.UTF-8", "PYTHONDONTWRITEBYTECODE": "1",
        "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull, "GIT_TEMPLATE_DIR": "",
        "GIT_CONFIG_COUNT": "4", "GIT_CONFIG_KEY_0": "core.hooksPath", "GIT_CONFIG_VALUE_0": os.devnull,
        "GIT_CONFIG_KEY_1": "commit.gpgSign", "GIT_CONFIG_VALUE_1": "false",
        "GIT_CONFIG_KEY_2": "protocol.allow", "GIT_CONFIG_VALUE_2": "never",
        "GIT_CONFIG_KEY_3": "protocol.file.allow", "GIT_CONFIG_VALUE_3": "always",
        "GIT_TERMINAL_PROMPT": "0", "RUSTUP_AUTO_INSTALL": "0", "CARGO_NET_OFFLINE": "true",
        "CARGO_TARGET_DIR": str(evidence / "cargo-target"), "npm_config_offline": "true",
        "NEXT_TELEMETRY_DISABLED": "1",
    }


def atomic(path, data):
    pending = path.with_suffix(".next")
    with open(pending, "w") as stream:
        json.dump(data, stream, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(pending, path)


@contextmanager
def cancellation():
    """Defer signals through spawn so a newly created child is never orphaned."""
    requested = []
    previous = {}
    def record(signum, _frame):
        requested.append(signum)
    try:
        for signum in (signal.SIGINT, signal.SIGTERM):
            previous[signum] = signal.signal(signum, record)
        yield requested
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)


def stop_group(proc, grace=0.5):
    """Bound cleanup by process group, not just the possibly exited leader."""
    try:
        os.killpg(proc.pid, signal.SIGTERM)
    except ProcessLookupError:
        proc.wait()
        return
    deadline = time.monotonic() + grace
    while time.monotonic() < deadline:
        proc.poll()
        try:
            os.killpg(proc.pid, 0)
        except ProcessLookupError:
            break
        time.sleep(0.02)
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    proc.wait(timeout=5)


def run_stage(stage, root, evidence, env, cancelled):
    row = {"id": stage["id"], "status": "running", "started": time.time()}
    problems = prerequisites(stage, root)
    if problems:
        row.update(status="unavailable", blockers=problems)
        return row
    argv = command(stage, root)
    row["argv"] = argv
    row["executables"] = executable_identity(stage, argv, env)
    log = evidence / (stage["id"] + ".log")
    row["log"] = str(log)
    proc = None
    with open(log, "wb") as output:
        try:
            proc = subprocess.Popen(argv, cwd=root, env=env, stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
            deadline = time.monotonic() + stage.get("timeout_seconds", 300)
            while True:
                if cancelled:
                    row.update(status="interrupted", signal=cancelled[0])
                    break
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    row.update(status="interrupted", reason="timeout")
                    break
                try:
                    row["exit_code"] = proc.wait(timeout=min(0.1, remaining))
                    break
                except subprocess.TimeoutExpired:
                    pass
        except OSError as error:
            row.update(status="unavailable", reason=type(error).__name__, finished=time.time())
            return row
        finally:
            if proc is not None:
                try:
                    stop_group(proc)
                except (OSError, subprocess.SubprocessError) as error:
                    row.update(status="cleanup-unresolved", reason=str(error), pid=proc.pid, pgid=proc.pid)
    output = log.read_text(errors="replace")
    summary = test_summary(stage.get("test_kind"), output)
    row.update(**summary, finished=time.time(), log_sha256=digest(log))
    if cancelled and row["status"] != "cleanup-unresolved":
        row.update(status="interrupted", signal=cancelled[0])
    if row["status"] in {"interrupted", "cleanup-unresolved"}:
        return row
    row["status"], reason = outcome(row["exit_code"], stage.get("test_kind"), summary)
    if reason:
        row["reason"] = reason
    return row


def run(profile, root, expected_sha, evidence):
    with cancellation() as cancelled:
        return run_profile(profile, root, expected_sha, evidence, cancelled)


def run_profile(profile, root, expected_sha, evidence, cancelled):
    root, evidence = pathlib.Path(root).resolve(), pathlib.Path(evidence).resolve()
    if evidence == root or root in evidence.parents:
        raise ValueError("evidence must be outside the source snapshot")
    identity = source_identity(root, expected_sha, profile["project"])
    evidence.mkdir(mode=0o700, parents=False, exist_ok=False)
    env = test_environment(evidence)
    report = {"profile": profile["id"], "source": identity,
              "platform": {"system": platform.system(), "release": platform.release(), "machine": platform.machine()},
              "runner_sha256": digest(pathlib.Path(__file__).resolve()),
              "python": {"version": sys.version, "executable": sys.executable, "sha256": digest(pathlib.Path(sys.executable).resolve())},
              "profile_sha256": hashlib.sha256(json.dumps(profile, sort_keys=True).encode()).hexdigest(),
              "status": "running", "full_ci_passed": False, "coverage": profile["coverage"],
              "not_covered": profile["not_covered"], "stages": []}
    statuses = {}
    for stage in profile["stages"]:
        atomic(evidence / "report.json", report)
        if cancelled:
            row = {"id": stage["id"], "status": "interrupted", "signal": cancelled[0]}
        elif any(statuses.get(name) != "passed" for name in stage.get("needs", [])):
            row = {"id": stage["id"], "status": "not-run", "reason": "prerequisite gate did not pass"}
        else:
            try:
                row = run_stage(stage, root, evidence, env, cancelled)
            except (ValueError, KeyError, OSError, subprocess.SubprocessError) as error:
                row = {"id": stage["id"], "status": "failed", "reason": str(error)}
        report["stages"].append(row)
        statuses[stage["id"]] = row["status"]
        try:
            current_identity = source_identity(root, expected_sha, profile["project"])
            if current_identity != identity:
                raise ValueError("source manifest changed during execution")
        except (ValueError, KeyError, OSError) as error:
            row.update(status="failed", source_changed=str(error))
            statuses[stage["id"]] = "failed"
            report["status"] = "failed"
            break
        if cancelled and row["status"] != "cleanup-unresolved":
            row.update(status="interrupted", signal=cancelled[0])
            statuses[stage["id"]] = "interrupted"
        if row["status"] in {"interrupted", "cleanup-unresolved"}:
            break
    if report["status"] != "failed":
        values = set(statuses.values())
        report["status"] = "passed" if values == {"passed"} else ("failed" if "failed" in values else "incomplete")
    if cancelled:
        report["signals"] = list(cancelled)
        if report["status"] == "passed":
            report["status"] = "incomplete"
    atomic(evidence / "report.json", report)
    return report


def main():
    profiles = json.loads((HERE / "fallback-profiles.json").read_text())
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", required=True, choices=sorted(profiles))
    parser.add_argument("--source-root", required=True, type=pathlib.Path)
    parser.add_argument("--expect-sha", required=True)
    parser.add_argument("--evidence", type=pathlib.Path)
    parser.add_argument("--doctor", action="store_true")
    args = parser.parse_args()
    try:
        profile = profiles[args.profile]
        if args.doctor:
            identity = source_identity(args.source_root.resolve(), args.expect_sha, profile["project"])
            rows = [{"id": stage["id"], "blockers": prerequisites(stage, args.source_root)} for stage in profile["stages"]]
            print(json.dumps({"source": identity, "stages": rows, "read_only": True}, indent=2))
            return 78 if any(row["blockers"] for row in rows) else 0
        if args.evidence is None:
            parser.error("--evidence is required to run; use --doctor for read-only inspection")
        report = run(profile, args.source_root, args.expect_sha, args.evidence)
        print(json.dumps({"status": report["status"], "source": report["source"],
                          "report": str(args.evidence.resolve() / "report.json"),
                          "stages": [{"id": row["id"], "status": row["status"], "tests": row.get("test_count"),
                                      "skipped": row.get("skipped"), "blockers": row.get("blockers")} for row in report["stages"]],
                          "full_ci_passed": False}, indent=2))
        return 0 if report["status"] == "passed" else (1 if report["status"] == "failed" else 78)
    except (ValueError, KeyError, OSError) as error:
        print(json.dumps({"status": "unavailable", "reason": str(error)}))
        return 78


if __name__ == "__main__":
    raise SystemExit(main())
