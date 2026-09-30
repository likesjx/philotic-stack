---
title: Agent Frontdoor — Remote Agents Reach Muninn + Intel-Graph Through membrane-mcp
doc_type: proposal
domain: operator-control-plane
status: in_progress
last_updated: 2026-09-30
tags:
- mcp
- membrane-mcp
- membrane-mcp-client
- muninn
- intel-graph
- remote-agents
- claude-code
- codex
- frontdoor
related_docs:
- MCP_CLIENT_FABRIC_PROPOSAL.md
- MCP_COORDINATION_ENDPOINT_PROPOSAL.md
- MCP_MEMBRANE_HARDENING_PROPOSAL.md
- KNOWLEDGE_ARCHITECTURE_PROPOSAL.md
- MUNINN_V07_CAPABILITY_ADOPTION_PROPOSAL.md
- ../reference/MUNINN_DIRECT_CLIENT_ACCESS.md
- ../reference/MCP_CREDENTIAL_LIFECYCLE.md
proposal_id: agent-frontdoor
implements:
- membrane-mcp
- membrane-mcp-client
implemented_by: []
active_seams:
- mcp-upstream-route-target
- agent-frontdoor-endpoint
- remote-muninn-bootstrap
- frontdoor-proxy-loopback-trust
---

# Agent Frontdoor — Remote Agents Reach Muninn + Intel-Graph Through membrane-mcp

## Problem

Local Claude Code and Codex on the Mac follow the session protocol in
`AGENTS.md` / `CLAUDE.md`. They bootstrap Muninn, recall the triad, claim work
in intel-graph and write back decisions. Every one of those surfaces is
loopback-only on the Mac:

| Surface | Where | How local clients reach it |
|---|---|---|
| Muninn continuity (`default` vault) | Mac, `127.0.0.1:8750` MCP | `muninn mcp` stdio proxy / `scripts/muninn_mcp.py` |
| Intel-graph | Mac, `127.0.0.1:8900`/`8901` (launchd) | `.mcp.json` `intel-graph` (HTTP) |

Cloud agents cannot reach either. This covers Claude Code on the web, Codex
cloud, and any other hosted MCP client ("Muse"-style agents). They have no
tailnet, no SSH key, no `muninn` binary, and a network allowlist. The session
gate `muninn_mcp.py bootstrap` therefore always exits 42 for them, so every
cloud session starts blocked or runs without continuity.

The only public surface is the Philotic MCP frontdoor on vps-jane
(`https://mcp.jaredlikes.com`, served by `membrane-mcp`). Today it exposes just
three tools, none of which are session tools:

- `context.capture`: Perplexity, write-only, into `self_agent-beacon-01`
- `life.recall`
- `life.observe` (opt-in)

## Decisions (operator, 2026-09-30)

1. **External agents go through the frontdoor and get specific tools.** They
   get no native Muninn access, no tunnels and no public intel-graph.
2. **Intel-graph and the continuity Muninn stay on the Mac (mac-jane).** No
   second graph and no vps-jane replica.
3. **"Muse" is a placeholder for any cloud agent.** The design is
   client-agnostic: anything that can send `Authorization: Bearer` to a
   Streamable-HTTP MCP URL.
4. **Remote agents should share continuity with local Claude and Codex.**
   They recall and write the same Muninn memory (`default` vault) using the
   same four tools, so skills and habits are identical across local and cloud.

## Design

```
cloud agent (Claude Code web / Codex cloud / any MCP client)
   │  HTTPS, Authorization: Bearer <per-agent token>
   ▼
mcp.jaredlikes.com/agent/mcp ──TLS proxy──► vps-jane membrane-mcp endpoint "agent-frontdoor" (:8912)
   │  per-agent grants (token_id = claude-cloud / codex-cloud / …), 30-day expiry, allotments
   │  tools named exactly as upstream: muninn_* and graph_*/session_*
   │  every tool pre-approved at provisioning (no 300 s approval hold)
   ▼  target = McpUpstream { upstream_id, target_node: mac-jane-aiua-01 }
   │  membrane runtime → EmitTask(role=mcp-client-runner, agent_id="mcp:<token_id>")
   ▼  mesh (tailnet)
mac-jane mcp-client-runner  (existing MCP client fabric)
   │  grant_agents ∋ "mcp:<token_id>", tool_allowlist, per-tool allotment
   ├─► muninn-local  (stdio `muninn mcp` → 127.0.0.1:8750, default vault)
   └─► intel-graph   (http 127.0.0.1:8901/mcp)
   ▼  datasource_response → EmitTask back to vps-jane membrane-mcp → HTTP reply
```

### Why this shape

- **Native Muninn and intel-graph never leave loopback.** This keeps the
  standing rules in `MUNINN_DIRECT_CLIENT_ACCESS.md`: no public `:8750` and no
  broad public bearer to native Muninn.
