---
title: Life Graph OS Proposal
doc_type: proposal
domain: memory-context
status: proposed
last_updated: 2026-06-05
tags:
- life-graph
- context-engine
- self-improving-agents
- adhd-support
- embeddings
- graphrag
- memgraphrag
related_docs:
- ARCHITECTURE_STATUS.md
- PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md
- GRAPH_DATASOURCE_PROPOSAL.md
- DISTRIBUTED_CRON_PROPOSAL.md
- MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md
- MEMORY_LAYERING_AND_WORK_PRODUCT_SPLIT_PROPOSAL.md
- EMBEDDINGS_IN_GRAPH_PROPOSAL.md
- EMBEDDINGS_TRAINING_DATA_PROPOSAL.md
- LOCAL_ONNX_INFERENCE_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: life-graph-os
implements:
- pluggable-context-engine
- graph-datasource
- memory-cultivation-true-up
implemented_by: []
active_seams:
- life-graph-schema
- life-graph-memorygraphrag-runner
- life-graph-attention-steward
- life-graph-agentic-growth-loop
- life-graph-semantic-retrieval
- life-graph-evidence-conflict
- life-graph-paracrine-heartbeat
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- SEAM_REGISTRY.md
- docs/task.md
---

# Life Graph OS Proposal

## Goal

Make Philotic a serious context engine for lived reality: a Life Graph OS that helps the operator remember, re-enter, follow through, and grow through daily interaction with philotes.

The target is not a bigger transcript archive. It is a living graph that captures roles, habits, systems, goals, commitments, open loops, preferences, concerns, and growth experiments well enough that agents can support the operator without constantly asking them to reconstruct their own life.

## Core Recommendation

Build Life Graph OS as a governed graph-plus-semantic retrieval layer on top of the existing Philotic memory and datasource boundaries:

1. keep `graph-datasource` generic: it should expose graph storage/query capabilities, not become Life Graph OS-specific
2. introduce a `data-memorygraphrag` runner/toolset as the MemoryGraphRAG manager that owns ontology/fact/passage workflows, retrieval strategies, and Life Graph tool projection
3. use Memgraph-backed `graph-datasource` as the first centralized graph/vector substrate for structured Cypher queries, graph mutations, and semantic retrieval
4. add a semantic indexing flywheel for graph nodes and selected relationships so fuzzy recall can find candidates before graph expansion
5. keep Muninn as the memory cultivation layer for summarization, forgetting, staleness, contradiction review, and compact continuity handles
6. introduce an Attention Steward that turns graph state into humane re-entry and follow-through, with explicit anti-nagging and anti-shame policy
7. let agents propose skills, tools, schema, and policy improvements from observed daily need, but govern writes through risk-tiered patch application

The working shape is GraphRAG-inspired, but Philotic should use it for context projection and action support, not just answer generation. The graph explains what matters and why; vectors help find the fuzzy entry point; policy decides what an agent may do with the result.

There are two similarly named inspirations here:

- Memgraph the database gives Philotic a plausible operational substrate for graph plus vector search.
- MemGraphRAG the research framework gives Philotic a useful memory architecture pattern: ontology, facts, and passages tied together by extraction, conflict handling, and graph-aware retrieval.

## Disposition

`accepted for current slices`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Current Slice

The first Life Graph OS boundary is now substantially implemented. Current proof:

- `life-graph-schema` (`schema-applied-live`): V001 migration live on Memgraph 3.10.1 vps-jane — 25 node types with uniqueness constraints and 18 property indexes. V002 adds `StewardshipInstruction` with 4 property indexes. V003 migrates 25 vector indexes across 5 semantic spaces to the canonical local ONNX baseline: 768d cosine using `Xenova/all-mpnet-base-v2`. Commits `2d528c3`, `86a730b`, `d8ed735`.
- `life-graph-paracrine-heartbeat` (`test-green-runtime-boundary`): cron can emit opt-in `paracrine_signal` heartbeats and philotes observe them without model re-entry.
- `life-graph-evidence-conflict` (`test-green-contracts`): `data-memorygraphrag` defines validated `EvidencePacket` and `ConflictHandoff` contracts with full test coverage.
- `life-graph-semantic-retrieval` (`spec-and-contracts-green`): 5 named retrieval strategies with Cypher patterns (`SEMANTIC_RETRIEVAL.md`), `RetrievalContextPacket` shape, policy filter rules, composite ranking model, and retrieval flywheel spec. Contracts wired in `data-memorygraphrag`. Commit `e381416`.
- `life-graph-memorygraphrag-runner` (`provider-handlers-green`): `data-memorygraphrag` runner with full `life.*` tool catalog, planner, and provider handlers for `life.observe`, `life.recall`, `life.commit`, `life.resolve`, `life.conflict`, and `life.patch.propose`. `life.observe` MERGE live and verified against Memgraph. `life.recall` now dispatches the five named strategies from `SEMANTIC_RETRIEVAL.md`. Provider write handlers compile and execute Memgraph MERGE/SET writes for commit, conflict, resolve, and patch proposal flows. 39 tests green. Commits `1883ea6`, `0265091`.
- `life-graph-philote-access` (`watched-live-green`): all seeded role profiles (`orchestrator`, `admin`, `codex`, `research`, `utility`, `architect`, `brain`, `virtuoso`) now carry the `life_graph` class and a vps-jane remote runner binding. mac-jane, mbp-jane, and vps-jane were deployed/restarted and live-smoked through `life.observe`/`life.recall` to the canonical vps-jane LifeGraph runner.
- `life-graph-agentic-growth-loop` (`test-green-contracts`): `data-memorygraphrag` defines patch gates, growth signals, drift categories, and risk-tiered policy evaluation.
- `life-graph-attention-steward` (`test-green-observe-policy`): SIL data model (`StewardshipInstruction`), Beacon stewardship contract, observe-only paracrine subscriber interface (8 signal types, 4 response types), anti-policy checklist. Spec at `docs/architecture/life-graph/ATTENTION_STEWARD.md`. Commit `86a730b`.

## Still Open (Backlog)

Per-seam open items for the next implementation round:

| Seam | Open |
|---|---|
| `life-graph-memorygraphrag-runner` | Retrieval quality logging and `life.recall.feedback`; basic hotel IPC invocation is live-smoked |
| `life-graph-semantic-retrieval` | Named strategy dispatch is provider-green; next is live retrieval quality logging and `life.recall.feedback` |
| `life-graph-evidence-conflict` | Runtime conflict detection and Muninn `true_up` / `contradiction_review` tool handoff still need wiring; provider `handle_conflict` / `handle_resolve` are test-green |
| `life-graph-attention-steward` | Active SIL entries and operator confirmation gate in philote (first slice is observe-only; active interruptions unlock after 5 confirmed SIL entries) |
| `life-graph-agentic-growth-loop` | Growth-loop philote role; background drift detector job |

Cross-cutting next pressure:

- embed `life.recall` in Beacon's turn context pipeline (claude-local)
- connect conflict handoff packets to Muninn `true_up` / `contradiction_review` tools
- add `life.recall` feedback path (`life.recall.feedback`) for the retrieval flywheel

## Codex Handoff — Group B Provider Completions

All Group B work lives in one file: `crates/data-memorygraphrag/src/provider.rs`.
Follow the `handle_observe` method (line ~108) as the pattern for every handler below.

### 1. `life.commit` — `handle_commit`

- Parse `LifeCommitInput` from `task.parameters`.
- Call `runner.plan(LifeGraphToolRequest::LifeCommit(input.clone()))`. If `!plan.allowed()` return blocked.
- If allowed, run a `MERGE (n:{label} {id: $id}) ON MATCH SET n.validation_state = 'confirmed', n.last_confirmed_at = $now` Cypher against Memgraph using the label and id from `input.evidence.claim_ref`.
- Return `{ status: "committed", node_id, label, validation_state: "confirmed" }`.

### 2. `life.resolve` — `handle_resolve`

