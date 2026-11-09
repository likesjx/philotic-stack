"""One fresh, disposable, network-none Memgraph authentication qualification."""
import argparse
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import tempfile
import time
from entry_inventory import inventory, InventoryFailure

IMAGE = "memgraph/memgraph-mage@sha256:e8e03640071c57de6a13f2f4c5f9ed1789797dc5e8e771bbe62203b20e1b8c27"
LABEL = "com.philotic.graph-auth-qualification"
ID = re.compile(r"[a-f0-9]{64}\Z")

class Failure(Exception):
    pass

def command(args, timeout=30):
    try:
        r = subprocess.run(["docker", "--host=unix:///var/run/docker.sock", *args],
                           text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                           timeout=timeout, check=False,
                           env={"PATH": "/usr/bin:/bin", "HOME": "/nonexistent"})
    except (subprocess.TimeoutExpired, OSError):
        raise Failure("docker_unavailable_or_uncertain") from None
    if len(r.stdout) + len(r.stderr) > 65536:
        raise Failure("docker_output_bound")
    return r

def call(args, timeout=30):
    r = command(args, timeout)
    if r.returncode:
        raise Failure("docker_command_failed")
    return r.stdout.strip()

def owned(ident, nonce, image):
    rows = json.loads(call(["inspect", ident]))
    if len(rows) != 1:
        raise Failure("inspect_shape")
    r = rows[0]
    if (r["Id"] != ident or r["Image"] != image
            or r["Config"].get("Labels", {}).get(LABEL) != nonce):
        raise Failure("container_identity")
    return r

def isolated(r, expected_sources=None):
    h = r["HostConfig"]
    if (h["NetworkMode"] != "none" or h["Privileged"]
            or h["RestartPolicy"]["Name"] != "no"
            or h.get("PidMode") not in ("", None)
            or h.get("IpcMode") not in ("private", "")
            or h.get("PortBindings") not in (None, {})
            or h.get("PublishAllPorts") or "ALL" not in h.get("CapDrop", [])
            or not any(s in ("no-new-privileges", "no-new-privileges:true")
                       for s in h.get("SecurityOpt", []))):
        raise Failure("container_isolation")
    expected = {"/var/lib/memgraph", "/fixture", "/qualification", "/deps"}
    if {m["Destination"] for m in r.get("Mounts", []) if m["Type"] == "bind"} != expected:
        raise Failure("mount_contract")
    if any(m["Type"] not in ("bind", "tmpfs") for m in r.get("Mounts", [])):
        raise Failure("unexpected_volume")
    if expected_sources is not None:
        binds=[m for m in r.get("Mounts", []) if m["Type"]=="bind"]
        if len(binds)!=len(expected_sources):raise Failure("mount_contract")
        for m in binds:
            expected=expected_sources.get(m["Destination"])
            if expected is None or m.get("Source")!=os.path.realpath(expected[0]) or m.get("RW") is not expected[1]:
                raise Failure("mount_contract")

def remove(ident, nonce, image):
    r = owned(ident, nonce, image)
    if r["State"]["Running"]:
        call(["stop", "--time", "20", ident], 30)
    call(["wait", ident], 10)
    if owned(ident, nonce, image)["State"]["Running"]:
        raise Failure("stop_unconfirmed")
    call(["rm", ident])
    if ident in call(["ps", "-aq", "--no-trunc"]).split():
        raise Failure("remove_unconfirmed")

def stop_for_restart(ident):
    call(["stop", "--time", "20", ident], 30)
    if call(["wait", ident], 10) != "0":
        raise Failure("restart_stop_not_graceful")


def remove_scratch(root):
    # No success flag or receipt may precede this fallible operation.
    shutil.rmtree(root)


def probe(ident, phase):
    return command(["exec", "--env", "PYTHONPATH=/deps", ident, "python3",
                    "/qualification/probe_graph.py", "/fixture/credentials.json", phase], 40)

