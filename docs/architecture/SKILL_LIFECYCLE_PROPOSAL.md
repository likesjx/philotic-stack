---
title: "Skill Lifecycle and Delegation Contract Proposal"
doc_type: proposal
domain: runtime-sessions
status: accepted
last_updated: 2026-03-13
tags:
  - skills
  - delegation
  - subagents
  - lifecycle
  - governance
  - active-seam
related_docs:
  - GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md
  - ROLE_ACTIVATION_AND_SUBAGENT_CONTRACTS_PROPOSAL.md
  - RUNTIME_AUTHORITY_LEASES_PROPOSAL.md
  - ARCHITECTURE_STATUS.md
proposal_id: skill-lifecycle-delegation-contract
active_seams:
  - governed-workflow-skills
  - role-incarnation-model
---

# Skill Lifecycle and Delegation Contract Proposal

## Goal

Lock the full contract for delegation skill lifecycle and subagent materialization.

This covers: skill definition validation, lease negotiation, hook subscription, the meta-skill that creates skills, and the two tools (`skill.register`, `subagent.spawn`) that operationalize the system.

The prior slice introduced `SubagentDelegation` as a compatibility-first wire contract with several optional fields. This proposal closes that: lease terms and hook subscriptions become required, a typed lifecycle state machine is defined for every skill, validation is layered across three explicit gates, and the hotel gains real `SpawnSubagent` execution with lease and hook registries backed by that state machine.

## Skill Lifecycle States

A skill is a validated state machine:

```
draft → validated → registered → active → deprecated
              ↓           ↓
           invalid     suspended  (dependency went offline)
```

- **draft** — definition has been constructed but no validation has run
- **validated** — passed Layer 1 static checks; consistent as a value independent of mesh state
- **registered** — passed Layer 2 capability checks against the current hotel and mesh; persisted to the graph; available for `subagent.spawn`
- **active** — at least one live subagent is currently executing under this skill
- **deprecated** — explicitly retired; no new spawns allowed; existing workers run to completion
- **invalid** — failed Layer 1 validation; permanent; the definition itself is malformed and cannot be repaired in-place
- **suspended** — passed Layer 1 and Layer 2 at registration time, but a required mesh capability has since gone offline; automatically retried when the dependency re-advertises

A suspended skill does not lose its registration. It resumes without re-running Layer 2 once the missing capability is visible again. An invalid skill must be discarded and a new one created.

## Validation Layers

### Layer 1 — Static / Internal Consistency

Gate: `draft → validated` (or `draft → invalid`)

No mesh access required. Pure function over the skill definition. A definition that fails any check here is permanently `invalid`. These are not warnings.

Named `ValidationError` variants:

- `TtlMustBePositive` — `lease.ttl_seconds` must be at least 1
- `RenewalIntervalExceedsTtl { renewal, ttl }` — `renewal_interval_seconds` must be less than `ttl_seconds`; a heartbeat interval that exceeds the idle lifetime means the first heartbeat can never arrive in time
- `MaxLifetimeBelowTtl { max_lifetime, ttl }` — `max_lifetime_seconds` must be greater than or equal to `ttl_seconds`; a hard cap below the idle window is incoherent
- `NotifyPersonaRequiresRenewalInterval` — `idle_behavior: NotifyPersona` without `renewal_interval_seconds` is incomplete; there is no interval on which the persona would be notified
- `AutoRenewRequiresMaxLifetime` — `idle_behavior: AutoRenew` without `max_lifetime_seconds` means the hotel has no hard cap to enforce; auto-renewing without a ceiling is not allowed
- `RequiresParentAckWithoutApprovalHook` — `completion_contract.requires_parent_ack: true` requires `HookKind::ApprovalNeeded` in the hook subscriptions; otherwise the parent has no way to receive the ack request
- `HookSubscriptionsEmpty` — at least one hook subscription is required; the fixed hooks are always delivered regardless, but the negotiated set must be non-empty to confirm the persona has acknowledged the hook vocabulary
- `IterationBudgetMustBePositive` — if `iteration_budget` is set, it must be at least 1

All variants that include field values carry the actual values in the error, not just the field names.

### Layer 2 — Capability Validation

Gate: `validated → registered` (or `validated → suspended` if the hotel is reachable but capabilities are temporarily absent)

Requires an active hotel connection:

