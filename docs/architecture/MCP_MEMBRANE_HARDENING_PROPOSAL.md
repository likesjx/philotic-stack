---
title: MCP Membrane Hardening — Perimeter-True, Authenticated, Identity-Bound
doc_type: proposal
domain: membrane-transport
status: proposed
disposition: proposed
last_updated: 2026-07-15
tags:
  - mcp
  - membrane-mcp
  - security
  - perimeter
  - vault
  - approval
  - hardening
proposal_id: mcp-membrane-hardening
implements:
  - mcp-membrane-gateway
implemented_by: []
active_seams:
  - mcp-perimeter-bind
  - mcp-route-publication-gate
  - mcp-authenticated-provisioning
  - mcp-ipc-identity
  - mcp-protocol-conformance
  - mcp-dispatch-test-harness
related_docs:
  - MCP_MEMBRANE_GATEWAY_PROPOSAL.md
  - MCP_COORDINATION_ENDPOINT_PROPOSAL.md
  - MCP_CLIENT_FABRIC_PROPOSAL.md
  - OPERATOR_IDENTITY_AND_DANGEROUS_ACTION_CEREMONIES_PROPOSAL.md
source_of_truth_targets:
  - docs/architecture/MCP_MEMBRANE_HARDENING_PROPOSAL.md
---

# MCP Membrane Hardening — Perimeter-True, Authenticated, Identity-Bound

## Problem

The 2026-07-15 MCP audit (provider + consumer + philote-capability passes) found
the `membrane-mcp` gateway functionally live and philote-provisionable, but with
a security posture that does not match its own design language. The gateway
proposal's model — exposure tiers, vault-backed tokens, provisioning-as-
authorization — is only partially enforced by the code:

- The listener **always binds `0.0.0.0`** regardless of `ExposureTier`
  (`crates/membrane-mcp/src/main.rs:161`, `:521`), while the fence comment
  claims "Local: loopback-only listener" (`crates/perimeter-core/src/fence.rs:27`).
  A `Local` endpoint is LAN-reachable.
- Every philote **auto-publishes its entire `default_toolset`** as MCP routes
  with `auth: McpAuthScheme::None` on every startup
  (`crates/philote/src/runtime.rs:1855-1877` `mcp_routes_from_profile`,
  `:1884` `register_mcp_routes`) via the doc-deprecated `UpdateMcpRoutes` path
  that is still fully live.
- A philote **cannot provision an authenticated endpoint**: the `mcp.provision`
  catalog schema (`crates/philote/src/catalog.rs:2707-2817`) has no `auth`
  field, and no philote tool can write a token hash into the vault. Everything
  a philote provisions is `McpAuthScheme::None`.
- The IPC trust boundary is the socket, not guest identity:
  `ProvisionMcpEndpoint` trusts a self-asserted `owner_agent_id`
  (`crates/philote/src/tool_exec.rs:5166`, handler
  `crates/aiua/src/service/ipc.rs:7829-8048`), and the generic `SetConfig`
  handler writes any key — including `__mcp_endpoint__:*` / `__mcp_routes__` —
  with no ACL (`ipc.rs:3995-4001`).
- Assorted protocol and correctness gaps undermine trust in what external
  clients see (errors returned as success, dead auth scheme, dropped streaming,
  zero tests on the dispatch surface).

This proposal is the closure plan: make the implemented gateway match the
promised security model, end to end, with tests that prove it.

## Full Gap Inventory (audit 2026-07-15)

### A — Security

