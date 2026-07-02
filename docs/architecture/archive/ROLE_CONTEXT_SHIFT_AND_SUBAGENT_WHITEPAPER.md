---
title: Role Context Shift And Subagent Whitepaper
doc_type: proposal
domain: runtime-sessions
status: proposed
disposition: obsolete
last_updated: 2026-03-31
tags:
- roles
- incarnations
- subagents
- delegation
- context
- active-seam
related_docs:
- ARCHITECTURE_STATUS.md
- AGENT_INCARNATION_PROPOSAL.md
- GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md
- ROLE_POSTURE_AND_ADMIN_PROPOSAL.md
- PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md
- MODEL_CONTROLLER_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: role-context-shift-subagent-whitepaper
implements: []
implemented_by: []
active_seams:
- context-swapped-roles
- delegated-subagent-execution
- role-vs-subagent-boundary
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Role Context Shift And Subagent Whitepaper

## Goal

Capture the design shift from “multiple concurrent role processes by default” toward a cleaner Philotic model:

- one shared self
- one shared durable memory substrate
- role postures as context shifts
- subagents as the default unit of delegated concurrent work

This paper is provisional.

It is meant to lock the topology before narrower implementation proposals harden the details.

## Disposition

`proposed`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Executive Summary

The design pressure behind the role model was not primarily process multiplication.

It was focus.

Roles were introduced to save context and attention by narrowing what the active being carries into the current cognitive path:

- role addendum
- role-local working memory
- role-specific toolset
- role-specific skillset

That motivation points toward **context-swapped roles**, not toward “every role is a simultaneously running full process” as the default truth.

The cleaner architecture is:

- **roles/incarnations**
  - same being, same personality, same durable memory, different posture
- **subagents**
  - bounded spawned workers that perform delegated labor and report back

This separates:

- posture
from
- concurrency

That is the real design shift.

## Core Recommendation

Philotic should treat role incarnations as **shared-self context shifts by default**.

They should share:

- base identity
- base personality
- durable memory
- user relationship continuity

They should vary by:

- role addendum
- role-local working memory
- effective toolset
- effective skillset
- turn-loop posture

Subagents should be the default way to create delegated or parallel work.

Separate concurrently materialized role processes should remain possible, but only when there is real operational pressure for them.

## Why This Shift Matters

If roles are modeled as separate always-on processes too early, the system pays unnecessary cost in:

- duplicated initialization
- duplicated projection work
- duplicated memory loading
- coordination overhead
- identity drift risk

And it loses the original reason the role model existed:

- focus and token savings

If roles are modeled as context shifts instead, the system gets:

- narrower active context
- clearer posture boundaries
- cheaper switching
- one coherent self
- better separation between identity and labor distribution

## The Ontology

### Agent

The agent is the enduring self.

It owns:

- soul
- identity
- durable memory
- user relationship continuity
- long-horizon commitments and values

### Role / Incarnation

A role is a named posture of the same self.

It should not be treated as a second being.

A role contributes:

- a role addendum
- an effective toolset
- an effective skillset
- role-local working memory
- loop/routing posture

The role should primarily act as a **selective context filter**.

It answers:

- what subset of the self should be foregrounded right now?
- what tools and skills should be available?
- what working memory should be active?

### Subagent

A subagent is not a role peer.

It is a bounded delegated worker.

It should usually receive:

- a task packet
- selected context
- selected tools
- selected skills
- time/iteration budget
- explicit return contract

It should not automatically receive:

- full durable memory
- the entire role posture stack
- communication-plane ownership
- broad system authority

## Roles Are For Focus Compression

This is the key design principle.

Roles conserve attention.

They exist to make the active cognitive path smaller and sharper.

That means a role should primarily influence:

- what self-description is projected
- what memory is foregrounded
- what tools are legal
- what skills are surfaced
- what working memory is active

In other words:

roles are about **focus compression**, not automatic concurrent multiplicity.

## Subagents Are For Labor Distribution

Subagents conserve throughput.

They exist to do bounded work that does not require the full active cognitive posture to remain occupied.

That means subagents should be the default answer when the system needs:

- parallelism
- bounded delegated execution
- scoped tool access
- task-specific work packets

This gives Philotic a clean conceptual split:

- roles conserve focus
- subagents conserve throughput

## Default Execution Model

The default runtime model should be:

- one active role/incarnation on the cognitive path
- shared self and durable memory
- role-local working memory
- subagents for delegated labor

This implies that a same-agent handoff is usually a **context shift**, not a transfer between two fully independent enduring minds.

## When Concurrent Role Processes Still Make Sense

Separate simultaneous role materialization should still be allowed when it is genuinely needed.

