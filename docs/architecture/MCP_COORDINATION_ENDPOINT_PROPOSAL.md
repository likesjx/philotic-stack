---
title: MCP Coordination Endpoint — External Orchestrator Drives Philotes + Lifegraph
doc_type: proposal
domain: operator-control-plane
status: proposed
last_updated: 2026-06-25
tags:
- mcp
- membrane-mcp
- operator-control-plane
- philote
- subagent
- lifegraph
- life-graph-runner
- coordination
- connector
related_docs:
- GRAPH_DATASOURCE_PHILOTE_PROPOSAL.md
- RUNTIME_AUTHORITY_LEASES_PROPOSAL.md
- OPERATOR_IDENTITY_AND_DANGEROUS_ACTION_CEREMONIES_PROPOSAL.md
- AGENT_WORKFLOW_PROPOSAL.md
proposal_id: mcp-coordination-endpoint
implements:
- membrane-mcp
implemented_by: []
active_seams:
- mcp-coordination-tool-catalog
- philote-chat-dispatch-mapping
- lifegraph-mcp-retrieval
---

# MCP Coordination Endpoint — External Orchestrator Drives Philotes + Lifegraph

## Problem

An external orchestrator (a Cowork/Claude session acting as operator) can already
coordinate ephemeral subagents the way Claude Code does — spawn, assign, collect,
release. It cannot coordinate **philotes**, even though the hotel IPC already exposes
every primitive required: `SendOperatorChatTurn`, `SpawnSubagent` /
`AssignSubagentTask` / `RenewSubagentLease` / `ReleaseSubagent`, `HandoffToRole`,
and `QueryStatus` / `QueryTimeline`. The primitives exist; there is no door the
external orchestrator can knock on.

Symmetrically, the **lifegraph** is materialized and reachable inside the mesh — a
`life-graph-runner` guest per hotel, fed by `observe` and queried by `recall` /
`open_loops_by_context` with semantic retrieval, plus an `attention-steward` that
surfaces `lifegraph:open_loop` signals. But an external operator has no path to
recall life context or record observations, so any coordination it does is blind to
the operator's own life state.

Both gaps share one shape: the mesh speaks a rich internal language, and there is no
**MCP surface** that projects the operator-relevant verbs outward where an external
agent runtime can consume them as ordinary tools.

## Vision

One provisioned MCP endpoint on the hotel — served by the existing `membrane-mcp`
crate — exposes a small, curated **coordination toolset**. The external orchestrator
connects to that endpoint as a normal MCP connector and gains two capabilities at
once:

1. **Philote coordination** — chat a named philote, spawn/assign/collect a subagent,
   check status — routed through `McpRouteTarget::Philote`.
2. **Lifegraph integration** — recall, observe, and list open loops — routed to the
   `life-graph-runner` role, using the same routing template the existing
   `search_docs` tool uses for a datasource.

The result: the orchestrator coordinates philotes that are themselves lifegraph-aware,
and the orchestrator's own decisions are informed by the same life context. "Drive my
philotes like Claude Code, integrated with my lifegraph" becomes a connected toolset,
not a new subsystem.

## Goal

Project a curated set of operator coordination verbs over MCP from the hotel, so an
external orchestrator can dispatch to philotes and the lifegraph through one connected
endpoint — reusing the existing IPC verbs, routing model, lease/authorization
machinery, and `life-graph-runner`, adding only a tool catalog and the inbound
transforms that map each tool to its IPC action.

## Core Recommendation

Provision a single `McpEndpointConfig` (owned by an operator/admin agent, e.g.
`agent-<hotel>-operator`) whose `tools` catalog includes the coordination verbs below,
and dispatch each through `membrane-mcp` to its IPC action. Reuse — do not duplicate —
the `search_docs` → datasource pattern (`McpToolSpec` + `McpInboundTransform::FieldMap`
+ `McpRouteTarget`).

### What already exists (do not rebuild)

- `membrane-mcp`: HTTP MCP server with `tools/list` + `tools/call`, auth, inbound/
  outbound transforms, and dispatch to `McpRouteTarget::{Philote, Tool, Datasource}`.
- IPC verbs: `SendOperatorChatTurn` (→ `OperatorChatTurnReply`), `SpawnSubagent`,
  `AssignSubagentTask`, `RenewSubagentLease`, `ReleaseSubagent`, `AcceptSubagentLease`,
  `FireSubagentHook`, `HandoffToRole`, `QueryStatus`, `QueryTimeline`.
- Endpoint model: `McpEndpointConfig`, `McpToolSpec`, `ExposureTier`,
  `McpPreapprovalRule`; lifecycle via `ProvisionMcpEndpoint` / `GetMcpEndpointStatus` /
  `RevokeMcpEndpoint`, lease via `AcquireMcpMembraneLease`.
