# Graph Intelligence Documentation Status

## Summary: What We Have Documented

### Core Architecture Documents

| Document | Status | Purpose |
|----------|--------|---------|
| **GRAPH_INTELLIGENCE_PROPOSAL.md** | `accepted-current-slice` | Original proposal for graph system |
| **GRAPH_AS_SOURCE_OF_TRUTH.md** | Complete | Architecture vision: graph owns truth, files are projections |
| **DOC_TAGGING_FRONTMATTER_PROPOSAL.md** | `accepted-current-slice` | Frontmatter schema for all docs |
| **ARCHITECTURE_STATUS.md** | Updated 2026-03-29 | Current implementation status with Graph Intelligence section |
| **GLOSSARY.md** | Updated | Core terms + graph vocabulary (node, edge, mcp, writeback, mutation) |

### Crate Documentation

| Document | Location | Coverage |
|----------|----------|----------|
| **README.md** | `crates/graph-intelligence/` | Complete API reference, UI features, MCP tools, REST endpoints |
| **ui/index.html** | `crates/graph-intelligence/ui/` | Web UI with drill-down for proposals/seams/tasks/crates |

---

## Web UI Features (Documented)

### Views
- **Dashboard** - Stats, pipeline, active tasks, activity feed, crate overview
- **Proposals** - Kanban board (5 columns: Proposed → Implemented)
- **Proposal Detail** - Properties, seams, diagrams (tabs), tasks, activity
- **Seams** - Table with status, tasks, tests, verification ladder
- **Seam Detail** - Properties, architecture diagram, tasks, tests, code, ladder
- **Tasks** - Table with status, priority, proposal, seam, agent
- **Task Detail** - Properties + links to parent proposal/seam
- **Crates** - Tree view with expandable modules
- **Crate Detail** - Architecture tabs (Class/C4/Interactions), modules, related proposals
- **Search** - Full-text with navigation by node kind
- **Timeline** - Mutation history with sorting

### Interactive Features
- **Clickable Cards** - All proposal/seam/task cards navigate to detail pages
- **Breadcrumb Navigation** - Hierarchical path through views
- **Refresh Buttons** - Every view has manual refresh
- **Live Indicator** - WebSocket connection status in sidebar
- **Diagram Tabs** - Proposal, Container, Context views with PlantUML
- **Copy/Render** - PlantUML source copy + PNG generation
- **Verification Ladder** - T/S/L badges on seam tables

---

## MCP Tools (Documented)

### Query Tools
```
graph_status       → Node/edge/snippet counts
graph_query        → Filter by kind, worktree, status
graph_node         → Single node with edges
graph_proposals    → All proposals with status/seams
graph_skeleton     → PlantUML class diagram
graph_snippet      → Code signatures/bodies
graph_search       → Full-text search
graph_scan         → Trigger rescan
```

### Mutation Tools
```
graph_create_node  → Create proposal/seam/task/decision
graph_update_node  → Update status, properties
graph_create_edge  → Link nodes (proposal→seam, etc.)
graph_decide       → Record decisions with audit trail
graph_writeback    → Sync graph to markdown files
```

---

## REST API Endpoints (Documented)

### Core
```
GET /api/status
GET /api/nodes?kind={kind}
GET /api/nodes/{id}
GET /api/proposals
GET /api/seams
GET /api/search?q={query}
GET /api/mutations?limit={n}
POST /api/scan
```

### Diagrams
```
GET /api/skeleton/{crate}
GET /api/c4/context/{system}
GET /api/c4/container/{system}
GET /api/c4/component/{crate}
GET /api/c4/proposal/{id}
GET /api/c4/seam/{id}
GET /api/diagram/sequence/{fn}
GET /api/diagram/state/{enum}
GET /api/diagram/interactions/{crate}
```

---

## Frontmatter Schema (Implemented)

```yaml
---
title: "Title"
doc_type: proposal              # proposal|seam|task-surface|reference|workflow|status|historical|architecture
domain: runtime-sessions
status: accepted-current-slice
disposition: accepted
proposal_id: unique-id

# Relationships
related_docs: []
task_refs: []
active_seams: []
implements: []
implemented_by: []

# Metadata
source_of_truth_targets: []
tags: []
last_updated: 2026-03-29
---
```

---

## What's New (Not in Original Proposal)

1. **Full Web UI** - Complete drill-down interface (not just API)
2. **Real-time Updates** - WebSocket push for live UI refresh
3. **C4 Diagrams** - Context, Container, Component, Proposal, Seam levels
4. **Behavioral Diagrams** - Sequence, State, Module Interactions
5. **Verification Ladder** - T/S/L badges in UI
6. **MCP Mutation Tools** - Agents can now own the graph
7. **Reverse Writeback** - Graph → markdown serialization
8. **Graph as Source of Truth** - New architecture paradigm
9. **Source of Truth Targets** - Which files a proposal affects
10. **Task Integration** - Tasks shown in proposals/seams, full task view

---

## Gaps (Minor)

| Item | Status | Notes |
|------|--------|-------|
| `philotic-graph` crate | Not started | SVER-aware driver, CLI (`phil graph`) |
| `query.rs` module | Not separated | Currently in `engine.rs` |
| `fleet.rs` | Not started | Agent identity registry |
| `muninn_bridge.rs` | Not started | Session memory integration |
| `self_model.rs` | Not started | Agent self-reflection queries |
| Auto-writeback job | Planned | Periodic sync of derived files |
| ARCH_RULES.md generation | Not started | Aggregate rules from implemented proposals |
| ROADMAP.md generation | Not started | Dependency-ordered seam view |

---

## Documentation Quick Reference

```
docs/architecture/
├── GRAPH_INTELLIGENCE_PROPOSAL.md      (original proposal)
├── GRAPH_AS_SOURCE_OF_TRUTH.md         (architecture vision)
├── DOC_TAGGING_FRONTMATTER_PROPOSAL.md (frontmatter schema)
├── ARCHITECTURE_STATUS.md              (current state - updated 2026-03-29)
├── GLOSSARY.md                         (vocabulary - updated with graph terms)
└── SEAM_REGISTRY.md                    (seam definitions)

crates/graph-intelligence/
├── README.md                           (complete API/UI reference)
└── ui/index.html                       (live web interface)

Servers:
├── Web UI:    http://localhost:8900
├── REST API:  http://localhost:8900/api/
├── MCP:       http://localhost:8901/mcp
└── WebSocket: ws://localhost:8900/ws
```

---

## Status: IMPLEMENTED

The Graph Intelligence system is **fully functional** with:
- ✅ SQLite graph engine
- ✅ Code/doc/git scanners
- ✅ REST/MCP/WebSocket servers
- ✅ Complete web UI with drill-down
- ✅ C4 and behavioral diagrams
- ✅ Full-text search
- ✅ Mutation audit trail
- ✅ Graph-as-source-of-truth architecture
- ✅ Agent-facing mutation tools
- ✅ Optional writeback to markdown

**Next slice**: Auto-generation of derived files (STATUS, REGISTRY, ROADMAP) from graph state.
