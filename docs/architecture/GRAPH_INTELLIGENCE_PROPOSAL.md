---
title: Graph Intelligence — Project Context Engine
doc_type: proposal
domain: product-management-plane
status: accepted-current-slice
last_updated: 2026-04-24
tags:
- graph
- intelligence
- context
- mcp
- scanners
- sver
- agent-substrate
related_docs:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
- GRAPH_AS_SOURCE_OF_TRUTH.md
- SEAM_REGISTRY.md
- DOMAIN_MAP.md
- INTERACTIVE_ONBOARDING_PROPOSAL.md
- COGNITIVE_LOOP_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: graph-intelligence
implements: []
implemented_by: []
active_seams:
- graph-engine-schema
- code-scanner
- doc-scanner
- git-scanner
- snippet-store
- graph-query-api
- graph-mcp-server
- graph-writeback
- sver-process-model
- muninn-bridge
- graph-web-ui
- agent-self-model
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
- DOMAIN_MAP.md
- SEAM_REGISTRY.md
---

# Graph Intelligence — Project Context Engine

## Goal

Build a queryable, always-current graph representation of the entire
development surface — code, documentation, git history, process state,
and agent decisions — so that every agent harness and every human
operator starts every session with full awareness instead of re-reading
74K lines of Rust and 70+ architecture docs.

Two crates:

- **`graph-intelligence`** — generic, publishable engine: scans Rust
  codebases, indexes markdown frontmatter, tracks git state, serves
  queries via MCP/REST/WebSocket.
- **`philotic-graph`** — SVER-aware driver: process lifecycle, Muninn
  integration, agent traceability, `phil graph` CLI.

## Disposition

`proposed`

## Current Slice

- Make `DOMAIN_MAP.md` the authoritative catalog of first-class domain nodes.
- Link proposals to domains through graph edges, and link seams to their domain nodes from the seam registry.
- Treat `ARCHITECTURE_STATUS.md` as a legacy/transitional projection of graph state rather than the owner of truth.
- Keep `ARCHITECTURE.md` as the durable hierarchy reference, with generated UML/PlantUML diagrams for the graph-visible layers.
- Define orphan semantics in the graph health model:
  - **workstream orphan**: an active workstream with no active backing session
  - **seam orphan**: a seam with no adopted domain/proposal path or no live workstream adoption path
  - **resolution**: adopt by linking the seam to a proposal/domain and starting or reattaching a session; otherwise mark the seam closed/superseded/deferred and remove it from the active surface
- Add graph-native proposal management so status/disposition changes and
  agent work-focus records are recorded together. This makes proposal state a
  shared graph object while preserving each agent's operational stance toward
  that proposal as structured `agent_work_focus` state.

---

## The Problem

Every agent session starts from zero. Perplexity Computer, Claude Code,
Codex, Cursor, Jane — they all have to re-read the codebase, re-discover
the architecture, re-learn what's in flight. This burns tokens, time, and
context window. Humans face the same problem at a different scale: losing
track of which proposals are active, which seams block which, and what 36
commits in a feature branch actually changed.

The codebase already contains a rich entity-relationship model (60+
proposal IDs, 15 domains, 72 seams, 10 status values, explicit
relationship edges in frontmatter) but it's trapped in flat files that
no agent can query and no human can visualize.

## The Deeper Value: Self-Describing Infrastructure

Once the agent fleet uses the graph to understand the codebase, the
graph becomes the manual for how the agents themselves work. An agent
can query its own operational graph:

- "What proposals govern my behavior?"
- "What code implements my cognitive loop?"
- "What decisions did my previous sessions make?"
- "What patterns emerge from my routing history?"

The architecture documentation and the running system become projections
of the same truth. The graph doesn't describe the system — it IS the
system's self-model. Agents reading the graph to understand how they
work, then updating it through their actions, creates a continuous
self-documenting feedback loop.

New agents (or new instances) don't need briefing documents. They query
the graph: "What am I? What's my role? What did my predecessors learn?"

---

## Architecture

### Two-Crate Split

```
graph-intelligence/           (generic, publishable on crates.io)
  Scanner → Graph Engine → Query API → MCP/REST/WebSocket

philotic-graph/               (SVER-aware, philotic-stack workspace)
  SVER Process Model + Muninn Bridge + Agent Traceability + CLI
```

#### `graph-intelligence` (generic engine)

Knows nothing about SVER, seams, proposals, Muninn, or philotic-stack.
It scans Rust codebases, indexes markdown with YAML frontmatter, tracks
git state, and serves queries. Anyone with a Rust workspace and markdown
docs can use it.