| ID | Gap | Evidence | Severity |
|---|---|---|---|
| A1 | Listener binds `0.0.0.0` unconditionally; `Local`/`Lan` tiers have no listener-level auth, so "Local" endpoints are LAN-reachable. Fence docs claim loopback. | `membrane-mcp/src/main.rs:161,521`; `perimeter-core/src/fence.rs:27-37` | high (DEF-053) |
| A2 | Philote startup pushes its full `default_toolset` as auth-`None` `McpRouteRecord`s every boot, via the deprecated-but-live `UpdateMcpRoutes` path. Served whenever any membrane-mcp guest runs. | `philote/src/runtime.rs:1855-1911`; `aiua/src/service/ipc.rs:7738-7780` | high (DEF-055) |
| A3 | No authenticated self-provisioning: `mcp.provision` schema omits `auth`; no philote tool wraps `AddVaultEntry`/`RotateSecret`; `McpEndpointConfig` has no endpoint-wide auth default. | `philote/src/catalog.rs:2731-2784`; `membrane-mcp/src/server.rs:277` | high |
| A4 | `McpAuthScheme::HmacSha256` is provisionable but dead: filtered out of `tools/list`, rejected on `tools/call`. Valid config → invisible, uncallable tool. | `mcp_route.rs:79-80`; `server.rs:214,235`; `auth.rs:300-303` | medium (DEF-054) |
| A5 | IPC identity: `owner_agent_id` self-asserted on provision; generic `SetConfig` writes `__mcp_*` keys with no ACL; only `RevokeMcpEndpoint` checks ownership. | `tool_exec.rs:5166`; `ipc.rs:3995-4001,7829-8077` | medium |
| A6 | Approval asymmetry: config path skips approval for loopback callers (`requires_approval = !preapproved && !is_loopback`); legacy path honors `require_approval` regardless. | `server.rs:305` vs `:427` | medium |
| A7 | `GET /mcp` SSE handler runs before the ingress fence — unauthenticated handler on the public bind (keepalive only today, but a footgun as SSE grows). | `server.rs:87-103` | low |
| A8 | Rate allotments are in-memory and reset on membrane restart; `tools/list` triggers unmetered per-tool vault verification with no upstream rate limit. | `mcp_route.rs:101`; `auth.rs:118-151,191-200` | low |

### B — Protocol / correctness

| ID | Gap | Evidence | Severity |
|---|---|---|---|
| B1 | Philote business errors returned as **successful** MCP results (no `isError`); `ToolCallResult::error()` exists, never called. Dispatch timeouts do return JSON-RPC errors → inconsistent semantics. | `main.rs:277-279`; `server.rs:374-381,500-506`; `protocol.rs:166` | medium (DEF-056) |
| B2 | `StreamingToken` replies silently dropped ("not yet implemented"). | `main.rs:261-264` | medium |
| B3 | Error-code constants (`ALLOTMENT_EXCEEDED`, `TOKEN_EXPIRED`, `APPROVAL_REQUIRED`, …) defined but never returned; exhaustion surfaces as generic `PERMISSION_DENIED`. | `protocol.rs:63-75`; `server.rs:290` | low |
| B4 | Protocol version hard-coded `2024-11-05`, client's requested version ignored; `initialized` matched by the wrong name (spec: `notifications/initialized`); no `Mcp-Session-Id`; SSE is a keepalive stub; no `tools/list` pagination. | `server.rs:87,142-151,168-186` | low |
| B5 | `Template` inbound/outbound transforms are Phase-4 placeholders that error at runtime. | `mcp_endpoint.rs:83-85,111-112`; `transform.rs:41,127` | low |
| B6 | Static-only mode (`--ipc-socket` absent) serves a listener whose every `tools/call` times out after 30s. | `main.rs:517-528` | info |

### C — Architecture / ops / hygiene

| ID | Gap | Evidence | Severity |
|---|---|---|---|
| C1 | Zero tests on the entire HTTP/JSON-RPC dispatch surface (`server.rs`, `routing.rs`, `main.rs`); no end-to-end `tools/call` test besides the bash UAT script. | `crates/membrane-mcp` | high |
| C2 | Two half-merged operating modes: legacy route-table vs config-driven endpoints. The default `{hotel}:membrane-mcp` guest (`MCP_MEMBRANE_REQUIRED`) never receives endpoint configs — legacy mode only; per-endpoint guests never serve legacy routes. | `main.rs:204`; `ipc.rs:7924`; `aiua/src/main.rs:2511-2541` | medium |
| C3 | Dead/stale code: `dispatch.rs` unused (tracked in DEFECTS tech debt), unused `tower-http` cors feature, stale "Slice 1/2" comments describing already-implemented stubs, `routing.rs` helpers unused. | `dispatch.rs`; `Cargo.toml:20`; `auth.rs:158-160` | low |
| C4 | Doc drift: `LIFE_GRAPH_OS_PROPOSAL.md:128` references a nonexistent "Muninn MCP client pattern" (the real pattern is REST); gateway proposal calls `UpdateMcpRoutes` deprecated while it remains the only auto-publication path. | docs | low |
| C5 | No operator CLI surface for MCP (list/inspect/revoke endpoints & routes) — everything is tool/IPC-driven; auditability depends on graph spelunking. | `aiua/src/main.rs:454` | low |
| C6 | Cross-hotel routing unresolved: config path collapses targets to local (`target_node: None`); the gateway proposal's open question was never answered. | `server.rs:352` | info |

### D — Consumer direction

