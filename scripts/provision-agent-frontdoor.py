#!/usr/bin/env python3
"""
Provision the agent frontdoor MCP endpoint (proposal: agent-frontdoor).

Remote agents (cloud Claude Code, Codex, any MCP client that can send a bearer
header) get the same continuity and coordination tools local clients use, by
name, without native Muninn or intel-graph ever leaving loopback:

  agent --HTTPS+bearer--> membrane-mcp endpoint (this hotel, e.g. vps-jane)
        --mesh EmitTask--> mcp-client-runner on the upstream hotel (the Mac)
        --loopback-------> muninn-local (stdio `muninn mcp`) / intel-graph (:8901)

Each agent gets its own bearer grant (token_id = agent name). membrane-mcp
presents the caller to mcp-client-runner as `mcp:<token_id>`, so the upstream
registrations on the Mac must list those principals in `grant_agents`
(see docs/HANDOFF-2026-09-30-agent-frontdoor-mac-jane.md).

Raw tokens are minted here, written to TOKEN_OUT_DIR/<agent>.token (0600), and
never printed. Move them into the operator secret store, then delete the files.
Re-running rotates every agent's token (new vault refs).

Usage (on the frontdoor hotel):
  PHILOTIC_HOTEL_SOCKET=/run/philotic/vps-jane.sock \
  AGENTS=claude-cloud,codex-cloud \
  python3 scripts/provision-agent-frontdoor.py

  DRY_RUN=1 AGENTS=claude-cloud python3 scripts/provision-agent-frontdoor.py
    prints the endpoint config (with placeholder vault refs) and exits.
"""

import hashlib
import json
import os
import pathlib
import secrets
import socket
import struct
import sys
import time

SOCKET_PATH = os.environ.get("PHILOTIC_HOTEL_SOCKET", "/run/philotic/vps-jane.sock")
ENDPOINT_ID = os.environ.get("ENDPOINT_ID", "agent-frontdoor")
OWNER_AGENT_ID = os.environ.get("OWNER_AGENT_ID", "agent-beacon-01")
PORT = int(os.environ.get("PORT", "8912"))
EXPOSURE = os.environ.get("EXPOSURE", "mesh")
AGENTS = [a.strip() for a in os.environ.get("AGENTS", "claude-cloud,codex-cloud").split(",") if a.strip()]
UPSTREAM_NODE = os.environ.get("UPSTREAM_NODE", "mac-jane-aiua-01")
MUNINN_UPSTREAM = os.environ.get("MUNINN_UPSTREAM", "muninn-local")
GRAPH_UPSTREAM = os.environ.get("GRAPH_UPSTREAM", "intel-graph")
EXPIRES_DAYS = int(os.environ.get("EXPIRES_DAYS", "30"))
CALLS_PER_HOUR = int(os.environ.get("CALLS_PER_HOUR", "300"))
TOKEN_OUT_DIR = pathlib.Path(
    os.environ.get("TOKEN_OUT_DIR", str(pathlib.Path.home() / ".philotic" / "agent-frontdoor-tokens"))
).expanduser()
DRY_RUN = os.environ.get("DRY_RUN", "0") in {"1", "true", "yes"}

