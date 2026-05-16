---
title: Capability Pool and Purpose Composition Proposal
doc_type: proposal
domain: operator-control-plane
status: proposed
last_updated: 2026-04-04
tags:
  - capability
  - roles
  - toolsets
  - skills
  - admin
  - control-plane
related_docs:
  - ROLE_POSTURE_AND_ADMIN_PROPOSAL.md
  - CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
  - MEMORY_LAYERING_AND_WORK_PRODUCT_SPLIT_PROPOSAL.md
  - GRAPH_DATASOURCE_PROPOSAL.md
proposal_id: capability-pool-and-purpose-composition
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
---

# Capability Pool and Purpose Composition Proposal

## Goal

Replace the current flat, single-profile role capability model with a graph-backed composition model that supports:

- a base capability set shared by all agents
- reusable functional capability pools
- purpose-driven capability bundles
- role templates that start from purpose rather than bespoke one-off payloads
- admin-managed maintenance in the web control plane

The point is to stop manufacturing a new `toolset_profile` every time a role needs one extra tool and one less identity crisis.

## Disposition

Proposed.

Current runtime truth already has the first useful primitive:

- each role references one `toolset_profile`
- profiles grant tools and skills
- roles can carry posture/governance separately via `role_identity_addendum` and `role_manifest`

What is missing is composition. The current model is too flat for:

- shared defaults across all agents
- layered functional capability sets
- purpose-driven role authoring
- admin-maintained role templates
- clear GUI representation of authored vs effective capability surfaces

## Current Slice

Park the capability composition boundary now so future role/admin work can build toward it intentionally.

This slice should:

- define the capability graph layers
- define ownership between graph design state and runtime effective state
- define role templates as admin-maintained starting points
- clarify how additive per-role overrides work
- require GUI representation for authored and effective capability surfaces

This slice does **not** implement the full graph schema, runtime resolver, or admin editor yet.

## Core Recommendation

Philotic should move from a singular `toolset_profile` string toward a graph-backed capability model with four authored layers:

1. base capability set
2. functional capability pools
3. purpose bundles
4. role templates and role-local overrides

Runtime sessions should continue to materialize one effective tool/skill surface, but that surface should be resolved from graph-owned composition rather than hand-authored flat profiles alone.

## Capability Layers

### 1. Base Capability Set

Capabilities all agents should receive by default.

Examples:

- session introspection
- memory recall
- rule proposal
- safe return/handoff primitives

This is the universal floor, not the place to hide half the platform because it was convenient.

### 2. Functional Capability Pools

Reusable grouped capability families.

Examples:

- memory
- governance
- delegation
- workspace
- routing
- configuration
- admin
- model management

Pools should grant tools and skills together when they serve one operational function.

### 3. Purpose Bundles

Purpose bundles compose one or more functional pools into role intent.

Examples:

- orchestrator
- researcher
- coder
- admin
- operator
- reviewer

Purpose should drive capability shape. We should not keep pretending that purpose emerges naturally from a bag of tools after the fact.

### 4. Role Templates and Overrides

Role templates should be admin-managed starting points that bind:

- purpose bundle(s)
- default posture/addendum guidance
- default governance/manifest language
- default approval expectations
- optional model/context loop preferences

Individual roles may then apply additive overrides:

- add a tool
- add a skill
- add a pool
- tighten posture/governance

Subtractive denies can be deferred until we actually need them.

## Ownership Model

### Graph-Owned Design State

The agent graph / control-plane graph should own:

- base capability definitions
- capability pools
- purpose bundles
- role templates
- role-to-purpose links
- additive role overrides

This is the designed authority.

### Runtime-Owned Effective State

The hotel/runtime should own:

- resolved effective toolset
- resolved effective skillset
- approval gating at execution time
- session-local projection of capability state

This is the activated authority.

The graph defines what should be available.
The runtime defines what is currently in force.

## Role Templates

Philotic should have standard admin-maintained role templates, beginning with:

- orchestrator
- researcher
- coder
- admin

Templates should be readable by role managers and editable only through admin-governed paths.

`role.configure` should eventually be able to start from a template, then apply overrides, instead of forcing every role to be authored from scratch like the platform has never met itself before.

## Tool and Skill Grants

The model should support:

- inherited grants from base capability
- inherited grants from functional pools
- inherited grants from purpose bundles
- inherited grants from templates
- additive per-role tool grants
- additive per-role skill grants

This lets us express:

- what everyone gets
- what this purpose needs
- what this exact role additionally needs

without exploding the number of flat profile variants.

## GUI Requirements

The web/admin GUI must represent both:

### Authored Capability Structure

- base capability set
- functional pools
- purpose bundles
- role templates
- per-role overrides

### Effective Capability Result

- resolved tools
- resolved skills
- approval-sensitive tools clearly marked
- source attribution for each grant
  - inherited from base
  - inherited from pool
  - inherited from purpose
  - inherited from template
  - added directly on role

If this stays backend-only, we will recreate capability confusion with prettier nouns.

## Admin Governance

Admins should be the maintainers of:

- capability pools
- purpose bundles
- role templates
- shared capability defaults

Regular roles may consume templates or request changes, but template governance should remain admin-owned.

This aligns with the broader control-plane expectation that shared institutional capability shape is operator/admin maintained, not quietly improvised by any role that happens to be holding a keyboard.

## Relation To Existing Role Model

Current runtime truth:

- `role_identity_addendum` = role posture overlay
- `role_manifest` = role governance/contract
- `toolset_profile` = current singular capability reference

Recommended future direction:

- preserve addendum and manifest as posture/governance layers
- evolve capability authoring beyond one flat profile string
- keep runtime effective projection simple even if authored graph structure becomes richer

## Follow-On Seams

1. Define graph schema for capability pools, purpose bundles, role templates, and additive overrides.
2. Update role manager flows so `role.configure` can start from a template.
3. Define a migration path from singular `toolset_profile` to composed capability resolution.
4. Add admin GUI surfaces for authored capability structure and effective resolved capability views.
5. Decide whether `toolset_profile` survives as a derived/materialized runtime artifact or becomes legacy compatibility vocabulary.
