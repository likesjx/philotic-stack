---
title: "Role Activation And Subagent Contracts Proposal"
doc_type: proposal
domain: runtime-sessions
status: accepted-current-slice
last_updated: 2026-03-13
tags:
  - roles
  - subagents
  - contracts
  - handoff
  - active-seam
related_docs:
  - ARCHITECTURE_STATUS.md
  - AGENT_INCARNATION_PROPOSAL.md
  - GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md
  - ROLE_CONTEXT_SHIFT_AND_DELEGATED_SUBAGENTS_WHITEPAPER.md
  - PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: role-activation-subagent-contracts
implements: []
implemented_by:
  - ../../crates/agent-core/src/session.rs
  - ../../crates/ansible/src/service/ipc.rs
active_seams:
  - role-activation-contract
  - same-identity-handoff-packet
  - delegated-subagent-contract
  - role-materialization-rubric
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# Role Activation And Subagent Contracts Proposal

## Goal

Turn the role-context-shift whitepaper into explicit seams that can drive implementation without quietly reintroducing the old concurrent-role ontology through convenience and habit.

## Core Recommendation

Break the design into four contracts, in this order:

1. **role activation contract**
2. **same-identity handoff packet**
3. **delegated subagent contract**
4. **role materialization rubric**

The first three should be implemented before Philotic broadens concurrent role materialization semantics. Otherwise the runtime will likely choose whatever happened to be easiest for the current process tree and call it architecture later, which is a beloved industry tradition for a reason.

## Disposition

`accepted for current slice`

## Current Slice

The first compatibility-first `RoleActivation` substrate now exists:

- hotel session snapshots now include a transitional `role_activation` object derived from the active role incarnation record
- `agent-core` hydrates that object into `SessionState`
- context projection now carries `role_activation`
- prompt/session projection now renders role addendum, toolset profile, and effective skillset posture from the typed activation object
- manual same-identity role handoff now carries a richer compatibility-first packet through the existing `HandoffBundle` wire contract
- the shared IPC surface now includes a first compatibility-first `SubagentDelegation` contract plus `SpawnSubagent` request shape
- `agent-core` can now build a lightweight default delegation packet from live session state
- hotel IPC now returns an explicit structured `SUBAGENT_NOT_IMPLEMENTED` response for `SpawnSubagent` instead of pretending the boundary exists only spiritually

This slice is intentionally narrower than the full proposal:

- `RoleActivation` currently carries only the fields available from the active session snapshot and role record
- the first same-identity handoff packet now exists, but only through the transitional `HandoffBundle` wire shape
- the delegated subagent contract now exists as a compatibility-first shared wire/runtime contract, but not yet as an executing worker lifecycle
- materialize-vs-shift behavior is still policy/documentation truth rather than enforced runtime selection

## Seam 1: Role Activation Contract

### Purpose

Define what it means to activate a role of the same self without smearing process mechanics into identity semantics.

### Contract Questions

- what role-scoped state becomes active?
- what base-self state remains shared?
- what working-memory state is preserved per role?
- what context layers are reprojected on activation?
- what toolsets and skillsets become effective?

### Recommended Shape

`RoleActivation`

- `agent_id`
- `role_name`
- `session_id`
- `activation_reason`
- `requested_by`
- `base_identity_ref`
- `role_addendum_ref`
- `toolset_profile_ref`
- `skillset_profile_ref`
- `working_memory_policy`
- `memory_projection_policy`

### Owner

- hotel/runtime decides activation
- context engine assembles the active projection
- role records remain durable graph truth

### First Implementation Target

Make same-identity role activation possible without requiring distinct role processes to be the default mental model.

## Seam 2: Same-Identity Handoff Packet

### Purpose

Define the governed context bundle for moving from one role posture of the same self to another.

### Important Rule

This packet is not the same as peer delegation.

Same-identity handoff can assume:

- shared durable memory ownership
- shared base identity
- shared operator relationship continuity

It should therefore be smaller and more posture-oriented than a peer delegation bundle.

### Recommended Shape

`SameIdentityHandoffPacket`

- `from_role`
- `to_role`
- `handoff_reason`
- `active_goal`
- `active_constraints`
- `relevant_session_facts`
- `working_summary`
- `suggested_memory_refs`
- `expected_return_mode`
- `cleanup_actions`

### Return Modes

- `return_when_complete`
- `return_on_block`
- `stay_active_until_manual_return`

### First Implementation Target

Define the packet shape and use it in the generic `handoff.to_role` workflow contract before adding more workflow subclasses.

Current slice note:

- the existing `HandoffBundle` wire contract now carries a compatibility-first version of this packet
- richer field ownership and workflow-level assembly are still pending

## Seam 3: Delegated Subagent Contract

### Purpose

Define bounded delegated labor explicitly so subagents do not inherit role semantics by accident.

### Important Rule

Subagents are workers, not postures of the same self.

That means they should receive a bounded mission packet rather than a full identity projection.

### Recommended Shape

`SubagentDelegation`

- `parent_agent_id`
- `parent_role`
- `subagent_kind`
- `goal`
- `context_packet`
- `allowed_tools`
- `allowed_skills`
- `memory_allowance`
- `writeback_allowance`
- `iteration_budget`
- `ttl_seconds`
- `completion_contract`

### Completion Contract

- `summary_required`
- `artifact_refs`
- `failure_summary`
- `requires_parent_ack`

### Default Policy

- no membrane ownership
- no durable memory writes unless explicitly granted
- no nested spawning unless policy allows it
- tool and skill access should be explicit or delegation-policy-derived

### First Implementation Target

Land `SpawnSubagent` with a lightweight one-shot runtime contract and explicit policy surface before teaching subagents more personality than they need.

## Seam 4: Role Materialization Rubric

### Purpose

Prevent process materialization from becoming the hidden default meaning of role activation.

### Decision Rule

The runtime should answer:

1. Is this a posture shift?
2. Is concurrent labor required?
3. Is isolation required?
4. Is distinct placement required?
5. Is long-lived background work required?

If only `1` is true:

- shift context in-place

If `2` through `5` are true:

- wake or materialize a separate role process
- or spawn a subagent, depending on whether this is same-self posture work or bounded delegated labor

### First Implementation Target

Write the rubric into hotel/runtime policy before adding more materialization heuristics.

## Prioritized Implementation Order

1. define the canonical shared-self role activation contract
2. define the same-identity handoff packet
3. define the delegated subagent contract
4. thread role addendum/toolset/skillset into active context projection
5. only then formalize the materialize-vs-shift runtime rule

## Relationship To Existing Proposals

- [ROLE_CONTEXT_SHIFT_AND_DELEGATED_SUBAGENTS_WHITEPAPER.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROLE_CONTEXT_SHIFT_AND_DELEGATED_SUBAGENTS_WHITEPAPER.md)
  - states the higher-level design shift
- [AGENT_INCARNATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_INCARNATION_PROPOSAL.md)
  - carries the existing runtime substrate and should remain honest about current implementation
- [GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md)
  - owns the governed workflow layer that should carry the same-identity handoff contract
- [PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md)
  - will eventually need explicit role activation and role-scoped projection support

## Active Work Surface

See [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md) under:

- `Agent Incarnation Model`
- `Context And Memory Engines`
