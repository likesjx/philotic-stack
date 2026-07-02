---
title: Memory Cultivation and True-Up Proposal
doc_type: proposal
domain: memory-context
status: implemented
disposition: accepted-current-slice
last_updated: 2026-05-20
tags:
- muninn
- memory
- graph-intelligence
- agent-graph
- spacetime
- true-up
- cultivation
related_docs:
- ARCHITECTURE_STATUS.md
- GRAPH_INTELLIGENCE_PROPOSAL.md
- GRAPH_INTELLIGENCE_STATUS.md
- GRAPH_DATASOURCE_PHILOTE_PROPOSAL.md
- MUNINN_MEMORY_PROTOCOL_PROPOSAL.md
- MEMORY_CONTEXT.md
- SEAM_REGISTRY.md
- docs/task.md
task_refs:
- docs/task.md
proposal_id: memory-cultivation-true-up
implements:
- graph-intelligence
- muninn-memory-protocol
implemented_by: []
active_seams:
- memory-spacetime-frame
- memory-shaping-context
- memory-cultivation-loop
- graph-muninn-true-up
- memory-promotion-gates
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- SEAM_REGISTRY.md
- docs/task.md
---

# Memory Cultivation and True-Up Proposal

## Goal

Make Philotes capable of tending their own long-term memory without letting
Muninn become a second source of truth beside AgentGraph, repo docs, code, and
observed runtime state.

The desired system has three properties:

1. memory writes are shaped by graph, session, user, and spacetime context
2. memory is cultivated over time through linking, evolution, trust updates,
   consolidation, decay, and promotion review
3. contradictions between Muninn, AgentGraph, repo truth, and runtime truth are
   detected and resolved through an explicit true-up workflow

The north star is simple: **AgentGraph owns structured truth; Muninn owns
cognitive continuity; Philote owns the reflective loop that keeps them aligned.**

## Disposition

Implemented and deployed on the current hotels. The first implementation slice
is intentionally narrow and source-compatible:

- add a shared `MemorySpacetimeFrame` / `MemoryShapingContext` type
- attach that frame to Philote memory recall/write projections
- record true-up findings as graph nodes/mutations using existing
  graph-intelligence primitives
- keep promotion into graph/docs gated by evidence or operator approval

Implemented in the first source slice:

- `MemorySpacetimeFrame`, `GraphAnchors`, and `MemoryShapingContext` are defined
  in Philote session types
- recalled memory projection now includes temporal kind, observed/verified
  timestamps, spatial scope, space summary, authority, and validation level
- recalled Muninn metadata can deserialize an explicit `spacetime_frame`
- fallback inference preserves older memory records that do not yet carry a
  frame
- shaped `memory.remember` writes now attach spacetime frame, graph anchors, and
  graph-derived Muninn entities/relationships
- `memory.cultivate` reports closeout/staleness candidates without mutating
  memory
- `memory.true_up` classifies memory-vs-observed/graph mismatches
- `memory.promote_candidate` gates promotion behind authority, validation, and
  evidence, unless the operator explicitly approves
- graph-intelligence records true-up findings as audited task nodes/mutations
  using existing node primitives

Runtime verification:

- local Homebrew install contains `memory.cultivate`, `memory.true_up`, and
  `memory.promote_candidate`
- `mbp-jane` restarted from `/opt/homebrew/bin/aiua` with installed Philote
  guests and the new memory tools present in `/opt/homebrew/bin/philote`
- `vps-jane` restarted `philotic-hotel` from `/opt/philotic/bin/aiua`, spawned
  `/opt/philotic/bin/philote`, and the new memory tools are present in the
  installed Philote binary

Do not start by adding a magical autonomous memory daemon. That way lies a very
organized hallucination engine wearing a nametag.

## Core Recommendation

Add a **Memory Cultivation Plane** that sits between Philote, Muninn, and
AgentGraph.

```text
AgentGraph/repo/runtime truth
        -> MemoryShapingContext
        -> Philote cognitive envelope
        -> Muninn recall/write
        -> Cultivation and true-up pass
        -> promotion candidates
        -> AgentGraph/docs/code only after evidence gates
```

The plane has four responsibilities:

1. **Shape memories at birth** with graph and spacetime coordinates.
2. **Project memories honestly** into the cognitive envelope with authority,
   scope, and freshness labels.
3. **Cultivate memories** after meaningful sessions by linking, evolving,
   consolidating, trust-marking, or archiving.