- every name in `allowed_tools` must resolve in the abstract tool catalog
- every name in `allowed_skills` must resolve to a skill in `registered` state
- `lease_terms` must fall within hotel-enforced policy bounds; specifically, `ttl_seconds` may not exceed the hotel's configured ceiling and `max_lifetime_seconds` may not exceed the hotel's hard cap

A Layer 2 failure that is transient (capability offline) results in `suspended` rather than `invalid`. A failure that is structural (tool name does not exist in the catalog at all) results in a registration refusal with an explicit structured error — but not `invalid`, because the definition itself is coherent; the mesh simply cannot satisfy it at this time.

### Layer 3 — Spawn-Time

Gate: point-in-time check on `SpawnSubagent`, not a state transition

Every `SpawnSubagent` request runs a lightweight point-in-time check even when the skill is `registered`:

- all names in `allowed_tools` are still live in the current mesh
- the hotel can currently honor the requested lease terms

Two outcomes:

- **`SpawnSubagentOk { subagent_guest_id, confirmed_lease }`** — requested terms matched; the confirmed lease may differ only by timestamps and epoch, not by policy values
- **`SpawnSubagentProposal { subagent_guest_id, confirmed_lease, delta }`** — hotel can honor a modified set; `delta` is a structured diff of which fields changed and why

On `SpawnSubagentProposal`, the persona must either send `AcceptSubagentLease { subagent_guest_id }` or `AbortSpawn { subagent_guest_id }`. The hotel holds the guest in a pre-materialization state until one of these arrives. An `AbortSpawn` at this stage is not a failure — it is a valid outcome.

## Data Contracts

### `HookKind`

```
Progress
TurnStarted
ToolCall
TurnCompleted
ApprovalNeeded
```

Hook subscriptions are negotiated at spawn time. The negotiated set determines what `FireSubagentHook` calls the hotel will route versus drop. Fixed hooks are always routed regardless of the negotiated set.

**Fixed hooks (always delivered, non-negotiable):**

- `subagent.complete` — emitted by worker when task is fully resolved
- `subagent.failed` — emitted by worker or hotel when execution fails unrecoverably
- `lease.expiring` — emitted by hotel when `idle_behavior = NotifyPersona` and the lease is approaching expiry

Fixed hooks are delivered to the parent's session inbox whether or not they appear in `hook_subscriptions`. The negotiated set only controls the optional hooks.

### `IdleBehavior`

```
Terminate
NotifyPersona   // requires renewal_interval_seconds
AutoRenew       // requires max_lifetime_seconds
```

`Terminate` — hotel destroys the worker process and releases the lease when `ttl_seconds` elapses without a renewal.

`NotifyPersona` — hotel fires `lease.expiring` before expiry. The persona must `RenewSubagentLease` or `ReleaseSubagent` in response. If neither arrives before hard expiry, hotel terminates.

`AutoRenew` — hotel renews the lease without persona involvement, up to `max_lifetime_seconds`. Past that cap, the hotel terminates regardless of task state.

### `SubagentLeaseTerms`

```rust
pub struct SubagentLeaseTerms {
    pub ttl_seconds: u64,
    pub renewal_interval_seconds: Option<u64>,
    pub max_lifetime_seconds: Option<u64>,
    pub idle_behavior: IdleBehavior,
}
```

- `ttl_seconds` — idle lifetime; timer resets on each `AssignSubagentTask` or `RenewSubagentLease`
- `renewal_interval_seconds` — how often the persona must heartbeat when `idle_behavior = NotifyPersona`
- `max_lifetime_seconds` — hard cap enforced by hotel; persona cannot override it after spawn
- `idle_behavior` — what the hotel does when the lease expires without renewal

### `HookSubscription` (routing refinement)

Each hook subscription carries a typed route so the hotel knows where to deliver the event without the persona having to specify it at spawn time.

```rust
pub struct HookSubscription {
    pub hook_kind: HookKind,
    /// Where to deliver this event. Defaults to `PersonaAgent`.
    pub route: HookRoute,
    /// Required when `route = Discard`; identifies the local skill to invoke.
    pub handler_skill: Option<String>,
}

pub enum HookRoute {
    /// Deliver to the persona agent that spawned this subagent (default).
    PersonaAgent,
    /// Deliver to any active role with this name on the mesh.
    Role { role_name: String },
    /// Fire locally for side-effects only. Requires `handler_skill`.
    Discard,
}
```

The skill definition includes two mandatory routing declarations in addition to the per-hook subscription list:

- **`completion_route`** — where `subagent.complete` is routed; cannot be `Discard`
- **`failure_route`** — where `subagent.failed` is routed; cannot be `Discard`

Both default to `PersonaAgent`. Setting either to `Role` routes the terminal event to the named role rather than back to the spawning persona — useful for fan-out patterns where a coordinator role receives completions from multiple workers.

Layer 1 validation rejects `completion_route: Discard` and `failure_route: Discard` with the variants `CompletionRouteMustNotDiscard` and `FailureRouteMustNotDiscard`. This prevents silent discard of terminal events.

### Updated `SubagentDelegation`

The compatibility-first optional fields in the prior slice become required:

```rust
pub struct SubagentDelegation {
    pub parent_agent_id: String,
    pub parent_role: String,
    pub subagent_kind: String,
    pub goal: String,
    pub context_packet: SubagentContextPacket,
    pub allowed_tools: Vec<String>,
    pub allowed_skills: Vec<String>,
    pub memory_allowance: Option<String>,
    pub writeback_allowance: Option<String>,
    pub iteration_budget: Option<u32>,
    pub completion_contract: SubagentCompletionContract,
    // formerly optional, now required:
    pub lease_terms: SubagentLeaseTerms,
    /// Negotiated set; each entry carries its own delivery route.
    pub hook_subscriptions: Vec<HookSubscription>, // min 1
    pub completion_route: HookRoute,
    pub failure_route: HookRoute,
}
```

Persona agents do not construct `SubagentDelegation` manually. The `subagent.spawn` tool assembles it from the registered skill definition. The persona provides a `skill_id` and a `goal`.

### New IPC Verbs

**`AssignSubagentTask { subagent_guest_id, lease_epoch, delegation }`**

Send a new bounded task to an already-materialized worker. Implicitly renews the lease. `lease_epoch` must match the current epoch in the hotel's lease registry; a stale epoch causes a fencing rejection.

**`RenewSubagentLease { subagent_guest_id, lease_epoch }`**

Explicit heartbeat from persona to hotel without assigning a new task. Used for long-running tasks where the persona wants to signal liveness. `lease_epoch` must match.

**`ReleaseSubagent { subagent_guest_id }`**

Terminate the worker immediately and release the lease. The worker receives a shutdown signal and has a grace window to emit `subagent.complete` or `subagent.failed` before the hotel forcibly destroys it.

**`FireSubagentHook { subagent_guest_id, hook_kind, payload }`**

Emitted by the worker over IPC. Hotel checks the negotiated hook set and either routes to the parent's session inbox or drops. Fixed hooks always route.

### New IPC Responses

**`SpawnSubagentOk { subagent_guest_id, confirmed_lease: LeaseEnvelope }`**

Fast path. Requested terms matched hotel policy. Worker is materializing. `confirmed_lease` is the authoritative lease record for this subagent's lifetime.

**`SpawnSubagentProposal { subagent_guest_id, confirmed_lease, delta }`**

Hotel can honor modified terms. `delta` describes what changed. Persona must `AcceptSubagentLease` or `AbortSpawn` before the worker materializes.

### `SkillValidationState`

```
Draft
Validated
Registered
Suspended { reason: String }
Invalid { errors: Vec<String> }
Deprecated
```

`reason` in `Suspended` is a human-readable description of what dependency went offline. `errors` in `Invalid` carries the structured `ValidationError` variant strings.

### `SkillSourceSnapshot`

Recorded at registration time. Frozen. Used to detect policy drift between when a skill was registered and when it is being used.

```rust
pub struct SkillSourceSnapshot {
    pub mesh_catalog_version: String,
    pub hotel_policy_version: String,
    pub registered_at: u64,
    pub registered_by: String,  // persona guest_id
}
```

## Field Sourcing Map

Every field in a delegation skill definition has a declared source type. The meta-skill follows this map strictly. It never asks for what it can derive.

Source types: `persona_input`, `session_state`, `mesh_catalog`, `skill_registry`, `hotel_policy`, `role_capabilities`

| Field | Source |
| --- | --- |
| goal | persona_input |
| context_packet | session_state (auto-populated) |
| allowed_tools | mesh_catalog + persona_input (mesh provides universe, persona narrows) |
| allowed_skills | skill_registry |
| lease.ttl_seconds | hotel_policy (ceiling) + persona_input (value) |
| lease.idle_behavior | persona_input |
| lease.max_lifetime_seconds | hotel_policy (derived, not asked) |
| hook_subscriptions | role_capabilities + persona_input |
| completion_contract | persona_input |
| iteration_budget | persona_input |

