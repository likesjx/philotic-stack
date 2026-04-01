---
title: Graph Datasource — Live System Context for Philotes
doc_type: proposal
domain: cognitive-plane
status: proposed
last_updated: 2026-04-01
tags:
- graph
- datasource
- graph-intelligence
- philote
- admin-role
- system-context
- architecture-search
- platform-management
- cognitive-substrate
- workstream-tracking
related_docs:
- GRAPH_INTELLIGENCE_PROPOSAL.md
- GRAPH_AS_SOURCE_OF_TRUTH.md
- COGNITIVE_LOOP_PROPOSAL.md
- CONTEXT_GRAPH_RUNNER_PROPOSAL.md
- AGENT_WORKFLOW_PROPOSAL.md
- CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
proposal_id: graph-datasource-philote
implements:
- graph-intelligence
implemented_by: []
active_seams: []
---

# Graph Datasource — Live System Context for Philotes

## Problem

Philotes (agents) are currently blind to the system they inhabit. An
admin role trying to answer "how does session checkpointing work?",
"what crates own the mesh plane?", or "what workstreams are in
planning?" has no path to the answer except operator-written skills
that go stale or direct REST knowledge that leaks port details into
the cognitive layer.

The deeper problem is platform management. A senior engineer navigates
this codebase by reading architecture docs, searching functions,
tracing seams across crates, and checking what proposals are driving
current work. An admin philote cannot do any of that today — not
because the information doesn't exist, but because there's no bridge
from the cognitive layer to the indexed knowledge.

`graph-intelligence` already scans and indexes the entire surface:
architecture docs, proposals, seams, crate structure, functions,
snippets, decisions, git history, workstreams. The data is there,
always current. Agents just can't reach it.

## Vision

The codebase graph becomes a first-class datasource. An admin philote
(or any role that needs system context) calls `graph.query` through the
standard `DatasourceProvider` fabric and gets back live, structured
knowledge about the platform it's running inside — the same knowledge
a senior engineer uses to manage it.

This is the foundation for a self-managing platform: agents that can
read the architecture, understand the codebase, orient themselves on
active workstreams, and make platform decisions without the operator
having to narrate the current state every session.

It also eliminates a class of skills: any skill that answers "what is
the state of X in the system" is replaced by a graph query. The
knowledge stays current because the scanner runs continuously, not
because a human updated a doc.

## Architecture

### The Filter: graph-intelligence → graph-datasource

`graph-intelligence` stays as-is: a standalone scanner + REST/MCP
server with its own rich schema (nodes, edges, snippets, embeddings,
FTS). It is the **source of truth** for all system context.

`graph-datasource` gains a new provider: **`GraphIntelligenceProvider`**
that implements `DatasourceProvider` and routes queries to the
`graph-intelligence` engine. Tool-runner registers it alongside
`SqliteCypherProvider`.

```
philote (admin role)
  └─ tool call: graph.query { ... }
       └─ tool-runner
            └─ GraphIntelligenceProvider (DatasourceProvider)
                 └─ GraphEngine (graph-intelligence)
                      └─ SQLite: nodes/edges/snippets/FTS/embeddings
```

No port knowledge in the cognitive layer. No skill needed. The graph
is just another datasource partition, identical in interface to memory
or bash from the philote's perspective.

### Query Vocabulary

