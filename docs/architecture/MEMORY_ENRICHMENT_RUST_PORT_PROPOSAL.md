---
title: Memory Enrichment — Port to Rust via model-router
doc_type: proposal
domain: cognitive-plane
status: proposed
last_updated: 2026-04-08
proposal_id: memory-enrichment-rust-port
tags:
- memory
- enrichment
- model-router
- philote
- muninndb
- cognitive-loop
related_docs:
- COGNITIVE_LOOP_PROPOSAL.md
- MEMORY_LAYERING_AND_WORK_PRODUCT_SPLIT_PROPOSAL.md
- GRAPH_DATASOURCE_PHILOTE_PROPOSAL.md
implements: []
implemented_by: []
active_seams:
- memory-enrichment-pipeline
---

# Memory Enrichment — Port to Rust via model-router

## Status

Proposed. Blocked on: model-router being the confirmed enrichment source.

## Context

The current memory enrichment integration (`codex/muninn-upstream-sync`) calls
`POST /api/engrams/{id}/enrich` on MuninnDB to mark stages as completed. This
is a structural placeholder — it signals intent but doesn't generate real
enrichment content (entities, relationships, summaries). Real enrichment
currently depends on MuninnDB's LLM plugin, which is not configured in local
or production deployments.

The REST endpoints (`GET /api/enrichment/candidates`, `POST /api/engrams/{id}/enrich`)
have been promoted from MCP-only in the local muninndb fork, enabling a
proper Rust integration path.

## Problem

Enrichment delegation to MuninnDB's LLM plugin is wrong for our architecture:

1. **Wrong source of truth**: The agent (philote) has the full turn context —
   what the user said, what tools were called, what the model reasoned about.
   MuninnDB's background pipeline sees only the stored text, stripped of context.

2. **Unconfigured in practice**: The enrichment plugin requires a separate LLM
   API key and provider config in MuninnDB. Neither bjork nor coach nor jane
   have this configured. Enrichment silently never runs.

3. **Wrong timing**: Background enrichment runs on a sweep schedule, minutes
   after the turn. The Attend phase fires immediately after the turn completes —
   that's the right moment to extract entities while context is hot.

4. **model-router already exists**: We have a model provider routing guest
   (Gemini/ElevenLabs). Enrichment is a natural fit for a cheap, fast Gemini
   Flash call — structured JSON output, deterministic schema.

## Proposal

Move enrichment generation into the philote Attend phase, driven by model-router:

```
Turn completes
  ↓
Attend phase: remember() → EngramRef
  ↓ (if high-value tag: decision/preference/identity/fact/architecture)
Spawn async task:
  1. GET /api/enrichment/candidates?vault=X&limit=1  (find our engram)
  2. model-router call: Gemini Flash
     - prompt: "Extract entities and relationships from this memory: {content}"
     - response schema: { entities: [...], relationships: [...], summary: "..." }
  3. POST /api/engrams/{id}/enrich?vault=X
     - body: { entities, relationships, summary, expected_updated_at, source: "philote-gemini" }
```

The model-router call is fire-and-forget, non-blocking. If it fails, the
memory is stored without enrichment — no regression.

## Interface Changes

### `MemoryEngine` trait

Add `enrich_with_content()` alongside `retry_enrich()`:

```rust
/// Generate and apply enrichment for a stored engram using the provided
/// content extractor. Called from the Attend phase with a model-router client.
async fn enrich_with_content(
    &self,
    id: &EngramId,
    extractor: &dyn EnrichmentExtractor,
) -> anyhow::Result<()> {
    Ok(()) // default no-op
}
```

### `EnrichmentExtractor` trait (new, in `memory-core`)

```rust
#[async_trait]
pub trait EnrichmentExtractor: Send + Sync {
    async fn extract(&self, content: &str) -> anyhow::Result<EnrichmentOutput>;
}

pub struct EnrichmentOutput {
    pub summary: Option<String>,
    pub entities: Vec<EnrichmentEntity>,
    pub relationships: Vec<EnrichmentRelationship>,
}
```

### `GeminiEnrichmentExtractor` (in `model-router` or `philote`)

Implements `EnrichmentExtractor` by calling Gemini Flash with a structured
output schema. Cost estimate: ~100 tokens per memory = $0.000015 per enrichment
at Flash pricing. Acceptable for high-value memories only.

## Trigger Condition

Enrichment runs only on high-value tag signals (same as current `retry_enrich`):
`decision`, `preference`, `identity`, `fact`, `architecture`, `constraint`, `operator`.

Turn-level memories (`turn:*` concept prefix) are never enriched — too
ephemeral, too cheap to justify a model call.

## MuninnDB Side

No changes needed. The local fork already has:
- `GET /api/enrichment/candidates` — live ✅
- `POST /api/engrams/{id}/enrich` — live ✅

Upstream PR to `scrypster/muninndb` should be opened when this proposal
moves to `accepted`.

## What This Unlocks

- **Entity graph**: Agents accumulate a structured entity graph over time
  (people, systems, concepts, decisions) that improves recall quality
- **Relationship traversal**: Hebbian association + explicit relationship edges
  = richer spreading activation during recall
- **Contradiction detection**: MuninnDB's contradiction engine can fire on
  entities from related memories
- **LTP trigger**: Entities that appear in multiple enriched memories become
  potentiation candidates — architectural facts that resist decay

## Blockers

1. **model-router structured output**: Gemini Flash structured JSON output
   needs to be validated for the enrichment schema. Existing tool-call
   plumbing in model-router should cover this.

2. **EnrichmentExtractor wiring in philote**: The extractor needs access to
   the model-router IPC channel from the Attend phase. Currently the Attend
   phase has `self.memory_engine_for()` but no model-router handle. A second
   optional field on the runtime struct, or a dedicated enrichment channel,
   is needed.

3. **Rate limiting**: Need a per-vault per-minute cap on enrichment calls to
   avoid Gemini quota exhaustion during memory-heavy sessions.

## Decision

Port when:
- model-router structured output is validated for enrichment schema
- Attend phase has a clean path to the model-router client
- At least one agent (bjork or coach) has enough stored memories to
  validate entity graph quality

Do NOT port earlier — the placeholder `retry_enrich()` is production-safe
and prevents work on an unstable interface.
