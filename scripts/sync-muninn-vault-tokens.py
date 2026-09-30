#!/usr/bin/env python3
"""Re-sync an observer hotel's Muninn vault tokens from the Cortex hotel.

Why: restoring an observer's store from a Cortex checkpoint replaces its Muninn
auth store with the Cortex's, so vault tokens this hotel minted locally stop
existing — every recall for those vaults then fails with HTTP 401 ("stored token
is stale"), and an observer cannot mint replacements (minting is a write, and
observers reject writes with 421). The Cortex hotel holds tokens that the shared
auth store accepts; this copies them into the observer hotel's own vault.

Run it ON the observer Mac (it ssh's to the Cortex for the token values):

    python3 scripts/sync-muninn-vault-tokens.py --socket ~/.philotic/bjork/aiua-mac-jane.sock
    python3 scripts/sync-muninn-vault-tokens.py --socket ~/.philotic/jane/aiua-mbp-jane.sock --dry-run

Token values move over ssh only and are never printed or written to disk in the
clear. Vaults the Cortex has no token for are provisioned there first by the
normal forwarded-write path (a tagged memory that is immediately forgotten).
"""
import argparse, json, os, socket, struct, subprocess, sys, time

CORTEX_SSH = os.environ.get("PHILOTIC_CORTEX_SSH", "deploy@jane-vps")
CORTEX_SOCKET = os.environ.get("PHILOTIC_CORTEX_SOCKET", "/run/philotic/vps-jane.sock")
CORTEX_NODE = os.environ.get("PHILOTIC_CORTEX_NODE", "vps-jane-aiua-01")
MEMORY_VAULT_PREFIXES = ("self_", "user_")
MEMORY_VAULTS = {"default", "fleet_knowledge"}


class Hotel:
    """Minimal IPC client: 4-byte big-endian length prefix + JSON frames."""

    def __init__(self, sock_path, guest_id, role):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.settimeout(30)
        self.s.connect(os.path.expanduser(sock_path))
        self.request("register", {"guest_id": guest_id, "role": role, "supported_tools": []})

    def _exact(self, n):
        buf = b""
        while len(buf) < n:
            chunk = self.s.recv(n - len(buf))
            if not chunk:
                raise SystemExit("hotel closed the socket")
            buf += chunk
        return buf

    def request(self, operation, payload=None):
        body = {"operation": operation}
        if payload is not None:
            body["payload"] = payload
        data = json.dumps(body).encode()
        self.s.sendall(struct.pack(">I", len(data)) + data)
        while True:
            (n,) = struct.unpack(">I", self._exact(4))
            resp = json.loads(self._exact(n))
            if isinstance(resp, dict) and any(
                k in resp for k in ("ok", "error", "value_json", "key", "secret")
            ):
                return resp

    def config(self, key):
        return (self.request("get_config", {"key": key}) or {}).get("value_json")


def registry(hotel):
    raw = hotel.config("vault_registry")
    if not raw:
        return {}
    entries = json.loads(raw)
    if isinstance(entries, str):
        entries = json.loads(entries)
    return {e["vault_name"]: e["secret_ref"] for e in entries if isinstance(e, dict)}


def is_memory_vault(name):
    return name in MEMORY_VAULTS or name.startswith(MEMORY_VAULT_PREFIXES)


def cortex_script(body):
    """Run a small python program on the Cortex host over ssh; stdout is its output."""
    # The Cortex hotel socket is owned by the service user, so the helper runs as
    # that user (sudo -n; the deploy account has passwordless sudo there).
    remote = os.environ.get("PHILOTIC_CORTEX_PYTHON", "sudo -n -u philotic python3 -")
    proc = subprocess.run(
        ["ssh", "-o", "ConnectTimeout=15", "-o", "BatchMode=yes", CORTEX_SSH, remote],
        input=body.encode(), capture_output=True,
    )
    if proc.returncode != 0:
        raise SystemExit(
            "Cortex helper failed: " + (proc.stderr.decode().strip().splitlines() or ["?"])[-1][:200]
        )
    return proc.stdout.decode()


