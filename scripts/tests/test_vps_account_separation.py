#!/usr/bin/env python3
"""Local tests for the VPS account-separation role.

Runs on this machine only (ansible_connection=local). Never loads
ansible/inventory/hosts.ini. Creates and then deletes fixture users.

    python3 scripts/tests/test_vps_account_separation.py
"""

from __future__ import annotations

import hashlib
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ANSIBLE = ROOT / "ansible"
PLAYBOOK = ANSIBLE / "vps_account_separation.yml"
CFG = ANSIBLE / "tests" / "ansible.cfg"
LOCAL_INV = ANSIBLE / "tests" / "localhost.ini"
ENV = os.environ.copy()
ENV.update(
    {
        "ANSIBLE_CONFIG": str(CFG),
        "ANSIBLE_ROLES_PATH": str(ANSIBLE / "roles"),
        "ANSIBLE_LOCALHOST_WARNING": "False",
        "ANSIBLE_DEPRECATION_WARNINGS": "False",
        "ANSIBLE_HOST_KEY_CHECKING": "False",
    }
)

CREATED_USERS: list[str] = []
GROUPS_WE_CREATED: list[str] = []
HARDENING = Path("/etc/ssh/sshd_config.d/99-philotic-sshd-hardening.conf")
SUDOERS = Path("/etc/sudoers.d/philotic-vps-accounts")
PIPELINE_SUDOERS = Path("/etc/sudoers.d/philotic-deploy-pipeline")
DEPLOY_MARKER = Path("/etc/sudoers.d/philotic-test-deploy-marker")
REQUEST_BIN = Path("/usr/local/sbin/philotic-request-restart")
ENTRYPOINT = Path("/usr/local/sbin/philotic-ansible-entrypoint")
APPROVAL_HOME = Path("/var/lib/philotic-approval")
UNIT_DIR = Path("/etc/systemd/system")
UNIT_FILES = [
    UNIT_DIR / "philotic-approval-broker.service",
    UNIT_DIR / "philotic-approval-broker.socket",
]


def run(cmd: list[str], check: bool = True, input_text: str | None = None) -> subprocess.CompletedProcess[str]:
    print("+", " ".join(cmd), flush=True)
    result = subprocess.run(
        cmd,
        input=input_text,
        text=True,
        capture_output=True,
        env=ENV,
        check=False,
    )
    if result.stdout:
        sys.stdout.write(result.stdout)
    if result.stderr:
        sys.stderr.write(result.stderr)
    if "BEGIN OPENSSH PRIVATE KEY" in result.stdout or "BEGIN OPENSSH PRIVATE KEY" in result.stderr:
        raise SystemExit("private key material appeared in command output")
    if check and result.returncode != 0:
        raise SystemExit(f"command failed ({result.returncode}): {' '.join(cmd)}")
    return result


def sudo(cmd: list[str], check: bool = True, input_text: str | None = None) -> subprocess.CompletedProcess[str]:
    return run(["sudo", "-n", *cmd], check=check, input_text=input_text)


def playbook(extra: list[str]) -> subprocess.CompletedProcess[str]:
    return run(
        [
            "ansible-playbook",
            str(PLAYBOOK),
            "-i",
            str(LOCAL_INV),
            *extra,
        ],
        check=False,
    )


def recap_counts(output: str) -> tuple[int, int]:
    recap = output.split("PLAY RECAP")[-1]
    changed = re.search(r"changed=(\d+)", recap)
    failed = re.search(r"failed=(\d+)", recap)
    if not changed or not failed:
        raise SystemExit(f"no play recap in output:\n{output}")
    return int(changed.group(1)), int(failed.group(1))


def sha256(path: Path) -> str:
    data = sudo(["cat", str(path)]).stdout.encode()
    return hashlib.sha256(data).hexdigest()


def snapshot_sshd() -> dict[str, str]:
    paths: list[Path] = []
    config = Path("/etc/ssh/sshd_config")
    if sudo(["test", "-f", str(config)], check=False).returncode == 0:
        paths.append(config)
    dropin = Path("/etc/ssh/sshd_config.d")
    if sudo(["test", "-d", str(dropin)], check=False).returncode == 0:
        listing = sudo(["find", str(dropin), "-maxdepth", "1", "-type", "f"]).stdout
        paths.extend(Path(line) for line in listing.splitlines() if line)
    return {str(path): sha256(path) for path in paths}


