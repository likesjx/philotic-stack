---
title: Life Graph Semantic Retrieval
doc_type: specification
domain: memory-context
status: proposed
last_updated: 2026-07-07
tags:
- life-graph
- semantic-retrieval
- vector-search
- graph-expansion
- context-packet
seam: life-graph-semantic-retrieval
related_docs:
- ../LIFE_GRAPH_OS_PROPOSAL.md
- LIFE_GRAPH_SCHEMA.md
- ATTENTION_STEWARD.md
source_of_truth_targets:
- SEAM_REGISTRY.md
- docs/task.md
---

# Life Graph Semantic Retrieval

Specification for the semantic retrieval pipeline: named strategies, Cypher patterns, policy filters, ranking model, and the `RetrievalContextPacket` output shape.

## Pipeline Overview

```
query (natural language or embedding)
  → 1. vector pivot: CALL vector_search.search(...) against relevant space(s)
  → 2. graph expansion: bounded Cypher traversal from candidate nodes
  → 3. policy filter: drop stale, low-confidence, retired, or context-inappropriate nodes
  → 4. ranking: score by role relevance, recency, confidence, and path specificity
  → 5. context projection: assemble RetrievalContextPacket with evidence paths
```

Graph edges provide precision and explainability. Vectors provide fuzzy recall and phrasing tolerance. Neither alone is sufficient.

**Implementation layer**: `data-memorygraphrag` runner (seam: `life-graph-memorygraphrag-runner`) invokes named strategies via `graph-datasource`. Raw Cypher mutations of the Life Graph are not permitted through this pipeline — retrieval is read-only.

---

## RetrievalContextPacket

The output of every retrieval strategy. `data-memorygraphrag` assembles this; the context engine consumes it.

> Note: `EvidencePacket` (seam: `life-graph-evidence-conflict`) adds conflict detection and adjudication metadata on top of this packet for records involved in known conflicts. The `RetrievalContextPacket` is the base retrieval output; `EvidencePacket` wraps or extends it when conflict context is needed.

### Shape

| Field | Type | Description |
|---|---|---|
| `packet_id` | `string` | Unique ID for this retrieval result |
| `strategy` | `string` | Name of the strategy that produced this packet |
| `query_summary` | `string` | Short description of the query intent |
| `role_context` | `string` | Active role when retrieval was invoked |
| `assembled_at` | `string` | ISO 8601 timestamp |
| `embedding_spaces_queried` | `list<string>` | Which vector spaces were searched |
| `candidates` | `list<CandidateEntry>` | Ranked list of matched nodes with evidence paths |
| `policy_filter_log` | `list<string>` | Human-readable log of what was dropped and why |
| `context_tokens_estimate` | `int` | Rough token budget consumed by this packet |
| `retrieval_quality_signals` | `RetrievalQualitySignals` | Metadata for the retrieval flywheel |

### CandidateEntry

| Field | Type | Description |
|---|---|---|
| `node_id` | `string` | Life Graph node ID |
| `label` | `string` | Memgraph label (e.g. `Goal`, `OpenLoop`) |
| `title_or_name` | `string` | Human-readable label from node properties |
| `similarity` | `float` | Cosine similarity score from vector search (0.0–1.0) |
| `expansion_depth` | `int` | Hop count from the vector pivot node (0 = direct hit) |
| `path_summary` | `list<string>` | Relationship labels traversed to reach this node |
| `confidence` | `float` | Node's `confidence` from provenance envelope |
| `validation_state` | `string` | Node's `validation_state` |
| `ranking_score` | `float` | Final composite score after ranking |
| `policy_flags` | `list<string>` | Flags set by policy filter (for audit, not removal — removal already happened) |

### RetrievalQualitySignals

| Field | Type | Description |
|---|---|---|
| `vector_hit_count` | `int` | Raw candidates from vector search before expansion |
| `expansion_hit_count` | `int` | Nodes added by graph expansion |
| `policy_dropped_count` | `int` | Nodes removed by policy filter |
| `top_similarity_score` | `float` | Highest similarity in candidate set |
| `strategy_duration_ms` | `int` | End-to-end strategy execution time |

---

## Named Retrieval Strategies

### 1. `open_loops_by_context`

**Purpose**: Surface unresolved items relevant to the current topic or conversation context.

**Input**: query embedding, optional scope filter (`personal` | `project` | `relationship`)

**Cypher pattern**:
```cypher
// Step 1: vector pivot
CALL vector_search.search("life_event_semantic__OpenLoop", 10, $query_vector)
YIELD node AS pivot, similarity
WHERE similarity > 0.4

// Step 2: graph expansion (depth 1)
OPTIONAL MATCH (pivot)-[r:BLOCKED_BY|NEEDS_FOLLOWUP|CONTAINS]->(related)
WHERE related.validation_state <> 'retired'

// Step 3: policy filter inline
WHERE pivot.status = 'open'
  AND pivot.confidence >= 0.3
  AND (pivot.validation_state = 'confirmed' OR pivot.validation_state = 'inferred')

RETURN pivot, similarity, related, type(r) AS rel_type
ORDER BY similarity DESC, pivot.observed_at DESC
LIMIT 15
```