def run_layout_probe(ident, phase):
    if phase not in ('seed','observe'): raise Failure('invalid_probe_phase')
    result=command(["exec", "--env", "PYTHONPATH=/deps", ident, "python3", "/qualification/layout_probe.py", "/fixture/credentials.json", phase], 40)
    if result.returncode or result.stdout.strip() != 'graph_layout:'+phase+':passed': raise Failure(phase+'_failed_or_unknown')
    print('graph_layout:'+('seed:acknowledged' if phase=='seed' else 'observe:passed'),flush=True)

def ready(ident, nonce, image, phase):
    # Retry only read-only connection assertions. Any semantic failure stops now.
    for _ in range(45):
        r = owned(ident, nonce, image)
        if not r["State"]["Running"]:
            raise Failure("graph_exited")
        result = probe(ident, phase)
        if result.returncode == 0:
            if result.stdout.strip() != "graph_auth:" + phase + ":passed":
                raise Failure("probe_receipt")
            print("graph_auth:" + phase + ":passed", flush=True)
            return
        safe = result.stdout.strip()
        if safe != "graph_auth:failed:transport_unavailable":
            allowed = {"positive_response", "unexpected_initial_users",
                       *("denial_" + case + "_" + reason
                         for case in ("wrong_password", "empty_password", "empty_basic", "no_auth_scheme", "unknown_user")
                         for reason in ("not_authentication", "accepted")),
                       "positive_authentication_denied", "dependency_unavailable",
                       "driver_or_query_failure"}
            category = safe.removeprefix("graph_auth:failed:")
            raise Failure("probe_" + (category if category in allowed else "invalid"))
        time.sleep(1)
    raise Failure("graph_readiness_timeout")

