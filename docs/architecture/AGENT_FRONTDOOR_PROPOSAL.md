---
title: Agent Frontdoor — Remote Agents Reach Muninn + Intel-Graph Through membrane-mcp
doc_type: proposal
domain: operator-control-plane
status: in_progress
last_updated: 2026-10-05
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

> **Correction (2026-09-30, after reading `origin/develop`):** this branch was cut
> from a stale `main`. On develop, Muninn runs as a cluster: the **writable Cortex
> is on vps-jane**; mac-jane and mbp-jane run read-only observers that reject
> writes with HTTP 421 (`MUNINN_MEMORY_CORE_PROPOSAL.md`). So the `muninn-local`
> upstream should be registered on **vps-jane itself** (same hotel as the
> frontdoor, no mesh hop); only `intel-graph` stays on mac-jane. Also verify
> whether local Claude Code / Codex `muninn_remember` writes against the Mac
> observer still land. Rebase this branch onto `origin/develop` before any PR.
>
> **Applied (mac-jane session):** Muninn is now the `muninn-cortex` upstream on
> vps-jane; `develop` was merged in (not rebased). Local **Claude**'s `muninn`
> server already targets the Cortex over the tailnet (writes land). Local **Codex**'s
> `muninn` server targets the Mac observer `127.0.0.1:8750`, so Codex writes are
> rejected today, and its Mac-local key is `unauthorized` on the Cortex; it needs
> a Cortex key + URL (follow-up, operator).

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
   │  membrane runtime → mcp-client-runner, agent_id="mcp:<token_id>"
   ├─ muninn_*  target = McpUpstream { upstream_id: muninn-cortex }            (local CreateTask)
   │     vps-jane mcp-client-runner ─► muninn-cortex (http 127.0.0.1:8750/mcp, Cortex, default vault)
   └─ graph_*   target = McpUpstream { upstream_id: intel-graph, target_node: mac-jane-aiua-01 }  (mesh EmitTask)
         mac-jane mcp-client-runner ─► intel-graph (http 127.0.0.1:8901/mcp)
   each runner checks grant_agents ∋ "mcp:<token_id>", tool_allowlist, per-tool allotment
   ▼  datasource_response → back to vps-jane membrane-mcp → HTTP reply
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

  Authority stays with each upstream registration, on the hotel that owns the
  data (vps-jane for the Muninn Cortex, mac-jane for intel-graph).
- **Caller identity is end-to-end.** The frontdoor authenticates the bearer and
  forwards `agent_id = mcp:<token_id>`. A transport extra cannot override it.
  The upstream-side grant check then decides per agent, so revoking one agent is
  one grant removal on either side.
- **Same tool names as upstream.** `scripts/muninn_mcp.py`, the
  `muninn-memory-habit` and `graph-intelligence` skills and the harness text
  work unchanged. Only the transport differs.

### Tool set (v1)

| Upstream | Tools |
|---|---|
| `muninn-cortex` | `muninn_where_left_off`, `muninn_recall`, `muninn_remember`, `muninn_decide` |
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
- `scripts/register-frontdoor-upstreams.py` (`UPSTREAMS=…`), with fail-closed
  allowlists and `grant_agents = ["mcp:claude-cloud", "mcp:codex-cloud", …]`:
  - on vps-jane: `muninn-cortex`, an HTTP upstream to the Cortex at
    `127.0.0.1:8750/mcp`, with the `default`-vault Cortex bearer stored via
    `provision_mcp_upstream_credential`;
  - on mac-jane: `intel-graph`, an HTTP upstream to `127.0.0.1:8901/mcp`
    (no token; loopback bind).
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

## Reality gaps found on mac-jane (2026-09-30)

The cloud session drafted S1-S3 without tailnet access. Checking against the
live Mac changed the design in one place and fixed two script bugs:

