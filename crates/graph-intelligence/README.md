# Graph Intelligence

**Query-able, always-current graph representation of the entire development surface.**

This crate provides the core graph engine, scanners, and web UI for the Philotic Stack intelligence system. It indexes code, documentation, git history, process state, and agent decisions into a queryable SQLite graph.

---

## Quick Start

```bash
# Build and run the server
cargo run -p philotic-web -- graph serve

# Server endpoints:
# - Web UI: http://localhost:8900
# - REST API: http://localhost:8900/api/
# - MCP Server: http://localhost:8901/mcp
# - WebSocket: ws://localhost:8900/ws
```

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     GRAPH INTELLIGENCE                       │
├─────────────────────────────────────────────────────────────┤
│                                                              │
│  Scanners → Graph Engine → Query API → Servers → UI/Agents  │
│                                                              │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐    │
│  │ Code     │  │ Docs     │  │ Git      │  │ Mutations│    │
│  │ Scanner  │  │ Scanner  │  │ Scanner  │  │ Log      │    │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘    │
│       └──────────────┴──────────────┴──────────────┘         │
│                      │                                       │
│              ┌───────▼───────┐                               │
│              │ GraphEngine   │                               │
│              │ (SQLite + FTS)│                               │
│              └───────┬───────┘                               │
│                      │                                       │
│       ┌──────────────┼──────────────┐                        │
│       ▼              ▼              ▼                        │
│  ┌────────┐    ┌────────┐    ┌────────┐                    │
│  │ REST   │    │ MCP    │    │ WebSock│                    │
│  │ API    │    │ Server │    │ et     │                    │
│  └────┬───┘    └────┬───┘    └────┬───┘                    │
│       └──────────────┴──────────────┘                        │
│                      │                                       │
│              ┌───────▼───────┐                               │
│              │ Web UI      │                               │
│              │ (Vanilla JS)│                               │
│              └─────────────┘                               │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

---

## Web UI Features

The web interface provides real-time visibility into the graph with drill-down capabilities.

### Dashboard
- **Stats Cards**: Clickable counts for Proposals, Seams, Tasks, Crates, Types, Functions
- **Proposal Pipeline**: Visual bar showing distribution by status (Proposed, Accepted, In Progress, Implemented, Deferred)
- **Active Tasks**: List of pending/in-progress tasks with priority indicators
- **Recent Activity**: Mutation timeline showing agent actions
- **Crate Overview**: Table of all crates in the workspace

### Proposals (Kanban View)
- **5-column Kanban**: Proposed → Accepted → In Progress → Implemented → Deferred
- **Proposal Cards**: Show name, domain, seam count, assigned agent
- **Click to Drill Down**: Opens detailed proposal page

### Proposal Detail Page
- **Breadcrumb Navigation**: Dashboard > Proposals > [Proposal Name]
- **Status Badge**: Current disposition
- **Properties Grid**: Domain, doc_type, last_updated, proposal_id, current_slice, assigned_agent
- **Active Seams List**: Clickable links to seam detail pages
- **Tags**: Visual badge display
- **Architecture Diagrams**: Tabs for Proposal View, C4 Container, C4 Context
  - PlantUML source display
  - Copy button
  - Render button (generates PNG via plantuml.com)
- **Related Tasks**: Tasks linked to this proposal
- **Recent Activity**: Mutation history for this proposal

### Seams (Table View)
- **Columns**: Name, Status, Owning Proposal, Tasks, Tests, Verification Ladder
- **Status**: Derived from parent proposal (or "orphaned")
- **Verification Ladder**: T/S/L badges (Test/Smoke/Live)
- **Click to Drill Down**: Opens seam detail page

### Seam Detail Page
- **Breadcrumb Navigation**: Dashboard > Seams > [Proposal?] > [Seam Name]
- **Parent Proposal Link**: If linked, shows proposal name
- **Properties Grid**: All seam properties
- **Architecture Diagram**: PlantUML diagram specific to seam
- **Related Tasks**: Tasks assigned to this seam
- **Related Tests**: Tests covering this seam
- **Implementing Code**: Code nodes linked via edges
- **Verification Ladder**: Current verification status
- **Recent Activity**: Mutation history