**Ranking weights**: proposal sketch superseded — see the implemented `open_loops_by_context` column in the Ranking Model table below.

---

### 2. `goals_and_next_actions`

**Purpose**: Find active goals and their available next actions, anchored to the current context.

**Input**: query embedding, optional role filter

**Cypher pattern**:
```cypher
// Step 1: vector pivot on goals
CALL vector_search.search("goal_system_semantic__Goal", 8, $query_vector)
YIELD node AS goal, similarity
WHERE similarity > 0.35
  AND goal.status IN ['active', 'paused']
  AND goal.validation_state <> 'retired'

// Step 2: expand to next actions
OPTIONAL MATCH (goal)-[:CONTAINS|ADVANCES]->(action:NextAction)
WHERE action.status = 'available'

// Step 3: expand to blocking concerns
OPTIONAL MATCH (goal)-[:BLOCKED_BY]->(blocker)

RETURN goal, similarity, collect(action) AS next_actions, collect(blocker) AS blockers
ORDER BY similarity DESC
LIMIT 8
```

**Ranking weights**: proposal sketch superseded — `goals_and_next_actions` ships with the default profile; see the Ranking Model table below.

---

### 3. `commitments_approaching`

**Purpose**: Surface commitments due within a time window, with people and goals context.

**Input**: `due_within_hours: int` (default 72), optional role filter

**Cypher pattern**:
```cypher
// Direct time query — no vector pivot needed
MATCH (c:Commitment)
WHERE c.status = 'open'
  AND c.due_at IS NOT NULL
  AND c.due_at <= $deadline_threshold
  AND c.validation_state <> 'retired'

// Expand to who was promised
OPTIONAL MATCH (c)-[:PROMISED_TO]->(person:Person)

// Expand to related goals
OPTIONAL MATCH (c)<-[:NEEDS_FOLLOWUP]-(related)

RETURN c, person, collect(related) AS related_nodes
ORDER BY c.due_at ASC
LIMIT 10
```

**Note**: This strategy uses time-based direct query rather than vector pivot. A secondary vector search on `memory_bridge_semantic__Commitment` can be appended to find semantically similar commitments when the direct query returns fewer than 3 results.

---

### 4. `re_entry_context`

**Purpose**: Help the operator re-enter a domain after a gap. Returns recent events, open loops, and active commitments in that domain.

**Input**: query embedding (domain description), `gap_since: string` (ISO 8601 timestamp)

**Cypher pattern**:
```cypher
// Step 1: vector pivot across two spaces
CALL vector_search.search("life_event_semantic__Event", 6, $query_vector)
YIELD node AS event_pivot, similarity AS event_sim
WHERE event_sim > 0.4 AND event_pivot.observed_at >= $gap_since

CALL vector_search.search("goal_system_semantic__Goal", 5, $query_vector)
YIELD node AS goal_pivot, similarity AS goal_sim
WHERE goal_sim > 0.35 AND goal_pivot.status = 'active'

// Step 2: expand events to open loops and next actions
OPTIONAL MATCH (event_pivot)-[:NEEDS_FOLLOWUP]->(loop:OpenLoop)
WHERE loop.status = 'open'

OPTIONAL MATCH (goal_pivot)-[:CONTAINS]->(action:NextAction)
WHERE action.status = 'available'

RETURN event_pivot, event_sim, goal_pivot, goal_sim,
       collect(DISTINCT loop) AS open_loops,
       collect(DISTINCT action) AS next_actions
ORDER BY event_sim DESC
LIMIT 12
```

---

### 5. `cross_domain_entanglement`

**Purpose**: Discover connections between two domains that the operator may not have explicitly linked (e.g., does sleep affect coding quality?).

**Input**: two query embeddings (`domain_a_vector`, `domain_b_vector`), max expansion depth (default 2)

**Cypher pattern**:
```cypher
// Step 1: pivot each domain separately
CALL vector_search.search("life_event_semantic__Signal", 8, $domain_a_vector)
YIELD node AS a_pivot, similarity AS a_sim
WHERE a_sim > 0.4

CALL vector_search.search("goal_system_semantic__Goal", 8, $domain_b_vector)
YIELD node AS b_pivot, similarity AS b_sim
WHERE b_sim > 0.4

// Step 2: find nodes reachable from both pivot sets within 2 hops
MATCH path_a = (a_pivot)-[*1..2]-(bridge)
MATCH path_b = (b_pivot)-[*1..2]-(bridge)
WHERE bridge.validation_state <> 'retired'

RETURN bridge,
       avg(a_sim) AS domain_a_relevance,
       avg(b_sim) AS domain_b_relevance,
       count(path_a) AS a_path_count,
       count(path_b) AS b_path_count
ORDER BY (domain_a_relevance + domain_b_relevance) DESC
LIMIT 8
```

