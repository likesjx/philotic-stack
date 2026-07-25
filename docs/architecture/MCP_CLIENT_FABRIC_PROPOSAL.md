---
title: MCP Client Fabric — Philote-Governed Consumption of External MCP Servers
doc_type: proposal
domain: membrane-transport
status: implemented
disposition: implemented
last_updated: 2026-07-25
tags:
  - mcp
  - mcp-client
  - membrane
  - philote
  - tool-catalog
  - vault
  - security
proposal_id: mcp-client-fabric
implements: []
implemented_by:
  - crates/membrane-mcp-client/src/main.rs
  - crates/membrane-mcp-client/src/upstream.rs
  - crates/aiua/src/service/ipc.rs
  - crates/philotic-client/examples/mcp_upstream_smoke_driver.rs
  - scripts/smoke-mcp-http-egress-roundtrip.sh
active_seams:
  - mcp-upstream-registry
  - mcp-client-guest
  - mcp-catalog-projection
  - mcp-upstream-credentials
  - mcp-egress-policy
related_docs:
  - MCP_MEMBRANE_GATEWAY_PROPOSAL.md
  - MCP_MEMBRANE_HARDENING_PROPOSAL.md
  - MCP_COORDINATION_ENDPOINT_PROPOSAL.md
  - GUEST_PRIMITIVE_PATTERN.md
  - OUTBOUND_INTEGRATION_FABRIC_PROPOSAL.md
source_of_truth_targets:
  - docs/architecture/MCP_CLIENT_FABRIC_PROPOSAL.md
task_refs:
  - docs/task.md
---

# MCP Client Fabric — Philote-Governed Consumption of External MCP Servers