def user_exists(name: str) -> bool:
    return sudo(["getent", "passwd", name], check=False).returncode == 0


def write_root_file(path: Path, content: str, mode: str) -> None:
    sudo(["tee", str(path)], input_text=content)
    sudo(["chmod", mode, str(path)])


def fixture_deploy() -> dict[str, str]:
    if user_exists("deploy"):
        raise SystemExit("deploy already exists on this machine; refusing to touch it")
    sudo(["groupadd", "-f", "docker"])
    sudo(["useradd", "-m", "-s", "/bin/bash", "deploy"])
    CREATED_USERS.append("deploy")
    sudo(["usermod", "-aG", "sudo,docker", "deploy"])
    write_root_file(
        DEPLOY_MARKER,
        "deploy ALL=(ALL:ALL) NOPASSWD: ALL\n# philotic-test-marker deploy-untouched\n",
        "0440",
    )
    sudo(["visudo", "-c", "-f", str(DEPLOY_MARKER)])
    sudo(["install", "-d", "-m", "0700", "-o", "deploy", "-g", "deploy", "/home/deploy/.ssh"])
    public = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPlaceholderDeployKeyForTestOnly philotic-test-deploy-marker\n"
    # Not a valid key; the role must not rewrite this file on the default path.
    write_root_file(Path("/home/deploy/.ssh/authorized_keys"), public, "0600")
    sudo(["chown", "deploy:deploy", "/home/deploy/.ssh/authorized_keys"])
    return {
        "passwd": sudo(["getent", "passwd", "deploy"]).stdout,
        "groups": sudo(["id", "-nG", "deploy"]).stdout,
        "marker": sha256(DEPLOY_MARKER),
        "keys": sha256(Path("/home/deploy/.ssh/authorized_keys")),
    }


def public_key(directory: Path, name: str) -> str:
    private = directory / name
    run(
        [
            "ssh-keygen",
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            "placeholder-not-a-real-key",
            "-f",
            str(private),
        ]
    )
    public = (directory / f"{name}.pub").read_text().strip()
    private.unlink()
    if "placeholder-not-a-real-key" not in public:
        raise SystemExit("generated public key is missing the placeholder comment")
    return public


def cleanup() -> None:
    for path in [SUDOERS, PIPELINE_SUDOERS, DEPLOY_MARKER, REQUEST_BIN, ENTRYPOINT, HARDENING, *UNIT_FILES]:
        sudo(["rm", "-f", str(path)], check=False)
    sudo(["rm", "-rf", str(APPROVAL_HOME)], check=False)
    for user in CREATED_USERS:
        sudo(["userdel", "-r", user], check=False)
    for group in GROUPS_WE_CREATED:
        sudo(["groupdel", group], check=False)