**Note**: This is the most expensive strategy. Only invoke when the operator explicitly asks a cross-domain question or when a `DriftFinding` requests entanglement analysis.

**Implementation** (`data-memorygraphrag::entanglement`): the runner implements this as a dual-similarity intersection plus one-hop living-cycle bridge discovery.

1. The same candidate labels (`Signal`, `Event`, `OpenLoop`, `Goal`) are swept against **both** domain embeddings (floor 0.3 so near-threshold second-side scores are still observed).
2. Nodes at or above the 0.4 threshold on **both** sides are true semantic entanglement, ranked by `min(score_a, score_b)` and labeled `entanglement_kind = semantic_both`.
3. Strong single-side hits become bridge anchors: nodes one living-cycle hop (`OWNS|SHAPES|SETS|SPAWNS|RELATES_TO`) from a strong domain-A hit AND a strong domain-B hit are labeled `bridge`, scored `min(best_a_anchor, best_b_anchor) × 0.6`.
4. Up to 3 single-domain hits per side are kept for context, damped ×0.5 and labeled `domain_a_only` / `domain_b_only`.

Each ranked packet carries `entanglement_kind`, per-side similarities, `entangled_via` anchors (for bridges), and a human-readable `entanglement_reason`; the response envelope adds per-kind counts and an `explanations` array answering WHY each hit is entangled.

---

## Policy Filter Rules

Applied after vector search and graph expansion, before ranking. Nodes matching any drop rule are removed; the reason is logged in `policy_filter_log`.

| Rule | Condition | Action |
|---|---|---|
| `stale_inferred` | `validation_state = 'inferred'` AND `observed_at` older than 30 days without `last_confirmed_at` | Drop |
| `retired` | `validation_state = 'retired'` OR `status = 'retired'` OR `status = 'done'` OR `status = 'fulfilled'` | Drop |
| `low_confidence` | `confidence < 0.2` | Drop |
| `conflicted_unresolved` | `validation_state = 'conflicted'` AND no `EVIDENCED_BY` edge pointing to a confirmed source | Downweight (not drop); flag `policy_flags: ["conflicted"]` |
| `expires_passed` | Node has `expires_at` field AND `expires_at` < now | Drop |
| `context_inappropriate` | Node's `source_membrane` is not accessible to the current agent's permission scope | Drop |

---

## Ranking Model

> **Implemented** — `data-memorygraphrag::projection::ranking_score`. This section is the source of truth for the shipped weights; the per-strategy Cypher blocks above retain the original proposal shapes for context.

After policy filtering, each surviving candidate receives a composite `ranking_score`, clamped to `[0.0, 1.0]`:

```
ranking_score = clamp(
    semantic_similarity × similarity
  + recency             × recency_score
  + confirmation        × confirmation_score
  + active_commitment   × active_commitment_score
  + graph_specificity   × specificity_score
  + role_relevance      × role_matched            // 0 or 1 — additive bonus
)
```

### Weight defaults (`RankingWeights`)

The five base weights sum to 1.0. `role_relevance` is an *additive soft-zoning bonus* on top of the base — it biases ranking toward the caller's domain without ever filtering cross-domain hits. Callers may supply explicit `ranking_weights`; the named strategy's server-side profile applies only when omitted.

| Weight | Default (`semantic_pivot`, `goals_and_next_actions`, fallback) | `re_entry_context` | `open_loops_by_context` |
|---|---|---|---|
| `semantic_similarity` | 0.45 | 0.35 | 0.35 |
| `graph_specificity` | 0.20 | 0.15 | 0.15 |
| `recency` | 0.10 | 0.30 | 0.10 |
| `confirmation` | 0.15 | 0.15 | 0.10 |
| `active_commitment` | 0.10 | 0.05 | 0.30 |
| `role_relevance` (additive bonus) | 0.15 | 0.15 | 0.15 |

### Term definitions (as implemented)