Rather than Cypher (the existing transpiler is too thin for
graph-intelligence's query model), `GraphIntelligenceProvider` exposes
a **structured query vocabulary** — the actual questions philotes ask
when managing a platform:

**Architecture & Code**
```json
{ "kind": "search", "text": "session checkpoint race condition" }
{ "kind": "context_for", "target": "crates/philote/src/session.rs" }
{ "kind": "crates" }
{ "kind": "snippet", "target": "crates/aiua/src/service/ipc.rs", "symbol": "compose_session_snapshot" }
{ "kind": "seams", "filter": { "domain": "cognitive-plane" } }
```

**Work in Flight**
```json
{ "kind": "proposals", "filter": { "status": "in-progress" } }
{ "kind": "workstreams" }
{ "kind": "workstream", "branch": "codex/graph-datasource" }
{ "kind": "tasks", "filter": { "status": "open" } }
{ "kind": "decisions", "limit": 10 }
```

**System State**
```json
{ "kind": "digest" }
{ "kind": "freshness" }
```

The vocabulary grows as the admin role's needs are better understood.
The provider translates each to `GraphEngine` calls internally.

### What an Admin Philote Can Do

With `graph.query` in its toolset, an admin philote can:

**Navigate the architecture:**
- "How does the session checkpoint work?" → search "session checkpoint",
  then `context_for` the relevant files → reads the architecture docs
  and code snippets, synthesizes an answer
- "What crates own the mesh control plane?" → `crates` + `seams`
  filtered to `domain: mesh` → understands the crate boundary and
  what each seam governs
- "How does role handoff work end to end?" → search "role handoff" →
  surfaces the proposal, the seams, the relevant functions across
  `philote`, `aiua`, `philotic-client`

**Manage work in flight:**
- "What are we working on right now?" → `workstreams` + `proposals`
  filtered to `in-progress` → sees every active codex branch, its
  linked proposal, current phase, and claimed seams
- "Is this workstream in planning or implementation?" → `workstream`
  by branch name → gets the phase, linked proposal, open tasks
- "What decisions were made about the session checkpoint fix?" →
  `decisions` filtered by tag or recency → full audit trail

**Make platform decisions:**
- "Should I add this tool to the admin role's toolset?" → `seams`
  governing tool registration + existing admin role toolset → can
  reason about fit and blast radius before proposing a rule
- "Is there already a proposal for X?" → `search` + `proposals` → 
  avoids duplicating work or contradicting in-flight decisions

None of this requires a skill. The graph is the skill.

### Workstream Tracking in the Graph

For the admin philote to reason about "what workstreams are in
planning vs. implementation", the graph scanner needs to enrich
workstream nodes beyond branch name:

**Workstream node (enriched):**
```
kind: workstream
name: codex/graph-datasource
properties:
  branch: codex/graph-datasource
  phase: planning | implementation | review | merged | abandoned
  linked_proposal: graph-datasource-philote
  linked_seams: [graph-query-api, ...]
  linked_tasks: [...]
  worktree_path: /Users/.../philotic-stack-graph-datasource
  last_commit: <sha>
  commits_ahead_develop: 12
  hot_files: [crates/graph-datasource/src/provider.rs, ...]
```

**Phase inference rules** (scanner derives from git + doc state):
- `planning` — branch exists, linked proposal in `proposed` status,
  no implementation commits yet (only docs/specs)
- `implementation` — commits touching `crates/` exist, proposal in
  `accepted-*` status
- `review` — PR open against develop
- `merged` — branch merged into develop
- `abandoned` — branch inactive >30 days with no PR

**Proposal → workstream edge**: The scanner reads the proposal doc's
`proposal_id` field and the branch name convention (`codex/<slug>`),
matches by slug, and writes a `Drives` edge from proposal to
workstream. This is the link that lets an admin philote say "show me
the workstream for this proposal" or "this workstream is in planning
because its proposal hasn't been accepted yet."

This enrichment lives in the `git.rs` scanner and requires no manual
maintenance — it's derived entirely from branch state and proposal
frontmatter.

## What This Is Not

- **Not a rewrite of graph-intelligence**: The scanner, schema,
  embeddings, and REST/MCP surface stay exactly as-is. This adds a
  new consumer path.

- **Not Cypher over graph-intelligence**: The Cypher transpiler in
  `graph-datasource` is for the `ag_node`/`ag_edge` partition model.
  `GraphIntelligenceProvider` uses its own vocabulary. Cypher
  unification is Phase 3.

- **Not a replacement for action skills**: Skills that perform
  actions (`role.configure`, `bash.exec`, `rule.propose`) are
  unaffected. This replaces only read-world "what is the state of X"
  skills.

- **Not a search engine for the operator**: The query fabric is for
  philotes. The operator already has the REST UI and `phil graph`
  CLI. This is agent-facing, not operator-facing.

## Phases

### Phase 1 — `GraphIntelligenceProvider` *(target: next sprint)*

1. Add `GraphIntelligenceProvider` to `graph-datasource` crate
   - Implements `DatasourceProvider` with `id = "graph"`
   - Wraps `GraphEngine` directly (co-resident) or HTTP (remote)
   - Handles the structured query vocabulary above

2. Register in `tool-runner`'s provider registry
   - Available as `graph.query` tool call
   - Guarded by capability/rights model

3. Add `graph.query` to the admin role's default toolset

4. Validate: Bjork admin role "status sweep" and "explain the
   session checkpoint architecture" — no pre-written skill

### Phase 2 — Workstream enrichment *(scanner work)*

1. Enrich workstream nodes with `phase`, `linked_proposal`,
   `linked_seams`, `commits_ahead`, `hot_files`
2. Add `Drives` edge: proposal → workstream (by slug matching)
3. Phase inference from git + proposal frontmatter status
4. Admin philote can now answer "what workstreams are in planning?"

### Phase 3 — Schema convergence *(future)*

`graph-datasource`'s thin `ag_node`/`ag_edge` schema adopts
`graph-intelligence`'s richer model (embeddings, FTS, snippets).
One schema, multiple access paths, Cypher as optional sugar.

### Phase 4 — Partition routing *(future)*

`graph_id` as routing key:
- `graph_id = "system"` → `GraphIntelligenceProvider` (this proposal)
- `graph_id = "agent-bjork-01"` → per-agent memory graph
- `graph_id = "session:{id}"` → short-session working memory

Admin philote can cross-partition: "what did I decide last week about
X, and what does the current architecture say about it?"

## Open Questions

- **Co-resident vs. HTTP**: Direct `GraphEngine` call (fast, no port)
  vs. HTTP (allows remote graph server). Likely: direct for local
  hotel, HTTP for cross-hotel mesh queries.

- **Scan freshness signal**: `graph.query { kind: "freshness" }` should
  tell the philote when the last scan ran and whether it's stale, so
  the admin role can trigger a rescan before answering architecture
  questions in a long-running session.

- **Write path**: Phase 1 is read-only. A `graph.decide` tool (Phase 2)
  lets the admin philote write decisions back, closing the loop on
  self-documenting platform management.

- **Availability**: Provider returns `{ available: false, reason: ... }`
  when graph-intelligence is not running, so philote can degrade
  gracefully rather than hard-failing.

- **intel-graph scan cadence**: For the admin philote to have current
  information, the graph needs to be scanned regularly — on hotel
  startup, after commits, and on a periodic heartbeat. This is an ops
  concern but shapes the freshness SLA of graph queries.

## Relationship to Existing Work

- **graph-intelligence** (`accepted-current-slice`): Independent.
  This adds a consumer, not new scanner functionality.

- **graph-datasource** (`codex/graph-datasource`): The worktree has
  the `DatasourceProvider` trait and `SqliteCypherProvider`.
  `GraphIntelligenceProvider` is a new impl in that crate.

- **CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL**: Graph query is the tool
  that makes an admin role useful without extensive skill authoring.
  Complementary — admin surface is the UI, this is the data layer.

- **skill reduction**: Skills that are "queries about system state"
  should be retired in favor of graph queries over time.
  Not an immediate action — retire as equivalents are validated.