4. **True-up contradictions** between recalled memory, graph state, current
   repo/docs/code, runtime observations, and operator statements.

## Ownership Split

| Substrate | Owns | Must not own |
| --- | --- | --- |
| Repo/docs/code | implemented and intended truth, human-reviewable contracts | fuzzy recollection of why something mattered |
| AgentGraph / graph-intelligence | proposals, seams, sessions, workstreams, decisions, tests, structure, mutation audit | autobiographical or relationship memory |
| Muninn | durable cognitive continuity, preferences, reality gaps, lessons, semantic/entity memory | canonical project state, task queue, deployed-runtime claims |
| Philote | projection, reasoning, cultivation, promotion requests | silent mutation of canonical truth |

If Muninn and AgentGraph disagree, the system should not pick the more poetic
answer. It should run true-up.

## Spacetime Frame

Every durable memory and true-up finding should carry a compact frame:

```rust
pub struct MemorySpacetimeFrame {
    pub observed_at: DateTime<Utc>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_until: Option<DateTime<Utc>>,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub temporal_kind: TemporalKind,
    pub spatial_scope: SpatialScope,
    pub hotel_id: Option<String>,
    pub node_id: Option<String>,
    pub workspace_path: Option<String>,
    pub repo_id: Option<String>,
    pub branch: Option<String>,
    pub worktree_id: Option<String>,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub primary_user_id: Option<String>,
    pub authority: MemoryAuthority,
    pub validation_level: Option<ValidationLevel>,
}
```

Initial enums:

```text
TemporalKind:
- event
- state
- preference
- rule
- decision
- hypothesis
- gap
- checkpoint

SpatialScope:
- self
- agent
- user
- session
- workspace
- hotel
- mesh
- global

MemoryAuthority:
- observed_runtime
- observed_repo
- graph_structured
- user_stated
- verified_memory
- inferred_memory
- external
- untrusted

ValidationLevel:
- unverified
- check-green
- test-green
- smoke-green
- watched-live-green
```

Temporal rule: an old state memory is not automatically false. It may be
expired, superseded, or scoped to a different hotel/worktree. True-up should
preserve history instead of flattening time into a shrug.

## Memory Shaping Context

`MemoryShapingContext` is the object Philote should assemble before recall,
write, cultivation, or promotion review.

```rust
pub struct MemoryShapingContext {
    pub frame: MemorySpacetimeFrame,
    pub graph_anchors: GraphAnchors,
    pub recall_policy: RecallPolicy,
    pub write_policy: WritePolicy,
    pub promotion_policy: PromotionPolicy,
}

pub struct GraphAnchors {
    pub proposal_id: Option<String>,
    pub seam_id: Option<String>,
    pub task_id: Option<String>,
    pub decision_id: Option<String>,
    pub test_run_id: Option<String>,
    pub source_doc: Option<String>,
    pub source_file: Option<String>,
    pub affected_nodes: Vec<String>,
}
```

The shaping context answers:

- what graph object does this memory attach to?
- where and when was it observed?
- who was it true for?
- how strong is the evidence?
- should this memory ever be promoted to graph/docs/code?

## Graph To Muninn

AgentGraph should shape Muninn writes.

For every Philote memory write, include graph-derived anchors as Muninn
entities and relationships:

- `proposal_id`
- `seam_id`
- `task_id`
- `decision_id`
- `test_run_id`
- `session_id`
- `agent_id`
- `primary_user_id`
- `hotel_id`
- `workspace_path`
- `branch`
- `validation_level`

Example:

```text
Concept: deployed-runtime-truth-gap
Content: As of 2026-05-20T09:54:00-04:00, local source had the Muninn
entity overlay but vps-jane and mbp-jane running Philote binaries did not.
Temporal kind: state
Authority: observed_runtime
Spatial scope: hotel
Entities: Philote, Muninn, vps-jane, mbp-jane
Relationships: Philote projects MuninnEntityOverlay; vps-jane lacks overlay at observed_at
```

This makes Muninn recall graph-aware without making Muninn the graph.

## Muninn To Graph

Muninn should influence AgentGraph through promotion candidates, not direct
truth mutation.

Promotion candidates can be:

- recurring reality gap -> defect, seam, or rule candidate
- durable preference -> agent/user profile candidate
- repeated relationship -> graph edge candidate
- stale operational claim -> true-up finding
- validated decision -> graph decision record
- cultivation insight -> proposal/task/process update candidate