Sourcing order is deterministic:

```
session_state → hotel_policy → mesh_catalog → skill_registry → role_capabilities → persona_input
```

By the time `persona_input` fields are elicited, the solution space is already constrained by all prior sources. The meta-skill presents only the persona-input fields that cannot be derived, in the order they appear in the sourcing chain.

A missing required source is a hard stop. If `mesh_catalog` is unreachable, the meta-skill fails with `source_unavailable: mesh_catalog` rather than accepting persona-supplied substitute data. Personas cannot override source authorities.

## The Meta-Skill (`skill-creator`)

The meta-skill is the only authorized path to creating delegation skills. It is itself a delegation skill with a maximally strict contract:

```yaml
skill: skill-creator
kind: subagent
lease:
  ttl_seconds: 600
  renewal_interval_seconds: 120
  idle_behavior: notify_persona
  max_lifetime_seconds: 1800
hooks:
  subscriptions: [progress, approval_needed]
completion_contract:
  summary_required: true
  artifact_refs_expected: true
  requires_parent_ack: true
  failure_summary_required: true
```

The meta-skill carries the full Layer 1 ruleset as inline executable constraints, not documentation. Each field is validated at the moment it is determined. Cross-field invariants (`NotifyPersonaRequiresRenewalInterval`, `RequiresParentAckWithoutApprovalHook`, etc.) are checked immediately when both sides of the invariant are known. The meta-skill cannot produce output that fails Layer 1.

**Idempotency**: the meta-skill derives skill IDs deterministically from canonical field content. Re-running with the same intent against the same sourced values returns the existing registered skill rather than creating a duplicate. The meta-skill signals this as a successful resolution, not a collision.

**Authorization gate**: the meta-skill is only invocable by roles with `skill_creation_authority` in hotel policy. This gate is checked before materialization. A role without `skill_creation_authority` receives a `NotAuthorized { required: "skill_creation_authority" }` rejection immediately.

The meta-skill does not run Layer 2 validation itself. It hands off the validated definition to `skill.register`, which runs Layer 1 independently (defense in depth) and then runs Layer 2 against the hotel.

## The Tools

### `skill.register`

Used by the meta-skill to write a fully validated skill definition to the graph.

Behavior:

- runs Layer 1 validation independently on the provided definition; does not trust that upstream work was correct
- returns structured `ValidationError` variants on any Layer 1 failure; never returns a generic failure string for a validation issue
- runs Layer 2 validation against the hotel; surfaces capability resolution errors with structured detail
- on success, atomically writes the skill record and its `SkillSourceSnapshot` together; partial writes are not possible
- sets `validation_state: Registered` on the written record
- `approval_class: "skill_registry_write"` — operator-level authorization is required; this is enforced in the approval policy before execution

`skill.register` is not callable directly by persona agents. It is a tool granted only to the `skill-creator` meta-skill via its `allowed_tools` definition.

### `subagent.spawn`

Used by persona agents to materialize a subagent from a registered skill ID.

Parameters:

- `skill_id` (required) — must reference a skill in `Registered` state
- `goal` (required) — the specific bounded goal for this spawn instance
- `context_override` (optional) — additional context fields to merge into the auto-populated `context_packet`; cannot override fields sourced from `session_state`
- `tool_restriction` (optional) — a subset of `allowed_tools` from the skill definition; cannot add tools not in the registered definition

Behavior:

- rejects skills in any state other than `Registered` with `SkillNotReady { state, reason }`
- assembles the full `SubagentDelegation` from the registered definition plus the provided goal; persona does not construct the delegation manually
- runs Layer 3 spawn-time check
- returns `SpawnSubagentOk` or `SpawnSubagentProposal`

Tool description (for cognitive tool filtering):

> Spawn a registered subagent skill by ID to handle a bounded delegated goal. Requires a registered skill_id and a specific goal. Returns subagent_guest_id and confirmed lease terms. Use only when the task is clearly delegable and a registered skill exists for it.

The description is precise because imprecise tool descriptions cause overselection. `subagent.spawn` should never be the model's first-reach tool for anything except explicit subagent materialization.

## Subagent Worker Lifecycle

The worker run loop:

```
boot → receive delegation → execute task → emit subagent.complete → idle/waiting
                                                  ↑___________________________|
                                                  ← receive AssignSubagentTask
```

