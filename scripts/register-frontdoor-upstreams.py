#!/usr/bin/env python3
"""
Register the agent-frontdoor upstreams on the hotel that owns each one.

Pairs with scripts/provision-agent-frontdoor.py (run on the frontdoor hotel).
Registers MCP client-fabric upstreams with fail-closed tool allowlists and
grants them to the frontdoor caller principals (`mcp:<agent>`):

  muninn-cortex  http   http://127.0.0.1:8750/mcp   on vps-jane (the Muninn Cortex)
  intel-graph    http   http://127.0.0.1:8901/mcp   on mac-jane

Muninn is registered on vps-jane, not the Mac: the Mac Muninn nodes are
cluster observers and reject writes (-32002), so `muninn_remember` /
`muninn_decide` only work against the Cortex. The Cortex bearer (the same
`default`-vault token local Claude uses) is read from MUNINN_TOKEN_FILE and
stored in the hotel vault via provision_mcp_upstream_credential; it is never
printed.

Usage:
  # on vps-jane (hotel socket path as deployed)
  UPSTREAMS=muninn-cortex OWNER_AGENT_ID=agent-beacon \
  MUNINN_TOKEN_FILE=~/.philotic/muninn-cortex.token \
  PHILOTIC_HOTEL_SOCKET=/opt/philotic/data/aiua-vps-jane.sock \
  python3 scripts/register-frontdoor-upstreams.py

  # on mac-jane
  UPSTREAMS=intel-graph OWNER_AGENT_ID=agent-bjork-01 \
  PHILOTIC_HOTEL_SOCKET=~/.philotic/bjork/aiua-mac-jane.sock \
  python3 scripts/register-frontdoor-upstreams.py

  DRY_RUN=1 ... prints the configs and exits.
"""

import json
import os
import socket
import struct
import sys
import time

SOCKET_PATH = os.path.expanduser(os.environ.get("PHILOTIC_HOTEL_SOCKET", "/tmp/philotic-aiua.sock"))
OWNER_AGENT_ID = os.environ.get("OWNER_AGENT_ID", "")
AGENTS = [a.strip() for a in os.environ.get("AGENTS", "claude-cloud,codex-cloud").split(",") if a.strip()]
UPSTREAMS = [u.strip() for u in os.environ.get("UPSTREAMS", "").split(",") if u.strip()]
MUNINN_URL = os.environ.get("MUNINN_URL", "http://127.0.0.1:8750/mcp")
MUNINN_TOKEN_FILE = os.environ.get("MUNINN_TOKEN_FILE", "")
GRAPH_URL = os.environ.get("GRAPH_URL", "http://127.0.0.1:8901/mcp")
PER_TOOL_ALLOTMENT = int(os.environ.get("PER_TOOL_ALLOTMENT", "300"))
DRY_RUN = os.environ.get("DRY_RUN", "0") in {"1", "true", "yes"}

# Must match the frontdoor tool set in provision-agent-frontdoor.py.
MUNINN_TOOLS = ["muninn_where_left_off", "muninn_recall", "muninn_remember", "muninn_decide"]
GRAPH_TOOLS = [
    "graph_status",
    "graph_digest",
    "graph_next_task",
    "graph_context_for",
    "graph_impact",
    "graph_search",
    "graph_agent_dashboard",
    "session_start",
    "session_activity",
    "session_close",
    "graph_decide",
    "graph_record_test_run",
]

# upstream_id -> (url, tools, needs Muninn credential)
UPSTREAM_SPECS = {
    "muninn-cortex": (MUNINN_URL, MUNINN_TOOLS, True),
    "intel-graph": (GRAPH_URL, GRAPH_TOOLS, False),
}