- **It reuses the MCP client fabric instead of building a new proxy.**
  `membrane-mcp-client` already speaks MCP to exactly these two servers. It
  already enforces:
  - a fail-closed tool allowlist
  - per-tool allotments
  - response-size caps
  - a stdio command allowlist
  - owner/`grant_agents` authorization

  Authority stays with the upstream registration on the Mac, which is the
  hotel that owns the data.
- **Caller identity is end-to-end.** The frontdoor authenticates the bearer and
  forwards `agent_id = mcp:<token_id>`. A transport extra cannot override it.
  The Mac-side grant check then decides per agent, so revoking one agent is
  one grant removal on either side.
- **Same tool names as upstream.** `scripts/muninn_mcp.py`, the
  `muninn-memory-habit` and `graph-intelligence` skills and the harness text
  work unchanged. Only the transport differs.

### Tool set (v1)

| Upstream | Tools |
|---|---|
| `muninn-local` | `muninn_where_left_off`, `muninn_recall`, `muninn_remember`, `muninn_decide` |
| `intel-graph` | `graph_status`, `graph_digest`, `graph_next_task`, `graph_context_for`, `graph_impact`, `graph_search`, `graph_agent_dashboard`, `session_start`, `session_activity`, `session_close`, `graph_decide`, `graph_record_test_run` |

Deliberately excluded:

- graph maintenance tools (`graph_scan`, `graph_embed*`, `graph_writeback`,
  `graph_create_*`, `graph_update_node`, exports)
- Muninn admin, vault or batch tools
- anything LifeGraph (it keeps its own endpoint and grant class)

## Slices

### S1 — `McpUpstream` route target (code, this branch)

- `ansible-mesh-core::mcp_route::McpRouteTarget::McpUpstream { upstream_id, target_node }`
  plus `mcp_caller_principal(token_id) -> "mcp:<token_id>"`.
- `membrane-mcp` transform carries `target_node`. The endpoint-config dispatch
  puts it in `raw_transport.target_node`, and the membrane runtime already turns
  that into a cross-hotel `EmitTask`. This lifts the "config path always
  dispatches locally" limit for upstream targets only; `Philote` config targets
  are unchanged.
- The legacy route path rejects `McpUpstream`, after auth: legacy routes have no
  action mapping to name the remote tool.
- `membrane` runtime:
  - `target_kind = "mcp_upstream"` routes to role `mcp-client-runner`;
  - the payload is the datasource `execute_tool` shape with
    `tool_name = mcp:<upstream_id>.<action>` and `agent_id = mcp:<token_id>`;
  - `agent_id` is re-stamped after transport extras merge, so it can't be
    spoofed.
- The philote-facing `mcp.provision` schema is **not** extended. Exposing an
  upstream publicly is an operator provisioning act, not something an agent
  self-serves.
- Unit tests:
  - wire-shape round trip;
  - transform `target_node`;
  - runtime `EmitTask` to the pinned node, projected name, anti-spoof `agent_id`;
  - local fallback.

**Verification gate:** `cargo test -p ansible-mesh-core -p membrane-mcp -p membrane`.
It could not run in the authoring (cloud) session because `static.crates.io`
is not on that environment's allowlist. It is the first step of the mac-jane
handoff.

### S2 — Remote bootstrap in `scripts/muninn_mcp.py` (code, this branch)

- When `PHILOTIC_FRONTDOOR_URL` (or `--frontdoor-url`) is set, the helper uses a
  Streamable-HTTP client:
  - every message is a single `POST`;
  - `Mcp-Session-Id` is honored if the server sends one;
  - SSE response bodies are parsed.
- The token comes from `PHILOTIC_AGENT_MCP_TOKEN` (or `…_TOKEN_FILE`). It is a
  distinct credential from `MUNINN_MCP_TOKEN`.
- `bootstrap` never tries to start a local daemon in frontdoor mode, and still
  exits 42 on any failure, so the operator-approval rule is unchanged.
- Verified against a mock frontdoor:
  - ready → 0
  - bad token → 42
  - missing tools → 42
  - native path unchanged
  - SSE body parsed

### S3 — Provision the endpoint (ops: mac-jane + vps-jane)

- `scripts/provision-agent-frontdoor.py` mints one bearer per agent and stores
  only BLAKE3 hashes in the hotel vault. Raw tokens go to 0600 files, never
  stdout. It then provisions `agent-frontdoor` (default port 8912,
  exposure `mesh`) with every tool pre-approved. `DRY_RUN=1` prints the config.
- On mac-jane:
  - register `muninn-local` as a stdio upstream;
  - register `intel-graph` as an HTTP upstream;
  - add allowlists and `grant_agents = ["mcp:claude-cloud", "mcp:codex-cloud", …]`.
  - See `docs/HANDOFF-2026-09-30-agent-frontdoor-mac-jane.md`.