The worker never exits voluntarily. The hotel keeps the process alive while the lease is active. Task assignment is decoupled from worker materialization: a worker can receive multiple sequential tasks within a single lease lifetime via `AssignSubagentTask`.

The worker fires hooks via `FireSubagentHook` IPC. The hotel checks the negotiated set registered at spawn time, routes matching hooks to the parent's session inbox, and drops non-matching hooks. Fixed hooks (`subagent.complete`, `subagent.failed`, `lease.expiring`) are always routed regardless of what is in the negotiated set.

The persona holds an `active_subagents` map in `SessionState`:

```rust
pub active_subagents: HashMap<String, SubagentHandle>
```

where:

```rust
pub struct SubagentHandle {
    pub guest_id: String,
    pub lease_epoch: u64,
    pub current_goal: Option<String>,
    pub spawned_at: u64,
}
```

The persona uses `lease_epoch` in every `AssignSubagentTask` and `RenewSubagentLease` call. The hotel rejects stale epochs. This is the fencing mechanism that prevents a persona from accidentally driving a worker whose lease has already been rotated.

## Disposition

`accepted — current slice`

## Current Slice

**In this slice (Blocks A–F):**

- data contracts: `HookKind`, `IdleBehavior`, `SubagentLeaseTerms`, updated `SubagentDelegation` with required lease terms and hook subscriptions, new IPC verbs (`AssignSubagentTask`, `RenewSubagentLease`, `ReleaseSubagent`, `FireSubagentHook`, `AcceptSubagentLease`, `AbortSubagentSpawn`, `RegisterSkill`), new IPC responses (`SpawnSubagentOk`, `SpawnSubagentProposal`, `SubagentLeaseRenewed`, `SkillRegistered`)
- Layer 1 validation as pure functions in `ansible-mesh-core`, with all named `ValidationError` variants
- `SkillValidationState` and `SkillSourceSnapshot` on `AbstractSkillRecord`; `upsert_abstract_skill`, `get_abstract_skill`, `list_abstract_skills` on `GraphStorage` trait and `SqliteGraphStorage`
- real `SpawnSubagent` execution in hotel (replaces the explicit `SUBAGENT_NOT_IMPLEMENTED` stub from the prior slice)
- lease registry in hotel state: tracks active leases by `subagent_guest_id`, enforces epoch fencing, handles `idle_behavior` timers
- hook subscription registry in hotel state: records negotiated set per `subagent_guest_id`, routes or drops `FireSubagentHook` accordingly; `SubagentHookRecord` carries typed `HookSubscription` list with per-hook `HookRoute`
- `HookSubscription` routing refinement: each subscription entry now carries `route: HookRoute` and optional `handler_skill`; `completion_route` and `failure_route` are top-level fields on the skill definition; Layer 1 rejects `Discard` on either terminal route
- `philote` crate split into lib + two binary targets:
  - `philote` binary — persona cognitive loop (`AgentRuntime`, unchanged)
  - `philote-worker` binary — `WorkerRuntime` implementing `AgentDriver`; receives `SubagentDelegation` via `InboundTask`, dispatches single-model-call execution, fires `TurnCompleted` hook via `FireSubagentHook`
  - `AgentDriver` trait — shared `run()` contract for both runtime types
- `RegisterSkill` hotel handler: translates `IpcRequest::RegisterSkill` → `SkillDraft` → `validate_skill_layer1` → `apply_validation_to_record` → `graph.upsert_abstract_skill` → `IpcResponse::SkillRegistered`
- `skill.register` and `subagent.spawn` tools added to abstract tool catalog with full schemas; both classified as `"capability"` class
- `is_local_agent_tool()` and `execute_local_agent_tool()` wired in `philote` for both tools: IPC dispatch → response parsing → `handle_tool_result` continuation

**Not in this slice:**

- Layer 2 mesh-capability validation (the Layer 2 path is defined and referenced; the hotel plumbing to check capability advertisement against the skill definition is not yet wired)
- `skill-creator` meta-skill materialization (the contract is defined here; the skill record itself and its authorized invocation path are deferred)
- authorization gate enforcement for `skill_creation_authority` (the `skill.register` tool is currently accessible to any agent; the hotel-side auth gate on materialization is deferred to a future slice)
- `active_subagents` map in `SessionState` for multi-turn subagent tracking (handle and epoch tracking deferred; current slice fires-and-forgets via single-call IPC)
- Layer 3 spawn-time capability check on `SpawnSubagent` (defined; hotel runs lease negotiation only at present)