- **The Mac Muninn is a cluster observer and rejects writes.** `muninn mcp` on
  mac-jane reads fine under the client's scrubbed env (only `HOME` is needed;
  it defaults to `127.0.0.1:8750`, no token), but `muninn_remember` returns
  `-32002 … writes are accepted only on the Cortex`. Local Claude does not use
  the Mac listener for Muninn at all: its `muninn` server is the vps Cortex
  over the tailnet. So the Muninn upstream moved to **`muninn-cortex` on
  vps-jane** (HTTP to the Cortex's loopback listener). Side benefit: Muninn
  continuity no longer depends on the Mac being awake; only the `graph_*`
  tools do.
- **`register_guest` is not an IPC op** (it is `register`). The drafted
  scripts, copied from `provision-lifegraph-mcp.py` / `provision-mcp-bearer.py`,
  printed the error and carried on unidentified (see DEF-209).
- **The hotel pushes an unsolicited frame after connect** (a blob-endpoint
  advert), so a one-frame-per-call reader returns every reply one op late and
  a refused registration looks green. Both frontdoor scripts now read until
  the op's own reply arrives.
- Confirmed as assumed: mac-jane's node id is `mac-jane-aiua-01`;
  `mcp-client-runner` runs there; intel-graph (`:8901`) has no token;
  the Cortex bearer local Claude uses resolves to the `default` vault.
- Upstream owners: `agent-bjork-01` (intel-graph on mac-jane),
  `agent-beacon` (muninn-cortex and the endpoint on vps-jane).

## Next slices (2026-10-05)

S1–S3, S5 and S6 are live (public UAT 2026-10-01: 16 tools, round trips
single-wrapped, grant revocation immediate). The work below closes what the
2026-10-01 handoff (`docs/HANDOFF-2026-10-01-agent-frontdoor-cloud.md`) left
open. Each slice is small and can be done from a cloud session up to the
deploy step.

### F1 — Task expiry for frontdoor dispatches: rest of DEF-211 (S–M)

**Problem.**
- Inside the 15 s peer TTL, an `execute_tool` EmitTask to a sleeping mac-jane
  is queued with `created_at: 0, expires_at: None` (`aiua/src/service/ipc.rs:6807-6822`).
- The caller sees a 30 s timeout (`membrane-mcp/src/server.rs:34`), and the
  task can still run when the Mac wakes. A retried `session_start` or
  `graph_decide` may then apply twice.

**Plan.**
- This is slice L5 of `MESH_DELIVERY_GUARANTEES_PROPOSAL.md`, scoped down:
  1. stamp `created_at` and `expires_at` (ms) = now + 25 s on `execute_tool`
     EmitTasks;
  2. the mesh dispatcher (`mesh_dispatcher.rs:261-356`) skips expired events
     and answers the local originator with a `TARGET_NODE_UNREACHABLE`-style
     error, so membrane-mcp returns a clear error before its own 30 s
     timeout;
  3. the receiver refuses expired envelopes as well.
- If L4's dead-letter table isn't there yet, log plus a heal tag
  `mesh_task_expired` is enough for F1.

**Tests.**
- Dispatcher unit test: an expired event is never sent and produces the
  error reply.
- `ipc.rs`: `execute_tool` envelopes carry `expires_at`.

**Live check.**
1. With mac-jane asleep, `graph_status` via `/agent/mcp` errors in ≤ 25 s.
2. After waking the Mac, nothing runs late (the intel-graph session log is
   unchanged).

### F2 — Fix the two older provisioning scripts: DEF-209 (S)

Port the `register` op plus the read-until-reply framing, already used by
`scripts/provision-agent-frontdoor.py`, to these two scripts. Use an
owner-prefixed guest id (`<owner>:mcp-provisioner`) and role `operator`
(not `hotel.internal`, which `PERIMETER_ENFORCEMENT_PROPOSAL.md` P2 will
reserve).
- `scripts/provision-lifegraph-mcp.py`
- `scripts/provision-mcp-bearer.py`

Factor the shared framing into `scripts/philotic_ipc.py` so the four
provisioning scripts stop drifting.

**Verification.**
- `DRY_RUN`-style unit test of the framing against a fake socket that pushes
  an unsolicited frame first.
- Re-run `provision-lifegraph-mcp.py` on vps-jane against the existing
  endpoint (idempotent), then `mcp-client-uat.sh lifegraph-recall`.

### F3 — Server-side caller tag on Muninn writes (S)

**Where.** `membrane-mcp-client` already receives the authenticated principal
(`agent_id = mcp:<token_id>`, `crates/membrane-mcp-client/src/main.rs:199`).

**Plan.**
- Add an optional per-tool `inject_caller_tags: bool` to `McpUpstreamToolGrant`
  (`ansible-mesh-core/src/mcp_upstream.rs`, `#[serde(default)]`).
- When set, `execute_call` appends `["remote-agent", "<token_id>"]` to
  `arguments.tags` (creating the array if absent) before calling upstream.
- Enable it for `muninn_remember` and `muninn_decide` on the `muninn-cortex`
  registration.

**Tests.**
- Tags are appended, not replaced.
- Absent `tags` creates the array.
- Tools without the flag are untouched.

**Live check.** `muninn_remember` via the frontdoor, then `muninn_recall` with
`tags_any: ["claude-cloud"]` returns it.

### F4 — Local Codex on mac-jane writes to the Cortex (S, ops)

Local Codex points at the Mac Muninn observer, which rejects writes with
`-32002`. To fix it:
- give the Codex harness a Cortex bearer (operator Keychain) and the tailnet
  URL of the vps-jane native Muninn MCP, through an SSH tunnel per
  `MUNINN_DIRECT_CLIENT_ACCESS.md`;
- or point it at the frontdoor with a `codex-local` grant (simpler, and the
  same tools).

The decision is operator-side. Recommendation: the frontdoor grant. It gives
one path and one credential class for every agent.

**Verification.** A Codex `muninn_remember` from mac-jane appears on the
Cortex.

### F5 — Real argument schemas in `tools/list` (S–M)

v1 advertises `{"type":"object"}`. This slice:
- extends `scripts/provision-agent-frontdoor.py` to fetch each upstream's
  projected catalog (`GetMcpUpstreams` returns the reported
  `McpUpstreamCatalog`) and copy `input_schema` and `description` into the
  endpoint tool specs;
- runs it from vps-jane for `muninn-cortex`, and over the mesh query for
  `intel-graph`'s catalog on mac-jane.

**Verification.** `tools/list` shows real `properties` for `muninn_recall`
and `graph_context_for`.

### F6 — Token rotation drill and expiry alarm (S, ops)

- **Expiry.** Grants expire around 2026-10-30. Add a `phil doctor` check
  `mcp.grant-expiry` that warns 7 days before any endpoint grant's
  `expires_at`.
- **Rotation drill** before the 10-30 expiry:
  1. rerun `provision-agent-frontdoor.py` (rotates every agent);
  2. update the cloud environment variables and the Keychain;
  3. run `mcp-client-uat.sh agent-frontdoor`;
  4. confirm the old token is refused.
- Record the drill per `MCP_CREDENTIAL_LIFECYCLE.md`.

### F7 — Housekeeping (S)

- Remove the stale unchecked "S1 … not yet compiled" line in `docs/task.md`,
  which the checked S1 line below it supersedes.
- Move this proposal to `implemented`, with per-slice SVER levels, once F1
  and F6 are done.

**Ordering:** F2, F3, F5 and F7 are independent and cloud-doable. F1 rides
with mesh L5 or ships standalone. F4 and F6 need the operator.

## Open questions

- Should frontdoor writes land in `default`, or in a `remote_agents` vault that
  local recall also reads? The v1 answer is `default`, per decision 4.
- Should `graph_*` writes from cloud agents (`session_start`, `graph_decide`) be
  distinguishable in the graph dashboard? The `agent` argument already carries
  the agent name.
- Per-tool schemas: v1 advertises `{"type":"object"}` pass-through schemas.
  A later slice can copy the Mac's projected catalog (`ReportMcpUpstreamCatalog`)
  into the endpoint config so clients see real argument schemas.