### Tasks (Table View)
- **Columns**: Name, Status, Priority, Proposal, Seam, Agent
- **Status Colors**: pending (gray), in_progress (amber), completed (green)
- **Click to Drill Down**: Task detail page

### Task Detail Page
- **Properties Grid**: All task properties
- **Navigation Links**: Jump to parent Proposal or Seam

### Crates (Tree View)
- **Expandable Tree**: Crate > Modules > Types/Functions
- **PlantUML Button**: Generate skeleton diagram per crate
- **Detail Button**: Navigate to crate detail page

### Crate Detail Page
- **Architecture Tabs**:
  - Class Diagram (PlantUML skeleton)
  - C4 Component diagram
  - Module Interactions diagram
- **Modules List**: All modules in crate
- **Related Proposals**: Proposals affecting this crate

### Search
- **Full-text Search**: Across nodes, types, functions, snippets
- **Search Results**: Grouped by kind with file paths
- **Click Navigation**: Routes to appropriate detail page based on node kind

### Timeline
- **Mutation History**: All graph changes
- **Columns**: Time, Agent, Action, Target, Reason
- **Sortable Headers**: Click to sort

---

## MCP Tools (for Agents)

Agents interact with the graph via Model Context Protocol tools:

### Query Tools
| Tool | Description |
|------|-------------|
| `graph_status` | Get node counts, edge count, snippet count |
| `graph_query` | Query nodes by kind, worktree, status |
| `graph_node` | Get single node with edges |
| `graph_proposals` | List all proposals with status and seams |
| `graph_skeleton` | Generate PlantUML class diagram for crate |
| `graph_snippet` | Get code snippets for node |
| `graph_search` | Full-text search |
| `graph_scan` | Trigger full rescan |

### Mutation Tools
| Tool | Description |
|------|-------------|
| `graph_create_node` | Create proposal, seam, task, decision |
| `graph_update_node` | Update status, properties |
| `graph_create_edge` | Create relationships |
| `graph_decide` | Record architectural decisions |
| `graph_writeback` | Sync graph to markdown files |

### Example: Update Proposal Status
```json
{
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "name": "graph_update_node",
    "arguments": {
      "id": "doc:my-proposal",
      "properties": {"status": "accepted-current-slice"},
      "reason": "Implementation complete"
    }
  },
  "id": 1
}
```

---

## REST API Endpoints

| Endpoint | Description |
|----------|-------------|
| `GET /api/status` | Node counts, edge count, snippet count |
| `GET /api/nodes` | List nodes (optional `?kind=` filter) |
| `GET /api/nodes/{id}` | Single node with edges |
| `GET /api/proposals` | All proposals |
| `GET /api/seams` | All seams |
| `GET /api/skeleton/{crate}` | PlantUML skeleton |
| `GET /api/snippets/{node_id}` | Code snippets |
| `GET /api/search?q={query}` | Full-text search |
| `GET /api/mutations?limit=N` | Mutation history |
| `POST /api/scan` | Trigger rescan |

### C4 Diagram Endpoints
| Endpoint | Description |
|----------|-------------|
| `GET /api/c4/context/{system}` | C4 Context diagram |
| `GET /api/c4/container/{system}` | C4 Container diagram |
| `GET /api/c4/component/{crate}` | C4 Component diagram |
| `GET /api/c4/proposal/{id}` | Proposal architecture diagram |
| `GET /api/c4/seam/{id}` | Seam detail diagram |

### Behavioral Diagrams
| Endpoint | Description |
|----------|-------------|
| `GET /api/diagram/sequence/{fn_id}` | Sequence diagram |
| `GET /api/diagram/state/{enum_id}` | State machine diagram |
| `GET /api/diagram/interactions/{crate}` | Module interaction diagram |

---

## Graph Schema