Promotion requires one of:

- explicit operator approval
- current repo/docs/code observation
- current runtime observation
- validation event (`test-green`, `smoke-green`, or `watched-live-green`)
- repeated high-confidence memories across sessions

This keeps the graph honest. Otherwise one fuzzy memory could edit the map and
we would have invented bureaucracy-powered gaslighting.

## Cultivation Loop

Philote should run cultivation at boundaries, not every turn.

Trigger points:

- session close
- after a coherent implementation slice
- after deploy/runtime verification
- after context compression
- when `muninn_contradictions` reports a conflict
- when recalled memory conflicts with graph/runtime truth
- scheduled quiet maintenance

Cultivation actions:

- `link`: create `supports`, `contradicts`, `supersedes`, `refines`,
  `references`, or `depends_on` associations between memories
- `evolve`: update a memory when later evidence changes its validity window or
  interpretation
- `consolidate`: merge redundant memory fragments into a sharper memory
- `trust`: mark memories verified, inferred, external, or untrusted
- `archive`: remove outdated low-value state memories from active recall
- `promote`: emit a gated candidate for AgentGraph/docs/code
- `decay`: reduce confidence or retrieval priority for old unverified state
- `enrich`: fill missing summary/entities/relationships/classification

Low-risk cultivation may happen automatically:

- linking related memories
- marking inferred memories as stale when explicit newer evidence exists
- adding missing spacetime metadata when directly observable
- creating a promotion candidate

High-risk cultivation requires a gate:

- deletion/forgetting
- promotion into AgentGraph/docs/code
- marking user-stated preferences untrusted
- overwriting graph-backed decisions
- changing active task or seam status

## True-Up Loop

`memory.true_up` compares Muninn memory against AgentGraph, repo/docs/code,
runtime state, validation records, and current operator statements.

Input scope:

- `entity`
- `proposal`
- `seam`
- `session`
- `hotel`
- `workspace`
- `agent`
- `user`
- `mesh`

Finding types:

- `confirmed`
- `stale`
- `contradicted`
- `underspecified`
- `promote_candidate`
- `demote_candidate`
- `split_required`
- `merge_required`
- `needs_operator`

Authority ladder:

1. current observed runtime truth
2. current repo/docs/code truth
3. AgentGraph structured fact with validation evidence
4. explicit recent user/operator statement
5. verified Muninn memory
6. inferred Muninn memory
7. old or unstamped memory

True-up should write a report, not hide the reconciliation:

```rust
pub struct MemoryTrueUpFinding {
    pub finding_id: String,
    pub finding_type: TrueUpFindingType,
    pub scope: TrueUpScope,
    pub entities: Vec<String>,
    pub muninn_ids: Vec<String>,
    pub graph_ids: Vec<String>,
    pub evidence_refs: Vec<String>,
    pub resolution: Option<String>,
    pub recommended_action: String,
    pub requires_operator: bool,
    pub frame: MemorySpacetimeFrame,
}
```

## Graph-Intel Representation

First slice should use existing graph-intelligence nodes instead of blocking on
schema expansion:

- true-up finding -> `NodeKind::Decision` or `NodeKind::Task` with
  `properties.kind = "memory_true_up_finding"`
- promotion candidate -> `NodeKind::Task` with
  `properties.kind = "memory_promotion_candidate"`
- cultivation report -> `NodeKind::Decision` with
  `properties.kind = "memory_cultivation_report"`
- related memory ids -> node properties and `references` edges where possible
- affected proposal/seam/session/test nodes -> normal graph edges

Once proven, add explicit kinds:

- `MemoryFinding`
- `MemoryPromotionCandidate`
- `MemoryCultivationReport`
- `MemorySpacetimeFrame`

The first version should be boring. Boring schemas deploy.

## Philote Tool Surface

Add one bounded internal tool family:

```text
memory.cultivate
memory.true_up
memory.promote_candidate
```

`memory.cultivate` modes:

- `closeout`
- `contradiction_check`
- `cluster_cleanup`
- `staleness_review`
- `promotion_review`
- `identity_reflection`

`memory.true_up` modes:

- `topic`
- `entity`
- `proposal`
- `seam`
- `hotel`
- `workspace`
- `session`

Tool projection rule:

- conversational turns may receive recall only
- closeout/reflection turns may receive cultivate/true-up
- promotion tools require explicit intent or operator confirmation
- graph/docs/code writes remain separate tools with their own approval policy