def main(deps):
    if (os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("CI") != "true"
            or os.environ.get("RUNNER_OS") != "Linux"
            or os.environ.get("GITHUB_REPOSITORY") != "likesjx/philotic-stack"
            or os.environ.get("GITHUB_REF") != "refs/heads/codex/synthetic-memgraph-layout"):
        raise Failure("approved_disposable_job_required")
    source = Path(__file__).resolve().parent
    deps = Path(deps).resolve(strict=True)
    if not (deps / "neo4j/__init__.py").is_file():
        raise Failure("dependencies_absent")
    uid, gid = os.getuid(), os.getgid()
    if uid == 0:
        raise Failure("nonroot_fixture_uid_required")
    nonce = secrets.token_hex(16)
    image_info=json.loads(call(["image", "inspect", IMAGE]))
    if type(image_info) is not list or len(image_info)!=1 or IMAGE not in (image_info[0].get("RepoDigests") or []):
        raise Failure("image_digest")
    image = image_info[0]["Id"]
    if not re.fullmatch(r"sha256:[a-f0-9]{64}", image):
        raise Failure("image_identity")
    if call(["ps", "-aq"]) or call(["volume", "ls", "-q"]):
        raise Failure("exclusive_empty_runner_required")
    root = Path(tempfile.mkdtemp(prefix="graph-auth-", dir=os.environ["RUNNER_TEMP"]))
    ident = None
    creation_attempted = False
    cleanup_ok = False
    stage = "prepare"
    try:
        for part in ("state", "fixture"):
            (root / part).mkdir(mode=0o700)
        original, replacement = secrets.token_hex(32), secrets.token_hex(32)
        passfile = root / "fixture/passfile"
        passfile.write_text("beta_fixture:" + original + "\n")
        passfile.chmod(0o600)
        config = root / "fixture/credentials.json"
        config.write_text(json.dumps({"password": original, "replacement": replacement}))
        config.chmod(0o600)
        stage = "create"
        creation_attempted = True
        ident = call(["create", "--pull", "never", "--name", "graph-auth-" + nonce,
                      "--label", LABEL + "=" + nonce, "--network", "none",
                      "--user", str(uid) + ":" + str(gid), "--cap-drop", "ALL",
                      "--security-opt", "no-new-privileges", "--read-only",
                      "--restart", "no", "--memory", "2g", "--cpus", "2", "--pids-limit", "256",
                      "--log-driver", "json-file", "--log-opt", "max-size=1m",
                      "--log-opt", "max-file=1", "--entrypoint", "python3",
                      "--tmpfs", "/tmp:rw,noexec,nosuid,nodev,size=64m",
                      "--tmpfs", "/var/log/memgraph:rw,nosuid,nodev,size=64m,uid=" + str(uid) + ",gid=" + str(gid),
                      "--mount", "type=bind,src=" + str(root / "state") + ",dst=/var/lib/memgraph",
                      "--mount", "type=bind,src=" + str(root / "fixture") + ",dst=/fixture,readonly",
                      "--mount", "type=bind,src=" + str(source) + ",dst=/qualification,readonly",
                      "--mount", "type=bind,src=" + str(deps) + ",dst=/deps,readonly",
                      IMAGE, "/qualification/launch_graph.py"])
        if not ID.fullmatch(ident):
            raise Failure("create_uncertain")
        mount_contract={"/var/lib/memgraph":(str(root/"state"),True),"/fixture":(str(root/"fixture"),False),
                        "/qualification":(str(source),False),"/deps":(str(deps),False)}
        isolated(owned(ident, nonce, image),mount_contract)
        stage = "fresh"
        call(["start", ident])
        ready(ident, nonce, image, stage)
        stage = "seed"
        run_layout_probe(ident,"seed")
        stage = "restart"
        stop_for_restart(ident)
        call(["start", ident])
        ready(ident, nonce, image, stage)
        stage = "changed_passfile"
        stop_for_restart(ident)
        # Initial-user input must not reset the existing account on restart.
        passfile.write_text("beta_fixture:" + replacement + "\n")
        call(["start", ident])
        ready(ident, nonce, image, stage)
        stage = "observe"
        run_layout_probe(ident,"observe")
        stage = "stop_for_inventory"
        stop_for_restart(ident)
        stopped=owned(ident, nonce, image)
        if (stopped["State"].get("Running") is not False or stopped["State"].get("ExitCode") != 0
            or stopped["State"].get("Dead") is not False or stopped["State"].get("Pid") != 0):
            raise Failure("stopped_state_unverified")
        isolated(stopped,mount_contract)
        stage = "inventory"
        counts=inventory(root / "state")
        print("graph_layout:inventory:" + json.dumps(counts,sort_keys=True,separators=(",",":")), flush=True)
    except Exception as e:
        safe = str(e) if isinstance(e, Failure) else "unexpected_local_failure"
        print("graph_auth:stage=" + stage + ":failure=" + safe, flush=True)
        raise Failure("qualification_failed") from None
    finally:
        try:
            # Reconcile a lost create response by the exact unpredictable label.
            if ident is None and creation_attempted:
                ids = call(["ps", "-aq", "--no-trunc", "--filter", "label=" + LABEL + "=" + nonce]).split()
                if len(ids) == 1 and ID.fullmatch(ids[0]):
                    ident = ids[0]
                else:
                    raise Failure("create_reconciliation")
            if ident is not None:
                remove(ident, nonce, image)
            if call(["ps", "-aq", "--no-trunc"]) or call(["volume", "ls", "-q"]):
                raise Failure("cleanup_remaining")
            remove_scratch(root)
            cleanup_ok = True
            print("graph_auth:cleanup:passed", flush=True)
        except Exception:
            print("graph_auth:cleanup:inconclusive", flush=True)
        if not cleanup_ok:
            raise Failure("cleanup_inconclusive")
    print("graph_auth:qualification:passed; production_unchanged", flush=True)

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--deps", required=True)
    try:
        main(parser.parse_args().deps)
    except Failure as error:
        print("graph_auth:failed:" + str(error), flush=True)
        raise SystemExit(1)
    except Exception:
        print("graph_auth:failed:unexpected_local_failure", flush=True)
        raise SystemExit(1)
