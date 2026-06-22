---
title: MCP Membrane Gateway — Philote-Configured, Transform-Driven
doc_type: proposal
domain: membrane-transport
status: proposed
last_updated: 2026-06-22
tags:
  - mcp
  - membrane
  - gateway
  - philote
  - tool-runner
  - router
  - approval
  - a2a
proposal_id: mcp-membrane-gateway
implements: []
implemented_by: []
active_seams:
  - mcp-endpoint-config
  - mcp-transform-engine
  - mcp-preapproval
  - membrane-materialization
related_docs:
  - GUEST_PRIMITIVE_PATTERN.md
  - ARCHITECTURE.md
  - PORT_BLUEPRINT.md
  - LIFE_GRAPH_OS_PROPOSAL.md
  - MUNINN_MEMORY_PROTOCOL_PROPOSAL.md
source_of_truth_targets:
  - docs/architecture/MCP_MEMBRANE_GATEWAY_PROPOSAL.md
---

# MCP Membrane Gateway — Philote-Configured, Transform-Driven

## Problem

The initial `membrane-mcp` implementation (PR #55) treats the MCP membrane
as a proxy to the philote's cognitive loop: every `tools/call` creates a
philote turn and waits up to 30s for the model to respond. This has several
problems:

- The philote's cognitive loop is not the right backend for most MCP tool calls
- Approval semantics are tangled — the MCP ingress gate and the tool-execution
  gate inside the philote are separate concerns that interact incorrectly
- The philote cannot declare what MCP tools it wants to expose; routes are
  either operator-stored or naively derived from `default_toolset`
- There is no way to configure per-endpoint ports, URIs, or transform rules

## Vision

The philote **configures** the MCP membrane; it is not the membrane's backend.

A philote uses a dedicated cognitive tool (`mcp.provision`) during its turn to
declare one or more MCP endpoints. Each endpoint specifies:

- **Listening port / URI** — where the MCP server binds
- **Tool listing** — what tools are advertised to MCP clients (`tools/list`)
- **Inbound transforms** — per tool: how to map MCP args → router envelope
- **Outbound transforms** — per tool: how to map router response → MCP response
- **Pre-approval rules** — which envelope types are pre-approved at config time

The hotel materializes a `membrane-mcp` guest for that port if one is not
already running. The guest operates autonomously using the transform rules —
dispatching router envelopes into the mesh (to datasources, tool-runner, other
philotes) without the configuring philote being involved in the hot path.

The philote's turn that calls `mcp.provision` is the authorization event. The
pre-approval rules embedded in the config carry that authority forward so that
future requests matching the declared envelope shape are not blocked.

## Surface Hygiene

The MCP membrane exposes a deliberately small external surface. It should not
blur continuity memory, LifeGraph evidence, and raw graph/runtime operations.

Use [mcp-surface-hygiene](/Users/jaredlikes/code/philotic-stack/skills/mcp-surface-hygiene/SKILL.md)
when reviewing or provisioning MCP endpoints.

Current boundary rules:

- `context.capture` is a Perplexity-to-Muninn continuity route. It stores notes,
  decisions, references, and memory-worthy context in Muninn. It does not write
  to the operator LifeGraph.
- `life.recall` may be exposed as a governed read path for LifeGraph context
  packets.
- `life.observe` may be exposed only when the description says it proposes
  evidence with `validation_state=proposed`; it must not sound like confirmed
  truth.
- `life.commit`, `life.resolve`, and raw graph mutation tools remain unavailable
  to ordinary external clients unless an admin endpoint has explicit
  operator-approved preapproval rules.
- Separate endpoints are preferred when clients need different authority levels,
  for example `perplexity-memory` for Muninn capture and `lifegraph-readonly`
  for LifeGraph recall.

## Architecture

```
┌────────────────────────────────────────────────────────────┐
│  Philote (cognitive loop)                                  │
│                                                            │
│  tool call: mcp.provision {                                │
│    port: 8910,                                             │
│    tools: [{ name: "search_docs", ... }],                  │
│    preapproval_rules: [{ action: "datasource.query", ... }]│
│  }                                                         │
└────────────────┬───────────────────────────────────────────┘
                 │ IpcRequest::ProvisionMcpEndpoint
                 ▼
┌────────────────────────────────────────────────────────────┐
│  Hotel (aiua)                                              │
│                                                            │
│  - stores McpEndpointConfig in context graph               │
│  - materializes membrane-mcp guest on :8910 if needed      │
│  - stores pre-approval rules against envelope signatures   │
└────────────────┬───────────────────────────────────────────┘
                 │ update_mcp_config push
                 ▼
┌────────────────────────────────────────────────────────────┐
│  membrane-mcp guest (port 8910)                            │
│                                                            │
│  tools/call "search_docs" { query: "..." }                 │
│    → inbound transform → RouterEnvelope {                  │
│        action: "datasource.query",                         │
│        target: "graph-datasource-01",                      │
│        payload: { query: "..." }                           │
│      }                                                     │
│    → dispatch into mesh                                    │
│    → outbound transform → MCP response                     │
└────────────────────────────────────────────────────────────┘
```

## Core Types

### McpEndpointConfig

```rust
pub struct McpEndpointConfig {
    /// Stable ID for this endpoint (e.g. "bjork-mcp-01").
    pub endpoint_id: String,
    /// Agent that owns and may update this endpoint.
    pub owner_agent_id: String,
    /// Port the membrane-mcp guest should bind on.
    pub port: u16,
    /// Optional path prefix (default "/mcp").
    pub path: Option<String>,
    /// Tools advertised to MCP clients.
    pub tools: Vec<McpToolSpec>,
    /// Pre-approval rules established by the philote's provisioning turn.
    pub preapproval_rules: Vec<McpPreapprovalRule>,
    /// Unix epoch. LWW merge key.
    pub updated_at: u64,
}
```

### McpToolSpec

```rust
pub struct McpToolSpec {
    /// MCP tool name (e.g. "search_docs").
    pub name: String,
    /// Human description for tools/list.
    pub description: String,
    /// JSON Schema for input arguments.
    pub input_schema: serde_json::Value,
    /// How to map MCP args to a router envelope.
    pub inbound_transform: McpInboundTransform,
    /// How to map the router response to an MCP result.
    pub outbound_transform: McpOutboundTransform,
    /// Per-tool auth override (inherits endpoint default if absent).
    pub auth: Option<McpAuthScheme>,
}
```

### McpInboundTransform

Maps MCP `tools/call` arguments to a `RouterEnvelope`. Supports two modes:

```rust
pub enum McpInboundTransform {
    /// Direct field mapping: MCP arg path → envelope field path.
    /// Simple cases: no code, just declared mappings.
    FieldMap {
        action: String,
        target: McpRouteTarget,
        mappings: Vec<FieldMapping>,  // { from: "$.args.query", to: "$.payload.query" }
    },
    /// Jinja2-style template rendered against the full MCP request context.
    /// For non-trivial shapes.
    Template {
        template: String,
    },
}
```

### McpOutboundTransform

Maps a router envelope response to an MCP result:

```rust
pub enum McpOutboundTransform {
    /// Extract a field from the response payload as the MCP result.
    Extract { path: String },
    /// Return the full response payload as JSON.
    PassThrough,
    /// Render a template against the response.
    Template { template: String },
}
```

### McpPreapprovalRule

```rust
pub struct McpPreapprovalRule {
    /// Envelope action pattern this rule matches (exact or glob).
    pub action_pattern: String,
    /// Target constraint (None = any target).
    pub target: Option<McpRouteTarget>,
    /// The philote turn ID that established this approval.
    pub approved_by_turn: String,
    /// Unix epoch when this rule was established.
    pub approved_at: u64,
    /// Optional expiry. None = permanent until config update.
    pub expires_at: Option<u64>,
}
```

## New IPC Surface

### IpcRequest::ProvisionMcpEndpoint

```rust
ProvisionMcpEndpoint {
    config: McpEndpointConfig,
}
```

Hotel response: `IpcResponse::McpEndpointProvisioned { endpoint_id, port, materialized: bool }`.

The `materialized` flag tells the philote whether a new guest was spawned or an
existing one was updated. The philote can emit a confirmation to the user.

### IpcRequest::RevokeMcpEndpoint

```rust
RevokeMcpEndpoint {
    endpoint_id: String,
    owner_agent_id: String,
}
```

Hotel tears down the membrane-mcp guest and removes the config.

## New Philote Tool: `mcp.provision`

Added to the philote catalog. Class: `config`. Requires operator approval on
first call per endpoint (approval recorded as part of the turn checkpoint).

```json
{
  "tool_name": "mcp.provision",
  "description": "Declare or update an MCP endpoint this agent exposes to external callers. Specifies the port, tool listing, inbound/outbound transforms, and pre-approval rules. The hotel materializes a membrane-mcp guest for this endpoint if one is not running.",
  "input_schema": {
    "type": "object",
    "properties": {
      "endpoint_id": { "type": "string" },
      "port": { "type": "integer" },
      "tools": { "type": "array", "items": { "$ref": "#/$defs/McpToolSpec" } },
      "preapproval_rules": { "type": "array", "items": { "$ref": "#/$defs/McpPreapprovalRule" } }
    },
    "required": ["endpoint_id", "port", "tools"]
  }
}
```

## Pre-Approval Semantics

Pre-approval is declared in the philote's provisioning turn. The hotel stores the
rules alongside the endpoint config. When `membrane-mcp` dispatches an envelope,
it checks the pre-approval table before emitting an approval-required event.

**The provisioning turn IS the authorization event.** The operator approves
`mcp.provision` (class `config` → requires approval by default). That approval,
recorded in the turn checkpoint, is what backs the pre-approval rules. Future
requests matching those rules are not blocked — the authorization is already on
the record.

This is intentionally different from runtime approval:
- **Runtime approval** (existing system): operator approves individual tool calls
  during a philote's cognitive turn
- **Provisioning approval** (this proposal): operator approves the entire endpoint
  configuration once; the config carries that authority forward

## membrane-mcp Changes

The existing `SharedRoutingTable` is replaced by `McpEndpointConfig` as the
authoritative routing source. The runtime changes:

1. `handle_push("update_mcp_config")` replaces `update_mcp_routes` — receives full
   `McpEndpointConfig`, rebuilds the transform table
2. `handle_tools_list()` reads from the config's `tools` array directly
3. `handle_tools_call()` applies `inbound_transform`, dispatches envelope via the
   hotel's router fabric, applies `outbound_transform` to the response
4. Pre-approval check happens before dispatch — if rule matches, no park

The `pending_responses` oneshot correlation map remains for the response path.

## Relationship to PR #55 (Current Work)

PR #55 establishes the foundation:
- `MembraneGuest` trait + `MembraneRuntime` generic driver
- Auth infrastructure (vault-backed BLAKE3, `VaultHashCache`)
- IPC lease lifecycle (`AcquireMcpMembraneLease` etc.)
- `handle_push` extension point on `MembraneGuest`

All of this carries forward. The routing model flips: instead of `UpdateMcpRoutes`
(philote self-registers as backend), `ProvisionMcpEndpoint` (philote declares full
endpoint config including transforms). The `UpdateMcpRoutes` / `RevokeMcpRoutes`
IPC surface is deprecated in favour of `ProvisionMcpEndpoint`.

## Phases

### Phase 1 — Core types + `ProvisionMcpEndpoint` IPC
- `McpEndpointConfig`, `McpToolSpec`, `McpInboundTransform`, `McpOutboundTransform`,
  `McpPreapprovalRule` in `ansible-mesh-core::mcp_endpoint`
- `IpcRequest::ProvisionMcpEndpoint` / `IpcResponse::McpEndpointProvisioned`
- Hotel stores config, fans out `update_mcp_config` push to `mcp-membrane` guest
- Pre-approval rules stored in hotel config under `__mcp_preapproval__:<endpoint_id>`

### Phase 2 — membrane-mcp transform engine
- Replace `SharedRoutingTable` with config-driven transform table
- `FieldMap` inbound transform (declarative, no eval)
- `PassThrough` / `Extract` outbound transforms
- Pre-approval check against stored rules

### Phase 3 — `mcp.provision` philote tool
- Catalog entry (class `config`, requires approval)
- Tool handler calls `IpcRequest::ProvisionMcpEndpoint`
- Tool result tells the model whether a new guest was spawned
- Hotel materializes `membrane-mcp` guest if `port` not yet claimed

### Phase 4 — Template transforms + multi-endpoint
- `Template` inbound/outbound transforms (Jinja2-style or mustache)
- Multiple concurrent endpoints per hotel
- `mcp.revoke` tool for teardown

## Open Questions

- **Transform language**: `FieldMap` covers 80% of cases. What's the right
  template language for Phase 4? Avoid Turing-complete eval in the hot path.
- **Port allocation**: Does the philote pick the port, or does the hotel assign
  one and return it? Operator-visible port registry is needed.
- **Multi-hotel routing**: If the target philote or datasource lives on a different
  hotel, the router envelope needs to cross the mesh. Does membrane-mcp dispatch
  directly or go through the local hotel's router?
- **Config persistence**: `McpEndpointConfig` should survive hotel restart.
  Stored in the context graph as a first-class node type?
- **Guest naming**: How is the membrane-mcp guest ID derived from the endpoint?
  Convention: `mcp-membrane-<endpoint_id>`.