CORTEX_HELPER = '''
import json, os, socket, struct, sys
sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); sock.settimeout(30)
sock.connect(%(socket)r)
def call(op, payload=None):
    body = {"operation": op}
    if payload is not None:
        body["payload"] = payload
    data = json.dumps(body).encode()
    sock.sendall(struct.pack(">I", len(data)) + data)
    while True:
        hdr = b""
        while len(hdr) < 4:
            hdr += sock.recv(4 - len(hdr))
        (n,) = struct.unpack(">I", hdr)
        buf = b""
        while len(buf) < n:
            buf += sock.recv(n - len(buf))
        resp = json.loads(buf)
        if isinstance(resp, dict) and any(k in resp for k in ("ok", "error", "value_json", "key", "secret")):
            return resp
call("register", {"guest_id": "hotel", "role": "hotel", "supported_tools": []})
raw = (call("get_config", {"key": "vault_registry"}) or {}).get("value_json")
entries = json.loads(raw) if raw else []
if isinstance(entries, str):
    entries = json.loads(entries)
refs = {e["vault_name"]: e["secret_ref"] for e in entries if isinstance(e, dict)}
out = {}
for vault in %(vaults)r:
    ref = refs.get(vault)
    if not ref:
        continue
    resp = call("get_secret", {"secret_ref": ref})
    # IpcResponse::SecretData { secret_ref, value_json } — value_json is the
    # JSON-encoded plaintext.
    raw_secret = resp.get("value_json")
    if raw_secret:
        try:
            secret = json.loads(raw_secret)
        except Exception:
            secret = raw_secret
        if isinstance(secret, str) and secret:
            out[vault] = secret
print(json.dumps(out))
'''


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--socket", required=True, help="this hotel's IPC socket")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    hotel = Hotel(args.socket, "muninn-token-sync", "hotel")
    local = {v: r for v, r in registry(hotel).items() if is_memory_vault(v)}
    if not local:
        print("no memory vaults registered on this hotel")
        return 0
    print("memory vaults on this hotel:", ", ".join(sorted(local)))

    wanted = sorted(local)
    tokens = json.loads(cortex_script(CORTEX_HELPER % {"socket": CORTEX_SOCKET, "vaults": wanted}))
    missing = [v for v in wanted if v not in tokens]
    print("tokens available on the Cortex:", len(tokens), "| missing there:", missing or "none")

    for vault in missing:
        # Provision on the Cortex through the ordinary forwarded-write path.
        print(f"  provisioning {vault} on the Cortex via a forwarded write…")
        task = {
            "action": "memory.write_forward", "op": "remember", "vault": vault,
            "concept": f"vault-token-provision-{int(time.time())}",
            "content": "Provisioning marker written by scripts/sync-muninn-vault-tokens.py; safe to delete.",
            "tags": ["delete-me", "vault-provision"], "metadata": None,
            "origin_node": "token-sync", "origin_agent": "token-sync", "session_id": "token-sync",
        }
        if args.dry_run:
            continue
        hotel.request("emit_task", {
            "target_node": CORTEX_NODE, "target_role": "hotel.memory_write_forward",
            "target_guest_id": None, "task_json": json.dumps(task),
        })
    if missing and not args.dry_run:
        time.sleep(8)
        tokens = json.loads(cortex_script(CORTEX_HELPER % {"socket": CORTEX_SOCKET, "vaults": wanted}))
        print("tokens available after provisioning:", len(tokens))

    changed = 0
    for vault, token in sorted(tokens.items()):
        if args.dry_run:
            print(f"  would install token for {vault}")
            continue
        resp = hotel.request("rotate_secret", {"secret_ref": local[vault], "plaintext": token})
        ok = resp.get("ok")
        print(f"  {vault}: {'installed' if ok else 'FAILED ' + str(resp.get('message'))[:80]}")
        changed += 1 if ok else 0

    if changed and not args.dry_run:
        # The reload reply is a status push shape; don't block the run on it.
        try:
            hotel.s.settimeout(5)
            hotel.request("refresh_memory_config")
        except Exception:
            pass
        print(f"installed {changed} token(s); asked the hotel to reload its memory config")
        print("If recalls still 401, restart the hotel so every guest refetches its config.")
    return 0


sys.exit(main())
