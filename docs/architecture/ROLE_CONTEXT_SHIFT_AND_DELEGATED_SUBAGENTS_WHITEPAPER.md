---
title: Role Context Shift And Delegated Subagents Whitepaper
doc_type: proposal
domain: runtime-sessions
status: proposed
last_updated: 2026-03-31
tags:
- roles
- subagents
- context
- delegation
- active-seam
related_docs:
- ARCHITECTURE_STATUS.md
- AGENT_INCARNATION_PROPOSAL.md
- GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md
- ROLE_ACTIVATION_AND_SUBAGENT_CONTRACTS_PROPOSAL.md
- ROLE_POSTURE_AND_ADMIN_PROPOSAL.md
- PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md
- MEMORY_RELATION_LIFECYCLE_WHITEPAPER.md
task_refs:
- docs/task.md
proposal_id: role-context-shift-subagents
implements: []
implemented_by: []
active_seams:
- role-context-shift
- delegated-subagent-contract
- concurrent-role-materialization
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Role Context Shift And Delegated Subagents Whitepaper

## Goal

Capture the design shift for role handling before the implementation story grows a second head.

Philotic should treat same-identity roles as context shifts of one continuous self by default, not as permanently concurrent sibling minds. Parallel work should usually come from delegated subagents, not from materializing every role as its own always-running process.

This paper is intentionally provisional. It establishes the preferred runtime semantics and the decision rubric for when separate concurrent role processes are actually justified.

## Disposition

`proposed`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Executive Summary

The core claim is:

roles conserve focus; subagents provide labor.

That means:

- one base agent identity remains continuous
- durable memory remains shared across roles
- role addenda, toolsets, skillsets, and working memory are role-local
- same-agent handoff usually means context shift, not process fork
- delegated bounded work usually happens through subagents

This architecture exists to save attention and context tokens, not to create a tiny parliament of semi-duplicate selves arguing through IPC because the diagrams looked impressive.

## Core Recommendation

Philotic should adopt three distinct execution semantics:

1. **base agent self**
2. **role/incarnation posture**
3. **delegated subagent**

### Base Agent Self

The base self remains singular and continuous.

It owns:

- soul
- identity
- user relationship continuity
- durable memory ownership
- long-horizon commitments and values

### Role / Incarnation Posture

A role is an additive posture of the same self.

It varies:

- role addendum
- effective toolset
- effective skillset
- turn-loop posture
- working memory

It does **not** create:

- a new self
- a new durable memory store
- a new operator relationship
- a new canonical identity

Default runtime meaning:

- activate the role by shifting context and posture
- narrow the active projection to what that role needs
- keep shared self and durable memory intact

### Delegated Subagent

A subagent is not a role.

A subagent is a bounded delegated worker spawned by an active role.

Default properties:

- task-scoped mission
- constrained tools and skills
- bounded time/iteration budget
- little or no durable memory
- no automatic communication-plane ownership
- report-back contract to the parent role

## Why This Shift Matters

The original role model carried a useful instinct: roles help preserve focus and reduce context bloat.

That instinct is stronger when roles are treated as context filters rather than always-running sibling processes.

If every role becomes a simultaneous process by default, the system risks paying back its token savings in:

- duplicate initialization
- duplicate memory projection
- orchestration overhead
- identity drift risk
- confused ownership of who is actually speaking

Context-shift roles are the cleaner expression of the original goal:

- same self
- less active surface area
- clearer posture
- lower prompt burden

## Shared-Self Role Contract

Every role/incarnation should be interpreted through this contract:

- **shared across roles**
  - base identity
  - durable memory
  - operator relationship continuity
  - core values and commitments
- **role-local**
  - role addendum
  - working memory
  - effective toolset
  - effective skillset
  - loop posture and policy

This means role activation is closer to changing hats than spawning a sibling organism.

## Default Runtime Semantics

Philotic should prefer:

- one active conversational cognitive process
- role activation as context shift
- per-role working-memory separation
- delegated subagents for parallel labor

This is the default, not an absolute law.

Concurrent materialized role processes may still be useful later, but they should be justified by runtime pressure rather than assumed by ontology.

## Handoff As Context Shift

For same-identity role handoff, the workflow semantics stay valuable, but the meaning changes slightly.

The handoff skill still governs:

- whether a role transition is warranted
- what context bundle to carry
- what cleanup should happen before yielding
- what return conditions should apply

But the default result is:

- activate a different role posture of the same self
- swap the active context projection
- preserve shared self and durable memory continuity

So the handoff skill remains the right governed workflow surface. It just no longer implies that the target role must already be a separate persistent process.

## Subagents As Delegated Labor

The active role should generally spin off subagents when it needs:

- parallel work
- narrow bounded investigation
- isolated tool execution
- limited-risk exploration
- asynchronous completion

The delegation contract should determine:

- mission packet
- allowed context
- allowed tools
- allowed skills
- memory allowance
- write-back allowance
- completion and failure contract

This is strongly aligned with the existing handoff/delegation workflow intuition. The same governed skills can remain in place, but same-identity role handoff and delegated subagent spawning should not be conflated.

## When Separate Concurrent Role Processes Are Warranted

Separate role materialization should be considered an optimization or operational exception, not the default semantic model.

It is warranted when one or more of these are true:

- true background work must continue while another role remains conversationally active
- tool or environment isolation is materially different
- the role needs different placement or runtime resources
- the role is long-running enough that keeping it materialized is cheaper than repeated activation
- operator visibility or governance requires distinct live process state

In other words:

- if the need is posture, shift context
- if the need is concurrent labor or isolation, materialize or delegate

## Relationship To Memory And Context

This shift fits the newer memory/context direction cleanly.

Roles should share:

- durable autobiographical memory
- durable user relationship memory
- stable identity projection

Roles should differ in:

- what memory is projected now
- what working memory is active now
- what tool/skill affordances are visible now

This means the context engine should eventually support:

- base identity projection
- role addendum projection
- role-scoped affordance projection
- per-role working-memory projection
- shared durable memory retrieval

## Relationship To Utility Inference And Cognitive Routing

This role model also helps the `request_class` split.

The active role can decide whether work remains:

- `cognitive` in the main agent loop
- delegated to a subagent
- or handled by utility inference such as embeddings or classification

That keeps role posture tied to focus and judgment rather than making every role a permanent execution lane.

## First Recommended Proposal Changes

This whitepaper implies the following proposal adjustments:

1. same-identity role handoff should be described as context shift first
2. delegated subagents should be the default parallel-work mechanism
3. concurrent role materialization should remain allowed, but be framed as conditional
4. role addendum, toolset, skillset, and working memory should be treated as separate overlays on one shared self

The first implementation-facing breakdown for that work now lives in [ROLE_ACTIVATION_AND_SUBAGENT_CONTRACTS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROLE_ACTIVATION_AND_SUBAGENT_CONTRACTS_PROPOSAL.md).

## Open Questions

- How much per-role working memory should survive deactivation?
- What exact context packet should same-identity handoff carry when the target role is activated in-place rather than via a distinct process?
- When should a role be re-materialized instead of simply reactivated?
- Which delegation policies determine subagent tool and skill access?
- How should operator UX distinguish role activation from delegated subagent work?

## Active Work Surface

See:

- [AGENT_INCARNATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_INCARNATION_PROPOSAL.md)
- [GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md)
- [ROLE_POSTURE_AND_ADMIN_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROLE_POSTURE_AND_ADMIN_PROPOSAL.md)
- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