Covered by the companion proposal `MCP_CLIENT_FABRIC_PROPOSAL.md`
(proposal_id `mcp-client-fabric`): **no MCP client machinery exists anywhere in
the stack** — no philote tool, no IPC variant, no outbound transport, no
credential model. Net-new work, deliberately separated from this hardening
proposal so the security closure is not blocked on new capability.

## Vision

An operator can say, truthfully: *"Nothing listens beyond the perimeter tier I
approved, nothing is callable without the credential class I approved, every
integration point was created by an identified agent through an audited
approval, and the test suite proves all of it."*

Design principles:

1. **Perimeter-true by construction** — the bind address derives from the
   exposure tier; a tier the listener cannot honor is a provisioning error,
   not a comment.
2. **Nothing implicit** — no route exists that a provisioning decision (with
   identity and approval attached) did not create. Startup auto-publication
   dies or becomes opt-in-per-agent with `require_approval` fail-closed.
3. **Secrets only via the vault, self-service included** — a philote can mint
   and rotate endpoint credentials through a governed tool without ever seeing
   stored hashes.
4. **Identity at the IPC boundary** — the hotel knows which guest is asking;
   owner assertions are verified, and `__mcp_*` config keys are writable only
   through their dedicated handlers.
5. **One dispatch mode** — the endpoint-config model is the only model; the
   legacy route table is retired, not coexisting.
6. **Honest protocol** — errors are errors (`isError`), codes are specific,
   the advertised protocol version is negotiated, and conformance is tested.

## Design

### S1 — Perimeter-true binding (closes A1, A7)

- `McpEndpointConfig.exposure` drives the socket bind:
  `Local → 127.0.0.1`, `Lan/Mesh → 0.0.0.0` (Mesh may later prefer the
  tailnet address), `Internet → 0.0.0.0` + mandatory bearer.
- Perimeter shifts that would *widen* the effective audience re-bind only
  after re-validation against the endpoint's declared tier; narrowing rebinds
  immediately. `update_perimeter` push handling gains a bind-reconcile step.
- The ingress fence runs for **every** route including `GET /mcp` and
  `/health` (health may stay open on loopback only).
- Fix the `fence.rs` tier documentation to describe what is enforced.

### S2 — Route publication gate (closes A2, C2; retires legacy mode)

- `register_mcp_routes` startup auto-publication becomes **opt-in per agent**
  via an explicit config field (`mcp_auto_publish: bool`, default `false`) on
  the agent profile; when enabled, published routes carry
  `require_approval = true` and `auth != None` is mandatory outside loopback.
- Migration: one release with the flag honored + a startup warning when routes
  exist without it; then delete `UpdateMcpRoutes`/`RevokeMcpRoutes`/
  `GetMcpRoutes` and the `SharedRoutingTable` legacy dispatch arm
  (`server.rs:397-517`).
- The default `{hotel}:membrane-mcp` guest either learns to serve endpoint
  configs (subscribe to `update_mcp_config` fan-out for endpoints pinned to
  its port) or is removed in favor of per-endpoint guests only. Decision
  point: keep `MCP_MEMBRANE_REQUIRED` as "materialize a default endpoint from
  config" rather than "run an empty legacy guest".

### S3 — Authenticated self-provisioning (closes A3, A4)

- Add `auth` to the `mcp.provision` schema (per-tool and endpoint-default),
  accepting `bearer` initially. Provisioning a non-loopback endpoint with
  `auth: none` is rejected unless the operator approval explicitly carries an
  `allow_unauthenticated` acknowledgment (surfaced in the approval prompt).
- New philote tool **`mcp.grant_token`** (class `config`, approval-gated):
  generates a token, writes `BLAKE3(token)` to the vault under a
  `mcp_endpoint_token` secret-kind ref, attaches the grant to the named
  endpoint/tool, and returns the raw token **once** in the tool result with a
  storage warning. Companion `mcp.rotate_token` / `mcp.revoke_token` wrap
  rotation and revocation. (Vault writes go through a new narrow IPC —
  `ProvisionMcpTokenGrant` — not the general `AddVaultEntry`.)
- `HmacSha256`: **remove** from `McpAuthScheme` (serde alias kept for one
  release, mapped to a provisioning error) unless a concrete consumer appears;
  a config that cannot authenticate anyone must not be representable.

### S4 — IPC identity and config-key ACL (closes A5, A6)

- The hotel already knows the guest identity behind each IPC connection
  (guest registration/lease); `ProvisionMcpEndpoint`, `RevokeMcpEndpoint`,
  and `ProvisionMcpTokenGrant` verify `owner_agent_id` against that identity
  and reject mismatches (`FORBIDDEN`), with an override only for
  operator/admin-class callers.
