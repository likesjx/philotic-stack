---
title: Life Graph Attention Steward
doc_type: specification
domain: memory-context
status: proposed
last_updated: 2026-06-04
tags:
- life-graph
- attention-steward
- sil
- beacon
- paracrine
seam: life-graph-attention-steward
related_docs:
- ../LIFE_GRAPH_OS_PROPOSAL.md
- LIFE_GRAPH_SCHEMA.md
- ../DISTRIBUTED_CRON_PROPOSAL.md
source_of_truth_targets:
- SEAM_REGISTRY.md
- docs/task.md
---

# Life Graph Attention Steward

Specification for the Attention Steward: the behavioral policy layer that turns Life Graph state into humane re-entry and follow-through support without becoming a nagging machine.

## Summary

The Attention Steward is not a scheduler. It is a **paracrine subscriber role-type** that receives typed signals from the cron-backed heartbeat engine (seam: `life-graph-paracrine-heartbeat`) and decides what, if anything, to do about them.

Beacon is the primary Attention Steward for the operator's cross-domain Life Graph. Specialized roles (Coach, etc.) may subscribe to attention signals for their domain but do not own the canonical cross-domain posture.

**First-slice posture: observe-only.** The Attention Steward records observations and proposes SIL entries. It does not interrupt the operator, send notifications, or surface reminders until there is enough evidence to trust the timing and tone policy.

---

## StewardshipInstruction Node

`StewardshipInstruction` is a first-class Life Graph node (see V002 migration). It is the durable, queryable unit of the SIL.

### Properties

| Property | Type | Description |
|---|---|---|
| `id` | `string` | Unique ID |
| `situation` | `string` | Context pattern where this rule applies. Human-readable. |
| `trigger` | `string` | What makes the rule eligible to fire (signal type, graph state, cadence hint) |
| `recommended_action` | `string` | `surface` \| `defer` \| `ask` \| `summarize` \| `nudge` \| `suppress` \| `escalate` |
| `tone` | `string` | `direct` \| `gentle` \| `tiny-step` \| `reflective` \| `celebratory` \| `quiet` |
| `evidence_refs` | `list<string>` | Node IDs or signal IDs that grounded this rule |
| `reinforcement_count` | `int` | Times this rule fired and the outcome was positive |
| `friction_count` | `int` | Times this rule fired and the operator found it annoying, untimely, or wrong |
| `exceptions` | `list<string>` | Situation patterns where this rule must NOT fire |
| `owner` | `string` | Agent ID of the steward responsible (default: `agent:beacon`) |
| `status` | `string` | `proposed` \| `active` \| `dampened` \| `retired` \| `blocked` |
| `created_at` | `string` | ISO 8601 |
| `last_evaluated_at` | `string \| null` | ISO 8601, last time the rule was evaluated against a signal |
| `last_fired_at` | `string \| null` | ISO 8601, last time the rule resulted in an action |

Provenance envelope applies: `source_membrane`, `provenance`, `confidence`, `validation_state`, `observed_at`, `last_confirmed_at`.

### SIL Status Lifecycle

```
proposed  →  active  →  dampened  →  retired
                ↓                       ↑
              blocked  ────────────────→
```

| Status | Meaning |
|---|---|
| `proposed` | Created by agent or operator; not yet evaluated. Observe-only. |
| `active` | Evaluated and confirmed — either by reinforcement or explicit operator approval. May fire. |
| `dampened` | Friction count exceeds reinforcement by threshold; needs review before re-activation. |
| `retired` | Operator or steward has decided this rule is no longer needed. Soft delete. |
| `blocked` | Explicitly suppressed — must not fire under any circumstances until unblocked. |

---

## SIL Update Loop

```text
paracrine signal arrives
  → Attention Steward evaluates applicable StewardshipInstructions
  → records observation (Signal node in Life Graph)
  → if pattern is new: propose a StewardshipInstruction (status: proposed)
  → if existing rule matches: apply in observe-only mode, record outcome signal
  → if reinforcement threshold reached: propose status change to active (requires confirmation)
  → if friction threshold reached: auto-dampen, surface review request to Beacon
```

Thresholds (first slice defaults, tunable via AttentionPatch):
- **Reinforcement threshold**: 3 independent positive outcomes before proposing `active`
- **Friction threshold**: 2 negative outcomes relative to reinforcements triggers auto-dampen
- **Evidence window**: only count outcomes from the past 30 days

SIL updates by status:

| Change | Gate |
|---|---|
| `proposed` → `active` | Operator confirmation required |
| `active` → `dampened` | Automatic when friction threshold exceeded |
| `dampened` → `active` | Operator confirmation required |
| any → `retired` | Operator or Beacon with strong evidence |
| any → `blocked` | Operator only |

---

## Beacon Stewardship Contract

Beacon (`agent:beacon`) is the default `owner` for all cross-domain StewardshipInstructions.

### What Beacon owns as Attention Steward

- Cross-domain attention posture: noticing when goals, habits, commitments, and open loops are entangled
- Re-entry support: surfacing the right context when the operator returns to a domain after interruption
- Commitment follow-through: prompting when due dates approach or promised items go stale
- Conflict arbitration: when two SIL rules conflict (e.g. Coach wants to nudge, Beacon wants to suppress), Beacon's cross-domain rule takes precedence

### What Beacon does NOT own

- Domain-specific coaching content (owned by Coach role)
- Health data interpretation (owned by specialized health roles when they exist)
- Work-session detail (owned by the relevant project philote)

### Delegation pattern