- Parse `LifeResolveInput` from `task.parameters`.
- Call `runner.plan(...)`. Check `plan.allowed()`.
- Step 1 of the plan: `life.conflict.resolve` → run `MERGE (n:{label} {id: $id}) ON MATCH SET n.validation_state = 'proposed', n.adjudication_status = 'resolved'` for each `graph_fact_ref` in the handoff.
- Step 2 (if present in plan): `memory.true_up` / `memory.contradiction_review` / etc. → call Muninn MCP tool `muninn_evolve` or `muninn_decide` with the `conflict_id` and `resolution_summary` as payload. Use the existing Muninn MCP client pattern from the codebase.
- Return `{ status: "resolved", handoff_id, conflict_id, muninn_step: <step action or "none"> }`.

### 3. `life.patch.propose` — `handle_patch_propose`

- Parse `LifePatchProposalInput` from `task.parameters`.
- Call `runner.plan(...)`. Check `plan.allowed()`. High-risk patches (`PatchRisk::High`) require `operator_approved: true`.
- Write the patch as a node of the appropriate `*Patch` label (e.g. `SchemaPatch`, `SkillPatch`) using `MERGE (n:{label} {id: $id}) ON CREATE SET ...` with all fields from `input`: `summary`, `rationale`, `risk`, `status: "proposed"`, provenance fields.
- Return `{ status: "proposed", patch_id, patch_kind, risk, requires_operator }`.

### 4. Named strategy dispatch in `handle_recall`

Currently `handle_recall` runs a generic multi-label search across all labels in each pivot's space. Extend it to dispatch to named strategy Cypher when `query.operator_intent` matches a known strategy name:

| `operator_intent` | Cypher pattern | Key filters |
|---|---|---|
| `"open_loops_by_context"` | Vector pivot on `life_event_semantic__OpenLoop` top_k=10, expand `BLOCKED_BY\|NEEDS_FOLLOWUP\|CONTAINS` depth 1 | `status = 'open'`, `confidence >= 0.3` |
| `"goals_and_next_actions"` | Vector pivot on `goal_system_semantic__Goal` top_k=8, expand `CONTAINS\|ADVANCES` to `NextAction` | `status IN ['active','paused']` |
| `"commitments_approaching"` | Direct time query on `Commitment` by `due_at <= $deadline` (no vector), expand `PROMISED_TO` | `status = 'open'` |
| `"re_entry_context"` | Vector pivot on `life_event_semantic__Event` + `goal_system_semantic__Goal` top_k=6, filter `observed_at >= $gap_since` | recent + `status = 'active'` |

See `docs/architecture/life-graph/SEMANTIC_RETRIEVAL.md` for the full Cypher patterns.

Fall back to the current generic multi-label search for any unrecognised `operator_intent`.

### 5. `life.conflict` detection in `handle_conflict` (new tool kind)

Add `"life.conflict"` to the `invoke` dispatch. Parse two `EvidencePacket`s from `task.parameters["existing"]` and `task.parameters["candidate"]`. Compare `claim_ref.id` + `claim_ref.label` + `validation_state`. Emit a `ConflictHandoff` using `ConflictFindingType::DirectContradiction` if both reference the same node ID with conflicting `validation_state`. Write the conflict as a `CONTRADICTS` edge in Memgraph between the two node IDs. Return the serialised `ConflictHandoff`.

### Reference files for Codex

| File | Purpose |
|---|---|
| `crates/data-memorygraphrag/src/provider.rs` | The file to edit; `handle_observe` at line ~108 is the reference pattern |
| `crates/data-memorygraphrag/src/lib.rs` | All contract types: `LifeCommitInput`, `LifeResolveInput`, `LifePatchProposalInput`, `ConflictHandoff` |
| `crates/data-memorygraphrag/src/cypher.rs` | Cypher compilation helpers to extend or mirror |
| `docs/architecture/life-graph/SEMANTIC_RETRIEVAL.md` | Named strategy Cypher patterns (strategies 1–5) |
| `docs/architecture/life-graph/LIFE_GRAPH_SCHEMA.md` | Node labels, property shapes, provenance envelope fields |

Current confidence:

- `schema-live-green`: V001+V002 applied and verified on vps-jane Memgraph
- `test-green` for all contract and projection slices (32 tests in `data-memorygraphrag`)
- `runtime-verified`: `life.observe` writes `Signal` nodes to Memgraph; confirmed via `mgconsole`
- not yet: end-to-end `life.recall` invocation from a philote turn

## Why This Exists

Philotic already has pieces of the right machine:

- hotel-owned runtime authority
- graph-backed sessions and work products
- Muninn memory recall and cultivation
- graph-datasource as a Cypher-facing agent tool surface
- role-aware philotes that can work across membranes and hotels

The missing layer is the one the operator actually wants to live inside: a system that can keep track of life with enough structure to help, enough semantic recall to find the right thing when wording changes, and enough judgment to avoid becoming a noisy productivity machine.

ADHD support is a first target, not a side effect. The system should help with:

- re-entering work after interruption
- surfacing open loops at the right time
- remembering commitments and promises
- connecting goals to habits, systems, and next actions
- reducing friction without turning rest, recovery, or ambiguity into failure

## Canonical Ownership Split

Life Graph OS must not collapse all context into one magical database.

| Layer | Owner | Responsibility |
| --- | --- | --- |
| Life Graph | `graph-datasource` backed by Memgraph first | structured lived reality, relationships, commitments, growth state |
| Muninn | Muninn memory engine | compact continuity, summarization, forgetting, staleness, relationship salience |
| Context Engine | Philotic context engine | turn-ready projection, ranking, budgeting, provenance |
| Paracrine Loop | hotel/runtime signal plane | heartbeat-style maintenance signals, scan triggers, low-agency background observations |
| Attention Steward | reusable role-type, primarily assigned to Beacon | timing, re-entry, follow-through, anti-nagging policy |
| Agentic Growth Steward | governed agent workflow | propose and validate skills, tools, schema, and policy improvements |

The graph stores structured truth and evidence. Muninn cultivates memory. The context engine composes. Philotes act within policy.

## Cron-Backed Paracrine Heartbeat Loop

Heartbeat-style work should flow through the paracrine loop by default.

Cron should be the first clock source for durable scheduled maintenance, and the paracrine loop should be the semantic signal bus. That means Attention Steward should not be modeled as a bespoke scheduler or a conversational role that wakes itself up. It should be a role-type/capability that subscribes to paracrine signals produced from cron-backed heartbeat jobs:

```text
cron job or runtime heartbeat
  -> paracrine signal
  -> subscribed role-types evaluate
  -> observe-only findings, SIL updates, or gated action proposals
  -> Beacon arbitrates cross-domain attention
```

The existing distributed cron subsystem can register durable schedules, sync jobs across hotels, deduplicate guaranteed firings, and deliver pre-packaged envelopes. That is a good substrate for a heartbeat engine. The heartbeat engine should evolve cron from "deliver this payload to this role" toward "emit this typed paracrine signal on this cadence."

Use the same pattern for most background maintenance:

- attention scans
- stale open-loop checks
- retrieval quality reviews
- embeddings flywheel evaluation
- Muninn cultivation prompts
- MemoryGraphRAG conflict scans
- low-agency growth-loop observations

The paracrine loop should carry small, typed signals rather than full cognitive turns. A role-type may respond by recording an observation, proposing a patch, or requesting a normal turn, but a heartbeat should not silently become a user-facing interruption.

Baseline heartbeat job shape:

```text
job_id
schedule
owner_hotel
guaranteed
target_signal_type
target_role_type
scope
subject_query
priority
policy_tags
payload_template
```

Baseline paracrine signal shape:

```text
signal_id
signal_type
scope
source_hotel
target_role_type
subject_refs
cadence
priority
observed_at
expires_at
payload_summary
policy_tags
```

Attention Steward consumes these signals as `target_role_type = attention-steward`, with Beacon as the primary cross-domain steward.

This split gives the system three replaceable layers:

- `cron` owns durable timing, persistence, mesh sync, and deduplication
- `heartbeat engine` turns schedules into typed paracrine signals
- `paracrine subscribers` decide whether to observe, update SIL, propose a patch, or request a normal turn