```
src/
  lib.rs              public API
  schema.rs           nodes, edges, mutations, snippets
  engine.rs           SQLite graph engine
  query.rs            graph query interface
  writeback.rs        frontmatter mutation serializer
  plantuml.rs         skeleton generation
  scanner/
    mod.rs
    code.rs           syn-based Rust AST scanner
    docs.rs           YAML frontmatter + markdown scanner
    git.rs            commits, branches, worktrees
  server/
    mod.rs
    api.rs            REST (axum)
    mcp.rs            MCP tool server
    ws.rs             WebSocket change feed
```

#### `philotic-graph` (SVER driver)

```
src/
  lib.rs
  sver.rs             SVER process model (proposal lifecycle,
                       seam states, slice tracking, valid transitions)
  muninn_bridge.rs    session → Muninn memory integration
  config.rs           frontmatter field mapping, node type registry,
                       domain definitions
  fleet.rs            agent identity registry for traceability
  self_model.rs       agent self-reflection queries
  cli.rs              phil graph subcommand wiring
```

### Graph Schema (SQLite)

```sql
-- Core graph
CREATE TABLE nodes (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,      -- proposal, seam, crate, module, type, fn, ...
    name        TEXT NOT NULL,
    properties  TEXT NOT NULL,      -- JSON
    file_path   TEXT,               -- source file (if applicable)
    worktree    TEXT DEFAULT 'develop',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE edges (
    source_id   TEXT NOT NULL REFERENCES nodes(id),
    target_id   TEXT NOT NULL REFERENCES nodes(id),
    relation    TEXT NOT NULL,      -- implements, contains, depends_on, tests, ...
    properties  TEXT DEFAULT '{}',
    worktree    TEXT DEFAULT 'develop',
    PRIMARY KEY (source_id, target_id, relation, worktree)
);

-- Code snippets (signatures stored always; bodies on demand)
CREATE TABLE snippets (
    id          TEXT PRIMARY KEY,
    node_id     TEXT NOT NULL REFERENCES nodes(id),
    kind        TEXT NOT NULL,      -- fn, struct, trait, enum, impl, test
    signature   TEXT NOT NULL,      -- compressed: "pub fn name(args) -> Ret"
    doc_comment TEXT,
    body        TEXT,               -- full source (stored, returned on request)
    body_hash   TEXT,               -- fast change detection
    file_path   TEXT NOT NULL,
    line_start  INTEGER,
    line_end    INTEGER,
    language    TEXT DEFAULT 'rust'
);

-- Mutation audit trail (every graph change)
CREATE TABLE mutations (
    id          TEXT PRIMARY KEY,
    timestamp   TEXT NOT NULL,
    agent       TEXT,               -- "aria", "perplexity-computer", "human"
    session     TEXT,               -- agent session ID
    action      TEXT NOT NULL,      -- status_change, decision, link_created, ...
    target_node TEXT REFERENCES nodes(id),
    from_value  TEXT,
    to_value    TEXT,
    reason      TEXT,
    details     TEXT DEFAULT '{}'   -- JSON
);

-- Scan snapshots (track when each scan ran)
CREATE TABLE snapshots (
    id          TEXT PRIMARY KEY,
    scan_time   TEXT NOT NULL,
    commit_sha  TEXT,
    worktree    TEXT,
    metrics     TEXT DEFAULT '{}'   -- JSON: {loc, crates, types, fns, tests, unwraps}
);

-- Full-text search
CREATE VIRTUAL TABLE nodes_fts USING fts5(name, properties, content=nodes);
CREATE VIRTUAL TABLE snippets_fts USING fts5(signature, doc_comment, body, content=snippets);
```

### Node Types

| Kind | Source | Example ID |
|---|---|---|
| `proposal` | doc scanner (frontmatter) | `proposal:desktop-membrane` |
| `seam` | SEAM_REGISTRY.md | `seam:onboarding-tui-flow` |
| `task` | task.md (parsed) | `task:fix-status-mismatches` |
| `slice` | active seam increments | `slice:desktop-membrane-s3` |
| `domain` | frontmatter domain field | `domain:membrane-transport` |
| `crate` | Cargo.toml | `crate:aiua` |
| `module` | .rs file path | `module:aiua::service::ipc` |
| `type` | pub struct/trait/enum | `type:GraphDomain` |
| `function` | pub fn | `fn:handle_spawn_subagent` |
| `impl_block` | impl Trait for Type | `impl:MemoryEngine::for::MuninnRestEngine` |
| `snippet` | code block | `snip:GraphDomain::new` |
| `commit` | git log | `commit:4e99b41` |
| `branch` | git branch | `branch:codex/stage-routing` |
| `workstream` | named parallel effort | `workstream:stage-routing` |
| `worktree` | git worktree | `worktree:/home/user/philotic-stack-routing` |
| `test` | #[test] function | `test:test_graph_domain_new` |
| `component` | runtime guest process | `component:model-controller-gemini` |
| `skill` | SVER skill definition | `skill:check-engine` |
| `agent` | agent identity | `agent:aria` |
| `session` | agent work session | `session:aria-2026-03-28-001` |
| `decision` | recorded decision | `decision:defer-perimeter-egress` |