```text
Beacon receives paracrine signal
  → evaluates cross-domain StewardshipInstructions
  → if domain-specific: emit sub-signal to domain steward role
  → domain steward responds with proposed observation
  → Beacon decides whether to surface, defer, or suppress
```

Domain stewards may not surface observations to the operator directly. They submit to Beacon and Beacon decides. This prevents multiple agents competing for operator attention simultaneously.

---

## Paracrine Subscriber Contract

The Attention Steward subscribes to signals where `target_role_type = "attention-steward"`.

### Inbound signal shape

The heartbeat engine (seam: `life-graph-paracrine-heartbeat`) delivers signals with this shape:

| Field | Type | Description |
|---|---|---|
| `signal_id` | `string` | Unique ID for this signal instance |
| `signal_type` | `string` | Typed category — see Signal Types below |
| `scope` | `string` | `personal` \| `project` \| `relationship` \| `health` \| `work` |
| `source_hotel` | `string` | Hotel that emitted the signal |
| `target_role_type` | `string` | Always `"attention-steward"` for Attention Steward signals |
| `subject_refs` | `list<string>` | Life Graph node IDs the signal concerns |
| `cadence` | `string` | How often this signal fires (`daily`, `weekly`, etc.) |
| `priority` | `string` | `low` \| `medium` \| `high` |
| `observed_at` | `string` | ISO 8601 timestamp |
| `expires_at` | `string \| null` | Signal expires and should not fire actions after this time |
| `payload_summary` | `string` | Human-readable summary of what triggered this signal |
| `policy_tags` | `list<string>` | Tags for SIL rule matching (e.g. `["adhd-support", "re-entry"]`) |

### Signal Types (first slice)

| Type | Fired when |
|---|---|
| `open_loop_staleness` | An OpenLoop node has not been updated in N days |
| `commitment_approaching` | A Commitment's `due_at` is within threshold window |
| `commitment_overdue` | A Commitment's `due_at` has passed without `status: fulfilled` |
| `goal_no_recent_action` | A Goal has no linked NextAction with `status: available` |
| `habit_gap` | A Habit's last confirmed instance is past its cadence |
| `re_entry_hint` | Session context suggests operator has returned to a domain after gap |
| `growth_experiment_due` | A GrowthExperiment's `ends_at` is approaching |
| `drift_finding_proposed` | A new DriftFinding has been proposed and needs review |

### Allowed response types (observe-only mode)

In the first slice, the Attention Steward **only** produces:

| Response | Description |
|---|---|
| `record_observation` | Create a `Signal` node in the Life Graph with provenance, confidence, and subject refs. No operator-visible output. |
| `propose_sil_entry` | Create a `StewardshipInstruction` with `status: proposed`. No operator-visible output. |
| `update_sil_metadata` | Increment `reinforcement_count` or `friction_count` on an existing SIL entry. No operator-visible output. |
| `defer_signal` | Log that the signal was received but conditions not met. No node created. |

**Not allowed in first slice:**
- Sending a Telegram message
- Injecting context into an ongoing agent turn
- Creating a scheduled follow-up turn
- Proposing a Commitment on behalf of the operator
- Modifying any Life Graph node other than `Signal` and `StewardshipInstruction`

Active interruptions (nudges, reminders, re-entry prompts) are unlocked only after:
1. At least 5 SIL entries have reached `active` status through the reinforcement loop
2. Operator has explicitly approved the first active SIL entry
3. The relevant `AttentionPatch` carries `risk_tier: confirm_first` or lower

---

## Anti-Policy

The Attention Steward must actively check for and avoid these patterns.

| Anti-pattern | Detection | Response |
|---|---|---|
| Nagging | Same `signal_type` + `subject_refs` fired actions more than once per 24h | Create DriftFinding, dampen SIL entry |
| Stale facts treated as current | `validation_state != confirmed` and `last_confirmed_at` older than 14 days | Downweight confidence, flag for Muninn cultivation |
| Productivity bias | `surface` actions outnumber `suppress` or `defer` 4:1 over 7-day window | Create DriftFinding `drift_type: productivity_bias` |
| Shame language | Tone inferred from payload references failure, falling behind, or broken streaks | Flag signal for Beacon review before any action |
| Overgeneralizing from bad days | Single negative event triggering 3+ `proposed` SIL entries | Batch into one DriftFinding, hold 24h before proposing |
| Agent convenience | SIL entries that reduce agent uncertainty but don't help operator | Beacon review required before any such entry reaches `active` |

---

## First Slice Deliverables

1. `StewardshipInstruction` node type in Memgraph (V002 migration) with uniqueness constraint, property indexes on `status`, `owner`, `trigger`
2. This spec as the SIL contract for all philotes that interact with Beacon
3. Relationship types: `GOVERNS` (Beacon → StewardshipInstruction), `EVALUATES` (StewardshipInstruction → any node), `OUTCOME_OF` (Signal → StewardshipInstruction)
4. Paracrine subscriber interface defined above — Codex's heartbeat seam can wire against `target_role_type = "attention-steward"` and the signal shape defined here

## Open Questions

- Should `StewardshipInstruction` nodes be in the Life Graph partition or a separate `sil_graph` partition?
- What is the right friction/reinforcement threshold ratio for ADHD-support contexts specifically?
- Should Beacon evaluate SIL rules inline on the philote turn loop, or as a separate background role invocation?
- How should the Attention Steward handle conflicting signals — e.g. a re-entry hint and an overdue commitment arriving simultaneously?