## Runner And Toolset Boundary

`graph-datasource` should remain a generic graph provider boundary. It knows how to create partitions, execute Cypher, expose schema, validate queries, and run provider-backed graph/vector operations. It should not know that a graph is "life" except through labels, partition metadata, and access policy.

The Life Graph OS-specific runner should be `data-memorygraphrag` until a better name earns its keep. That runner manages the higher-level MemoryGraphRAG toolset:

- ontology/fact/passage extraction and indexing
- named retrieval strategies
- evidence packet assembly
- conflict detection and adjudication requests
- Life Graph tool projection such as `life.observe`, `life.recall`, `life.commit`, `life.resolve`, and `life.patch.propose`
- policy-aware write plans that can be executed through `graph-datasource`

This keeps the substrate reusable for project graphs, agent work graphs, and future domain graphs while giving Life Graph OS a coherent memory-specific capability layer.

## Stewardship Model

Beacon is the primary steward of the operator's Life Graph: the chief-of-staff role responsible for keeping the graph coherent, useful, and humane across daily life.

That means Beacon should own the highest-level Life Graph posture:

- noticing unresolved open loops and stale commitments
- coordinating re-entry across goals, systems, habits, and projects
- asking for confirmation when facts, priorities, or attention policy are ambiguous
- delegating specialized support to roles such as Coach without handing them canonical ownership of the Life Graph
- reviewing drift findings and proposed schema, skill, tool, or attention patches before they become durable behavior

Specialized roles may read from and contribute to Life Graph OS, but Beacon should remain the default steward for cross-domain prioritization, follow-through, and operator-level coherence.

## First Schema Vocabulary

The initial graph should stay small enough to use, but expressive enough to stop forcing life into generic note blobs.

### Node Types

- `Person`
- `Role`
- `Goal`
- `System`
- `Habit`
- `Project`
- `Commitment`
- `OpenLoop`
- `NextAction`
- `Routine`
- `Decision`
- `Preference`
- `Value`
- `Concern`
- `Event`
- `Signal`
- `GrowthHypothesis`
- `GrowthExperiment`
- `DriftFinding`
- `CapabilityPatch`
- `SkillPatch`
- `ToolPatch`
- `SchemaPatch`
- `AttentionPatch`
- `SystemPatch`

### Relationship Types

- `OWNS`
- `SUPPORTS`
- `CONTAINS`
- `ADVANCES`
- `BLOCKED_BY`
- `PROMISED_TO`
- `RECURS`
- `NEEDS_FOLLOWUP`
- `SUPERSEDES`
- `CONTRADICTS`
- `EVIDENCED_BY`
- `REDUCES_FRICTION_FOR`
- `SUGGESTS_PATCH`
- `APPLIES_TO_ROLE`

Every inferred node or relationship should carry provenance, confidence, validation state, source membrane, observed time, and last-confirmed time.

## Embeddings And Semantic Retrieval

Graph edges provide precision, explainability, and policy hooks. Embeddings provide fuzzy recall, semantic candidate generation, and phrasing tolerance. Treating either one as the whole answer would be tidy, wrong, and probably very satisfying to a diagram.

Life Graph OS should support multiple vector spaces rather than one undifferentiated embedding pile:

- `life_event_semantic`
- `goal_system_semantic`
- `skill_tool_semantic`
- `role_person_semantic`
- `memory_bridge_semantic`

Each embedded record should store:

- `embedding_model`
- `embedding_model_gen`
- `embedding_dims`
- `embedding_hash`
- `embedding_updated_at`
- `embedding_source_text_hash`
- `embedding_space`

The retrieval pipeline should be:

1. semantic vector search finds candidate graph nodes or edges
2. graph expansion follows bounded neighborhoods and typed paths
3. policy filters remove stale, unsafe, overconfident, or context-inappropriate candidates
4. role-aware ranking chooses what matters for this conversation turn
5. context projection emits a bounded packet with evidence paths, not raw graph sprawl

### Vector Database Posture

