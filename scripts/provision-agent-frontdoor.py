#!/usr/bin/env python3
"""
Provision the agent frontdoor MCP endpoint (proposal: agent-frontdoor).

Remote agents (cloud Claude Code, Codex, any MCP client that can send a bearer
header) get the same continuity and coordination tools local clients use, by
name, without native Muninn or intel-graph ever leaving loopback:

  agent --HTTPS+bearer--> membrane-mcp endpoint (this hotel, vps-jane)
        --local CreateTask--> mcp-client-runner (vps-jane) --> muninn-cortex (Cortex :8750)
        --mesh EmitTask-----> mcp-client-runner (mac-jane) --> intel-graph (:8901)

Muninn is served from the Cortex on the frontdoor hotel itself: the Mac Muninn
nodes are cluster observers that reject writes, and keeping Muninn off the Mac
means continuity still works while the Mac sleeps.

Each agent gets its own bearer grant (token_id = agent name). membrane-mcp
presents the caller to mcp-client-runner as `mcp:<token_id>`, so the upstream
registrations (both hotels) must list those principals in `grant_agents`
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

Real argument schemas (slice F5). `tools/list` advertises each upstream's own
`input_schema` instead of a pass-through object when one is known:
  - upstreams registered on THIS hotel are read from GetMcpUpstreams (the
    catalog the local mcp-client-runner reported);
  - upstreams on another hotel (intel-graph on mac-jane) come from a file
    exported there first:
      # on mac-jane
      EXPORT_SCHEMAS=/tmp/frontdoor-schemas.json OWNER_AGENT_ID=agent-bjork-01 \
        PHILOTIC_HOTEL_SOCKET=... python3 scripts/provision-agent-frontdoor.py
      # copy the file to vps-jane, then provision with
      SCHEMA_FILE=/tmp/frontdoor-schemas.json ... python3 scripts/provision-agent-frontdoor.py
  Tools without a known schema keep the pass-through object.
"""

import hashlib
import json
import os
import pathlib
import secrets
import sys
import time

from philotic_ipc import Ipc, IpcError

SOCKET_PATH = os.environ.get("PHILOTIC_HOTEL_SOCKET", "/run/philotic/vps-jane.sock")
ENDPOINT_ID = os.environ.get("ENDPOINT_ID", "agent-frontdoor")
OWNER_AGENT_ID = os.environ.get("OWNER_AGENT_ID", "agent-beacon")
PORT = int(os.environ.get("PORT", "8912"))
EXPOSURE = os.environ.get("EXPOSURE", "mesh")
AGENTS = [a.strip() for a in os.environ.get("AGENTS", "claude-cloud,codex-cloud").split(",") if a.strip()]
MUNINN_UPSTREAM = os.environ.get("MUNINN_UPSTREAM", "muninn-cortex")
# Empty = the upstream lives on this (frontdoor) hotel.
MUNINN_NODE = os.environ.get("MUNINN_NODE", "")
GRAPH_UPSTREAM = os.environ.get("GRAPH_UPSTREAM", "intel-graph")
GRAPH_NODE = os.environ.get("GRAPH_NODE", "mac-jane-aiua-01")
EXPIRES_DAYS = int(os.environ.get("EXPIRES_DAYS", "30"))
CALLS_PER_HOUR = int(os.environ.get("CALLS_PER_HOUR", "300"))
SCHEMA_FILE = os.environ.get("SCHEMA_FILE", "")
EXPORT_SCHEMAS = os.environ.get("EXPORT_SCHEMAS", "")
MAX_REMOTE_DESCRIPTION = 600
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


def upstream_target(upstream_id: str, node: str) -> dict:
    target = {"kind": "mcp_upstream", "upstream_id": upstream_id}
    if node:
        target["target_node"] = node
    return target


PASS_THROUGH_SCHEMA = {"type": "object", "additionalProperties": True}


def catalog_schemas(upstreams_reply: dict) -> dict:
    """Map remote tool name -> {description, input_schema} from a
    GetMcpUpstreams reply (`{"mcp_upstreams": [{config, catalog?}, ...]}`)."""
    schemas = {}
    for entry in upstreams_reply.get("mcp_upstreams") or []:
        catalog = (entry or {}).get("catalog") or {}
        for tool in catalog.get("tools") or []:
            name = tool.get("remote_name")
            schema = tool.get("input_schema")
            if name and isinstance(schema, dict) and schema.get("type") == "object":
                schemas[name] = {
                    "description": str(tool.get("description") or ""),
                    "input_schema": schema,
                }
    return schemas


