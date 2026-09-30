#!/usr/bin/env python3
"""
Register the agent-frontdoor upstreams on the hotel that owns them (mac-jane).

Pairs with scripts/provision-agent-frontdoor.py (run on the frontdoor hotel).
Registers two MCP client-fabric upstreams with fail-closed tool allowlists and
grants them to the frontdoor caller principals (`mcp:<agent>`):

  muninn-local  stdio  `<muninn> mcp`  (native Muninn, loopback :8750, default vault)
  intel-graph   http   http://127.0.0.1:8901/mcp

Prerequisites (operator ceremonies):
  phil mcp allow-command --command "$(command -v muninn)" --args-prefix mcp

Usage (on mac-jane):
  PHILOTIC_HOTEL_SOCKET=/tmp/philotic-aiua.sock \
  OWNER_AGENT_ID=<philote agent id on this hotel> \
  AGENTS=claude-cloud,codex-cloud \
  python3 scripts/register-frontdoor-upstreams.py

  DRY_RUN=1 ... prints both configs and exits.
"""

import json
import os
import shutil
import socket
import struct
import sys
import time

SOCKET_PATH = os.environ.get("PHILOTIC_HOTEL_SOCKET", "/tmp/philotic-aiua.sock")
OWNER_AGENT_ID = os.environ.get("OWNER_AGENT_ID", "")
AGENTS = [a.strip() for a in os.environ.get("AGENTS", "claude-cloud,codex-cloud").split(",") if a.strip()]
MUNINN_BIN = os.environ.get("MUNINN_BIN") or shutil.which("muninn") or "muninn"
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


def send_frame(sock, payload: dict) -> None:
    data = json.dumps(payload).encode()
    sock.sendall(struct.pack(">I", len(data)) + data)


def recv_frame(sock) -> dict:
    raw_len = b""
    while len(raw_len) < 4:
        chunk = sock.recv(4 - len(raw_len))
        if not chunk:
            raise RuntimeError("socket closed")
        raw_len += chunk
    length = struct.unpack(">I", raw_len)[0]
    data = b""
    while len(data) < length:
        chunk = sock.recv(length - len(data))
        if not chunk:
            raise RuntimeError("socket closed mid-frame")
        data += chunk
    return json.loads(data)


def ipc_call(sock, operation: str, payload: dict) -> dict:
    send_frame(sock, {"operation": operation, "payload": payload})
    return recv_frame(sock)


def upstream_config(upstream_id: str, transport: dict, tools: list[str], now: int) -> dict:
    return {
        "upstream_id": upstream_id,
        "owner_agent_id": OWNER_AGENT_ID,
        "transport": transport,
        "tool_allowlist": [{"remote_name": t, "allotment": PER_TOOL_ALLOTMENT} for t in tools],
        "grant_agents": [f"mcp:{a}" for a in AGENTS],
        "updated_at": now,
    }


def main() -> None:
    if not OWNER_AGENT_ID:
        print("ERROR: OWNER_AGENT_ID required (a philote agent id on this hotel)", file=sys.stderr)
        sys.exit(1)
    now = int(time.time())
    configs = [
        upstream_config(
            "muninn-local", {"kind": "stdio", "command": MUNINN_BIN, "args": ["mcp"]}, MUNINN_TOOLS, now
        ),
        upstream_config("intel-graph", {"kind": "http", "url": GRAPH_URL}, GRAPH_TOOLS, now),
    ]

    if DRY_RUN:
        print(json.dumps(configs, indent=2))
        return

    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(SOCKET_PATH)
    sock.settimeout(15.0)
    # Owner check accepts `<owner>:<suffix>` guest ids (aiua mcp_owner_identity_ok).
    reg = ipc_call(
        sock,
        "register_guest",
        {
            "guest_id": f"{OWNER_AGENT_ID}:frontdoor-provisioner",
            "role": "hotel.internal",
            "supported_tools": [],
        },
    )
    print(f"Register: {reg.get('ok', reg)}")

    failed = False
    for config in configs:
        resp = ipc_call(sock, "register_mcp_upstream", {"config": config})
        print(f"RegisterMcpUpstream {config['upstream_id']}: {json.dumps(resp)}")
        if resp.get("ok") is False or "error" in json.dumps(resp).lower():
            failed = True
    sock.close()
    if failed:
        print("ERROR: one or more upstream registrations failed (see above)", file=sys.stderr)
        sys.exit(1)
    print(f"\nGranted to: {', '.join('mcp:' + a for a in AGENTS)}")
    print("Verify: phil mcp upstreams")


if __name__ == "__main__":
    main()