- Reserve the `__mcp_` config-key prefix: the generic `SetConfig` handler
  rejects writes to reserved prefixes; MCP state changes flow only through
  their dedicated, validated handlers.
- Unify approval semantics across dispatch: loopback no longer bypasses
  approval; pre-approval rules are the only bypass, matching the gateway
  proposal's "provisioning turn is the authorization event".

### S5 — Protocol conformance and honest errors (closes B1–B4, B6)

- Business errors → `ToolCallResult::error()` with `isError: true`; map
  allotment exhaustion to `ALLOTMENT_EXCEEDED`, parked-approval timeout to
  `APPROVAL_REQUIRED`, auth failures to their specific codes.
- Streaming: accumulate `StreamingToken` frames into the final result (true
  streaming over SSE deferred; silent drop eliminated).
- Negotiate protocol version (echo client's if supported, else advertise
  ours), match `notifications/initialized` by its spec name, document
  resources/prompts as intentionally unimplemented in `initialize`
  capabilities.
- Static-only mode refuses to serve `tools/call` (immediate
  `METHOD_NOT_FOUND`-class error) instead of timing out.

### S6 — Dispatch test harness (closes C1; regression net for S1–S5)

- In-crate axum integration tests: `initialize`/`tools/list`/`tools/call`
  against an in-memory runtime stub — covering fence tiers × auth schemes ×
  approval states × both bind modes, the oneshot correlation path, and error
  mapping.
- A `just` smoke target wrapping `scripts/mcp-client-uat.sh` against a
  provisioned loopback endpoint, recorded to the graph via the standard
  test-run path.
- Hygiene sweep rides along: delete `dispatch.rs`, unused cors feature,
  stale slice comments; fix the `LIFE_GRAPH_OS_PROPOSAL.md` doc drift.

## Threat model (summary)

| Adversary | Today | After |
|---|---|---|
| LAN host, no credentials | Reaches every `Local`/`Lan` endpoint; can call any auth-`None` tool (incl. every auto-published toolset) subject only to per-action approval parks | Cannot connect to `Local` endpoints (loopback bind); `Lan`+ endpoints require bearer unless operator explicitly acknowledged unauthenticated |
| LAN host with a leaked token | Calls the granted tools; rate allotment resets on membrane restart | Same grant scope; rotation via `mcp.rotate_token`; allotment persistence optional follow-up |
| Compromised/prompt-injected philote | Can provision unauthenticated endpoints (post-approval), spoof `owner_agent_id`, and expose its whole toolset silently at startup | Provisioning still approval-gated; identity verified; no silent publication; unauthenticated exposure requires explicit operator acknowledgment |
| Process holding the hotel socket | Full MCP state control via `SetConfig` + spoofed provisioning | Reserved-prefix ACL + identity checks confine it to its own guest's authority (socket compromise remains root-equivalent for that guest — documented residual risk) |

## Phases

1. **Slice H1 (S1 + S4 approval unification)** — bind-by-tier + fence on all
   routes + loopback-approval fix. Small, highest risk-reduction.
   Verify: integration tests for tier×bind matrix; live loopback check on
   mac-jane.
2. **Slice H2 (S2)** — publication gate + legacy retirement (two-step).
   Verify: startup of a philote with flag off publishes nothing; grep-level
   proof `UpdateMcpRoutes` gone at step 2.
3. **Slice H3 (S3)** — schema `auth`, `mcp.grant_token`/rotate/revoke,
   HmacSha256 removal. Verify: philote provisions a bearer endpoint end to
   end with zero operator vault work; UAT with token.
4. **Slice H4 (S5)** — protocol/error conformance. Verify: conformance tests;
   external client (Claude Code `.mcp.json`) sees `isError` on a forced
   business error.
5. **Slice H5 (S6)** — test harness + hygiene sweep (rides throughout; lands
   as the closing slice with coverage assertions).

## Open Questions

- Should `Mesh` tier bind the tailnet interface specifically rather than
  `0.0.0.0` + fence? (Requires interface discovery; nice-to-have.)
- Durable rate allotments: persist to the context graph or accept
  reset-on-restart with a note? (A8 is low severity; default = accept + doc.)
- Does any consumer want HMAC request signing before we delete the enum
  variant? One release of deprecation gives time to object.
- Operator CLI (`phil mcp list|status|revoke`, C5): fold into H5 or defer to
  its own slice? Cheap; recommended in H5.
- Cross-hotel dispatch (C6): explicitly out of scope here; needs its own
  design if the coordination-endpoint proposal wants remote philotes.