- `life-graph-runner` role materialized per hotel; observe/recall verbs exercised by
  `crates/philotic-client/examples/life_graph_ipc_smoke_driver.rs`
  (`operator_intent: "open_loops_by_context"`, 768-dim embeddings).

### What this proposal adds

- A coordination **tool catalog** (the `tools` vec on one endpoint config).
- The inbound-transform **action/target mappings** for the chat and subagent verbs
  (the datasource template covers the lifegraph verbs almost verbatim; the
  chat/subagent verbs need their action strings + a `dispatch.rs` arm confirmed/added).
- Connector **wiring** so the external orchestrator can reach the endpoint.

## Proposed Tool Catalog

| MCP tool | Inputs | Target | IPC action |
|---|---|---|---|
| `philote.list` | — | operator query | `QueryOperatorTargetAgents` / `ListDesktopMembraneAgents` |
| `philote.chat` | `agent_id`, `message`, `session_id?` | `Philote{agent_id}` | `SendOperatorChatTurn` → `OperatorChatTurnReply` |
| `philote.spawn_subagent` | `parent_agent_id`, `role`, `task`, `lease_secs?` | `Philote{parent}` | `SpawnSubagent` (+ `AssignSubagentTask`) |
| `philote.subagent_status` | `subagent_id` \| `session_id` | operator query | `QueryStatus` / `QueryTimeline` |
| `philote.release_subagent` | `subagent_id` | `Philote{parent}` | `ReleaseSubagent` |
| `lifegraph.recall` | `query`, `scope?`, `k?` | `Philote{life-graph-runner}` | recall (semantic retrieval) |
| `lifegraph.observe` | `claim`, `source`, `scope?`, `confidence?` | `Philote{life-graph-runner}` | observe |
| `lifegraph.open_loops` | `context?` | `Philote{life-graph-runner}` | `operator_intent: open_loops_by_context` |

Each entry is one `McpToolSpec` with an `input_schema` and an
`McpInboundTransform::FieldMap` mapping arguments onto the IPC payload — exactly as
`search_docs` maps `query → payload.query` onto `datasource.query`.

## Design Notes

### Routing the chat/subagent verbs

`McpRouteTarget::Philote { agent_id }` already exists and `dispatch.rs`/`transform.rs`
already format it (`philote:{agent_id}`). The open implementation detail is the
**action string** each tool maps to and whether `dispatch.rs` already translates a
Philote-target call into a `SendOperatorChatTurn` / `SpawnSubagent` IPC request, or
whether a thin dispatch arm must be added. This is the first slice's job to prove —
flagged honestly as *inferred*, not *proven*.

### Lifegraph verbs

`lifegraph.recall` / `observe` / `open_loops` follow the `search_docs` → datasource
template directly, retargeted at the `life-graph-runner` role. The smoke driver already
demonstrates the payload shape (query_text, embedding_dims, operator_intent), so these
are the lowest-risk tools and the recommended first proof.

### Authorization, leases, exposure

- Set `ExposureTier::Local` initially (loopback only); widen to `Mesh` later.
- Coordination verbs that mutate (spawn/observe/release) should carry
  `McpPreapprovalRule`s or an auth scheme so they cannot fire unattended — align with
  `OPERATOR_IDENTITY_AND_DANGEROUS_ACTION_CEREMONIES_PROPOSAL.md`.
- Endpoint acquires an `AcquireMcpMembraneLease` like any membrane guest.

### Connecting the external orchestrator

Once provisioned at e.g. `http://127.0.0.1:8910/mcp`, the orchestrator adds it as an
MCP connector (the same shape as `.mcp.json`'s `intel-graph` http server). This is an
operator config action performed by the human operator, not by an agent.

## Disposition

`proposed`

## Current Slice

Smallest honest proof, lowest-risk first:

1. Provision a `Local` endpoint owned by an operator agent exposing **two** tools:
   `lifegraph.recall` (datasource-template, retargeted to `life-graph-runner`) and
   `philote.chat` (Philote target → `SendOperatorChatTurn`).
2. Confirm/add the `dispatch.rs` arm for the Philote chat action.
3. Connect the endpoint to the external orchestrator and prove end-to-end:
   one `lifegraph.recall` returns life context, one `philote.chat` returns an
   `OperatorChatTurnReply`.
4. Record the reality gap (which verbs were already wired vs needed a dispatch arm) and
   name the next seam (subagent lifecycle tools).

Out of scope for this slice: the full subagent toolset, `Mesh` exposure, multi-hotel
routing, and any change to philote cognition. Verification target: `smoke-green` via a
new example driver mirroring `life_graph_ipc_smoke_driver.rs`.

## Active Seams

- `mcp-coordination-tool-catalog` — the curated tool set and its schemas.
- `philote-chat-dispatch-mapping` — MCP Philote-target call → `SendOperatorChatTurn`.
- `lifegraph-mcp-retrieval` — lifegraph recall/observe/open_loops over MCP.
