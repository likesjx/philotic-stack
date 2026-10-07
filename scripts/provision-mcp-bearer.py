#!/usr/bin/env python3
"""
Provision (or reprovision) the MCP bearer token for vps-jane membrane-mcp.

Connects to the hotel UDS socket, calls AddVaultEntry with the BLAKE3 hex hash
of the bearer token, then calls UpdateMcpRoutes with the new vault_ref.

Usage:
  PHILOTIC_HOTEL_SOCKET=/run/philotic/vps-jane.sock \
  BEARER_TOKEN=<raw-token> \
  python3 provision-mcp-bearer.py
"""

import json
import os
import sys
import hashlib

from philotic_ipc import Ipc, IpcError, secret_ref_of

SOCKET_PATH = os.environ.get("PHILOTIC_HOTEL_SOCKET", "/run/philotic/vps-jane.sock")
BEARER_TOKEN = os.environ.get("BEARER_TOKEN", "")
AGENT_ID = os.environ.get("AGENT_ID", "agent-beacon-01")
TARGET_NODE = os.environ.get("TARGET_NODE", "mbp-jane-aiua-01")

if not BEARER_TOKEN:
    print("ERROR: BEARER_TOKEN env var required", file=sys.stderr)
    sys.exit(1)

try:
    import blake3 as blake3_lib
    def blake3_hex(data: bytes) -> str:
        return blake3_lib.blake3(data).hexdigest()
except ImportError:
    print("blake3 not available, using sha256 fallback (WRONG — install blake3 via pip)", file=sys.stderr)
    sys.exit(1)


def main():
    token_hash_hex = blake3_hex(BEARER_TOKEN.encode())
    print(f"Bearer token SHA-256 preview: {hashlib.sha256(BEARER_TOKEN.encode()).hexdigest()[:12]}")

    ipc = Ipc(SOCKET_PATH)
    try:
        ipc.register_operator(AGENT_ID, "mcp-provisioner")
    except IpcError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        sys.exit(1)

    # Store the BLAKE3 hash in the vault
    add_resp = ipc.call("add_vault_entry", {
        "vault_name": "default",
        "plaintext": token_hash_hex,
        "allowed_roles": ["mcp-membrane"]
    })
    print(f"AddVaultEntry: {add_resp}")

    if not add_resp.get("ok"):
        print("ERROR: AddVaultEntry failed", file=sys.stderr)
        sys.exit(1)

    secret_ref = secret_ref_of(add_resp)
    if not secret_ref:
        # Try to parse from nested response
        print(f"Full response: {json.dumps(add_resp, indent=2)}")
        sys.exit(1)

    print(f"New vault_ref: {secret_ref}")

    import time
    now = int(time.time())

    # Build the route record. This is intentionally a Muninn continuity capture
    # route, not a LifeGraph write path.
    route = {
        "agent_id": AGENT_ID,
        "tool_name": "context.capture",
        "description": (
            "Capture Perplexity context into Muninn continuity memory. "
            "Use for notes, decisions, references, and memory-worthy context that should be "
            "retrievable across sessions. This does not write to the operator LifeGraph; "
            "use the governed life.* tools for LifeGraph observations, recalls, or patches."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "The Perplexity context, note, decision, or reference to store in Muninn."
                },
                "category": {
                    "type": "string",
                    "description": "Muninn memory category for retrieval and filtering.",
                    "enum": ["memory", "note", "decision", "reference"]
                },
                "tags": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Optional Muninn retrieval tags. Do not use tags to imply LifeGraph truth."
                }
            },
            "required": ["content"]
        },
        "target": {
            "kind": "philote",
            "agent_id": AGENT_ID,
            "target_node": TARGET_NODE
        },
        "security": {
            "auth": {
                "scheme": "bearer_token",
                "grants": [{
                    "token_id": "perplexity",
                    "vault_ref": secret_ref,
                    "scopes": ["context.write"],
                    "allotment": {"max_per_window": 100, "window_secs": 3600}
                }]
            },
            "require_approval": False
        },
        "updated_at": now
    }

    update_resp = ipc.call("update_mcp_routes", {
        "agent_id": AGENT_ID,
        "routes": [route],
        "vault_ref": secret_ref
    }, expect_keys=("mcp_routes_agent_id",))
    print(f"UpdateMcpRoutes: {update_resp}")
    ipc.close()
    if update_resp.get("mcp_routes_agent_id") != AGENT_ID:
        print("ERROR: UpdateMcpRoutes failed", file=sys.stderr)
        sys.exit(1)
    print("\nProvisioned Perplexity/context.capture MCP bearer route")
    print(f"  token_id:  perplexity")
    print(f"  vault_ref: {secret_ref}")


if __name__ == "__main__":
    main()
