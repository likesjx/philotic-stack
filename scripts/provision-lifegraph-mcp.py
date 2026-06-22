#!/usr/bin/env python3
"""
Provision a governed LifeGraph MCP endpoint.

This is intentionally separate from provision-mcp-bearer.py:
- context.capture remains a Perplexity -> Muninn continuity route
- this endpoint exposes governed life.* tools through membrane-mcp

Defaults to read-only life.recall. Set INCLUDE_LIFE_OBSERVE=1 to also expose
life.observe as a proposed-evidence write path.

Usage:
  PHILOTIC_HOTEL_SOCKET=/run/philotic/vps-jane.sock \
  BEARER_TOKEN=<raw-token> \
  python3 scripts/provision-lifegraph-mcp.py
"""

import hashlib
import json
import os
import socket
import struct
import sys
import time

SOCKET_PATH = os.environ.get("PHILOTIC_HOTEL_SOCKET", "/run/philotic/vps-jane.sock")
BEARER_TOKEN = os.environ.get("BEARER_TOKEN", "")
ENDPOINT_ID = os.environ.get("ENDPOINT_ID", "lifegraph-readonly")
OWNER_AGENT_ID = os.environ.get("OWNER_AGENT_ID", "agent-beacon-01")
PORT = int(os.environ.get("PORT", "8911"))
EXPOSURE = os.environ.get("EXPOSURE", "mesh")
TOKEN_ID = os.environ.get("TOKEN_ID", "lifegraph-reader")
TARGET_ROLE = os.environ.get("TARGET_ROLE", "life-graph-runner")
INCLUDE_LIFE_OBSERVE = os.environ.get("INCLUDE_LIFE_OBSERVE", "0") in {"1", "true", "yes"}

if not BEARER_TOKEN:
    print("ERROR: BEARER_TOKEN env var required", file=sys.stderr)
    sys.exit(1)

try:
    import blake3 as blake3_lib

    def blake3_hex(data: bytes) -> str:
        return blake3_lib.blake3(data).hexdigest()

except ImportError:
    print("blake3 not available (install with: python3 -m pip install blake3)", file=sys.stderr)
    sys.exit(1)


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


def response_operation(resp: dict) -> str:
    return str(resp.get("operation") or resp.get("type") or "")


def is_endpoint_success(resp: dict) -> bool:
    payload = response_payload(resp)
    return (
        resp.get("ok") is True
        or response_operation(resp) == "mcp_endpoint_provisioned"
        or "endpoint_id" in payload
    )


def lifegraph_auth(secret_ref: str, scopes: list[str]) -> dict:
    return {
        "scheme": "bearer_token",
        "grants": [
            {
                "token_id": TOKEN_ID,
                "vault_ref": secret_ref,
                "scopes": scopes,
                "allotment": {"max_per_window": 120, "window_secs": 3600},
            }
        ],
    }


def datasource_transform(tool_name: str) -> dict:
    return {
        "kind": "field_map",
        "action": tool_name,
        "target": {
            "kind": "datasource",
            "datasource_id": TARGET_ROLE,
        },
        # Empty mapping intentionally passes the MCP args object through as
        # datasource arguments.
        "mappings": [],
    }


def life_recall_tool(secret_ref: str) -> dict:
    return {
        "name": "life.recall",
        "description": (
            "Read governed context packets from the operator LifeGraph. "
            "This is a read-only LifeGraph tool; it does not write Muninn memory "
            "and does not confirm LifeGraph truth."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "query_id": {"type": "string"},
                "query_text": {"type": "string"},
                "operator_intent": {
                    "type": "string",
                    "description": (
                        "Optional named strategy hint: open_loops_by_context, "
                        "goals_and_next_actions, commitments_approaching, re_entry_context."
                    ),
                },
                "max_context_packets": {"type": "integer", "default": 6},
            },
            "required": ["query_text"],
        },
        "inbound_transform": datasource_transform("life.recall"),
        "outbound_transform": {"kind": "pass_through"},
        "auth": lifegraph_auth(secret_ref, ["life.recall"]),
    }


