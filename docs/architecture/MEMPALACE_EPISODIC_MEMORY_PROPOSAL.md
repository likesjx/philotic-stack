---
title: MemPalace Episodic Memory Proposal
doc_type: proposal
domain: memory-context
status: accepted-current-slice
last_updated: 2026-07-24
tags:
  - mempalace
  - episodic-memory
  - agent-continuity
  - retention
related_docs:
  - KNOWLEDGE_ARCHITECTURE_PROPOSAL.md
  - MUNINN_MEMORY_PROTOCOL_PROPOSAL.md
  - LIFE_GRAPH_OS_PROPOSAL.md
  - OBSIDIAN_KNOWLEDGE_GARDEN_PROPOSAL.md
  - CREATIVE_LEARNING_FLYWHEEL_PROPOSAL.md
task_refs:
  - docs/task.md#mempalace-episodic-memory
proposal_id: mempalace-episodic-memory
implements:
  - cross-agent-knowledge-architecture
active_seams:
  - mempalace-episodic-lane
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
implemented_by:
  - mempalace-episodic-lane-slice-1
---

# MemPalace Episodic Memory Proposal

## Goal

Give Codex, Claude, Perplexity, and future agents a low-friction record of what happened without turning transcripts into canonical life truth or forcing Jared to curate every interaction.

## Core Recommendation

Use MemPalace as the **automatic episodic lane**: a searchable history of conversations, working turns, source events, and session outcomes.

MemPalace should answer:

- What happened in the relevant sessions?
- What language, examples, or source material surrounded a decision?
- Which agent encountered this idea or problem before?
- Where can an agent recover detail after compact continuity memory is insufficient?

It must not answer by authority:

- What is currently true about Jared's life? LifeGraph owns that.
- What compact lesson should shape the next interaction? Muninn owns that.
- What is the durable project or code truth? Repo docs and Intel Graph own that.
- What is the human-maintained note or artifact? Obsidian owns that.

## Disposition

Accepted for the current slice.

Current local observation on 2026-07-24:

- MemPalace is installed and its local `brain_local` vault reports 10,000 drawers.
- Existing rooms include imported and domain-specific material.
- No active Codex or Claude reflex hook was found in the inspected client configuration.
- No local `.mempalace_convos` capture files were found.

This proves storage exists. It does not prove automatic cross-agent capture, useful recall, retention governance, or successful re-entry.

Slice 1 implemented and verified on 2026-07-24:

- a validated `EpisodicEpisode` envelope with stable IDs, RFC3339 capture time, SHA-256 content identity, provenance, privacy, retention, and related context refs
- a MemPalace-owned local adapter for idempotent capture, filtered semantic recall, status, and scoped deletion
- redact-before-store, no-capture markers, private-result suppression, duplicate suppression, and ID-conflict detection
- a bounded `ContextPacket` projection whose refs carry `mem_palace_episode` kind and `episodic_evidence` authority
- a hardened generic lifecycle hook plus versioned REST endpoints; the legacy `/api/mempalace/turn` path now delegates to the governed adapter instead of appending files and running an invalid background `mempalace mine --yes`

Targeted Rust/Python tests and an isolated real-MemPalace smoke are green. This is not yet an installed-runtime claim for Codex, Claude, or Perplexity lifecycle configuration.

## Capture Contract

Each episodic record should carry a stable envelope:

```text
episode_id
session_id
client
agent_or_role
captured_at
source_event
content_or_summary
content_hash
provenance
privacy_class
retention_class
related_context_refs[]
```

Capture should be automatic at reliable lifecycle boundaries such as `Stop`, `Save`, or `PreCompact`, with idempotency keyed by `episode_id` or the source event hash.

Client-specific wings or rooms should preserve provenance without creating isolated memory silos. Unified recall may search across authorized wings, but every result must retain its originating client and session.

## Promotion Rules

An episode is evidence, not truth.

Promotion follows explicit lanes:

1. MemPalace provides a relevant episode with provenance.
2. The agent extracts a concise candidate.
3. A durable collaboration learning may be written to Muninn as an atomic memory.
4. A life claim, commitment, relationship fact, or contradiction may be proposed through `life.observe`.
5. LifeGraph promotion remains governed through `life.commit` or `life.resolve`.
6. Long-form synthesis or an authored artifact may be written to Obsidian through its governed projection.

Raw transcripts must not be copied wholesale into Muninn, LifeGraph, or Obsidian.

## Recall Projection

MemPalace recall should project into the shared `ContextPacket` as episodic evidence with:

- bounded excerpts or summaries
- score and retrieval rationale
- episode and session identifiers
- source client and timestamp
- explicit `episodic_evidence` authority

Current-turn evidence and observed runtime/repo truth outrank stale episode recall.

## Privacy And Retention

Automatic capture requires stronger deletion and privacy semantics, not weaker ones.

Before broad enablement:

- define `private`, `sensitive`, and `normal` capture classes
- support no-capture and redact-before-store markers
- define retention by event class rather than keeping everything forever
- provide deletion by session, episode, source, and time window
- verify secrets and large tool outputs are excluded or redacted
- keep capture local-first unless an explicit replication policy says otherwise

## Current Slice

The first implementation slice now:

1. Defines and validates the episode envelope.
2. Provides an idempotent local lifecycle hook whose default client is Codex.
3. Proves recall by `session_id`, topic, and client.
4. Projects bounded results into `ContextPacket`.
5. Keeps transcript content out of LifeGraph and Muninn unless a later explicit promotion action occurs.
6. Covers capture, recall, redaction, privacy filtering, duplicate conflict, and deletion behavior with tests plus an isolated live MemPalace smoke.

The next activation slice must install the hook into one verified client lifecycle surface and measure capture success over real eligible sessions. The generic hook being present in the repo is not evidence that any client currently invokes it.

## Success Measures

- capture succeeds without manual prompting on at least 95% of eligible test sessions
- duplicate lifecycle events do not create duplicate episodes
- a known prior episode is recalled in under two seconds locally
- recall results preserve provenance and authority labels
- sensitive fixtures are redacted or excluded
- agents can recover useful detail without copying whole transcripts into the active context

## Intentionally Incomplete

- cross-hotel replication
- automatic truth promotion
- indefinite retention
- broad capture across every client before one client is proven

## Next Seam

Install `mempalace-episodic-lane` on one verified local client lifecycle surface, measure capture/recall/re-entry quality, then expand to another client only if the first lane is useful.
