---
doc_type: proposal
domain: operator-control-plane
status: in-progress
last_updated: 2026-03-29
disposition: accepted for current slice
tags: [workstream, session, agent, dashboard, visibility]
refs: [EMBEDDINGS_IN_GRAPH_PROPOSAL, CONTEXT_GRAPH_RUNNER_PROPOSAL]
---

# AGENT_WORKSTREAM_TRACKING_PROPOSAL

## Goal

Enable real-time visibility into active agent work across the Philotic system. Track workstreams from start to completion with live session monitoring, progress metrics, and proposal linkage.

## Core Recommendation

1. **Workstream Model**: Distinct from seams (structural boundaries), workstreams represent active work efforts
   - Created when agent calls `session_start`
   - Links to seam (where) and proposal (what/why)
   - Tracks live metrics: files, lines, tests, phase

2. **Session Lifecycle**: Three MCP tools
   - `session_start`: Creates workstream + session, links to proposal/seam
   - `session_activity`: Records progress (edits, tests, phase changes)
   - `session_close`: Finalizes workstream with verification level

3. **Hospital Board View**: High-density status display (The Pitt style)
   - Alert levels: Critical (no session), Attention (idle), Stable (active)
   - Columns: Workstream, Status, Agent, Phase, Files, Lines, Tests, Activity
   - Click-through to workstream detail

4. **Proposal Tracking**: Every workstream tracks against a proposal
   - `workstream --governs--> proposal` (what we're achieving)
   - `workstream --part_of--> seam` (where we're working)
   - `session --working_on--> seam` (live agent activity)

## Disposition

`accepted for current slice` — Implementation in progress. Core MCP tools and Status Board UI functional.

## Current Slice

- ✅ Session MCP tools (`session_start`, `session_activity`, `session_close`)
- ✅ Workstream auto-creation on session start
- ✅ Status Board UI (hospital-style high-density view)
- ✅ Live sessions grid in Workstreams view
- ✅ Proposal linkage via `proposal_id` parameter
- 🔄 Agent protocol documentation (`docs/guides/AGENT_SESSION_PROTOCOL.md`)
- ⏳ WebSocket real-time updates for live board
- ⏳ Session metrics aggregation (lines changed, files touched)

## Schema Changes

```rust
// Node kinds (existing)
NodeKind::Session   // Agent session instance
NodeKind::Workstream // Active work effort

// Edge relations (existing + new)
EdgeRelation::WorkingOn  // session -> seam
EdgeRelation::Created    // session -> workstream
EdgeRelation::PartOf     // workstream -> seam
EdgeRelation::Governs    // workstream -> proposal
```

## SVER Impact

| Component | Impact | Notes |
|-----------|--------|-------|
| `graph-intelligence` | Minor | New MCP tools, no breaking changes to existing API |
| `agent instructions` | Minor | New required protocol for session tracking |
| `ui/index.html` | Patch | New views, backward compatible |
| `database schema` | None | Uses existing `Session` and `Workstream` node kinds |

**Migration**: None required. Purely additive features.

## Files Changed

- `crates/graph-intelligence/src/server/mcp.rs` — MCP tool implementations
- `crates/graph-intelligence/src/schema.rs` — Edge relations for session tracking
- `crates/graph-intelligence/ui/index.html` — Status Board + Live Sessions UI
- `docs/guides/AGENT_SESSION_PROTOCOL.md` — Agent usage documentation

## Verification

- `test-green`: MCP tool unit tests
- `smoke-green`: Server builds, UI loads, tools callable via curl
- `watched-live-green`: Session appears on Status Board when started

## Next Seam

1. Bulk seam creation from orphan tasks/proposals
2. Auto-detect inactive workstreams (no session > 24h)
3. Workstream completion workflow (verification ladder integration)

## Reality Gaps

- Semantic search currently returns no results for workstream-related queries — embeddings may need refresh
- Workstream nodes are created fresh on each session start (should check for existing active workstream)
- No automated cleanup of stale sessions (closed sessions remain as nodes)

---

**Created**: 2026-03-29  
**Slice**: codex/session-board-implementation  
**Verified**: smoke-green