### Edge Types

| Relation | From → To | Source |
|---|---|---|
| `implements` | proposal → seam | frontmatter |
| `implemented_by` | seam → module/type | code scan + frontmatter |
| `depends_on` | seam → seam | ROADMAP.md |
| `contains` | crate → module → type → fn | AST |
| `governs` | domain → proposal | frontmatter |
| `references` | commit → module | git diff |
| `tests` | test → fn/type | code analysis |
| `blocks` | task → task/seam | task.md |
| `decided_by` | decision → agent/session | mutation log |
| `applies_to` | decision → proposal/seam | mutation log |
| `imports` | module → module | use statements |
| `trait_impl` | type → trait | impl blocks |
| `overlaps` | workstream → workstream | hot-file analysis |

---

## Data Authority

### Split Authority Model

Docs and code remain durable authored references. The graph is the source of
truth for process state, traceability, and graph-managed proposal workflow.

**Docs own:** proposal content, domain assignment, related_docs, seam
definitions, task descriptions.

**Graph owns:** decision log, agent session traces, status/disposition
change history, traceability links (commit → seam), priority ordering,
workstream assignments, review annotations, and each agent's active
work-focus records for proposals.

**Shared (graph can export to docs):** `status`, `last_updated`,
`active_seams`, `implemented_by`. These are the ONLY frontmatter
fields graph writeback should mutate.

### Writeback Rules

1. Graph mutations to shared fields update graph state and mutation history
   immediately.
2. The serializer parses YAML frontmatter, updates only mapped fields,
   preserves all other content byte-for-byte.
3. Explicit writeback exports graph-managed shared fields to frontmatter.
4. Every committed writeback produces a git commit with provenance:
   `"graph: update <PROPOSAL> status → <new> (agent: <name>, session: <id>)"`.
5. Doc changes always target the develop worktree (proposals are shared
   across workstreams).
6. Code-linked data (commits, snippets) is scoped to the relevant
   worktree/branch.

### Multi-Worktree Model

```
Graph layers:
  └── Base layer (develop) — proposals, seams, domains, shared state
      ├── Branch overlay: codex/stage-routing — code nodes, snippets, commits
      ├── Branch overlay: feat/web-wizard — code nodes, snippets, commits
      └── Branch overlay: develop — canonical code scan
```

When a branch merges to develop, its overlay merges into base and is
archived.

---

## Muninn Integration

Muninn and the project graph serve complementary purposes:

| | Project Graph | Muninn |
|---|---|---|
| Nature | Structural ledger | Cognitive memory |
| Content | Facts, links, decisions | Learnings, preferences, patterns |
| Decay | Never — it's an audit trail | Ebbinghaus decay + Hebbian strengthening |
| Query | "What implements seam X?" | "What did we learn last time?" |
| Example | "ipc.rs is 13,243 LOC, 47 commits" | "ipc.rs refactors are painful" |

The **agent session** links them. An agent working on a task:

1. Queries the **graph** for structural context (types, status, dependencies)
2. Queries **Muninn** for cognitive context (learnings, preferences)
3. Does the work
4. Records decisions in the **graph** (traceability, status changes)
5. Records learnings in **Muninn** (patterns, preferences, lessons)
6. Both are linked by `agent` + `session` identifiers

---

## Agent Self-Model

The graph enables agents to query their own operational structure:

```
// Aria queries her own self-model
graph_query("
  MATCH (a:agent {name: 'aria'})
    -[:governs]-> (p:proposal)
    -[:implements]-> (s:seam)
    -[:implemented_by]-> (m:module)
  RETURN p.name, p.status, s.name, m.name
")

// Agent asks: "What decisions did I make recently?"
graph_query("
  MATCH (d:decision)-[:decided_by]->(s:session {agent: 'aria'})
  WHERE d.timestamp > '2026-03-25'
  RETURN d.action, d.reason, d.target_node
  ORDER BY d.timestamp DESC
")

// Agent asks: "What code implements me?"
graph_query("
  MATCH (p:proposal {id: 'cognitive-loop-architecture'})
    -[:implemented_by]-> (m:module)
    -[:contains]-> (t:type)
  RETURN m.name, t.name, t.kind
")
```

