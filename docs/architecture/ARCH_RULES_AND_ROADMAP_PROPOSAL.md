---
title: Architectural Rules Registry and Roadmap
doc_type: proposal
domain: workflow-docs
status: proposed
last_updated: 2026-03-31
tags:
- arch-rules
- roadmap
- enforcement
- process
related_docs:
- ARCHITECTURE_STATUS.md
- AGENTS.md
- SEAM_REGISTRY.md
task_refs:
- docs/task.md
proposal_id: arch-rules-and-roadmap
---

# Architectural Rules Registry and Roadmap

## Goal

Close two gaps in the current proposal lifecycle:

1. **Rule disappearance**: When a proposal reaches `implemented`, the architectural rules it established have no durable enforcement surface. Nothing in the workflow requires future work to check against them. Over time, implemented constraints become oral tradition.

2. **No cross-proposal sequencing view**: There is no way to see "what are the next seams in dependency order across all active architectural directions" without reading every active proposal individually and reconstructing the ordering mentally.

This proposal defines two new artifacts — `ARCH_RULES.md` and `ROADMAP.md` — and the process that connects proposals to them.

---

## Disposition

Proposed. No implementation started.

This proposal does not conflict with the existing seam/proposal lifecycle. It extends it by adding two persistent registries that proposals write to on state transitions.

---

## Core Recommendation

Two artifacts:

- `docs/architecture/ARCH_RULES.md` — living registry of standing architectural constraints
- `docs/architecture/ROADMAP.md` — dependency-ordered view of active seams across proposals

One process: when a proposal changes disposition, it updates these registries.

---

## Artifact 1: ARCH_RULES.md

### What It Is

A structured registry of active architectural constraints. Not a narrative. Not a proposal. A list that says: "these decisions are in force — if you violate them, that is a defect."

### Location

`docs/architecture/ARCH_RULES.md`

This lives in the architecture directory but is not a proposal. It has no frontmatter. It is a registry.

### Entry Schema

Each rule entry has six fields:

| Field | Description |
|---|---|
| `rule_id` | Short kebab-case slug, never recycled |
| `domain` | From the controlled domain vocabulary in AGENTS.md §4.1 |
| `source` | The proposal this rule came from |
| `rule` | One sentence — the concrete constraint |
| `level` | `hard` = architectural invariant; violation is a defect. `guidance` = preferred direction, deviation needs justification |
| `applies_to` | Where the rule matters — scope hint for the reader |

### What Belongs Here

Rules from `accepted` or `implemented` proposals that establish a lasting constraint on how the system is built. Not design goals, not implementation details, not rationale — just the constraint itself.

Rough test: if future code silently violated this sentence, would that be a defect? If yes, it belongs here.

### What Does Not Belong Here

- Design rationale (stays in the proposal)
- Open questions (stays in the proposal)
- Implementation details (stay in code/crate READMEs)
- Transitional architecture (by definition, not a rule yet)

---

## Artifact 2: ROADMAP.md

### What It Is

A dependency-ordered view of implementation seams across all active proposals. Not a task list. Not a schedule. A directed graph flattened into a table that answers: "if I want to build X, what must land first?"

### Location

`docs/architecture/ROADMAP.md`

No frontmatter. It is a registry.

### Entry Schema

| Field | Description |
|---|---|
| `seam_id` | Matches SEAM_REGISTRY entry |
| `source_proposal` | Primary parent proposal |
| `depends_on` | Seam IDs that must be complete before this one starts |
| `status` | `not-started` / `in-progress` / `complete` |
| `summary` | One line — what this seam delivers |

### What the Roadmap Is Not

- It is not a Gantt chart or timeline
- It is not a replacement for `docs/task.md` (tasks live there)
- It is not a commitment to sequencing in `docs/task.md` — the roadmap records architectural dependency, not sprint order
- It does not include every seam — only those where dependency ordering matters across proposal boundaries

---

## The Extraction Process

### On `accepted` disposition

When a proposal is marked `accepted`:

1. The author extracts rules into `ARCH_RULES.md` — add new rows, do not touch existing rows
2. The author adds seams to `ROADMAP.md` with their `depends_on` links filled in
3. Run `$architecture-docs-maintainer` to verify cross-links

### On `implemented` disposition

When a proposal is marked `implemented`:

