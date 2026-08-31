---
title: Data-Driven Tool Grants (SkillDAG)
doc_type: proposal
domain: tooling-execution
status: accepted-current-slice
last_updated: 2026-08-27
tags:
- tooling
- grants
- skilldag
- autopoiesis
related_docs:
- ARCHITECTURE_STATUS.md
- LIFE_GRAPH_OS_PROPOSAL.md
- SINGULAR_MESH_MEMBERSHIP_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: data-driven-tool-grants-skilldag
implemented_by:
- skill-admin-plane
- typed-skill-patch-compiler
active_seams:
- mesh-canonical-catalog-sync
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- docs/task.md
---

# proposal:data-driven-tool-grants-skilldag — PROPOSED (spec stage)

Filed 2026-07-14 (handoff PR #282, architecture item). Operator-flagged
principle from that session: **"we should not have any tool hard coded."**
Motivating case: the inability that night to disable one tool
(`life.observe.batch`) without a code change and deploy.

Note: the handoff says "full pros/cons in the proposal node", but no
intel-graph node was ever created — the handoff paragraph
(HANDOFF-2026-07-14-lifegraph-batch.md, "Architecture item") is the
canonical source.

## Problem

Tool grants are compiled into the binaries across five surfaces:

1. `skill_implied_tools` in `catalog.rs`
2. `tools_for_allowed_class` in `ipc.rs`
3. seeded `implied_tools` in `main.rs`
4. runner `supported_tools` in `main.rs`
5. `tool_catalog()`

So any enable/disable/grant/re-route needs a deploy.

## Goal

Grants become graph/config data — seeded once, editable at runtime with
**no deploy**. The hardcoded lists demote to a first-boot seed + fallback.

## SkillDAG decision

Keep the **authoritative, hot-path grants in the LOCAL hotel context
graph** (fast, always-available, per-hotel). Do NOT put runtime tool
resolution behind the remote LifeGraph (Memgraph on vps-jane), or every
agent bricks when it is down — that session's failure mode. The LifeGraph
is only an **optional reasoning/design layer**: the agent proposes changes
against it, and those changes **compile down** to the local toolset
(compiler pattern; autopoiesis fit).

## Slices

1. **Grant registry in the context graph** — verify by disabling
   `life.observe.batch` at runtime with no deploy.
2. **Runner routing as data** — the PR #277 reconcile is a precedent.
3. **Governance/audit.**
4. **Later** — SkillDAG reflection in the LifeGraph.

## Slice: orchestrator skill administration plane — IMPLEMENTED 2026-08-13

Disposition: implemented, `watched-live-green` on `mac-jane` 2026-08-13
(`codex/skill-admin-plane`, PR #430): 14/14 live drill checks against the
installed supervised runtime — unauth/wrong-role rejection, SkillDAG edge
persisted and listed, suspend/reinstate lifecycle, invalid-state rejection,
audit trail with actor identity, and boot-seed reconciliation. Found during
the drill: the Layer-1 validator flags dotted skill names
(`invalid_skill_name_chars`) while the house seeds themselves use dots —
pre-existing validator/catalog inconsistency, still open.

Before this slice the SkillDAG was a name without a structure: `AbstractSkillRecord`
had no skill→skill edge field, `skill.register` validated then **discarded**
`allowed_skills`/`allowed_classes`/`goal`/`subagent_kind` at the IPC boundary,
four lifecycle states (`Registered`/`Active`/`Suspended`/`Deprecated`) were
unreachable, assign/revoke were unaudited, and a `starts_with(agent_id)` check
let agent `aria2` administer agent `aria`.

What landed:

- **DAG edges persist**: `AbstractSkillRecord` gained `allowed_skills` (skill→skill
  dependency edges), `implied_classes`, `subagent_kind`, `goal_template`, and a
  populated `source_snapshot`; `skill.register` persists all of them.
- **Transitive resolution**: `resolve_transitive_skills` (ansible-mesh-core)
  expands the DAG closure with cycle/missing-edge diagnostics; session snapshot
  composition resolves the closure hotel-side before projecting tools.
- **Lifecycle is administrable**: new `skill.set_state` tool + `SetSkillState`
  IPC op (`active`/`suspended`/`deprecated`); suspended/deprecated skills stop
  contributing names, tools, and guidance to projection
  (`SkillValidationState::is_projectable`), reversibly.
- **One gate, audited**: `require_skill_admin` centralizes the
  orchestrator/management gate across register/assign/revoke/set_state/audit;
  every mutation writes a fail-closed `SkillRegistrationAuditRecord` (now with
  `action` + `detail`); new `skill.audit` tool + `ListSkillAudits` IPC op read
  the trail; `skill.list` now requires a registered identity; the agent-ownership
  check is exact-boundary (`guest_owns_agent`).

Still open (next seams): on-demand skill administration (`on_demand_skills` has
no IPC writer), data-driven turn relevance (`skill_is_relevant_for_turn` is still
compiled-in), profile-level ops (`profile.*`), harness/repo skill catalog
reconciliation, and richer patch-result/rollback reporting.

## Slice: typed SkillPatch compiler and scoped grants — IMPLEMENTED 2026-08-27

Disposition: implemented in `codex/skill-self-build`; test-green, pending fleet
rollout and Beacon conversational UAT.

This closes the missing compiler boundary without making LifeGraph the hot-path
catalog owner:

- a `SkillPatch` may carry typed `skill_definitions`; operator confirmation moves
  it to `approved_for_compilation` and returns the exact immutable definitions
  plus source `patch_id`
- `skill.register_batch` compiles 1–32 definitions into the local catalog under
  the existing unconditional human-approval and skill-admin gates, rejects
  duplicate names before any write, and stamps the patch id into each audit
- `skill.assign` and `skill.revoke` now mutate
  `RoleIncarnationRecord.assigned_skills`; the shared profile remains a baseline,
  so granting Beacon's orchestrator no longer silently grants every orchestrator
- runtime-registered skills with real source provenance converge between hotels
  over authenticated reliable execution transport with deterministic
  last-writer-wins conflict handling; compiled seeds remain release-owned and
  role assignments remain hotel-local authority

`life.patch.apply` still does not write the local catalog directly. It is the
review/confirmation boundary that releases an executable batch; the hotel-owned
skill administration plane remains the mutation boundary. That separation is
deliberate, not another politely worded dead end.

## Precedent

The PR #277 runner-routing reconcile is cited in the handoff as a
precedent for slice 2, and DEF-057's reconciling seeder (found in the
2026-07-19 role/tool deep-dive audit, fixed via codex/role-admin-hardening)
proved runtime DB changes can survive restarts.