This transforms agents from session-bound workers into entities with
structural self-awareness.

---

## CLI Surface

```bash
phil graph scan                     # full rescan (code + docs + git)
phil graph scan --watch             # file watcher, rescan on change
phil graph serve                    # MCP + REST + WebSocket + web UI
phil graph serve --port 8900
phil graph query "<query>"          # ad-hoc graph query
phil graph status                   # terminal dashboard summary
phil graph skeleton <crate>         # PlantUML for a crate
phil graph worktrees                # active worktrees + overlap
phil graph proposals                # proposal pipeline summary
phil graph decide <node> <action>   # record a traced decision
phil graph diff <branch>            # what changed vs develop (graph-aware)
```

---

## MCP Tools (for agent harnesses)

```
graph_query        — execute a graph query, return nodes + edges
graph_node         — get a single node with all edges
graph_skeleton     — PlantUML skeleton for a crate or module
graph_snippet      — get code snippet (signature or full body)
graph_status       — overall project status summary
graph_proposals    — proposal pipeline with statuses
graph_seams        — seam registry with dependencies
graph_worktrees    — active worktrees with overlap analysis
graph_decide       — record a traced decision
graph_update       — mutate a node property (with provenance)
graph_diff         — changes between branches (graph-aware)
graph_self         — agent self-model query
```

---

## Web UI

Served by `phil graph serve` alongside the API.

### Views

1. **Dashboard** — progress by domain, active workstreams, recent
   activity feed, health metrics (LOC, tests, unwraps).
2. **Graph Explorer** — interactive node-edge visualization
   (d3-force or cytoscape.js). Click any node to see connections.
   Filter by kind, domain, status.
3. **Proposal Pipeline** — kanban: proposed → accepted → in-progress →
   implemented → archived. Status changes create traced decisions.
4. **Workstream Monitor** — branches, worktrees, changed files,
   hot-file overlap matrix, merge readiness.
5. **Code Browser** — crate → module → type tree. PlantUML diagrams.
   Expandable snippets with syntax highlighting.
6. **Decision Timeline** — who changed what, when, why, from which
   session. Full audit trail visualization.
7. **Agent Activity** — per-agent session history, decisions made,
   files touched, Muninn memories surfaced.

---

## Implementation Seams

| # | Seam | Crate | Effort | Delivers |
|---|---|---|---|---|
| 1 | `graph-engine-schema` | graph-intelligence | S | SQLite schema + node/edge CRUD |
| 2 | `doc-scanner` | graph-intelligence | S | Frontmatter → graph nodes/edges |
| 3 | `code-scanner` | graph-intelligence | M | syn AST → types, fns, impls, snippets |
| 4 | `git-scanner` | graph-intelligence | S | Commits, branches, worktrees |
| 5 | `snippet-store` | graph-intelligence | S | Indexed code blocks + PlantUML |
| 6 | `graph-query-api` | graph-intelligence | S | REST query endpoints |
| 7 | `graph-mcp-server` | graph-intelligence | M | MCP tool interface for agents |
| 8 | `graph-writeback` | graph-intelligence | S | Frontmatter mutation serializer |
| 9 | `sver-process-model` | philotic-graph | M | SVER lifecycle + config |
| 10 | `muninn-bridge` | philotic-graph | S | Session → Muninn memory link |
| 11 | `agent-self-model` | philotic-graph | S | Self-reflection query patterns |
| 12 | `graph-web-ui` | philotic-graph | L | Interactive visualization |

Phases:
- **Phase 1** (seams 1–5): Scanners + engine. Queryable graph exists.
- **Phase 2** (seams 6–8): API layer. Agents can query and mutate.
- **Phase 3** (seams 9–11): SVER + Muninn + self-model. Full process.
- **Phase 4** (seam 12): Web UI. Humans get visualization.

---

## Open Questions

1. **Query language:** Cypher-like DSL, raw SQL with helpers, or a
   custom s-expression syntax? Recommendation: start with structured
   REST endpoints (GET /proposals?status=in-progress), add Cypher-like
   DSL in Phase 3.

2. **Scan trigger:** On-demand (`phil graph scan`), git hook
   (post-commit), or file watcher (fsnotify)? Recommendation: all
   three, file watcher for dev, git hook for CI.

3. **Graph DB upgrade path:** Start with SQLite. If query patterns
   outgrow it, CozoDB (Datalog, embedded, graph-native) is the upgrade.
   The schema is designed to be portable.

4. **Web UI framework:** Embed in the Rust binary (like phil serve does
   with jaredlikes-desktop), or separate frontend repo? Recommendation:
   embed, same pattern as the desktop membrane.