class Ipc:
    """Length-prefixed JSON IPC. The hotel pushes unsolicited frames on the same
    stream (e.g. a blob-endpoint advert right after connect), so a call reads
    until it sees a Standard ok/code envelope or a key it expects."""

    def __init__(self, path: str):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(15.0)
        self.sock.connect(path)

    def _read_exact(self, n: int) -> bytes:
        buf = b""
        while len(buf) < n:
            chunk = self.sock.recv(n - len(buf))
            if not chunk:
                raise RuntimeError("socket closed")
            buf += chunk
        return buf

    def call(self, operation: str, payload: dict, expect_keys=()) -> dict:
        data = json.dumps({"operation": operation, "payload": payload}).encode()
        self.sock.sendall(struct.pack(">I", len(data)) + data)
        for _ in range(50):
            (length,) = struct.unpack(">I", self._read_exact(4))
            frame = json.loads(self._read_exact(length))
            if isinstance(frame, dict) and (
                "ok" in frame or "code" in frame or any(k in frame for k in expect_keys)
            ):
                return frame
        raise RuntimeError(f"no response for {operation}")

    def close(self) -> None:
        self.sock.close()


def upstream_config(upstream_id: str, url: str, tools: list[str], now: int) -> dict:
    return {
        "upstream_id": upstream_id,
        "owner_agent_id": OWNER_AGENT_ID,
        "transport": {"kind": "http", "url": url},
        "tool_allowlist": [{"remote_name": t, "allotment": PER_TOOL_ALLOTMENT} for t in tools],
        "grant_agents": [f"mcp:{a}" for a in AGENTS],
        "updated_at": now,
    }


def read_muninn_token() -> str:
    if not MUNINN_TOKEN_FILE:
        print("ERROR: MUNINN_TOKEN_FILE required for muninn-cortex", file=sys.stderr)
        sys.exit(1)
    token = open(os.path.expanduser(MUNINN_TOKEN_FILE)).read().strip()
    # Accept either the raw key or a full "Bearer <key>" header value; the
    # client formats the header itself.
    return token.removeprefix("Bearer ").strip()


def main() -> None:
    if not OWNER_AGENT_ID:
        print("ERROR: OWNER_AGENT_ID required (a philote agent id on this hotel)", file=sys.stderr)
        sys.exit(1)
    unknown = [u for u in UPSTREAMS if u not in UPSTREAM_SPECS]
    if not UPSTREAMS or unknown:
        print(f"ERROR: UPSTREAMS must name one or more of {sorted(UPSTREAM_SPECS)}", file=sys.stderr)
        sys.exit(1)
    now = int(time.time())
    configs = [upstream_config(u, UPSTREAM_SPECS[u][0], UPSTREAM_SPECS[u][1], now) for u in UPSTREAMS]

    if DRY_RUN:
        print(json.dumps(configs, indent=2))
        return

    ipc = Ipc(SOCKET_PATH)
    # Owner checks accept `<owner>:<suffix>` guest ids (aiua mcp_owner_identity_ok).
    reg = ipc.call(
        "register",
        {
            "guest_id": f"{OWNER_AGENT_ID}:frontdoor-provisioner",
            "role": "hotel.internal",
            "supported_tools": [],
        },
    )
    if reg.get("ok") is not True:
        print(f"ERROR: register failed: {reg}", file=sys.stderr)
        sys.exit(1)

    failed = False
    for config in configs:
        upstream_id = config["upstream_id"]
        resp = ipc.call("register_mcp_upstream", {"config": config}, expect_keys=("mcp_upstream_id",))
        ok = resp.get("mcp_upstream_id") == upstream_id
        print(f"RegisterMcpUpstream {upstream_id}: {'ok' if ok else json.dumps(resp)}")
        failed |= not ok
        if ok and UPSTREAM_SPECS[upstream_id][2]:
            cred = ipc.call(
                "provision_mcp_upstream_credential",
                {"upstream_id": upstream_id, "owner_agent_id": OWNER_AGENT_ID, "credential": read_muninn_token()},
            )
            cred_ok = cred.get("ok") is True
            print(f"ProvisionMcpUpstreamCredential {upstream_id}: {'ok' if cred_ok else json.dumps(cred)}")
            failed |= not cred_ok
    ipc.close()
    if failed:
        print("ERROR: one or more upstream registrations failed (see above)", file=sys.stderr)
        sys.exit(1)
    print(f"\nGranted to: {', '.join('mcp:' + a for a in AGENTS)}")


if __name__ == "__main__":
    main()
