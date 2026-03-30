# Graph as Source of Truth

## Core Principle

**The SQLite graph is the canonical source of truth.** Markdown files are human-readable projections, not authorities. Agents mutate state via MCP tools; the graph owns the state; optional writeback keeps markdown files synchronized for human consumption.

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                         AGENTS                                  │
│  (Claude, Codex, Jane, Perplexity, Cursor, etc.)               │
└─────────────────┬───────────────────────────────────────────────┘
                  │
                  │ MCP Tools
                  │ graph_create_node()
                  │ graph_update_node()
                  │ graph_create_edge()
                  │ graph_decide()
                  │ graph_writeback()
                  ▼
┌─────────────────────────────────────────────────────────────────┐
│              GRAPH INTELLIGENCE SERVER                          │
│                                                                 │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐        │
│  │ REST API     │  │ WebSocket    │  │ MCP Server   │        │
│  │ :8900        │  │ /ws          │  │ :8901        │        │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘        │
│         └─────────────────┼─────────────────┘                  │
│                           ▼                                    │
│              ┌─────────────────────┐                           │
│              │   GraphEngine     │                           │
│              │   (SQLite + FTS)  │                           │
│              │   nodes, edges,     │                           │
│              │   snippets,       │                           │
│              │   mutations       │                           │
│              └───────────────────┘                           │
└─────────────────────────────────────────────────────────────────┘
                  │
                  │ Optional Writeback
                  │ (serialize to markdown)
                  ▼
┌─────────────────────────────────────────────────────────────────┐
│              MARKDOWN PROJECTIONS                               │
│                                                                 │
│  docs/architecture/   (proposals, architecture docs)            │
│  docs/process/        (workflows, tasks)                        │
│  docs/reference/      (glossary, domain maps)                   │
│                                                                 │
│  These files are READABLE VIEWS of graph state.              │
│  Humans edit them; agents scan them into the graph.            │
│  Writeback updates them to match graph truth.                 │
└─────────────────────────────────────────────────────────────────┘
```

---

## State Flow

### Current Flow (OLD)
```
Agent edits markdown → File saved → Scan picks up changes → Graph updated
```
**Problem**: Race conditions, stale cache, scanner bugs, file not reflecting graph truth

### New Flow (Graph as Source of Truth)
```
Agent calls graph_update_node() → Graph updated → Optional writeback → File updated
                                  ↓
                           WebSocket broadcast
                                  ↓
                           UI live updates
```
**Benefits**: Single source of truth, immediate consistency, audit trail via mutations table

---

## MCP Mutation Tools

| Tool | Purpose | When to Use |
|------|---------|-------------|
| `graph_create_node` | Create new proposal, seam, task, decision | Starting new work |
| `graph_update_node` | Update status, properties, tags | Status changes, adding metadata |
| `graph_create_edge` | Link nodes (proposal→seam, doc→task) | Establishing relationships |
| `graph_decide` | Record architectural decisions | Decision capture with audit trail |
| `graph_writeback` | Sync graph state to markdown | Keeping human-readable files current |

---

## Example: Updating Proposal Status

### OLD WAY (Don't do this)
```bash
# Edit file directly - BAD
sed -i 's/status: proposed/status: accepted-current-slice/' docs/architecture/MY_PROPOSAL.md
```

### NEW WAY (Graph as Source of Truth)
```bash
# Call MCP tool to update graph - GOOD
curl -X POST http://localhost:8901/mcp \
  -d '{
    "jsonrpc": "2.0",
    "method": "tools/call",
    "params": {
      "name": "graph_update_node",
      "arguments": {
        "id": "doc:my-proposal",
        "properties": {"status": "accepted-current-slice"},
        "reason": "Implementation complete, ready for next phase"
      }
    },
    "id": 1
  }'

# Write back to markdown for humans
curl -X POST http://localhost:8901/mcp \
  -d '{
    "jsonrpc": "2.0",
    "method": "tools/call",
    "params": {
      "name": "graph_writeback",
      "arguments": {
        "node_id": "doc:my-proposal",
        "commit": true,
        "agent": "Claude",
        "reason": "Status change from graph"
      }
    },
    "id": 2
  }'
```

---

## Frontmatter Schema (Graph-Owned)

Fields the graph captures and maintains:

```yaml
---
title: "Proposal Title"
doc_type: proposal              # proposal | reference | workflow | decision
domain: runtime-sessions        # From controlled vocabulary
status: accepted-current-slice  # Graph is source of truth
disposition: accepted           # Alternative status field
proposal_id: my-proposal        # Node ID suffix

# Relationships (edges in graph)
related_docs:
  - ARCHITECTURE_STATUS.md
  - SEAM_REGISTRY.md
task_refs:
  - docs/task.md
active_seams:
  - seam-id-1
  - seam-id-2

# Metadata
source_of_truth_targets:        # Files this doc writes back to
  - ARCHITECTURE_STATUS.md
tags:
  - graph
  - intelligence
last_updated: 2026-03-29        # Auto-updated by writeback
---
```

---

## Source of Truth Targets

Certain architecture files are **derived** from the graph:

| File | Derivation |
|------|-----------|
| `ARCHITECTURE_STATUS.md` | Aggregate of all proposal statuses, active seams, current slices |
| `SEAM_REGISTRY.md` | Union of all `active_seams` from proposals |
| `ROADMAP.md` | Dependency-ordered view of seams across proposals |
| `ARCH_RULES.md` | Rules from `accepted`/`implemented` proposals |

These files should be **generated**, not hand-edited. Use `source_of_truth_targets` in proposal frontmatter to declare what files your proposal affects.

---

## Agent Workflow Rules

### DO:
- Use `graph_update_node` for status changes
- Use `graph_create_edge` to link proposals to seams
- Use `graph_writeback` after significant changes
- Check graph state via `graph_status` or `graph_query` before acting
- Record decisions via `graph_decide` with full reasoning

### DON'T:
- Edit markdown frontmatter directly for status/disposition
- Assume file state matches graph state
- Use file modification time as truth
- Edit `source_of_truth_targets` files by hand

---

## Verification

To verify graph state vs files:

```bash
# Check graph status
curl -s http://localhost:8900/api/nodes/doc:graph-intelligence | jq '.properties.status'

# Check file status
grep "^status:" docs/architecture/GRAPH_INTELLIGENCE_PROPOSAL.md

# If different, file is stale - run writeback
```

---

## Recovery

If graph and files diverge:

1. **Graph is correct, files are stale**: Run `graph_writeback` for affected nodes
2. **Files are correct, graph is stale**: Trigger `/api/scan` to re-index
3. **Both wrong**: Fix via `graph_update_node`, then `graph_writeback`

The mutation log (`/api/mutations`) is the audit trail of truth.

---

## Future: Full Writeback Automation

Planned: Periodic job that:
1. Queries all nodes with `source_of_truth_targets`
2. Generates derived files (STATUS, REGISTRY, ROADMAP)
3. Commits with traceable message
4. Never conflicts with human edits (graph always wins)

---

## Related Proposals

- `DOC_TAGGING_FRONTMATTER_PROPOSAL.md` - Frontmatter schema
- `ARCH_RULES_AND_ROADMAP_PROPOSAL.md` - Rules registry concept
- `GRAPH_INTELLIGENCE_PROPOSAL.md` - This implementation

---

## Disposition

`accepted-current-slice` - Active implementation in progress.

Last updated: 2026-03-29
