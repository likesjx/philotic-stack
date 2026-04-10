---
title: Philotic Working Vocabulary
doc_type: reference
domain: workflow-docs
status: active
last_updated: 2026-03-31
tags:
- glossary
- vocabulary
- process
related_docs:
- AGENTS.md
- SEAM_REGISTRY.md
- DOC_TAGGING_FRONTMATTER_PROPOSAL.md
- ARCHITECTURE_STATUS.md
---

# Philotic Working Vocabulary

This document defines the canonical meaning of process and collaboration terms used across the repository.
It exists to prevent vocabulary drift and to make human-agent collaboration more precise.

When a term here conflicts with usage elsewhere in the docs, fix the usage — do not update this file
to match the drift.

---

## Core Terms

### proposal
An architecture or process document in `docs/architecture/` that describes intended direction.
Proposals carry a `Disposition` lifecycle: `proposed` → `accepted for current slice` → `implemented`
→ `superseded` / `deferred`.

A proposal is a design artifact, not an execution unit.
It is not the same as a task, a slice, or a seam.

### slice
The smallest coherent code change that:
- proves a direction
- can be tested meaningfully
- leaves the system in a coherent state

A slice is the unit of a commit and a push.
Each slice should map to an entry in `docs/task.md` and, when relevant, update a proposal's `Current Slice` and `Disposition`.

Slices are not the same as blocks. "Block" is an informal label sometimes used in Muninn notes
to group related slices; it has no formal standing in this process.

### seam
A registered architectural boundary where authority, ownership, or protocol transitions.
Seams have stable kebab-case IDs tracked in `SEAM_REGISTRY.md` and belong to exactly one primary domain.

A seam is a structural fact about the system — not a task, not a place to cut work, not a vague metaphor.

**Do not use "seam" informally** to mean "next natural place to split work."
Use **work boundary** for that.

### work boundary
An informal term for "the next natural place to split or hand off implementation work."
This is the concept that belongs in close-out notes, not `SEAM_REGISTRY.md`.

If a work boundary turns out to be structurally important and stable, promote it to a seam.

### domain
The primary scope organizer for architecture and process documents.
Controlled vocabulary is defined in `AGENTS.md §4.1`.
Every active architecture doc declares exactly one primary domain.
If a doc spans multiple domains, that is a seam problem — name it explicitly rather than smearing ownership.

### disposition
The lifecycle state of a proposal. Controlled values:
- `proposed` — direction under consideration
- `accepted for current slice` — direction is agreed; implementation is in progress
- `implemented` — the full proposal is live in the codebase
- `superseded` — a different proposal replaced this one
- `deferred` — intentionally not being worked on now

The disposition is a first-class status, not a trailing footnote.
Keep it current as slices land.

### workstream
One active implementation thread in a dedicated sibling git worktree.
One workstream = one `codex/<slug>` branch = one sibling worktree.
Do not share a checkout across multiple active workstreams.

### block (informal, discouraged outside Muninn notes)
An informal label grouping related slices within a workstream (e.g. "Block E", "Block F").
It has no formal standing in proposals, task tracking, or the seam registry.
Prefer naming the slices themselves. If Muninn notes use block labels, treat them as
internal working shorthand only.

### graph (intelligence graph)
The SQLite-based graph representation of the entire development surface — code,
documentation, git history, process state, and agent decisions.

The graph is the **canonical source of truth** for:
- Proposal status and disposition
- Seam ownership and state
- Task assignments
- Agent decisions and traceability
- Architecture relationships

Agents mutate the graph via MCP tools (`graph_update_node`, `graph_create_edge`,
`graph_decide`). Markdown files are human-readable projections, not authorities.

See `GRAPH_AS_SOURCE_OF_TRUTH.md` for the full architecture.

### node
An entity in the intelligence graph. Kinds include: `proposal`, `seam`, `task`,
`slice`, `domain`, `crate`, `module`, `type`, `function`, `test`, `commit`,
`branch`, `decision`, `agent`, `session`.

Each node has a unique ID (e.g., `doc:runtime-authority-leases`), kind, name,
and JSON properties.

### edge
A relationship between two nodes in the intelligence graph. Relations include:
`implements`, `implemented_by`, `depends_on`, `contains`, `governs`,
`references`, `tests`, `blocks`, `applies_to`, `imports`.

### mcp (model context protocol)
The protocol by which agents interact with the graph intelligence server.
MCP tools provide both query (`graph_status`, `graph_query`) and mutation
(`graph_create_node`, `graph_update_node`, `graph_writeback`) capabilities.

### writeback
The process of serializing graph state back to markdown frontmatter. Graph is
canonical; files are projections. Optional and explicit via `graph_writeback` tool.

### mutation
An audit log entry recording every graph change: agent, timestamp, action,
target node, from/to values, and reason. Stored in the `mutations` table.

---

## How These Relate

```
proposal  ──owns──►  seam(s)  ──tracked in──►  SEAM_REGISTRY.md
    │
    └──contains──►  Current Slice  ──produces──►  commit(s) on a workstream branch
                                                         │
                                             closes a work boundary
                                             (may or may not promote to a seam)
```

A proposal describes direction.
A seam names a durable boundary inside that direction.
A slice is what gets built.
A work boundary is the informal cut point between slices.

---

## Vocabulary That Lives Elsewhere

- **hotel / guest / apartment / incarnation**: system domain metaphors — see `CLAUDE.md` and `ARCHITECTURE.md`
- **turn / session / approval policy**: agent runtime concepts — see `AGENT_LOOP_PROPOSAL.md`
- **domain vocabulary** (controlled list): `AGENTS.md §4.1`
- **disposition values** (controlled list): `AGENTS.md §4`
