---
title: Governed Workflow Skills Proposal
doc_type: proposal
domain: runtime-sessions
status: accepted-current-slice
last_updated: 2026-03-31
tags:
- workflows
- roles
- delegation
- governance
- active-seam
related_docs:
- AGENT_INCARNATION_PROPOSAL.md
- ROLE_ACTIVATION_AND_SUBAGENT_CONTRACTS_PROPOSAL.md
- ROLE_CONTEXT_SHIFT_AND_DELEGATED_SUBAGENTS_WHITEPAPER.md
- RUNTIME_AUTHORITY_LEASES_PROPOSAL.md
- MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md
- CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
- ARCHITECTURE_STATUS.md
- SKILL_LIFECYCLE_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: governed-workflow-skills
active_seams:
- governed-workflow-skills
- peer-delegation-workflows
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Governed Workflow Skills Proposal

## Goal

Define a governed workflow layer for high-consequence cognitive operations such as role handoff, peer delegation, and external cognitive peer handoff.

The point is not to make every skill ceremonial. The point is to give Philotic a first-class contract for workflows that:

- move custody of work
- cross an identity or trust boundary
- require structured context packaging
- need operator visibility, lifecycle, or approval hooks

## Core Recommendation

Introduce a workflow-specific layer above plain `AbstractSkillRecord`.

Use that layer for workflows that decide:

- whether a handoff or delegation is warranted
- what the correct target class is
- what context is required
- what return/ack/completion contract applies
- what governance or approval gates must be satisfied

Recommended shape:

- `AbstractSkillRecord`
  - prompt-facing skill description
  - optional implied tool grants
- `WorkflowSkillRecord`
  - governed workflow metadata
  - target boundary class
  - invocation rules
  - context packaging rules
  - return contract
  - lifecycle/governance metadata

The first concrete implementation should be one generic orchestrator-owned workflow:

- `handoff.to_role`

It should stay generic and role-native by default. Do not jump straight to one bespoke workflow artifact per role unless later evidence shows the generic workflow cannot hold.

## Disposition

`accepted — current slice`

## Current Slice

The graph now has the first `AbstractSkillRecord` substrate plus seeded entries for:

- `handoff.to_role`
- `handoff.back`
- `role.governance`

The graph now also has its first real `WorkflowSkillRecord` seeds for:

- `handoff.to_role`
- `role.create_or_update`

For the role seam, the hotel no longer keeps that catalog truth only as hand-maintained Rust literals. The `role.authoring` abstract skill and `role.create_or_update` workflow now compile from repo-local markdown frontmatter embedded into the binary, which keeps installed hotels self-contained without making runtime seeding depend on a live source checkout.

`philote` now also prefers the workflow-shaped prompt surface directly: when both are available, `role.create_or_update` is projected to the model and the low-level `role.configure` tool is suppressed as compatibility residue. The role seam now also has a distinct hotel-side workflow execution plane via `ExecuteWorkflow { workflow_name: "role.create_or_update" }`, though that workflow still resolves internally through the existing role mutation machinery rather than a fully generic workflow executor.

The lifecycle and validation model for delegation skills is now fully defined in [SKILL_LIFECYCLE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SKILL_LIFECYCLE_PROPOSAL.md). That document owns: the `draft → validated → registered → active → deprecated / invalid / suspended` state machine, the three validation layers, `HookKind`, `IdleBehavior`, `SubagentLeaseTerms`, the updated required `SubagentDelegation` fields, new IPC verbs and responses, the field sourcing map, and the `skill-creator` meta-skill and tool contracts.

`WorkflowSkillRecord` now gains:

- `validation_state: SkillValidationState` — current position in the lifecycle state machine
- `field_sources: HashMap<String, String>` — sourcing map snapshot recording which source type resolved each field at registration time
- `source_snapshot: SkillSourceSnapshot` — frozen `mesh_catalog_version`, `hotel_policy_version`, `registered_at`, and `registered_by` captured at the moment of registration

What is still missing:

- a generic runtime invocation plane for `WorkflowSkillRecord` beyond the current role seam
- role metadata inputs for target selection in `handoff.to_role`
- explicit peer-delegation and external-cognitive-peer variants
- invocation/runtime enforcement beyond current handoff IPC
- Layer 2 mesh-capability validation wired in hotel
- `skill-creator` meta-skill materialization and authorization gate enforcement

Important design shift:

- same-identity role handoff is now best understood as governed context shift first
- separate concurrent role processes remain allowed, but are not the default semantic assumption
- delegated subagents, not peer roles, should usually carry bounded parallel labor

## Why This Needs Its Own Proposal

This concern is no longer just about role incarnation.

Philotic now needs one reusable framework for at least three workflow classes:

1. `same_identity_role_handoff`
2. `peer_agent_delegation`
3. `external_cognitive_peer_handoff`

They look similar procedurally, but they are not the same semantically.

If this stays buried inside the role-incarnation proposal, the repo will eventually pretend these are all “just handoff” with different strings. That would be elegant in the same way a mislabeled breaker panel is elegant.

## Workflow Classes

### 1. Same-Agent Role Handoff

Example:

- orchestrator → developer
- developer → orchestrator

Properties:

- same canonical identity
- same memory ownership
- same home-hotel/runtime authority
- different role posture and turn-loop policy
- usually a context-shift or posture-activation workflow, not a full identity transfer
- may materialize or wake a separate role process when isolation or background execution requires it

This is the first implementation target.

### 2. Peer Agent Delegation

Example:

- Aria delegates a bounded task to Jane
- one Philotic agent delegates to a different Philotic agent on the mesh

Properties:

- crosses identity boundary
- continuity is partial, not shared
- requires bounded context package
- return contract is task/delegation shaped, not true same-self handoff

This should eventually share the workflow framework, but not the same continuity semantics.

### 3. External Cognitive Peer Handoff

Example:

- hand off to Claude Code
- hand off to Codex
- hand off back from one of those runtimes into Philotic

Properties:

- crosses both identity and runtime boundary
- often crosses tool/secret/workspace trust boundaries too
- requires explicit packaging, bounds, and return channel
- should be treated as governed cognitive delegation, not as a casual “just another role”

This is close enough in process to belong in the same framework, and different enough in trust to require its own target class.

## What A Workflow Skill Governs

A governed workflow skill should answer:

- **Trigger**: when should this workflow be considered?
- **Target selection**: what role/peer/peer-runtime should receive the work?
- **Context package**: what information is required, and what must not be included?
- **Return contract**: what ack, summary, or completion is expected?
- **Cleanup**: what state updates happen before yielding control?
- **Governance**: who may invoke, publish, revise, or activate this workflow?
- **Visibility**: what should operators or the hotel be able to inspect?

## Proposed Record Shape

This is intentionally a recommendation, not current implementation:

```rust
WorkflowSkillRecord {
    workflow_name: String,
    workflow_kind: String, // handoff.to_role, delegate.to_peer, handoff.to_external_cognitive_peer
    owner_scope: String,   // orchestrator, admin, agent-home, etc.
    target_class: String,  // same_identity_role, peer_agent, external_cognitive_peer
    description: String,
    target_selection_policy: serde_json::Value,
    context_requirements: serde_json::Value,
    return_contract: serde_json::Value,
    governance: serde_json::Value,
    rollout_state: String, // draft, accepted, active, paused, deprecated
}
```

The exact field split can evolve, but the framework should carry more than prompt text.

## OpenClaw Lessons To Reuse Carefully

The OpenClaw `aiua` plugin had a useful `delegate` / `execute` split and a skill-pair manifest model.

Useful ideas to carry forward:

- delegation analysis and executor posture are different concerns
- lifecycle/governance metadata should be explicit
- rollout and evidence should be inspectable
- activation should not be a pure prompt convention

What not to copy literally:

- per-capability skill-pair manifests as the default unit
- plugin-era compensating structure for a weaker runtime foundation

Philotic already has:

- hotel authority
- context graph
- materialization
- leases
- routing
- handoff IPC

So the first default should be:

- one generic orchestrator-owned `handoff.to_role` workflow
- informed by role metadata
- not one bespoke manifest per role

## Boundary Contract

Keep these boundaries explicit:

- **workflow skill**
  - decides and packages
- **hotel/runtime**
  - materializes, routes, enforces authority, records state
- **target role or peer**
  - executes under its own posture
- **lease/materialization/control plane**
  - remain separate from workflow description

Workflow skills should not become a stealth control plane. They describe and govern invocation; they do not replace hotel authority.

For same-identity role handoff, they also should not quietly become a stealth process-materialization doctrine. The workflow decides and packages; the runtime chooses whether the target role is activated in-place, woken, or materialized.

## First Recommended Implementation Order

1. Keep `AbstractSkillRecord` as the current prompt-facing substrate.
2. Define the first generic `handoff.to_role` workflow contract in terms of:
   - trigger shape
   - target selection inputs
   - context bundle shape
   - return conditions
   - cleanup steps
3. Add role metadata needed by that workflow.
4. Decide whether `WorkflowSkillRecord` should be introduced immediately after that, or only once peer/external workflows need the richer lifecycle fields.
5. Then add:
   - `delegate.to_peer`
   - `handoff.to_external_cognitive_peer`

## Active Work Surface

See [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md):

- `Handoff Skill + Membrane Switching`
- `Agent Incarnation Model`
- future peer/external delegation follow-ons