Examples:

- a role must run long-lived background work while another role stays conversationally active
- a role needs stronger isolation because tools, models, or risk differ materially
- a role needs different placement or environment
- a role is effectively becoming a worker/service rather than a posture

So the decision rule is:

- if it is a **posture shift**, swap context
- if it is **concurrent labor or isolation**, materialize separately

The important point is that process separation becomes an operational choice, not the default ontology of roles.

## Handoff Skills

The existing handoff skill direction still works.

It just needs to be interpreted differently.

For same-agent role handoff, the handoff skill should generally do:

1. decide that a role change is warranted
2. assemble the context packet needed for the target role
3. persist or park the outgoing role-local working memory as needed
4. activate the target role posture
5. continue cognition under the new posture

In the default same-agent case, this is a **context shift**, not necessarily a full process handoff.

That means the current governed workflow direction is still valid:

- `handoff.to_role` remains the right generic workflow skill

But the semantics change:

- same-agent role handoff is primarily posture activation
- peer/external delegation remains true handoff/delegation

## Delegation And Subagents

Subagent spawning should be tightly connected to the delegation workflow/skill model.

The delegation contract should determine:

- goal/task packet
- allowed tools
- allowed skills
- memory allowance
- write-back allowance
- runtime budget
- expected result/return schema

This is important because subagents should not inherit affordances accidentally.

The delegating role should shape the subagent’s envelope deliberately.

## Personality And Memory

Roles should share:

- one personality
- one durable memory substrate
- one base self

They should not each get separate autobiographies.

They should each get:

- role addendum
- role-local working memory
- role-specific projection rules

This lets developer, architect, researcher, and other roles feel like the same being in different postures rather than sibling personas wearing a shared database like a trench coat.

## Working Memory

Working memory should remain role-local.

That matters because two roles of the same self may need:

- different active task state
- different tool histories
- different pending hypotheses
- different temporary assumptions

So the clean split is:

- shared durable memory
- role-local working memory

This is much healthier than either:

- one giant undifferentiated scratchpad
- or fully duplicated durable memory per role

## Memory Pressure

This shift also fits the current memory architecture.

Because roles are posture filters rather than separate minds, they do not each require a full independent memory projection all the time.

Instead:

- durable memory remains shared
- active role posture influences what subset of memory is projected
- subagents get only the memory excerpt they need

This saves focus and context tokens at exactly the layer the role model was meant to optimize.

## Router Boundary

This design also reinforces the need for request classes.

If the active role is on the cognitive path, it may make:

- `cognitive` model requests

If it spawns a subagent or uses bounded helper operations, those paths may instead involve:

- `transform`
- `synthesis`
- `embedding`
- or lightweight bounded cognitive work with stripped context

The point is that not every delegated worker needs the full cognitive ceremony of the parent role.

## Suggested Decision Rule

When work arrives:

1. should the same self continue under its current posture?
2. should the same self switch posture?
3. should the same self delegate bounded work to a subagent?
4. does the job require separate concurrent materialization for operational reasons?

Default answers should favor:

- posture switch before process split
- subagent before concurrent role process

## What This Changes In Existing Proposals

This shift changes the default interpretation of:

- role incarnations
- handoff semantics
- subagent delegation
- role materialization policy

It does **not** invalidate:

- role addenda
- role-specific toolsets and skillsets
- handoff workflow skills
- active routed incarnation concepts
- subagent runtime mode

It mainly changes the default assumption from:

- “roles are separate concurrently running minds”

to:

- “roles are context-swapped postures of one mind; subagents carry delegated concurrency”

## Open Questions

1. How should role-local working memory be stored and reactivated across posture switches?
2. Does the hotel own parked role working memory, or does `philote` manage it internally?
3. What exact packet shape should a role handoff/context shift carry?
4. What exact delegation packet should subagents receive?
5. When should a role be allowed to request concurrent materialization of another role rather than spawning a subagent?
6. How much of role switching should remain visible to the operator by default?
7. Should subagents ever be allowed limited memory write-back, and if so under what policy?

## First Honest Follow-On Proposal

The next narrower proposal should likely define:

1. role-local working memory activation/parking
2. same-agent handoff/context-shift packet shape
3. subagent delegation packet and affordance policy
4. the exact rubric for:
   - context switch
   - subagent spawn
   - concurrent role materialization

## Current Slice

This paper captures the current design shift:

- roles share the same self, personality, and durable memory
- roles differ by addendum, tools, skills, and working memory
- same-agent handoff is primarily a context shift
- subagents are the default unit of delegated parallel work
- concurrent role processes remain available as an operational escape hatch, not the default ontology

This is a design direction, not an implemented runtime claim.