- **Recency**: exponential decay from `observed_at` — `exp(-age_days / 20)` (~14-day half-life).
- **Confirmation**: `validation_state = confirmed` scores 1.0; all other states use the node's stored `confidence`. Unconfirmed nodes carrying a Muninn origin with `origin_trust >= 0.7` earn a `+0.15` lift on this term (capped at 1.0) — the first fusion term of the `lifegraph-muninn-promotion` seam. Nodes without `origin_trust` (everything written before Muninn provenance preservation) rank exactly as before.
- **Active commitment**: 1.0 for `Commitment` nodes with `status = 'open'`, else 0.0.
- **Specificity**: hub labels (`Person`, `Event`, `Project`) score 0.3; all other labels 1.0.
- **Role relevance**: 1.0 when the hit is tied to the caller's `active_role` domain — living-cycle edge to the V005 domain Role node, a `domain_slug` property match, or `observed_by` provenance mapping to the domain's steward agent — else 0.0. Philote's auto-recall lane sets `active_role` to a domain slug (`librarian | communications | companion | architect | chief_of_staff | musician | human`).

### Graph expansion and bridging (as implemented)

- **Read-side expansion** (`projection::expansion_cypher`): a single-round-trip, one-hop traversal over living-cycle edges from the ranked seed set. Expansion-discovered hits inherit `parent_score × 0.6` (`EXPANSION_SCORE_DECAY`), carry `expansion_origin` provenance, and are deduplicated against parents and earlier expansions. The caller's `ExpansionPolicy.allowed_edge_types` is intersected with the living-cycle vocabulary; unknown types are ignored, never interpolated into Cypher.
- **Cross-domain bridging** (`entanglement`): dual-similarity intersection plus one-hop living-cycle bridge discovery — see the `cross_domain_entanglement` implementation note above. Bridge hits score `min(best_a_anchor, best_b_anchor) × 0.6`.

### Edge vocabulary decision

The closed 5-type living-cycle set (`OWNS | SHAPES | SETS | SPAWNS | RELATES_TO`) is the **deliberate current vocabulary**: soft-zoning routes every agent domain through this small, whitelisted set, which keeps expansion Cypher injection-safe and ranking behaviour predictable. The original proposal's 14 domain-specific edge types (`BLOCKED_BY`, `NEEDS_FOLLOWUP`, `CONTAINS`, `PROMISED_TO`, `ADVANCES`, `APPLIES_TO_ROLE`, etc. — used in the strategy sketches above) remain **future candidates behind a deliberate extension decision**. Extending the vocabulary is an operator-flagged schema decision (governed `SchemaPatch`), not something an implementation slice should do incidentally.

---

## Strategy Selection

`data-memorygraphrag` selects the strategy based on turn intent. The intent is inferred from the operator's message or the active paracrine signal type:

| Turn intent / signal type | Strategy |
|---|---|
| General context question | `open_loops_by_context` |
| "What should I do next?" | `goals_and_next_actions` |
| "What did I promise?" / commitment signal | `commitments_approaching` |
| Re-entry after gap / `re_entry_hint` signal | `re_entry_context` |
| Cross-domain question ("does X affect Y?") | `cross_domain_entanglement` |
| Multiple intents | Compose: run two strategies, merge candidates, re-rank |

Agents may parameterize strategies freely. Agents may not write new strategies at runtime — new strategies require a `SchemaPatch` or `SkillPatch` through the governed patch workflow.

---

## Retrieval Flywheel

Every retrieval call records quality signals for later tuning:

1. Log `RetrievalQualitySignals` to the hotel's graph intelligence store
2. After turn completion, Beacon (or philote) may mark candidates as `useful`, `stale`, `missing`, `noisy`, or `overconfident` via `life.recall.feedback`
3. Findings accumulate into retrieval-tuning proposals:
   - Bridge edge gaps (two semantically related nodes not graph-connected)
   - Embedding space reassignment (a node type consistently appears in wrong space searches)
   - Ranking weight drift (role relevance systematically over- or under-weights)
4. Low-risk ranking/bridge improvements apply automatically (audit trail only)
5. Space reassignment and strategy changes require `SkillPatch` with `risk_tier: confirm_first`

---

## Context Budget

`data-memorygraphrag` enforces a token budget per retrieval call. Default budget: **2000 tokens** for the assembled context packet. When the ranked candidate set exceeds budget:

1. Drop candidates with `ranking_score < 0.4`
2. Truncate `path_summary` to the last 2 hops
3. Drop `policy_filter_log` from the packet (keep separately for audit)
4. If still over budget: drop candidates from the tail of the ranked list

Budget is tunable via `AttentionPatch`.

---

## Open Questions

- Should multi-space queries (like `re_entry_context`) run in parallel Bolt calls or sequentially? Parallel reduces latency but increases connection pressure on the single Memgraph instance.
- How should the ranking model handle nodes with no `embedding` yet (newly created, flywheel not yet run)? Current proposal: treat `similarity = 0.0`, rely entirely on graph expansion and non-similarity ranking factors.
- Should `cross_domain_entanglement` be gated to Beacon-only invocation, or can any philote use it?
- What is the right recency decay curve for ADHD-support contexts — faster or slower than the default 14-day half-life?