1. Rules **stay** in `ARCH_RULES.md` (the decision is now enforced by the implementation — keeping the rule explicit prevents regression)
2. Seams are marked `complete` in `ROADMAP.md`
3. The proposal itself is marked `superseded` only when a newer proposal explicitly replaces its core direction

### On `superseded` disposition

When a proposal is superseded:

1. Rules derived from the superseded proposal get a note pointing to the replacement rule ID
2. Old rule rows are marked `superseded-by: <rule_id>` rather than deleted — deletion loses the audit trail
3. Roadmap seams that are now cancelled or replaced update their status and note the replacement

---

## Checking Surface

`ARCH_RULES.md` is a required re-read surface in three workflows:

1. **`check-engine` end-of-session sweep** — include "did any work this session touch a `hard` arch rule boundary?" as a required check item
2. **`philotic-slice-closeout`** — include "check this slice against active `hard` arch rules" before marking done
3. **`AGENTS.md` §8.1 SVE Refresh** — add `ARCH_RULES.md` to the required re-read list alongside `ARCHITECTURE_STATUS.md`

The roadmap is a required read surface in one workflow:

1. **Before opening a new implementation slice** — check `ROADMAP.md` to verify claimed `depends_on` seams are complete before starting

---

## Seed Content

Initial content extracted from two proposals:

**From `AGENT_RESOURCE_MODEL_PROPOSAL.md`** (6 seams: `agent-resource-broker`, `demand-derived-materialization`, `agent-graph-toolrunner`, `agent-graph-mesh-sync`, `router-training-tap`, `functions-gemma-onnx`):

- Rule: Resource requests from agents must flow through the hotel resource broker, not be self-granted.
- Rule: Leases live at the resource instance level, not the agent level.
- Rule: Router-observable messages must carry `agent_id`, `session_id`, and `active_role` for training reconstruction.
- Rule: Agents may not write to the Hotel CG directly; only hotel processes may write hotel-authority state.
- Rule: When Hotel CG and agent graph disagree on a grant, the Hotel CG wins.

**From `GRAPH_LAYER_UNIFICATION_PROPOSAL.md`** (3 seams: `graph-domain-layer`, `graph-adapter-migration`, `graph-store-instances`):

- Rule: All domain graph operations must go through `GraphDomain`, not directly against the storage backend.
- Rule: The storage backend (`GraphStorage` impl) is a deployment-time choice, not a caller concern.

**From existing implemented foundations:**

- Rule: The hotel context graph is the canonical durable owner for session state; apartments are derived recovery projections.
- Rule: Telegram poll lease authority is anchored to the agent's home hotel, not the currently routed role.
- Rule: Poll leases live at the resource instance level; one poller per lease regardless of tenant count.

See `ARCH_RULES.md` for the canonical table. See `ROADMAP.md` for the sequenced seam view.

---

## Open Questions

1. **Rule ID stability**: Should rule IDs be scoped to proposals (e.g., `arm-001`) or flat (e.g., `hotel-cg-canonical-session-authority`)? Flat slugs are more readable at a glance; proposal-scoped IDs are easier to batch-update when a proposal is superseded.

2. **Rule enforcement in CI**: Is there a future where `ARCH_RULES.md` is machine-readable enough that a linting pass could flag violations automatically? This is a future capability — the registry design should not block it, but should not require it now.

3. **Roadmap update discipline**: Who owns `ROADMAP.md` updates when a seam completes mid-slice? Current answer: the slice author, as part of `philotic-slice-closeout`. Should there be an explicit close-out checklist item?

4. **Scope of `hard` vs. `guidance`**: The distinction between `hard` and `guidance` is subjective at the boundary. If a `guidance` rule is violated twice in a row without comment, it may indicate it should be elevated to `hard`. Is there a review cycle for this?

---

## Related Entry Points

- [ARCH_RULES.md](ARCH_RULES.md) — the registry this proposal defines
- [ROADMAP.md](ROADMAP.md) — the sequenced seam view this proposal defines
- [ARCHITECTURE_STATUS.md](ARCHITECTURE_STATUS.md) — current implemented truth
- [SEAM_REGISTRY.md](SEAM_REGISTRY.md) — canonical seam IDs
- [AGENTS.md](../../AGENTS.md) — standing protocol; §8.1 SVE Refresh and §4 Proposal Lifecycle
- [docs/task.md](../../docs/task.md) — active execution surface