Start with Memgraph vector indexes if they are sufficient for the first Life Graph OS retrieval slice. This keeps graph and vector retrieval co-located, reduces moving parts, and fits the current centralized `vps-jane` graph-datasource direction.

Use the embeddings flywheel deliberately:

- baseline vector dimension: `768`
- canonical local embedding model: `Xenova/all-mpnet-base-v2`
- preferred high-capacity dimension: `1536` or `3072` when the selected embedding model supports it without unacceptable latency/cost
- minimum acceptable dimension for the first local Life Graph OS semantic spaces: `768`
- never mix dimensions or models inside one `embedding_space`
- every embedding write records model, generation, dimension, source hash, and retrieval performance metadata
- re-embedding jobs should be scheduled when model generation changes, source text changes, or retrieval evaluation shows drift

The baseline should be large enough to support durable semantic retrieval across life domains, not just short note lookup. The current accepted local baseline is 768d because it is deployed, indexed, and runnable on vps-jane today; larger models remain a future migration when latency, cost, and reindexing are justified.

Add a dedicated vector database only when one of these is proven:

- Memgraph vector search cannot meet required scale, latency, or recall behavior
- multiple embedding spaces need lifecycle controls Memgraph cannot model cleanly
- ANN tuning, hybrid scoring, or bulk re-embedding workflows need stronger specialized support
- operational isolation is more valuable than keeping graph and vector retrieval in one engine

Muninn remains its own semantic memory layer. The Life Graph vector path is for graph entities and action support, not a replacement for Muninn's engram recall.

### Retrieval Flywheel

Semantic retrieval should improve through observed use:

1. record the query, selected strategy, candidate set, final context packet, and downstream outcome signal
2. mark which facts/passages were useful, ignored, stale, missing, or misleading
3. generate retrieval-tuning findings for bridge edges, ontology gaps, embedding-space choice, and ranking weights
4. apply only low-risk ranking/bridge improvements automatically
5. require Beacon or operator confirmation for schema changes, identity merges, and attention-policy changes

## Memgraph GraphRAG Applicability

Memgraph's GraphRAG direction is relevant because it treats retrieval as graph-native composition: semantic pivots, graph expansion, ranking, and prompt/context assembly can be expressed close to the database instead of orchestrated as many scattered calls.

Apply that idea to Philotic like this:

- `graph.query` remains the agent-facing Cypher interface for structured graph inspection
- Life Graph retrieval strategies become named query shapes, not one-off tool explosions
- vector search supplies candidate pivots for fuzzy human language
- Cypher expands from those pivots through meaningful relationships
- the context engine receives compact evidence paths and scored context packets
- write paths remain governed by Philotic patch policy, not by arbitrary agent-generated Cypher

Agentic GraphRAG maps cleanly to philotes selecting the right retrieval strategy per turn. The safety boundary is that agents may choose and parameterize retrieval strategies freely, but mutating the Life Graph requires a governed patch with provenance and risk tier.

## MemGraphRAG Memory Architecture

MemGraphRAG, the memory-based GraphRAG framework, is relevant for a different reason: it treats graph construction as an active memory problem instead of a one-pass chunk extraction problem.

The Life Graph OS adaptation should use a three-layer memory graph:

| Layer | Life Graph OS meaning | Philotic owner |
| --- | --- | --- |
| Ontology | allowed life domains, node types, relation types, frequencies, and schema confidence | Life Graph schema plus governed schema patches |
| Facts | concrete triples and events such as commitments, habits, metrics, decisions, and open loops | Life Graph fact layer in `graph-datasource` |
| Passages | raw evidence snippets from chats, notes, logs, calendar, health data, and membrane events | source-specific evidence records, Muninn engrams, or graph passage nodes |

That layer split maps well to Philotic's existing source-of-truth discipline:

- ontology is the controlled vocabulary that keeps agents from inventing a new life taxonomy every afternoon
- facts are the current structured beliefs, with confidence and validation state
- passages are inspectable grounding so the operator and agents can debug why the system believes something

### Extractor And Adjudicator Agents

Life Graph OS should start with a small librarian-agent society:

- `life.extractor` reads raw observations and proposes candidate ontology, fact, and passage records
- `life.conflict_detector` finds mutually exclusive, time-conflicting, or granularity-conflicting facts
- `life.conflict_handler` adjudicates using source passages, recency, source reliability, and operator confirmation when risk is high
- `life.bridge_builder` links aliases, related concepts, and semantically similar records without silently merging identity
- `life.retriever` chooses retrieval strategy and emits evidence-backed context packets

These are agent roles/capabilities, not necessarily separately materialized guests on day one.

### Evidence And Conflict Handling

Muninn already owns cognitive-memory cultivation, contradiction review, staleness, trust updates, and promotion gates through `memory.cultivate`, `memory.true_up`, and related memory tooling.

`data-memorygraphrag` should not duplicate that job. It should own structured graph evidence/conflict handling for ontology, fact, and passage records, then hand cognitive-memory implications to Muninn.

Use this split:

| Concern | Primary owner | Notes |
| --- | --- | --- |
| memory staleness, trust, consolidation, forgetting | Muninn | cognitive continuity and engram lifecycle |
| graph fact conflicts | `data-memorygraphrag` | conflicting triples, time-scoped facts, source reliability, passage evidence |
| promotion into durable graph truth | shared gate | requires evidence, validation, or explicit operator approval |
| contradiction surfaced during recall | Muninn first, then `data-memorygraphrag` if graph facts are involved | avoids dueling adjudicators |
| Life Graph evidence packet | `data-memorygraphrag` | consumed by context engine and Beacon |

The common contract should be an `EvidencePacket` with source refs, passage refs, confidence, validation state, observed time, valid time range, source reliability, conflict IDs, and adjudication status.

### Bridging And Ranking

The bridge layer should use both typed ontology and embeddings:

- type-based bridges connect records through shared ontology categories such as `Person`, `Habit`, `HealthMetric`, `Project`, or `Commitment`
- similarity bridges connect semantically close passages, facts, and aliases using embedding spaces
- identity bridges require stricter evidence and should never silently collapse names, family roles, usernames, and nicknames into one person

For retrieval, Life Graph OS should evaluate a memory-aware ranking strategy alongside simple vector-then-expand:

1. initialize candidate nodes from query semantics, active role, recent session state, and explicit operator intent
2. downweight generic hubs such as `Person`, `Event`, or `Project`
3. upweight specific passages, active commitments, blocked goals, and recently confirmed facts
4. propagate relevance through bounded graph neighborhoods using a PageRank-style or path-scoring algorithm
5. return the top facts and passages with their evidence paths

This is especially important for cross-domain questions like whether sleep, rowing, coding quality, and mood are entangled. Cosine similarity can find nearby language; heterogeneous graph ranking can find the structure that makes the question answerable.

## Agentic Growth Loop

The operator wants philotes to help build the system organically from daily interactions. That should become an explicit loop:

```text
daily interaction
  -> ObservedNeed | DriftFinding | CapabilityGap | GrowthExperiment
  -> proposed patch
  -> risk tier
  -> apply, confirm, or defer
  -> observe outcomes
  -> keep, tune, or revert
```

Patch types:

- `SkillPatch` for better agent behaviors and workflows
- `ToolPatch` for new or revised tool access
- `SchemaPatch` for graph vocabulary changes
- `AttentionPatch` for timing, reminder, and re-entry behavior
- `SystemPatch` for broader operating-loop changes

Risk tiers:

| Tier | Examples | Required gate |
| --- | --- | --- |
| Safe auto-update | stale markers, reminder timing hints, attach evidence to existing goal, cluster duplicate open loops | automatic with audit trail |
| Confirm first | create or retire goal, infer recurring habit cadence, change role support strategy | operator confirmation |
| Proposal only | identity/value inference, broad attention policy, new autonomous tool, notification channel expansion | explicit proposal and review |

## Baseline Operating Policy

The baseline should be conservative enough to build trust and instrumented enough to learn quickly:

- Beacon is the only default cross-domain steward.
- `graph-datasource` remains generic; `data-memorygraphrag` owns the Life Graph-specific toolset.
- Cron-backed heartbeat jobs emit typed paracrine signals.
- For the first slice, Attention Steward consumes paracrine heartbeat signals in observe-only mode.
- No autonomous broad notifications until observe-only evidence shows the timing and tone policy is helpful.
- Agents may auto-attach evidence, mark stale low-risk facts, and propose bridge/ranking improvements.
- Agents may not auto-create identity, values, broad goals, notification policy, or new autonomous tool access.
- New habits, recurring commitments, and role-level attention patterns require confirmation until reinforced across repeated evidence.
- Every write path records provenance, confidence, validation level, and rollback/audit metadata.
- Retrieval quality is measured from the start: useful, stale, missing, noisy, and overconfident context packets should all become tuning signals.

## Attention Steward SIL

The Attention Steward should maintain a building SIL: a Stewardship Instruction Layer of reinforced, situation-aware rules for when, how, and whether to surface something.

Each stewardship instruction should carry:

- `situation`: the context where the rule applies
- `trigger`: what makes the rule eligible
- `recommended_action`: surface, defer, ask, summarize, nudge, suppress, or escalate
- `tone`: direct, gentle, tiny-step, reflective, celebratory, or quiet
- `evidence_refs`: facts, passages, prior outcomes, or operator confirmations
- `reinforcement_count`: how often this rule helped
- `friction_count`: how often this rule annoyed, distracted, or misfired
- `exceptions`: situations where the rule should not fire
- `owner`: Beacon by default for cross-domain stewardship
- `status`: proposed, active, dampened, retired, or blocked

SIL updates should build up from use:

1. observe a repeated situation
2. propose a stewardship instruction
3. run it silently or as a low-risk suggestion
4. record outcome signals
5. reinforce, clarify, dampen, or retire it

The first Attention Steward slice should only create proposed or observe-only SIL entries from paracrine heartbeat signals. Active instructions that interrupt the operator require confirmation until there is enough evidence to trust the pattern.

## Negative Drift Checks

Self-improvement must include a drift detector, not just a growth accelerator.

Watch for:

- nagging or intrusive timing
- stale facts treated as current
- inferred goals presented as commitments
- productivity bias over rest, recovery, or play
- graph clutter that makes retrieval noisier
- overgeneralizing from one bad day
- agents optimizing their own convenience instead of operator outcomes
- tools or skills expanding agency without a matching policy gate

Every recurring drift finding should be able to generate a patch proposal or a rollback suggestion.

## First Implementation Slices

1. Keep `graph-datasource` generic and define `data-memorygraphrag` as the Life Graph / MemoryGraphRAG toolset runner.
2. Define the first Life Graph schema and Cypher migrations for `Role`, `Goal`, `System`, `Habit`, `Commitment`, `OpenLoop`, `NextAction`, and `GrowthExperiment`.
3. Add a small tool surface: `life.observe`, `life.recall`, `life.commit`, `life.resolve`, and `life.patch.propose`.
4. Add semantic indexing for Life Graph nodes with a `768`-dimension baseline, `embedding_model_gen`, and `embedding_space` fields.
5. Implement one retrieval strategy: semantic pivot plus bounded graph expansion into a context packet.
6. Add the first `EvidencePacket` and conflict handoff contract between `data-memorygraphrag` and Muninn.
7. Define the cron-backed heartbeat job shape and paracrine signal shape for Life Graph maintenance and role-type subscriptions.
8. Build an Attention Steward paracrine subscriber in observe-only mode with proposed SIL entries only.
9. Wire Beacon as the first Life Graph steward and chief-of-staff role, then let specialized roles such as Coach consume and contribute through governed tools.

## Open Questions

- Should Life Graph OS live as a dedicated datasource partition, a set of labels in the central graph, or both?
- Which embeddings model should be canonical for the first graph-vector slice, and how should re-embedding be scheduled?
- What is the smallest humane notification policy that supports ADHD follow-through without becoming a nag loop?
- How much Life Graph state should sync across hotels versus stay centralized on `vps-jane` behind mesh access?
- Should agent-generated patch proposals be stored in the Life Graph, Muninn, the project graph, or a dedicated governance graph?