def main() -> None:
    if shutil.which("ansible-playbook") is None or shutil.which("visudo") is None:
        raise SystemExit("ansible-playbook and visudo are required")
    sudo(["true"])
    defaults = (ANSIBLE / "roles/philotic_hotel/defaults/main.yml").read_text()
    unit = (ANSIBLE / "roles/philotic_hotel/templates/philotic-hotel.service.j2").read_text()
    secrets = (ANSIBLE / "roles/philotic_hotel/templates/secrets.env.j2").read_text()
    if "philotic_vault_key_via_load_credential: false" not in defaults:
        raise SystemExit("vault LoadCredential flag is not default-off")
    if "LoadCredential=philotic-vault-master-key" not in unit:
        raise SystemExit("unit template is missing the gated LoadCredential line")
    if "philotic_vault_key_via_load_credential" not in unit.split("LoadCredential")[0]:
        raise SystemExit("LoadCredential is not behind the flag")
    if "philotic_web_separate_unit: false" not in defaults:
        raise SystemExit("philotic-web separate unit flag is not default-off")
    web_unit = (ANSIBLE / "roles/philotic_hotel/templates/philotic-web.service.j2").read_text()
    if "User={{ philotic_web_user }}" not in web_unit or "LoadCredential=philotic-vault-master-key" not in web_unit:
        raise SystemExit("philotic-web unit is missing its user or LoadCredential")
    if "PHILOTIC_VAULT_MASTER_KEY=" in web_unit:
        raise SystemExit("philotic-web unit puts the vault key in the environment")
    roster = (ANSIBLE / "roles/vps_account_separation/defaults/main.yml").read_text()
    example = (ANSIBLE / "roles/vps_account_separation/examples/agents.yml.example").read_text()

    def assigned_groups(text: str) -> dict[str, str]:
        current = None
        found: dict[str, str] = {}
        for line in text.splitlines():
            stripped = line.strip().lstrip("-").strip()
            if stripped.startswith("name:"):
                current = stripped.split(":", 1)[1].strip()
            elif stripped.startswith("group:") and current:
                found[current] = stripped.split(":", 1)[1].strip()
                current = None
        return found

    expected = {
        "pepper": "agent-ops",
        "priti": "agent-ops",
        "bill": "agent-ops",
        "claude": "agent-ops",
    }
    if assigned_groups(roster) != expected or assigned_groups(example) != expected:
        raise SystemExit(f"roster groups are {assigned_groups(roster)} / {assigned_groups(example)}")
    with tempfile.TemporaryDirectory() as tmp:
        missing = Path(tmp) / "missing-group.yml"
        missing.write_text(
            "vps_accounts_agents:\n"
            "  - name: future\n"
            "    description: unset-group\n"
            "    public_key: \"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPlaceholderFutureAgentKeyNotReal future\"\n"
        )
        refused_roster = playbook(["-e", f"@{missing}"])
    if refused_roster.returncode == 0 or "needs group" not in refused_roster.stdout + refused_roster.stderr:
        raise SystemExit("roster without groups did not fail closed")
    if user_exists("agent-pepper") or user_exists("agent-future") or user_exists("jared-admin"):
        raise SystemExit("group fail-closed still created users")
    for name in ("jared-admin", "philotic-approval", "philotic", "agent-ops-example", "agent-read-example"):
        if user_exists(name):
            raise SystemExit(f"{name} already exists; refusing to reuse it")
    if sudo(["getent", "passwd", "970"], check=False).returncode == 0:
        raise SystemExit("uid 970 is already allocated")
    if sudo(["getent", "passwd", "999"], check=False).returncode == 0:
        raise SystemExit("uid 999 is already allocated")
    for group in ("agent-ops", "agent-readonly"):
        if sudo(["getent", "group", group], check=False).returncode == 0:
            raise SystemExit(f"{group} already exists; refusing to delete a preexisting group")

    print("== syntax-check ==")
    syntax = playbook(["--syntax-check"])
    if syntax.returncode != 0:
        raise SystemExit("syntax-check failed")

    print("== production inventory is refused without connecting ==")
    with tempfile.TemporaryDirectory() as tmp:
        bad = Path(tmp) / "bad.ini"
        bad.write_text(
            "[vps_account_separation]\n"
            "jane-vps ansible_connection=local ansible_host=31.97.130.98 "
            "ansible_python_interpreter=/usr/bin/python3\n"
        )
        refused = run(
            ["ansible-playbook", str(PLAYBOOK), "-i", str(bad)],
            check=False,
        )
        if refused.returncode == 0 or "Refusing" not in refused.stdout + refused.stderr:
            raise SystemExit("production inventory was not refused")
        if user_exists("jared-admin"):
            raise SystemExit("production refusal still created jared-admin")

    sshd_before = snapshot_sshd()
    deploy_before = fixture_deploy()
    sudo(["useradd", "--system", "--uid", "999", "--shell", "/usr/sbin/nologin", "--no-create-home", "philotic"])
    CREATED_USERS.append("philotic")

    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)
        jared = public_key(tmp_path, "jared")
        ops = public_key(tmp_path, "ops")
        reader = public_key(tmp_path, "reader")
        extra = tmp_path / "extra.yml"
        extra.write_text(
            "\n".join(
                [
                    "vps_accounts_narrow_deploy: false",
                    "vps_accounts_harden_sshd: false",
                    "vps_accounts_jared_admin_keys:",
                    f'  - "{jared}"',
                    "vps_accounts_agents:",
                    "  - name: ops-example",
                    "    group: agent-ops",
                    f'    public_key: "{ops}"',
                    "  - name: read-example",
                    "    group: agent-readonly",
                    f'    public_key: "{reader}"',
                    "",
                ]
            )
        )
        for account in ("agent-ops-example", "agent-read-example"):
            CREATED_USERS.append(account)
        CREATED_USERS.extend(["jared-admin", "philotic-approval"])

        GROUPS_WE_CREATED.extend(["agent-ops", "agent-readonly"])
        print("== converge ==")
        first = playbook(["-e", f"@{extra}"])
        if first.returncode != 0:
            raise SystemExit("converge failed")

        print("== account assertions ==")
        assert_accounts(jared, ops, reader)
        assert_deploy_unchanged(deploy_before)
        assert_sshd_unchanged(sshd_before)
        if HARDENING.exists():
            raise SystemExit("sshd hardening file was installed with the flag off")
        if PIPELINE_SUDOERS.exists() or ENTRYPOINT.exists():
            raise SystemExit("deploy narrowing files exist with the flag off")
        sudoers = sudo(["cat", str(SUDOERS)]).stdout
        if "\ndeploy " in f"\n{sudoers}" or sudoers.startswith("deploy "):
            raise SystemExit("default sudoers mentions deploy")
        sudo(["visudo", "-c", "-f", str(SUDOERS)])
        script = sudo(["cat", str(REQUEST_BIN)]).stdout
        for forbidden in ("systemctl", "reboot", "shutdown", "docker"):
            if forbidden in script:
                raise SystemExit(f"restart helper contains {forbidden}")

        print("== idempotence ==")
        second = playbook(["-e", f"@{extra}"])
        if second.returncode != 0:
            raise SystemExit("second converge failed")
        changed, failed = recap_counts(second.stdout + second.stderr)
        if changed != 0 or failed != 0:
            raise SystemExit(f"idempotence failed: changed={changed} failed={failed}")

        print("== check mode ==")
        checked = playbook(["-e", f"@{extra}", "--check"])
        if checked.returncode != 0:
            raise SystemExit("check mode failed")
        changed, failed = recap_counts(checked.stdout + checked.stderr)
        if changed != 0 or failed != 0:
            raise SystemExit(f"check mode was not a no-op: changed={changed} failed={failed}")
        assert_deploy_unchanged(deploy_before)
        assert_sshd_unchanged(sshd_before)

        print("== flagged tasks stay unapplied in check mode ==")
        flagged = playbook(
            [
                "-e",
                f"@{extra}",
                "-e",
                "vps_accounts_narrow_deploy=true",
                "-e",
                "vps_accounts_harden_sshd=true",
                "--check",
            ]
        )
        if flagged.returncode != 0:
            raise SystemExit("flagged check mode failed")
        assert_deploy_unchanged(deploy_before)
        assert_sshd_unchanged(sshd_before)
        if HARDENING.exists():
            raise SystemExit("check mode wrote the sshd drop-in")
        if PIPELINE_SUDOERS.exists():
            raise SystemExit("check mode wrote the deploy-narrowing sudoers")

    print("PASS test_vps_account_separation")


