# Graph Intelligence Skill

Use the project graph as your primary orientation and context source.
The graph contains the full codebase structure, all proposals, seams,
tasks, code types/functions, git state, and decision history.

## When to Use

- **Session start**: Query the graph BEFORE reading raw files. It's faster and more complete.
- **Before any code change**: Check what types, traits, and functions exist in the target module.
- **Before any proposal/status change**: Verify the current status and valid transitions.
- **After completing work**: Record your decisions with traceability.
- **When asked about architecture**: Query the graph, don't guess from memory.

## Available Tools (MCP)

If the graph server is running (`phil graph serve`), these MCP tools are available:

| Tool | Use When |
|---|---|
| `graph_status` | First thing in a session — get the big picture |
| `graph_query` | Find nodes by kind (proposal, crate, type, function, seam) |
| `graph_node` | Deep dive on a specific node — see all its edges |
| `graph_skeleton` | Get a PlantUML diagram of a crate's types before modifying it |
| `graph_snippet` | Get function signatures (or full bodies) without reading entire files |
| `graph_search` | Find anything by text search across code and docs |
| `graph_proposals` | See the proposal pipeline — what's active, what's blocked |
| `graph_decide` | Record a traced decision (ALWAYS do this for status changes) |
| `graph_scan` | Trigger a rescan after making changes |

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

If neither MCP nor CLI is available, the REST API is at `http://localhost:8900`:

```
GET /api/status              # overall stats
GET /api/proposals           # all proposals
GET /api/nodes?kind=type     # all types in the codebase
GET /api/nodes/:id           # single node with edges
GET /api/snippets/:node_id   # code snippets for a node
GET /api/skeleton/:crate     # PlantUML diagram
GET /api/search?q=text       # full-text search
```

## Session Protocol

1. **Orient**: `graph_status` → understand the current state
2. **Locate**: `graph_query` or `graph_search` → find what you need
3. **Inspect**: `graph_node` + `graph_snippet` → understand the code
4. **Work**: Make your changes
5. **Record**: `graph_decide` → trace what you did and why
6. **Rescan**: `graph_scan` → update the graph with your changes

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