### Node Kinds
- `proposal` - Architecture proposals
- `seam` - Cross-cutting concerns
- `task` - Work items
- `slice` - Implementation slices
- `domain` - Architectural domains
- `crate` - Rust crates
- `module` - Rust modules
- `type` - Structs, enums, traits
- `function` - Functions
- `test` - Test functions
- `commit` - Git commits
- `branch` - Git branches
- `decision` - Recorded decisions

### Edge Relations
- `implements` - proposal → seam
- `implemented_by` - seam → code
- `depends_on` - seam → seam
- `contains` - crate → module → type → fn
- `governs` - domain → proposal
- `references` - doc → doc
- `tests` - test → function
- `blocks` - task → task
- `applies_to` - proposal → seam
- `imports` - module → module

---

## WebSocket Events

Real-time updates pushed to UI:

| Event | Payload |
|-------|---------|
| `node_created` | `{ node_id, kind, name }` |
| `node_updated` | `{ node_id, updated_properties }` |
| `edge_created` | `{ source_id, target_id, relation }` |
| `mutation_recorded` | `{ mutation_id, target_node, action }` |
| `scan_complete` | `{ crates, modules, types, duration_ms }` |

---

## Frontmatter Schema

Graph captures these fields from markdown frontmatter:

```yaml
---
title: "Document Title"
doc_type: proposal              # proposal | reference | workflow | seam | task-surface | status | historical | architecture
domain: runtime-sessions        # From controlled vocabulary
status: accepted-current-slice  # Graph is source of truth
disposition: accepted          # Alternative status
proposal_id: unique-id           # Node ID suffix

# Relationships
related_docs:
  - ARCHITECTURE_STATUS.md
task_refs:
  - docs/task.md
active_seams:
  - seam-id-1
  - seam-id-2
implements: []
implemented_by: []

# Metadata
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
tags:
  - tag1
  - tag2
last_updated: 2026-03-29
---
```

---

## Graph as Source of Truth

The SQLite graph is the canonical source of truth. Markdown files are human-readable projections.

**State Flow:**
1. Agent calls `graph_update_node()` via MCP
2. Graph updates immediately
3. WebSocket broadcasts change
4. UI refreshes automatically
5. Optional: `graph_writeback()` syncs to markdown

See `GRAPH_AS_SOURCE_OF_TRUTH.md` for full architecture.

---

## File Structure

```
crates/graph-intelligence/
├── src/
│   ├── lib.rs              # Public API
│   ├── engine.rs           # SQLite graph engine
│   ├── schema.rs           # Node, Edge, Mutation types
│   ├── plantuml.rs         # Diagram generation
│   ├── c4.rs               # C4 model diagrams
│   ├── diagrams.rs         # Sequence/state diagrams
│   ├── writeback.rs        # Frontmatter serialization
│   ├── scanner/
│   │   ├── mod.rs
│   │   ├── code.rs         # Rust AST scanner
│   │   ├── docs.rs         # Markdown frontmatter scanner
│   │   └── git.rs          # Git scanner
│   └── server/
│       ├── mod.rs
│       ├── api.rs          # REST API (axum)
│       ├── mcp.rs          # MCP tool server
│       └── ws.rs           # WebSocket server
├── ui/
│   └── index.html          # Web UI (vanilla JS)
└── tests/
    └── writeback_test.rs   # Writeback tests
```

---

## Development

### Running Tests
```bash
cargo test -p graph-intelligence
```

### Building UI
The UI is a single HTML file at `ui/index.html`. No build step required.

### Adding MCP Tools
1. Add tool definition to `tool_definitions()` in `src/server/mcp.rs`
2. Add tool handler to `execute_tool()` match statement
3. Implement handler function (see existing examples)

---

## Related Documents

- `GRAPH_AS_SOURCE_OF_TRUTH.md` - Architecture vision
- `GRAPH_INTELLIGENCE_PROPOSAL.md` - Original proposal
- `DOC_TAGGING_FRONTMATTER_PROPOSAL.md` - Frontmatter schema
- `ARCHITECTURE_STATUS.md` - Current implementation status