## Cognitive Envelope Projection

Philote should project memory like this:

```text
[Recalled memory]
scope: user + workspace
temporal_kind: state
observed_at: 2026-05-20T09:54:00-04:00
last_verified_at: 2026-05-20T09:54:00-04:00
space: philotic-stack / develop / mbp-jane
authority: observed_runtime
validation: watched-live-green
content: Running Philote binary included scoped memory but not entity overlay.
```

AgentGraph overlays should remain clearly advisory when they come from Muninn:

```text
[Muninn entity overlay]
Advisory continuity hints from recalled memories. Current graph/code truth wins
on conflict.
```

## Implementation Plan

### Slice 1: Frame and Projection

- add `MemorySpacetimeFrame` and `MemoryShapingContext`
- derive the frame from `SessionState`, hotel/user context, workspace, branch,
  and validation state where available
- project frame metadata in recalled memory sections
- thread primary user identity into recall/write shaping
- tests:
  - frame carries session/agent/user identifiers
  - recalled memory projection includes temporal and authority metadata
  - missing frame data is omitted cleanly

### Slice 2: Shaped Muninn Writes

- update `memory.remember` dispatch to attach frame metadata
- attach graph anchors as Muninn entities/relationships
- add `promotion_candidate` metadata where policy allows
- tests:
  - writes include graph anchors
  - event/state memories require timestamps
  - scope is respected across `self`, `shared_user`, and `session`

### Slice 3: Cultivation Pass

- add internal `memory.cultivate` routine
- implement low-risk actions first: link, trust, evolve stale state memories,
  create promotion candidates
- produce a cultivation report
- tests:
  - contradictions are surfaced, not silently rewritten
  - stale state memory evolves with a validity window
  - promotion candidate does not mutate graph truth

### Slice 4: Graph True-Up Bridge

- add graph-intelligence helper APIs for memory true-up findings using existing
  nodes/mutations
- connect Philote true-up reports to graph records
- expose graph MCP/REST query for true-up findings by seam/proposal/entity
- tests:
  - true-up finding links to proposal/seam/session nodes
  - graph mutation audit captures the reconciliation
  - writeback renders human-readable proposal/task state

### Slice 5: Promotion Gates

- implement gated promotion from Muninn pattern to AgentGraph/doc/rule update
- require evidence or operator confirmation for high-agency promotions
- record promotion decisions in both graph and Muninn
- tests:
  - validated runtime fact can promote
  - inferred memory alone cannot promote
  - operator-approved promotion records rationale and evidence

### Slice 6: Deployment and Watch

- build and deploy Philote plus graph-intelligence changes to `mbp-jane` and
  `vps-jane`
- run watched live true-up on:
  - local source vs installed Philote binary
  - `mbp-jane` vs `vps-jane` Muninn status
  - a deliberately stale memory fixture
- record validation as graph test/smoke/watched-live evidence

## Deployment Gates

Before claiming this memory plane live:

- source tests pass for `philote` and `graph-intelligence`
- graph scanner indexes this proposal and seams
- `memory.cultivate` creates only low-risk changes by default
- promotion candidates require explicit gates
- installed Philote binary path changed on target hotel
- running Philote process uses the updated binary
- observed envelope includes spacetime and advisory overlay labels
- a stale memory can be evolved without deleting historical truth

## Open Questions

- Should `MemorySpacetimeFrame` live in `philotic-primitives-agent` first, or
  remain local to Philote until the shape stabilizes?
- Should AgentGraph add explicit memory node kinds, or keep memory artifacts as
  typed properties on `Decision`/`Task` nodes?
- What decay policy is appropriate for hotel-scoped runtime state memories?
- How much cultivation should run synchronously at closeout versus in a
  scheduled background process?
- Should user preference promotion require explicit operator confirmation every
  time, or only when it changes agent-visible behavior?

## Non-Goals

- no transcript archive in Muninn
- no automatic graph truth mutation from inferred memories
- no deletion-first cleanup policy
- no autonomous rewrite of repo docs from memory alone
- no single global memory scope for hotel-specific runtime observations

## First Work Item

Implement Slice 1 and Slice 2 together only if the data path stays small.
Otherwise, land Slice 1 first:

1. frame type
2. cognitive envelope projection
3. shaped recall/write metadata plumbing
4. tests

That gives Philote the basic "when and where was this true?" sense before we
ask it to cultivate anything more ambitious.
