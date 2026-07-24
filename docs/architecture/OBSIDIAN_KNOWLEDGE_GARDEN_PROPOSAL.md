---
title: Obsidian Knowledge Garden Proposal
doc_type: proposal
domain: memory-context
status: proposed
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

Proposed as an incremental indexing and governed-write slice.

Current local observation on 2026-07-24:

- the Obsidian CLI resolves the default vault as `Brain`
- the vault contains approximately 1,421 Markdown notes
- no governed incremental Obsidian-to-LifeGraph synchronization path has been proven

The vault is real and substantial. The useful graph projection is still intended architecture.

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

The first implementation slice should:

1. Index one explicitly selected folder in the `Brain` vault.
2. Create stable `Document` projections with hashes and provenance.
3. Prove incremental create, edit, rename, and delete behavior.
4. Add read-only search and read tools.
5. Add a review queue for proposed LifeGraph relationships.
6. Measure recall quality before indexing the entire vault.

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