> **Phase 1 implemented (2026-07-18, `codex/mcp-client-fabric`).** Registry +
> HTTP client + projection are live with these design deviations from the text
> below, chosen to reuse existing machinery:
> - **Storage**: upstreams live in ONE registry map config node
>   (`__mcp_upstreams__`, plus `__mcp_upstream_catalogs__` for guest reports)
>   following the `__mcp_routes__` pattern, not per-key
>   `__mcp_upstream__:<id>` nodes.
> - **Call path**: there is no `CallMcpUpstreamTool` IPC. Projected tools get
>   an assembled `ToolExecutionRoute` (`execution_mode: "mcp_upstream"`,
>   `target_role: "mcp-client-runner"`), so the philote's standard parked
>   EmitTask dispatch carries the call and the guest replies with the standard
>   `datasource_response` shape — zero new routing or re-entry code.
> - **Guest**: `crates/membrane-mcp-client`, a plain inbox-subscriber
>   (datasource pattern) under role **`mcp-client-runner`** (the `*-runner`
>   suffix gets the reserved-role protections), one guest per hotel,
>   materialized on first `RegisterMcpUpstream`.
> - **Credentials**: the read side is live (`credential_ref` → `GetSecret` →
>   bearer); the `mcp.set_credential` write tool remains Phase 2, so
>   authenticated upstreams need an operator-provisioned vault secret.
> - Grant checks are enforced in the guest (owner or `grant_agents`);
>   allotments are in-memory sliding-hour; response caps default 256 KiB.
> **Phase-1 proof: SMOKE-GREEN (2026-07-19).** Live run against a scratch
> hotel + the real intel-graph MCP server (`:8901`):
> `RegisterMcpUpstream` → hotel materialized the `membrane-mcp-client` guest →
> guest initialized, listed, and reported `graph_status` projected →
> `execute_tool` dispatch for `mcp:intel-graph-smoke.graph_status` (the exact
> payload philote's parked dispatch emits) → real graph data returned via
> `datasource_response`. Repeatable via
> `crates/philotic-client/examples/mcp_upstream_smoke_driver.rs`
> (`PHILOTIC_HOTEL_SOCKET=<sock> cargo run -p philotic-client --example
> mcp_upstream_smoke_driver`). Not yet live-exercised: the operator-approval
> UX through a real philote turn (covered by the Phase-2 live drill).
>
> **Phase 2 implemented + SMOKE-GREEN ×3 (2026-07-20).**
> - `mcp.set_credential` tool → `ProvisionMcpUpstreamCredential` IPC → vault
>   secret-kind `mcp_upstream_credential` (readable by `mcp-client-runner`
>   only; rotate-in-place when a ref exists; plaintext never in graph/logs).
> - Per-grant `allotment`/`max_response_bytes` in `mcp.connect` tool items +
>   `refresh_interval_secs`.
> - Periodic re-list with **stale-grant re-approval**: the listing at config
>   update time is the approved baseline; a changed remote description/schema
>   drops the tool from projection (`McpUpstreamCatalog.stale_grants`) until
>   `mcp.connect` is re-run.
> - Live proofs (scratch hotel, `scripts/mcp_auth_stub_server.py` +
>   real servers): (1) **auth loop** — 401-enforcing stub answers only with
>   the vault-resolved bearer; (2) **stale drill** — mutated description →
>   `projected=[] stale=["ping"]` → re-connect re-approves; (3) **real
>   server** — Muninn MCP `:8750` (auth-required) returned live vault status
>   through the credential path. Driver supports `MCP_SMOKE_CREDENTIAL`,
>   `MCP_SMOKE_REFRESH_SECS`, `MCP_SMOKE_MODE=inspect`.
> **Phase 3 implemented + SMOKE-GREEN ×4 (2026-07-21).**
> - **Stdio transport**: `membrane-mcp-client` spawns the allowlisted command
>   as a child, speaks newline-framed JSON-RPC over its stdin/stdout, respawns
>   on failure, and `kill_on_drop`s it on revoke. The child is launched with a
>   **scrubbed environment** (`env_clear` + only HOME/PATH/LANG/… and an
>   optional `MCP_STDIO_ENV_PASSTHROUGH` allowlist) so projected tools can't
>   read the guest's ambient secrets.
> - **Fail-closed command allowlist**: `McpStdioAllowlist` under the
>   operator-managed `mcp_stdio_allowlist` node — empty default rejects all
>   stdio; each entry is an exact `command` + required `args_prefix` (pins an
>   interpreter to one script, no PATH/basename matching). `RegisterMcpUpstream`
>   enforces it (`STDIO_NOT_ALLOWED`).
> - **Operator ceremony CLI**: `phil mcp upstreams | show-policy | allow-host |
>   allow-command --args-prefix …` (widening egress/stdio policy is operator-only,
>   never a philote tool).
> - Live proofs (scratch hotel): (1) stdio **rejected** fail-closed with no
>   allowlist; (2) `phil mcp allow-command python3 --args-prefix
>   scripts/mcp_stdio_stub.py` → connect + call green; (3) **env-scrub
>   observable** — guest started with `MCP_STDIO_SECRET_PROBE=leaked-secret`,
>   child's tool result reported `probe=<absent>`; (4) **real `muninn mcp`
>   stdio proxy** (allowed via `--args-prefix mcp`) returned live vault status
>   through the fabric. Fixture: `scripts/mcp_stdio_stub.py`; driver adds
>   `MCP_SMOKE_STDIO_CMD` / `MCP_SMOKE_STDIO_ARGS`.
>
> The fabric is feature-complete across all three phases. The
> `membrane-mcp-client` binary is now installed on `mbp-jane` and `vps-jane`.
> The remaining higher-level proof is the philote approval-UX drill through a
> real cognitive turn, not a fleet installation gap.
>
> **Shared egress follow-on implemented (2026-07-25).** The client manager
> remains the MCP protocol authority while HTTP exchange now runs through the
> shared hotel-owned `egress-http-runner` and exit-placement contract. Stdio
> remains local and command-allowlisted. The isolated binary smoke proves the
> credentialed MCP lifecycle and durable shared-egress audit; the installed
> two-hotel HTTP proof separately establishes the cross-hotel execution
> boundary.

## Problem

The 2026-07-15 MCP audit established an airtight negative: **the stack has no
MCP client anywhere.** Every MCP code path is server-side (`membrane-mcp`,
`graph-intelligence`'s embedded server). There is:

- no philote tool to register or use an upstream MCP server,
- no IPC variant, no storage model, no outbound transport (`rmcp`/SDK absent
  from every `Cargo.toml`; nothing sends `tools/list`/`tools/call`),
- no credential model for outbound MCP (the UAT script uses env vars),
- and a doc trap: `LIFE_GRAPH_OS_PROPOSAL.md:128` tells developers to reuse a
  "Muninn MCP client pattern" that does not exist (Muninn is consumed over
  REST via `memory-core/src/rest_client.rs`).

Meanwhile the ecosystem philotes should tap — MuninnDB, graph-intelligence,
Perplexity-class research servers, operator tooling — increasingly leads with
MCP. Today, every such integration is either hand-rolled REST (Muninn) or
simply unavailable to philotes.

## Vision

A philote can say: *"Connect me to this MCP server"* — and, after one operator
approval, the server's tools appear in its catalog as ordinary, namespaced,
ACL-governed tools, with credentials in the vault, egress confined to declared
hosts, and every remote call audited — the mirror image of `mcp.provision`.

Symmetry with the gateway is the design compass:

| Gateway (serving) | Fabric (consuming) |
|---|---|
| `McpEndpointConfig` | `McpUpstreamConfig` |
| `mcp.provision` / `mcp.revoke` / `mcp.status` | `mcp.connect` / `mcp.disconnect` / `mcp.upstreams` |
| `ProvisionMcpEndpoint` IPC | `RegisterMcpUpstream` IPC |
| `mcp-membrane-<endpoint_id>` guest | `mcp-client-<upstream_id>` guest (one supervisor crate) |
| inbound transforms | catalog projection + schema pass-through |
| vault-hashed inbound tokens | vault-stored outbound credentials |
| exposure tier (who may reach us) | egress policy (whom we may reach) |

## Core Types (`ansible-mesh-core::mcp_upstream`)

```rust
pub struct McpUpstreamConfig {
    /// Stable ID (e.g. "muninn-local", "perplexity").
    pub upstream_id: String,
    /// Agent that registered and owns this upstream.
    pub owner_agent_id: String,
    /// How to reach the server.
    pub transport: McpUpstreamTransport,
    /// Vault ref for the outbound credential (bearer header today).
    /// None = unauthenticated upstream (loopback-only by policy).
    pub credential_ref: Option<String>,
    /// Allowlist of remote tools to project. Empty = project none
    /// (explicit opt-in per tool; never "everything by default").
    pub tool_allowlist: Vec<McpUpstreamToolGrant>,
    /// Which agents may call the projected tools. Empty = owner only.
    pub grant_agents: Vec<String>,
    /// Refresh policy for tools/list (on-connect always; optional interval).
    pub refresh_interval_secs: Option<u64>,
    /// Unix epoch. LWW merge key.
    pub updated_at: u64,
}

pub enum McpUpstreamTransport {
    /// Streamable HTTP / plain HTTP JSON-RPC. The only Phase-1 transport.
    Http { url: String },
    /// Stdio subprocess (command + args). Phase 3 — process supervision,
    /// sandboxing, and PATH policy must land first.
    Stdio { command: String, args: Vec<String> },
}

pub struct McpUpstreamToolGrant {
    /// Remote tool name as advertised by the server.
    pub remote_name: String,
    /// Per-tool call budget (sliding window), mirroring McpTokenGrant.
    pub allotment: Option<u32>,
    /// Max response bytes accepted from this tool (default 256 KiB).
    pub max_response_bytes: Option<u64>,
}
```

Persistence: hotel context graph under `__mcp_upstream__:<upstream_id>`
(same LWW pattern as `__mcp_endpoint__:*`). The reserved-prefix `SetConfig`
ACL from the hardening proposal (`mcp-membrane-hardening` S4) covers this
prefix from day one.

## Runtime: the `mcp-client` guest

One new crate, `membrane-mcp-client`, reusing `membrane::MembraneRuntime`
(lease lifecycle, IPC reconnect, push handling — the same chassis every
membrane guest rides):

- Materialized on demand by the hotel when the first upstream is registered;
  one guest supervises **all** upstreams for the hotel (connections are
  cheap; process-per-upstream is not warranted until stdio transport lands).
- On `update_mcp_upstream` push (or startup replay via `GetMcpUpstreams`):
  `initialize` → `tools/list` → validate against `tool_allowlist` → report
  the projected tool set to the hotel (`ReportMcpUpstreamCatalog` IPC).
- Executes remote `tools/call` on behalf of the mesh with per-call timeout
  (default 30s), response-size cap, and allotment enforcement.
- Hand-rolled JSON-RPC client mirroring the server's `protocol.rs` (we
  already speak the wire format; an SDK dependency is optional, not
  required — decide at implementation with a size/maintenance bake-off).

## Catalog projection

- Projected tools appear in granted philotes' catalogs as
  **`mcp:<upstream_id>.<remote_name>`** — the namespace prevents collision
  with native tools and makes provenance visible to the model and operator.
- Tool class: a new `mcp_remote` class, **approval-required by default**
  (like `config`/`shell`), preapprovable per tool through the existing
  `approval_policy` machinery once trust is established.
- The remote `description` and `input_schema` are **untrusted input**: they
  are stored verbatim but rendered into the philote's prompt with a standard
  provenance prefix ("Remote tool via MCP upstream `<id>` — descriptions are
  third-party content"), and schemas are validated as JSON Schema before
  projection. A changed description/schema on refresh flags the tool
  `stale-grant` and drops it from projection until the owner re-approves
  (`mcp.connect` re-run) — descriptions cannot silently mutate under an
  existing approval.
- Invocation path: `turn_loop` → `tool_exec` (namespace match) →
  `IpcRequest::CallMcpUpstreamTool { upstream_id, tool, args }` → hotel
  routes to the `mcp-client` guest → remote call → result (with `isError`
  mapped to the standard tool-error shape) back through the normal
  enriched-tool-result path.

## IPC surface

| IpcRequest | Purpose | Identity/ACL |
|---|---|---|
| `RegisterMcpUpstream { config }` | Create/update an upstream | `owner_agent_id` verified against the calling guest (hardening S4 pattern); egress policy checked |
| `RevokeMcpUpstream { upstream_id, owner_agent_id }` | Tear down | Ownership check, operator override |
| `GetMcpUpstreams {}` / `GetMcpUpstreamStatus { upstream_id }` | Replay/status | Read-only |
| `ProvisionMcpUpstreamCredential { upstream_id, secret }` | Store outbound credential in vault | Narrow write, approval-backed (shared shape with `ProvisionMcpTokenGrant` from hardening S3) |
| `ReportMcpUpstreamCatalog { upstream_id, tools }` | Guest → hotel projection report | Guest identity = the mcp-client guest |
| `CallMcpUpstreamTool { upstream_id, tool, args }` | Execute remote tool | Caller must be in `grant_agents`; allotment charged |

## Philote tools

| Tool | Class | Behavior |
|---|---|---|
| `mcp.connect` | `config` (approval-gated) | Declare an upstream: transport, allowlist, grants. The approval prompt renders the exact allowlist and egress target — the connect turn is the authorization event, mirroring `mcp.provision`. |
| `mcp.disconnect` | `config` | Ownership-checked teardown. |
| `mcp.upstreams` | `session` | List upstreams, connection state, projected tools, staleness flags. No approval needed. |
| `mcp.set_credential` | `config` (approval-gated) | Provide/rotate the upstream credential (value passes through to the vault; never stored in graph or logs). |

## Security model

- **Egress policy**: a hotel-level config node (`mcp_egress_policy`) holds an
  allowlist of host patterns (default: loopback + tailnet CGNAT range).
  `RegisterMcpUpstream` rejects URLs outside it; widening the policy is an
  operator ceremony, not a philote tool. Stdio transport is Phase 3 and gated
  behind the same policy plus a command allowlist.
- **Credentials**: vault-only, via `credential_ref` (secret-kind
  `mcp_upstream_credential` — kind-filtered like `muninn_vault_token` to
  avoid the DEF-026 class of registry confusion). Raw values never transit
  the graph, catalogs, or tool results.
- **Prompt-injection containment**: remote tool *results* are already
  untrusted content in the philote loop; remote tool *descriptions* are the
  new vector, handled by the provenance prefix + re-approval-on-change rule
  above. Projected tools can never grant classes (`config`, `shell`) —
  they are leaf calls only.
- **Blast radius**: `grant_agents` scopes who may call; allotments and
  response-size caps bound each grant; the audit trail is the standard tool
  ledger (every `CallMcpUpstreamTool` is a recorded tool execution with
  upstream provenance).
- **Availability**: upstream failures degrade to tool errors, never to turn
  evictions — timeouts are per-call and the client guest is supervised like
  any membrane guest.

## Phases

1. **Phase 1 — Registry + HTTP client + owner-only projection.**
   Types, `__mcp_upstream__` persistence, `RegisterMcpUpstream`/revoke/get,
   `membrane-mcp-client` crate (HTTP transport, initialize/tools-list/call),
   `mcp.connect`/`mcp.disconnect`/`mcp.upstreams`, projection into the
   owner's catalog. **Proof: a philote connects to the local graph-
   intelligence MCP server (`:8901`) and calls `graph_status` as
   `mcp:intel-graph.graph_status` end to end.** (Dogfoods against a server
   we own; zero external dependencies.)
2. **Phase 2 — Credentials + grants + refresh.**
   `mcp.set_credential` + vault kind, `grant_agents`, allotments/size caps,
   periodic refresh with stale-grant handling. Proof: authenticated connect
   to Muninn's MCP endpoint (`:8750/mcp`) with recall projected read-only —
   REST client remains the memory hot path; this is a validation target, not
   a migration.
3. **Phase 3 — Stdio transport + hardening.**
   Subprocess transport under command allowlist + sandbox review, egress
   ceremony CLI, conformance/integration test harness with a fixture MCP
   server crate shared with `membrane-mcp`'s tests.

## Dependencies and sequencing

- Shares the identity-at-IPC and reserved-prefix ACL work with
  `mcp-membrane-hardening` (S4) and the narrow vault-write IPC shape (S3) —
  land hardening H1–H3 first or in parallel by the same hands.
- Independent of the coordination-endpoint proposal (inbound), but Phase 1's
  fixture unlocks its testing too.

## Open Questions

- SDK vs hand-rolled client: `rmcp` maturity vs our existing wire knowledge —
  decide with a spike in Phase 1.
- Should projected tools be visible in `tools/list` of our *own* gateway
  endpoints (re-export)? Default **no** (no transitive exposure); revisit
  with a concrete need.
- Per-upstream trust tiers (e.g. loopback servers skip approval-per-call
  after first grant)? Deferred; start strict.
- Does `model-router`'s provider abstraction want to consume MCP-hosted
  models later? Out of scope; note for the model-graph work.