# Frontdoor tool name == remote tool name, so agent skills and
# scripts/muninn_mcp.py work unchanged against either transport.
MUNINN_TOOLS = [
    ("muninn_where_left_off", "Muninn continuity: recent active memory (session orientation)."),
    ("muninn_recall", "Muninn continuity: recall memories for context phrases (supports tags_all/tags_any)."),
    ("muninn_remember", "Muninn continuity: store one atomic memory (1-3 sentences, one concept)."),
    ("muninn_decide", "Muninn continuity: record a decision with rationale."),
]
GRAPH_TOOLS = [
    ("graph_status", "Intel-graph: node/edge counts and proposal pipeline."),
    ("graph_digest", "Intel-graph: what is in flight right now."),
    ("graph_next_task", "Intel-graph: highest-priority unclaimed work."),
    ("graph_context_for", "Intel-graph: proposal + seams + code + verification for one target."),
    ("graph_impact", "Intel-graph: blast radius for a change."),
    ("graph_search", "Intel-graph: full-text search across code and docs."),
    ("graph_agent_dashboard", "Intel-graph: active agent sessions and claims."),
    ("session_start", "Intel-graph: claim work so other agents see it."),
    ("session_activity", "Intel-graph: heartbeat/progress on a claimed session."),
    ("session_close", "Intel-graph: release a claimed session."),
    ("graph_decide", "Intel-graph: record an architectural decision."),
    ("graph_record_test_run", "Intel-graph: record a verification/test run."),
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


def response_payload(resp: dict) -> dict:
    payload = resp.get("payload")
    return payload if isinstance(payload, dict) else resp


def is_endpoint_success(resp: dict) -> bool:
    payload = response_payload(resp)
    return (
        resp.get("ok") is True
        or str(resp.get("operation") or resp.get("type") or "") == "mcp_endpoint_provisioned"
        or "endpoint_id" in payload
    )


def upstream_target(upstream_id: str) -> dict:
    return {"kind": "mcp_upstream", "upstream_id": upstream_id, "target_node": UPSTREAM_NODE}


def build_tools(grants: list[dict]) -> list[dict]:
    auth = {"scheme": "bearer_token", "grants": grants}
    tools = []
    for upstream_id, catalog in ((MUNINN_UPSTREAM, MUNINN_TOOLS), (GRAPH_UPSTREAM, GRAPH_TOOLS)):
        for name, description in catalog:
            tools.append(
                {
                    "name": name,
                    "description": description,
                    # Arguments pass through verbatim; the upstream validates
                    # them against its own schema.
                    "input_schema": {"type": "object", "additionalProperties": True},
                    "inbound_transform": {
                        "kind": "field_map",
                        "action": name,
                        "target": upstream_target(upstream_id),
                        "mappings": [],
                    },
                    "outbound_transform": {"kind": "pass_through"},
                    "auth": auth,
                }
            )
    return tools


def build_config(grants: list[dict], now: int) -> dict:
    tools = build_tools(grants)
    return {
        "endpoint_id": ENDPOINT_ID,
        "owner_agent_id": OWNER_AGENT_ID,
        "port": PORT,
        "path": "/mcp",
        "exposure": EXPOSURE,
        "tools": tools,
        # Remote agents cannot sit through the 300s approval hold: every
        # projected tool is pre-approved by this operator provisioning run.
        # Authority stays with the upstream allowlist + grant_agents on the Mac.
        "preapproval_rules": [
            {
                "action_pattern": tool["name"],
                "target": tool["inbound_transform"]["target"],
                "approved_by_turn": "operator-provisioned:agent-frontdoor",
                "approved_at": now,
            }
            for tool in tools
        ],
        "updated_at": now,
    }


def grant(agent: str, vault_ref: str, now: int) -> dict:
    return {
        "token_id": agent,
        "vault_ref": vault_ref,
        "scopes": ["memory.continuity", "graph.coordinate"],
        "expires_at": now + EXPIRES_DAYS * 86400,
        "allotment": {"max_per_window": CALLS_PER_HOUR, "window_secs": 3600},
    }


def main() -> None:
    if not AGENTS:
        print("ERROR: AGENTS is empty", file=sys.stderr)
        sys.exit(1)
    now = int(time.time())

    if DRY_RUN:
        grants = [grant(a, f"<vault-ref:{a}>", now) for a in AGENTS]
        print(json.dumps(build_config(grants, now), indent=2))
        return

    try:
        import blake3 as blake3_lib
    except ImportError:
        print("blake3 not available (install with: python3 -m pip install blake3)", file=sys.stderr)
        sys.exit(1)

    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(SOCKET_PATH)
    sock.settimeout(10.0)
    reg = ipc_call(
        sock,
        "register_guest",
        {"guest_id": "agent-frontdoor-provisioner", "role": "hotel.internal", "supported_tools": []},
    )
    print(f"Register: {reg.get('ok', reg)}")

    TOKEN_OUT_DIR.mkdir(parents=True, exist_ok=True)
    os.chmod(TOKEN_OUT_DIR, 0o700)

    grants = []
    for agent in AGENTS:
        raw = secrets.token_urlsafe(32)
        resp = ipc_call(
            sock,
            "add_vault_entry",
            {
                "vault_name": "default",
                "plaintext": blake3_lib.blake3(raw.encode()).hexdigest(),
                "allowed_roles": ["mcp-membrane"],
            },
        )
        vault_ref = resp.get("secret_ref") or resp.get("data", {}).get("secret_ref")
        if not resp.get("ok") or not vault_ref:
            print(f"ERROR: AddVaultEntry failed for {agent}: {resp}", file=sys.stderr)
            sys.exit(1)
        token_path = TOKEN_OUT_DIR / f"{agent}.token"
        fd = os.open(token_path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as fh:
            fh.write(raw + "\n")
        print(f"  {agent}: vault_ref={vault_ref} sha256-preview={hashlib.sha256(raw.encode()).hexdigest()[:12]}")
        grants.append(grant(agent, vault_ref, now))

    config = build_config(grants, now)
    resp = ipc_call(sock, "provision_mcp_endpoint", {"config": config})
    sock.close()
    if not is_endpoint_success(resp):
        print(f"ERROR: ProvisionMcpEndpoint failed: {resp}", file=sys.stderr)
        sys.exit(1)

    print("\nProvisioned agent frontdoor")
    print(f"  endpoint_id:   {ENDPOINT_ID}  port={PORT} exposure={EXPOSURE}")
    print(f"  upstream node: {UPSTREAM_NODE} ({MUNINN_UPSTREAM}, {GRAPH_UPSTREAM})")
    print(f"  tools:         {len(config['tools'])}")
    print(f"  agents:        {', '.join(AGENTS)} (expires in {EXPIRES_DAYS}d)")
    print(f"  tokens:        {TOKEN_OUT_DIR}/<agent>.token  (move to secret store, then delete)")
    print(f"  Mac grant_agents must include: {', '.join('mcp:' + a for a in AGENTS)}")


if __name__ == "__main__":
    main()