def assert_accounts(jared: str, ops: str, reader: str) -> None:
    approval = sudo(["getent", "passwd", "philotic-approval"]).stdout
    if ":970:" not in approval or "nologin" not in approval:
        raise SystemExit(f"philotic-approval is not the expected system user: {approval}")
    groups = set(sudo(["id", "-nG", "philotic-approval"]).stdout.split())
    if groups & {"sudo", "docker", "deploy", "agent-ops", "agent-readonly"}:
        raise SystemExit(f"philotic-approval has privileged groups: {groups}")
    mode = sudo(["stat", "-c", "%a %U %G", str(APPROVAL_HOME)]).stdout.strip()
    if mode != "700 philotic-approval philotic-approval":
        raise SystemExit(f"approval dir mode is {mode}")
    denied = sudo(
        ["-u", "philotic", "touch", str(APPROVAL_HOME / "hotel-must-not-write")],
        check=False,
    )
    if denied.returncode == 0:
        raise SystemExit("hotel uid wrote the approval directory")
    agent_denied = sudo(
        ["-u", "agent-ops-example", "touch", str(APPROVAL_HOME / "agent-must-not-write")],
        check=False,
    )
    if agent_denied.returncode == 0:
        raise SystemExit("agent account wrote the approval directory")
    owned = sudo(
        ["-u", "philotic-approval", "/usr/bin/touch", str(APPROVAL_HOME / "owner-can-write")],
        check=False,
    )
    if owned.returncode != 0:
        raise SystemExit("approval owner could not write its directory")

    jared_keys = sudo(["cat", "/home/jared-admin/.ssh/authorized_keys"]).stdout
    if 'from="100.64.0.0/10"' not in jared_keys or "placeholder-not-a-real-key" not in jared_keys:
        raise SystemExit("jared-admin key is missing the Tailscale from= restriction")
    if jared.split()[1] not in jared_keys:
        raise SystemExit("jared-admin key material was not installed")

    ops_keys = sudo(["cat", "/home/agent-ops-example/.ssh/authorized_keys"]).stdout
    reader_keys = sudo(["cat", "/home/agent-read-example/.ssh/authorized_keys"]).stdout
    if ops.split()[1] not in ops_keys or reader.split()[1] not in reader_keys:
        raise SystemExit("agent keys were not installed")
    if ops.split()[1] in reader_keys or reader.split()[1] in ops_keys:
        raise SystemExit("agent accounts share a key")
    if 'from="' in ops_keys or 'from="' in reader_keys:
        raise SystemExit("agent keys picked up the jared-admin source restriction")

    ops_groups = sudo(["id", "-nG", "agent-ops-example"]).stdout.split()
    reader_groups = sudo(["id", "-nG", "agent-read-example"]).stdout.split()
    if "agent-ops" not in ops_groups or "sudo" in ops_groups or "docker" in ops_groups:
        raise SystemExit(f"agent-ops groups are wrong: {ops_groups}")
    if "agent-readonly" not in reader_groups or "agent-ops" in reader_groups or "sudo" in reader_groups:
        raise SystemExit(f"agent-readonly groups are wrong: {reader_groups}")

    requested = sudo(
        [
            "-u",
            "agent-ops-example",
            "sudo",
            "-n",
            str(REQUEST_BIN),
            "philotic-hotel.service",
        ]
    )
    if "restart requested" not in requested.stdout:
        raise SystemExit("agent-ops could not record a restart request")
    listing = sudo(["find", str(APPROVAL_HOME / "restart-requests"), "-name", "*.request"]).stdout
    if not listing.strip():
        raise SystemExit("restart request file was not written")
    readonly = sudo(
        [
            "-u",
            "agent-read-example",
            "sudo",
            "-n",
            str(REQUEST_BIN),
            "philotic-hotel.service",
        ],
        check=False,
    )
    if readonly.returncode == 0:
        raise SystemExit("agent-readonly was allowed to request a restart")
    systemctl = sudo(
        ["-u", "agent-ops-example", "sudo", "-n", "systemctl", "restart", "philotic-hotel"],
        check=False,
    )
    if systemctl.returncode == 0:
        raise SystemExit("agent-ops was allowed to run systemctl restart")

    unit = sudo(["cat", str(UNIT_FILES[0])]).stdout
    if "ReadWritePaths=/var/lib/philotic-approval" not in unit:
        raise SystemExit("broker unit does not confine writes to the approval dir")
    if "InaccessiblePaths=/opt/philotic/etc" not in unit:
        raise SystemExit("broker unit can still see hotel secrets")
    hotel = (ANSIBLE / "roles/philotic_hotel/templates/philotic-hotel.service.j2").read_text()
    if "InaccessiblePaths=/var/lib/philotic-approval" not in hotel:
        raise SystemExit("hotel unit does not exclude the approval dir")
    if re.search(r"ReadWritePaths=.*philotic-approval", hotel):
        raise SystemExit("hotel ReadWritePaths includes the approval dir")
    if "RuntimeDirectoryMode=0700" not in hotel:
        raise SystemExit("hotel RuntimeDirectoryMode is not 0700")
    onnx = (ANSIBLE / "roles/philotic_hotel/templates/philotic-onnx-sidecar.service.j2").read_text()
    if "InaccessiblePaths=/var/lib/philotic-approval" not in onnx:
        raise SystemExit("onnx unit does not exclude the approval dir")


def assert_deploy_unchanged(before: dict[str, str]) -> None:
    after = {
        "passwd": sudo(["getent", "passwd", "deploy"]).stdout,
        "groups": sudo(["id", "-nG", "deploy"]).stdout,
        "marker": sha256(DEPLOY_MARKER),
        "keys": sha256(Path("/home/deploy/.ssh/authorized_keys")),
    }
    if after != before:
        raise SystemExit(f"deploy changed:\n before={before}\n after={after}")


def assert_sshd_unchanged(before: dict[str, str]) -> None:
    after = snapshot_sshd()
    if after != before:
        raise SystemExit(f"sshd changed:\n before={before}\n after={after}")


if __name__ == "__main__":
    try:
        main()
    finally:
        cleanup()