def life_observe_tool(secret_ref: str) -> dict:
    return {
        "name": "life.observe",
        "description": (
            "Propose grounded evidence in the operator LifeGraph with "
            "validation_state=proposed. This is not a confirmed-truth write and "
            "must not be used as a Muninn continuity capture route."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "observation_id": {"type": "string"},
                "evidence": {"type": "object"},
                "proposed_graph_refs": {
                    "type": "array",
                    "items": {"type": "object"},
                    "default": [],
                },
            },
            "required": ["observation_id", "evidence"],
        },
        "inbound_transform": datasource_transform("life.observe"),
        "outbound_transform": {"kind": "pass_through"},
        "auth": lifegraph_auth(secret_ref, ["life.observe"]),
    }


def main() -> None:
    token_hash_hex = blake3_hex(BEARER_TOKEN.encode())
    print(f"Bearer token SHA-256 preview: {hashlib.sha256(BEARER_TOKEN.encode()).hexdigest()[:12]}")

    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(SOCKET_PATH)
    sock.settimeout(10.0)

    reg_resp = ipc_call(
        sock,
        "register_guest",
        {
            "guest_id": "lifegraph-mcp-provisioner",
            "role": "hotel.internal",
            "supported_tools": [],
        },
    )
    print(f"Register: {reg_resp}")

    add_resp = ipc_call(
        sock,
        "add_vault_entry",
        {
            "vault_name": "default",
            "plaintext": token_hash_hex,
            "allowed_roles": ["mcp-membrane"],
        },
    )
    print(f"AddVaultEntry: {add_resp}")

    if not add_resp.get("ok"):
        print("ERROR: AddVaultEntry failed", file=sys.stderr)
        sys.exit(1)

    secret_ref = add_resp.get("secret_ref") or add_resp.get("data", {}).get("secret_ref")
    if not secret_ref:
        print(f"Full response: {json.dumps(add_resp, indent=2)}", file=sys.stderr)
        sys.exit(1)

    now = int(time.time())
    tools = [life_recall_tool(secret_ref)]
    if INCLUDE_LIFE_OBSERVE:
        tools.append(life_observe_tool(secret_ref))

    config = {
        "endpoint_id": ENDPOINT_ID,
        "owner_agent_id": OWNER_AGENT_ID,
        "port": PORT,
        "path": "/mcp",
        "exposure": EXPOSURE,
        "tools": tools,
        "preapproval_rules": [
            {
                "action_pattern": "life.recall",
                "target": {"kind": "datasource", "datasource_id": TARGET_ROLE},
                "approved_by_turn": "operator-provisioned:lifegraph-mcp",
                "approved_at": now,
            }
        ],
        "updated_at": now,
    }

    if INCLUDE_LIFE_OBSERVE:
        config["preapproval_rules"].append(
            {
                "action_pattern": "life.observe",
                "target": {"kind": "datasource", "datasource_id": TARGET_ROLE},
                "approved_by_turn": "operator-provisioned:lifegraph-mcp",
                "approved_at": now,
            }
        )

    provision_resp = ipc_call(sock, "provision_mcp_endpoint", {"config": config})
    print(f"ProvisionMcpEndpoint: {provision_resp}")
    sock.close()

    if not is_endpoint_success(provision_resp):
        print("ERROR: ProvisionMcpEndpoint failed", file=sys.stderr)
        sys.exit(1)

    tool_names = ", ".join(tool["name"] for tool in tools)
    print("\nProvisioned LifeGraph MCP endpoint")
    print(f"  endpoint_id: {ENDPOINT_ID}")
    print(f"  port:        {PORT}")
    print(f"  exposure:    {EXPOSURE}")
    print(f"  tools:       {tool_names}")
    print(f"  token_id:    {TOKEN_ID}")
    print(f"  vault_ref:   {secret_ref}")


if __name__ == "__main__":
    main()