def load_schema_file(path: str) -> dict:
    data = json.loads(pathlib.Path(path).read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ValueError(f"{path}: expected a JSON object of tool name -> schema")
    return {
        k: v for k, v in data.items()
        if isinstance(v, dict) and isinstance(v.get("input_schema"), dict)
    }


def tool_description(curated: str, remote: dict | None) -> str:
    # The curated line comes first; the upstream's own description (our
    # servers, but still third-party text to the client) is appended, capped.
    if not remote or not remote.get("description"):
        return curated
    extra = remote["description"].strip()
    if len(extra) > MAX_REMOTE_DESCRIPTION:
        extra = extra[:MAX_REMOTE_DESCRIPTION].rstrip() + "…"
    return f"{curated}\n\n{extra}"


def build_tools(grants: list[dict], schemas: dict | None = None) -> list[dict]:
    schemas = schemas or {}
    auth = {"scheme": "bearer_token", "grants": grants}
    tools = []
    for upstream_id, node, catalog in (
        (MUNINN_UPSTREAM, MUNINN_NODE, MUNINN_TOOLS),
        (GRAPH_UPSTREAM, GRAPH_NODE, GRAPH_TOOLS),
    ):
        for name, description in catalog:
            remote = schemas.get(name)
            tools.append(
                {
                    "name": name,
                    "description": tool_description(description, remote),
                    # The upstream validates arguments against its own schema
                    # either way; advertising it just helps the client.
                    "input_schema": remote["input_schema"] if remote else PASS_THROUGH_SCHEMA,
                    "inbound_transform": {
                        "kind": "field_map",
                        "action": name,
                        "target": upstream_target(upstream_id, node),
                        "mappings": [],
                    },
                    "outbound_transform": {"kind": "pass_through"},
                    "auth": auth,
                }
            )
    return tools


def build_config(grants: list[dict], now: int, schemas: dict | None = None) -> dict:
    tools = build_tools(grants, schemas)
    return {
        "endpoint_id": ENDPOINT_ID,
        "owner_agent_id": OWNER_AGENT_ID,
        "port": PORT,
        "path": "/mcp",
        "exposure": EXPOSURE,
        "tools": tools,
        # Remote agents cannot sit through the 300s approval hold: every
        # projected tool is pre-approved by this operator provisioning run.
        # Authority stays with each upstream's allowlist + grant_agents.
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

    file_schemas = load_schema_file(SCHEMA_FILE) if SCHEMA_FILE else {}

    if DRY_RUN:
        grants = [grant(a, f"<vault-ref:{a}>", now) for a in AGENTS]
        print(json.dumps(build_config(grants, now, file_schemas), indent=2))
        return

    if EXPORT_SCHEMAS:
        # Read-only: dump this hotel's upstream catalogs for a provisioning run
        # on another hotel. Needs no blake3 and mints nothing.
        ipc = Ipc(SOCKET_PATH)
        try:
            ipc.register_operator(OWNER_AGENT_ID, "frontdoor-schema-export")
            schemas = catalog_schemas(ipc.call("get_mcp_upstreams", {}, expect_keys=("mcp_upstreams",)))
        except IpcError as exc:
            print(f"ERROR: {exc}", file=sys.stderr)
            sys.exit(1)
        finally:
            ipc.close()
        pathlib.Path(EXPORT_SCHEMAS).write_text(json.dumps(schemas, indent=2), encoding="utf-8")
        print(f"Exported {len(schemas)} tool schemas to {EXPORT_SCHEMAS}: {', '.join(sorted(schemas))}")
        return

    try:
        import blake3 as blake3_lib
    except ImportError:
        print("blake3 not available (install with: python3 -m pip install blake3)", file=sys.stderr)
        sys.exit(1)

    ipc = Ipc(SOCKET_PATH)
    # Owner checks accept `<owner>:<suffix>` guest ids (aiua mcp_owner_identity_ok).
    try:
        ipc.register_operator(OWNER_AGENT_ID, "frontdoor-provisioner")
    except IpcError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        sys.exit(1)

    TOKEN_OUT_DIR.mkdir(parents=True, exist_ok=True)
    os.chmod(TOKEN_OUT_DIR, 0o700)

    grants = []
    for agent in AGENTS:
        raw = secrets.token_urlsafe(32)
        resp = ipc.call(
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

    local = catalog_schemas(ipc.call("get_mcp_upstreams", {}, expect_keys=("mcp_upstreams",)))
    schemas = {**file_schemas, **local}
    advertised = sorted(n for n, _ in MUNINN_TOOLS + GRAPH_TOOLS if n in schemas)
    print(f"  schemas: {len(advertised)} of {len(MUNINN_TOOLS) + len(GRAPH_TOOLS)} tools ({', '.join(advertised) or 'none'})")

    config = build_config(grants, now, schemas)
    resp = ipc.call("provision_mcp_endpoint", {"config": config}, expect_keys=("endpoint_id",))
    ipc.close()
    if not is_endpoint_success(resp):
        print(f"ERROR: ProvisionMcpEndpoint failed: {resp}", file=sys.stderr)
        sys.exit(1)

    print("\nProvisioned agent frontdoor")
    print(f"  endpoint_id:   {ENDPOINT_ID}  port={PORT} exposure={EXPOSURE}")
    print(f"  upstreams:     {MUNINN_UPSTREAM}@{MUNINN_NODE or 'local'}, {GRAPH_UPSTREAM}@{GRAPH_NODE or 'local'}")
    print(f"  tools:         {len(config['tools'])}")
    print(f"  agents:        {', '.join(AGENTS)} (expires in {EXPIRES_DAYS}d)")
    print(f"  tokens:        {TOKEN_OUT_DIR}/<agent>.token  (move to secret store, then delete)")
    print(f"  upstream grant_agents must include: {', '.join('mcp:' + a for a in AGENTS)}")


if __name__ == "__main__":
    main()
