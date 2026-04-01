# Graph Intelligence Skill

Use the project graph as your primary orientation and context source.
The graph contains the full codebase structure, all proposals, seams,
tasks, code types/functions, git state, decision history, and agent sessions.

## When to Use

- **Session start**: Query the graph BEFORE reading raw files. It's faster and more complete.
- **Before any code change**: Check what types, traits, and functions exist in the target module.
- **Before any proposal/status change**: Verify the current status and valid transitions.
- **After completing work**: Record your decisions with traceability.
- **When asked about architecture**: Query the graph, don't guess from memory.
- **When picking what to work on**: Use `graph_next_task` to get a scored recommendation.
- **When assessing change impact**: Use `graph_impact` before committing.

## Recommended Agent Workflow

This is the standard flow for an agent picking up and completing work:

```
1. graph_next_task           → scored recommendation for highest-priority unclaimed work
2. graph_context_for         → one-call context: proposal body + seams + code + verification + diagram + conflict check
3. session_start             → claim the work, visible on dashboard
4. ... do the work ...
5. graph_impact              → blast-radius analysis: file → proposals → seams → tests
6. graph_advance_verification → move SVER state (e.g. proposed → test-green)
7. graph_decide              → record what you did and why
8. session_close             → release claim, visible on dashboard
9. graph_scan                → update the graph with your changes (auto-persists PlantUML diagrams)
```

⚠️ **CRITICAL**: Before calling `session_start`, you MUST verify the seam exists and follow the full session protocol. See the `session-hygiene` skill for the complete agent session protocol. Failure to follow this protocol creates orphaned workstreams and breaks the coordination system.

Use `graph_agent_dashboard` at any time to see who else is working and on what.

## Available Tools (MCP)

If the graph server is running (`just intel-graph-start`), these MCP tools are available at `http://127.0.0.1:8901/mcp`:

### Orientation & Query

| Tool | Use When |
|---|---|
| `graph_status` | First thing in a session — node/edge counts, proposal pipeline |
| `graph_query` | Find nodes by kind (proposal, crate, type, function, seam). Use `compact: true` to save tokens. |
| `graph_node` | Deep dive on a specific node — see all its edges |
| `graph_search` | Full-text search across code and docs |
| `graph_proposals` | Proposal pipeline with disposition, verification level, and active seams |
| `graph_digest` | Compressed domain→proposal→seam→verification overview in one call |

### Context & Planning

| Tool | Use When |
|---|---|
| `graph_context_for` | **One-call context assembly**: proposal body + seams + code signatures + verification + decisions + active sessions + PlantUML diagram |
| `graph_next_task` | **Scored work recommendation** with conflict avoidance (checks active sessions) |
| `graph_impact` | **Blast-radius analysis**: walks edges from a file or node to affected proposals, seams, and tests |

### Code Inspection

| Tool | Use When |
|---|---|
| `graph_skeleton` | PlantUML class diagram of a crate's types before modifying it |
| `graph_snippet` | Function signatures (or full bodies) without reading entire files |
| `graph_diagram` | Generate any PlantUML diagram: `c4_context`, `c4_container`, `c4_component`, `proposal_architecture`, `seam_detail`, `sequence`, `state`, `module_interaction`, `crate_classes` |

### Session Lifecycle

| Tool | Use When |
|---|---|
| `session_start` | Claim a workstream — creates Session + Workstream nodes, links to seam/proposal |
| `session_activity` | Report progress during work (files touched, phase changes) |
| `session_close` | Release the workstream claim when done |

### Mutation & Decisions

| Tool | Use When |
|---|---|
| `graph_decide` | Record a traced decision (ALWAYS do this for status changes) |
| `graph_advance_verification` | Move a proposal through the SVER ladder |
| `graph_mutate` | Direct node/edge mutations when the graph needs updating |
| `graph_scan` | Trigger a full rescan (also auto-persists PlantUML diagrams to `docs/architecture/generated/`) |

### Export & Persistence

| Tool | Use When |
|---|---|
| `graph_export_docs` | Sync graph state back to doc file frontmatter (supports `dry_run: true`) |
| `graph_export_sver` | Export SVER verification state to markdown |
| `graph_persist_diagrams` | Write canonical PlantUML diagrams to `docs/architecture/generated/` |
| `graph_agent_dashboard` | See all agent sessions, verification progress, per-agent summaries |

### Semantic (requires ONNX sidecar)

| Tool | Use When |
|---|---|
| `graph_embed` | Embed a single node |
| `graph_embed_batch` | Batch embed by kind (e.g. all proposals) |
| `graph_semantic_search` | Similarity search across embedded nodes |
| `graph_verify_semantic` | Check proposal↔code semantic alignment |

## Available CLI

If MCP isn't available, use the CLI:

```bash
phil graph status             # orientation — counts and proposal pipeline
phil graph proposals          # all proposals with status
phil graph seams              # all registered seams
phil graph skeleton <crate>   # PlantUML for a crate
phil graph search "<text>"    # find nodes and snippets
phil graph scan               # rescan after changes
```

## REST API

The REST API is at `http://localhost:8900`. Agents without MCP can use these endpoints directly.

```
# Read-only orientation
GET  /api/status              # overall stats
GET  /api/proposals           # all proposals
GET  /api/nodes?kind=type     # all types in the codebase
GET  /api/nodes/:id           # single node with edges
GET  /api/snippets/:node_id   # code snippets for a node
GET  /api/skeleton/:crate     # PlantUML diagram
GET  /api/search?q=text       # full-text search

# Agent workflow (full lifecycle via REST)
GET  /api/next_task           # recommended next work item
GET  /api/context/:target_id  # one-call context assembly (equivalent to graph_context_for)
GET  /api/dashboard           # agent activity dashboard (sessions, verification, per-agent)
GET  /api/impact/:target      # blast-radius analysis for a node or file
POST /api/session/start       # start a session (body: session_id, agent, seam_id, ...)
POST /api/session/close       # close a session (body: session_id, summary, files_touched)
POST /api/session/cleanup     # auto-close stale sessions (body: max_age_hours)
POST /api/decide              # record a decision (body: target_node, action, reason, agent, ...)
POST /api/test-run            # record test results (body: target_id, test_count, pass_count, ...)
POST /api/scan                # trigger a full rescan

# Health monitoring
GET  /api/health              # combined system health (sessions + proposals + graph)
GET  /api/health/sessions     # session hygiene: stale, orphaned, overloaded
GET  /api/health/proposals    # proposal pipeline: dispositions, verification, embeddings
```

## Session Protocol

1. **Orient**: `graph_status` or `graph_digest` → understand the current state
2. **Pick work**: `graph_next_task` → scored recommendation with conflict check
3. **Load context**: `graph_context_for` → proposal + seams + code + diagram in one call
4. **Claim**: `session_start` → register your session against the target seam/proposal
5. **Work**: Make your changes, use `graph_snippet` and `graph_node` as needed
6. **Check impact**: `graph_impact` → see what your changes affect
7. **Record**: `graph_decide` → trace what you did and why
8. **Advance verification**: `graph_advance_verification` → move SVER state
9. **Release**: `session_close` → release the workstream claim
10. **Rescan**: `graph_scan` → update the graph (auto-persists PlantUML diagrams)

## Decision Recording

EVERY significant decision MUST be recorded in the graph. This creates
the audit trail that makes the system self-documenting.

```
graph_decide({
  target_node: "proposal:desktop-membrane",
  action: "status_change",
  from_value: "proposed",
  to_value: "in-progress",
  reason: "Starting seam 4 — provider-native streaming",
  agent: "aria",
  session: "aria-2026-03-28-002"
})
```

Categories of decisions to record:
- Proposal status changes
- Architectural choices ("chose X over Y because Z")
- Seam completion
- Deferred work with rationale
- Bug discoveries linked to the affected module

## Traceability Rules

- Always include your agent name and session ID in decisions
- Link decisions to the specific proposal, seam, or module they affect
- Include a human-readable reason — future agents will read this
- The graph writeback will auto-commit frontmatter changes with your provenance

## Starting the Graph Server

```bash
just intel-graph-start       # start ONNX sidecar + graph intelligence server
just intel-graph-status      # check if running
just intel-graph-health      # health check both services
just intel-graph-ui          # open the web UI
```

The MCP endpoint is `http://127.0.0.1:8901/mcp`. The REST API is `http://127.0.0.1:8900`.
The web UI provides a visual dashboard at `http://127.0.0.1:8900`.

## Maintenance & Hygiene

Use these justfile recipes for ongoing graph health:

```bash
just intel-graph-health-check       # combined health: sessions + proposals + graph
just intel-graph-session-health     # session hygiene report
just intel-graph-session-cleanup    # auto-close stale sessions (default 4h)
just intel-graph-session-cleanup 8  # custom max age in hours
just intel-graph-proposal-health    # proposal pipeline health
just intel-graph-embed-proposals    # batch embed all proposals
just intel-graph-embed-all          # batch embed all embeddable node kinds
just intel-graph-maintain           # full maintenance: scan + cleanup + health + embed
```

Related skills:
- [$session-hygiene](../session-hygiene/SKILL.md) — session lifecycle monitoring and cleanup
- [$verification-orchestrator](../verification-orchestrator/SKILL.md) — SVER state and test-run pipeline
- [$proposal-pipeline](../proposal-pipeline/SKILL.md) — proposal lifecycle and metadata hygiene
