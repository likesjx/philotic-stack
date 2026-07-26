---
title: Obsidian Knowledge Garden Proposal
doc_type: proposal
domain: memory-context
status: accepted-current-slice
last_updated: 2026-07-24
tags:
  - obsidian
  - knowledge-garden
  - documents
  - lifegraph
related_docs:
  - KNOWLEDGE_ARCHITECTURE_PROPOSAL.md
  - LIFE_GRAPH_OS_PROPOSAL.md
  - MEMPALACE_EPISODIC_MEMORY_PROPOSAL.md
  - CREATIVE_LEARNING_FLYWHEEL_PROPOSAL.md
task_refs:
  - docs/task.md#obsidian-knowledge-garden
proposal_id: obsidian-knowledge-garden
implements:
  - cross-agent-knowledge-architecture
  - life-graph-os
active_seams:
  - obsidian-knowledge-projection
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# Obsidian Knowledge Garden Proposal

## Goal

Make Jared's notes, essays, research, plans, and creative artifacts instantly findable and meaningfully connected to LifeGraph while keeping the vault pleasant to read and edit as ordinary Markdown.

## Core Recommendation

Use Obsidian as the **human-readable knowledge and artifact surface**.

Obsidian owns note bodies and authored structure. LifeGraph owns stable relationships between those documents and the people, projects, goals, questions, ideas, experiments, and commitments they concern.

The integration is a governed projection, not a second copy of the vault:

- Markdown remains canonical for note content.
- LifeGraph stores a stable `Document` reference, metadata, provenance, and graph relationships.
- Muninn may remember why a note matters now.
- MemPalace may point to the episode in which the note was discussed or created.
- Intel Graph remains authoritative for repository artifacts and implementation truth.

## Disposition

Accepted for the first incremental indexing and governed-write slice.

Current local observation on 2026-07-24:

- the Obsidian CLI resolves the default vault as `Brain`
- the vault contains approximately 1,421 Markdown notes
- `Efforts/Ongoing` is the selected first projection scope (six Markdown notes)
- the governed index and narrow MCP server are test-green
- a real isolated sync against that scope is smoke-green
- the installed `mac-jane` upstream is connected with seven projected tools
- routed `knowledge.sync.status` and `knowledge.search` calls are watched-live-green

The vault is real and substantial. The first metadata projection now exists,
while cross-note entity acceptance and installed-runtime use remain intentionally
bounded.

## Document Projection

Each indexed note should project a stable document record:

```text
document_id
vault_id
relative_path
content_hash
title
headings[]
tags[]
outbound_links[]
created_at
modified_at
indexed_at
provenance
tombstoned_at?
```

The first version should use a deterministic identity derived from `vault_id` plus a persistent file identity or tracked rename lineage. A simple path-only identity is insufficient because renames would masquerade as deletion plus invention.

LifeGraph may then relate the document to canonical nodes without owning the note body:

```text
Document --ABOUT--> Person | Project | Goal | Topic
Document --DEVELOPS--> Question | Idea
Document --RECORDS--> Experiment | Learning
Document --PRODUCES--> Artifact
Document --SUPPORTS--> Commitment | Decision
```

These relation types are proposed schema additions until accepted in the LifeGraph ontology.

## Incremental Synchronization

The indexer should:

1. Discover created, changed, renamed, and deleted Markdown files.
2. Hash content and skip unchanged notes.
3. Parse frontmatter, headings, tags, and links.
4. Upsert the document projection idempotently.
5. preserve rename lineage
6. tombstone deleted documents rather than silently erasing graph history
7. emit a review queue for ambiguous entity links

Full-vault reindexing remains a repair operation, not the normal path.

## Governed Agent Access

Agents need narrow operations:

- `knowledge.search`
- `knowledge.read`
- `knowledge.create.propose`
- `knowledge.patch.propose`
- `knowledge.link.propose`
- `knowledge.sync.status`

Read operations may be broadly projected to trusted agents. Create, patch, and link operations should require a preview and approval until policy proves safe.

Raw shell or filesystem access is not the target integration contract. It is a debugging fallback—the architectural equivalent of entering through a window because the front door has opinions.

## Knowledge Gardening

An Astrid-style steward may suggest:

- duplicate or near-duplicate notes
- orphaned notes with no meaningful relationships
- unresolved links
- stale project notes
- ideas that connect across domains
- notes worth synthesizing into an artifact

The steward proposes; it does not rewrite the vault autonomously.

## Current Slice

The first implementation slice:

1. indexes `Brain/Efforts/Ongoing`
2. stores stable document identity, hashes, provenance, rename lineage, and
   tombstones in a derived SQLite index without copying note bodies
3. proves incremental create, edit, rename, delete, and unchanged-note behavior
4. exposes read-only search/read/status plus review-only create/patch/link
   proposals over a narrow stdio MCP server
5. projects results as `authored_knowledge` `ContextPacket` references
6. provisions the server through the generic MCP client fabric with an explicit
   seven-tool allowlist
7. directs Astrid to the governed tools and forbids vault edits through bash

Targeted Python/Rust tests, direct MCP negotiation, an isolated real-vault sync,
and installed routed calls through `mac-jane` are green. Search reads the
derived metadata index so cloud hydration cannot block quick recall; the
background refresh and explicit `knowledge.read` own current-body access.
The provisioner fails loudly when the hotel is absent or its stdio allowlist
has not been configured.

## Success Measures

- changed notes become searchable within 60 seconds
- unchanged notes are not reprocessed
- rename and deletion preserve lineage
- every graph result links back to its source note
- a user can move from a LifeGraph entity to the relevant note in one action
- useful cross-note connections increase without increasing duplicate note bodies

## Intentionally Incomplete

- indexing the whole vault in the first slice
- autonomous note rewriting
- treating backlinks as confirmed semantic truth
- moving LifeGraph truth into frontmatter

## Next Seam

Implement `obsidian-knowledge-projection` against one high-value folder, then use reviewed entity links to decide which ontology additions are actually warranted.