- Out-of-repo TLS proxy: add `mcp.jaredlikes.com/agent/mcp → 127.0.0.1:8912/mcp`
  with a path rewrite, mirroring `/lifegraph/mcp`.

### S4 — Hardening that must land before S3 is exposed

1. **Proxy loopback trust (`frontdoor-proxy-loopback-trust`).** `membrane-mcp`
   derives `is_loopback` from the TCP peer. If the TLS proxy runs on vps-jane
   and connects over `127.0.0.1`, every public request looks like loopback.
   Then the Mesh-tier fence passes without a header, and any
   `McpAuthScheme::None` tool is callable from the internet. The agent
   frontdoor itself is all-bearer, so it is not directly exposed. The fix is
   still a precondition for widening the frontdoor, via either:
   - treat any request carrying `X-Forwarded-For` / `Forwarded` as
     non-loopback; or
   - bind the proxy upstream to a non-loopback interface.
2. **Stamp caller identity on writes.** Memories written through the frontdoor
   should carry a client tag (`claude-cloud`, …) so recall can distinguish them.
   Option: a per-tool "inject caller tag" setting in `mcp-client-runner` for
   `muninn_remember` / `muninn_decide`. Until then, the skill already asks
   clients to tag `claude` / `codex`.
3. **Mac availability.** vps-jane is the only always-on hotel. When mac-jane is
   asleep, calls fail after the 30 s dispatch timeout. Target behavior: a fast,
   explicit "upstream hotel unavailable" error, so the helper reports it as a
   Muninn blocker rather than a hang.
4. **Response shape check.** `mcp-client-runner` returns the upstream MCP
   `result`, and membrane-mcp wraps the dispatch outcome. Verify end-to-end that
   clients see `{content:[…]}` once, not nested. Fix in the outbound transform
   if needed.

### S5 — Client config (per cloud environment)

- **Claude Code (cloud):**
  - environment network allowlist: `mcp.jaredlikes.com`;
  - environment variables: `PHILOTIC_AGENT_MCP_TOKEN` and
    `PHILOTIC_FRONTDOOR_URL=https://mcp.jaredlikes.com/agent/mcp`;
  - `.mcp.json` server entry:

    ```json
    "philotic": {
      "type": "http",
      "url": "https://mcp.jaredlikes.com/agent/mcp",
      "headers": { "Authorization": "Bearer ${PHILOTIC_AGENT_MCP_TOKEN}" }
    }
    ```

  Add that entry only once S3 is live, because the Mac config would otherwise
  show a failed server.
- **Codex:**

  ```toml
  [mcp_servers.philotic]
  url = "https://mcp.jaredlikes.com/agent/mcp"
  bearer_token_env_var = "PHILOTIC_AGENT_MCP_TOKEN"
  ```

  Add the same domain allowlist. To check: Codex cloud may expose *secrets*
  only to setup scripts, so the token may need to be a plain environment
  variable.
- **Claude.ai custom connectors / ChatGPT connectors** need OAuth
  (`/.well-known/oauth-protected-resource`, dynamic client registration,
  `WWW-Authenticate` on 401), which membrane-mcp does not implement. Deferred;
  the bearer-header path covers Claude Code and Codex.
- **Session protocol:** `CLAUDE.md` / `AGENTS.md` bootstrap stays
  `python3 scripts/muninn_mcp.py bootstrap`, which picks frontdoor mode from the
  environment. Graph orientation uses the frontdoor `graph_*` tools. `just`,
  `idea-sweep` and `harness-drift` are local-only and are skipped remotely.

### S6 — Credential class

Add `remote-agent-frontdoor` to `MCP_CREDENTIAL_LIFECYCLE.md`:

| Field | Value |
|---|---|
| Scope | four `muninn_*` plus v1 `graph_*`/`session_*` tools |
| Expiry | 30 days |
| Grants | one per agent |
| Storage | hotel vault hash, raw token in the operator secret store, cloud environment variable |
| UAT | `tools/list` shows exactly the v1 set; a `muninn_recall` round trip succeeds; a revoked token fails `tools/list`; a non-granted principal is refused by `mcp-client-runner` |

## Open questions

- Should frontdoor writes land in `default`, or in a `remote_agents` vault that
  local recall also reads? The v1 answer is `default`, per decision 4.
- Should `graph_*` writes from cloud agents (`session_start`, `graph_decide`) be
  distinguishable in the graph dashboard? The `agent` argument already carries
  the agent name.
- Per-tool schemas: v1 advertises `{"type":"object"}` pass-through schemas.
  A later slice can copy the Mac's projected catalog (`ReportMcpUpstreamCatalog`)
  into the endpoint config so clients see real argument schemas.
