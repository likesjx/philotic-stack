---
title: "Agent-Centric Resource Model"
doc_type: proposal
domain: runtime-sessions
status: accepted-current-slice
last_updated: 2026-03-27
tags:
  - resource-broker
  - demand-driven-materialization
  - agent-graph
  - graph-storage-trait
  - mesh-sync
  - training-data
  - onnx
  - rl-flywheel
  - active-seam
related_docs:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
  - TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md
  - TASK_RUNNER_PROPOSAL.md
  - LOCAL_ONNX_INFERENCE_PROPOSAL.md
  - MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md
  - CONTEXT_GRAPH_RUNNER_PROPOSAL.md
  - AGENT_LOOP_PROPOSAL.md
  - GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: agent-resource-model
active_seams:
  - agent-resource-broker
  - agent-graph-toolrunner
  - router-training-tap
---

# Agent-Centric Resource Model

## Goal

Define an architecture where agents are active participants in their own
resource lifecycle — declaring what they need, requesting it at runtime, and
owning their own graph state — while the hotel remains the authority on
effective runtime rights, bindings, routing, and system-level resource
management.

The current model is hotel-push: the hotel decides what guests to run and what
resources agents receive. This proposal moves to an agent-pull model: agents
declare and request resources, the hotel brokers those requests against a
rights and availability surface, and supporting guests are materialized only
when at least one agent demands them.

Six interconnected components compose this model:

1. Resource broker
2. Demand-derived materialization
3. Multi-tenant shared resource components
4. Agent graph as a required tool-runner resource
5. Two-tier graph authority (hotel vs. agent)
6. Router-observable throughput and training data pipeline

---

## Disposition

`accepted for current slice` — Seams 1–5 shipped on `codex/agent-resource-model`. DEF-003 fixed.

- Seam 1 (`agent-resource-broker`, commit a2aeb33): IPC types live; hotel handler
  stubs both request variants NOT_IMPLEMENTED.
- Seam 2 (`demand-derived-materialization`, commit 73e7467): `ResourceDeclaration`
  type added; `ResourceRegistry` + `boot_reconcile` in `aiua`; transitional boot
  call logs demand-derived set alongside existing `materialize_all` path.
- Seam 3 (`agent-graph-toolrunner`, commit dbac5d3): `AgentGraphStorage` trait +
  `SqliteAgentGraphStorage` (per-agent embedded SQLite); new `agent-graph-runner`
  crate with `agent.graph.*` tool surface and experience trace auto-recording.
- Seam 4 (`agent-graph-mesh-sync`, commit 3f2f566): LWW snapshot export/import,
  `EventKind::AgentGraphSync`, two-tier authority invariant (grants excluded),
  and now opportunistic task-carried graph hydration for transported
  agent-directed work when the source hotel already knows the owning agent.
- Seam 5 (`router-training-tap`, commit 4eb153b): `RouterTrainingRecord` +
  `SqliteRouterTraceStorage`, model-router wired to `PHILOTIC_ROUTER_TRACE_DB`,
  `ResourceType::RouterListener`.

This proposal supersedes the implicit static-guest-list model in the current
`materialized_guests` configuration surface. It does not conflict with active
work on role incarnation, skill lifecycle, or the ONNX runner — it provides
the resource substrate those features sit on top of.

---

## Current Slice

**Seams 1–5 complete.** B1 workstream shipped. DEF-003 fixed (110 aiua tests now pass).
Seam 2 remains labeled transitional — `materialize_all` still runs alongside
the demand-derived registry; full replacement lands after mesh-sync is proven in production.

---

## Component 1: Resource Broker

The hotel maintains a **resource registry** that is distinct from the
`GuestManager`. The `GuestManager` manages process lifecycle (spawn, supervise,
respawn). The resource registry manages the semantic layer above that: what
resources exist, who is using them, and whether requests can be satisfied.

### Resource request lifecycle

```
Agent issues ResourceRequest { resource_type, config_hint, agent_id }
  │
  ▼
Hotel resource registry
  ├─ Rights check: does this agent have the right to request this type?
  │   └─ Denied → ResourceDenied { reason }
  ├─ Instance exists and has capacity?
  │   └─ Yes → register agent as tenant → ResourceGranted { instance_id }
  └─ Instance does not exist (or over capacity on all instances)?
      └─ Trigger materialization → ResourceMaterializing { instance_id }
          └─ On ready → ResourceGranted { instance_id } (async notify)
```

### First-class IPC message types

- `ResourceRequest` — agent → hotel; declares a need
- `ResourceGranted` — hotel → agent; binding is live
- `ResourceDenied` — hotel → agent; reason included
- `ResourceMaterializing` — hotel → agent; materializing, will notify
- `ResourceReleased` — agent → hotel; agent no longer needs this resource
- `ResourceRevoked` — hotel → agent; hotel is withdrawing the grant

`ResourceRevoked` is the hotel's enforcement path. Rights revocation, tenant
limit reduction, or forced teardown all flow through this message. Agents must
respect revocation without cooperation — the hotel does not require agent
consent.

### Routing table

The hotel maintains two projections of the resource state:

- **Resource → tenants**: used for dispatch (a telegram message arrives — which
  agents receive it?)
- **Agent → resources**: used for lifecycle (an agent is removed — which
  resources lose a tenant and may need teardown?)

Both projections are derived from the canonical resource registry in the
hotel's context graph and rebuilt on restart.

---

## Component 2: Demand-Derived Materialization

Agent ODS records carry a `static_resource_declarations` field: the list of
resources this agent always needs when it is active.

At hotel boot or agent materialization, the hotel replays those declarations
through the resource broker as synthetic `ResourceRequest` messages. The
resulting derived guest set is what the hotel runs — not a separate
"what should be running" config.

### Boot reconciliation sequence

```
1. Load agent records from ODS
2. Union all static_resource_declarations across active agents
3. For each declaration: issue ResourceRequest through broker
4. Broker finds-or-creates instances, builds routing table
5. Agent graph tool-runners are materialized first (see Component 4)
6. Other declared resources materialize in dependency order
7. Agents are considered ready once their required resources are granted
```

### Suspension vs. removal

- **Suspended agent**: holds resource reservations; resources stay alive; no
  teardown triggered
- **Removed agent**: releases all resource reservations; resources with zero
  tenants are eligible for teardown per their teardown policy

This distinction is critical for resources with external lease cost (Telegram
poll lease, open WebSocket connections). A temporary agent suspend should not
force a lease drop and re-acquisition cycle.

---

## Component 3: Multi-Tenant Shared Resource Components

Some resources are singletons per hotel (one Telegram membrane, one
model-router instance). Others could theoretically have multiple instances.
The multi-tenant model handles the common case:

- The first agent requesting a resource type triggers materialization
- Subsequent requests from other agents register as additional tenants of the
  same instance
- The resource component itself handles dispatch to the correct tenant
- The hotel broker enforces max_tenants limits at request time; excess requests
  receive `ResourceDenied`

### Resource type metadata (in ODS)

```
ResourceTypeRecord {
  resource_type: TelegramMembrane | ModelRouter | ToolRunner | AgentGraph | ...,
  singleton: bool,
  max_tenants: Option<u32>,
  teardown_policy: OnZeroTenants | Manual | AfterDelay(Duration),
  requires_rights: [RightToken],
}
```

### Lease ownership

Leases (Telegram poll lease, desktop membrane lease, etc.) live at the
**resource instance level**, not the agent level. The resource holds one lease
for all its tenants. This keeps the external protocol clean: one poller, one
lease holder, regardless of how many agents are served.

---

## Component 4: Agent Graph as Required Tool-Runner Resource

Every agent has exactly one required resource: its **agent graph tool-runner**.

This is a special-purpose tool-runner that owns the agent's local graph state.
It is materialized before the agent is considered ready, and it is the last
resource released when an agent is removed.

### Why a tool-runner

The agent interacts with its own state through the same tool surface it uses
for everything else — no special internal API, no privileged channel. This
keeps the agent's cognitive interface uniform and makes the agent graph
inspectable through the standard tool execution path.

### Tool surface

```
agent.graph.read       — query agent graph by entity type or relationship
agent.graph.write      — write or update an entity in agent graph
agent.graph.query      — graph traversal with filter
agent.graph.declare    — add or update a static_resource_declaration
agent.graph.configure  — update cognitive configuration (memory policy,
                         tool preferences, skill registrations, role config)
agent.graph.recall     — retrieve relevant past experience entries
```

### What the agent graph stores

```
AgentGraph {
  // Resource state
  active_resource_grants:     [ResourceGrant],
  pending_resource_requests:  [ResourceRequest],
  static_declarations:        [ResourceRequest],  // source for ODS sync

  // Cognitive configuration
  tool_preferences:           [...],
  routing_preferences:        [...],
  memory_policy:              MemoryPolicy,
  context_assembly_config:    ContextAssemblyConfig,
  active_skills:              [SkillRecord],
  role_config:                RoleConfig,

  // Runtime working state
  current_role:               Option<RoleIncarnationRef>,
  session_local_facts:        [...],

  // Experience ledger (feeds RL training pipeline)
  experience_traces:          [ExperienceTrace],
}
```

The agent graph is the home for mutable agent-owned overlay state:

- routing preferences
- tool and skill preferences
- local policy refinements
- role-local cognitive configuration
- learned experience and evaluation traces

It is not the canonical home for shared catalog truth about what models, tools,
skills, or rights exist in the system at large.

### Storage substrate

The agent graph tool-runner uses the existing `AgentGraphStorage` trait — an
extension of the same `GraphStorage` abstraction already in use for the hotel
CG. The implementation is a reusable middle layer that:

- defines the domain operations the agent graph needs (`get_resource_grants`,
  `upsert_tool_preference`, `upsert_routing_preference`, `record_experience_trace`, etc.)
- hides the backend behind the trait boundary
- starts with the SQLite backend already in production
- allows a backend swap (RocksDB, or anything else) later via a single impl
  swap, with no changes to callers
- now feeds stored routing preferences back into live session snapshot bindings so `philote` can compile advisory turn-routing overrides from agent-local graph state instead of relying on prompt-only preference residue

The abstraction layer is the asset. The backend is a deployment-time decision.

### Mesh sync

The agent graph syncs over the existing mesh CRDT infrastructure — the same
transport already used for hotel CG state. The agent graph tool-runner
serializes its state to a snapshot (using the storage trait's export surface),
the mesh CRDT layer carries it, and the receiving hotel's tool-runner imports
and applies it.

Current implemented pressure adds a second continuity aid on top of that
background sync: when the source hotel already knows which agent owns an
agent-directed transported task, it may attach the current `agent_graph_snapshot`
directly to the task payload. The receiving hotel hydrates that snapshot into
its local per-agent store before delivery. This does not replace mesh sync; it
reduces the very awkward window where an agent arrives before its graph does.

That ownership hint should prefer an explicit `agent_id` carried in the routed
payload when session-derived inference is unavailable. Remote operator-chat and
peer-delegation paths now do this so continuity does not depend on a lucky
local session record or a guest-id shape the sending hotel cannot actually
resolve.

That continuity path is now explicitly home-hotel scoped. Routed payloads should
carry `authority_hotel` alongside `agent_id`, and a sending hotel should only
attach an authoritative `agent_graph_snapshot` when it is that agent's current
home authority. Transport placement and durable identity ownership are related,
but letting them quietly impersonate each other would be a very efficient way
to smear agent selfhood across the mesh.

Receiving hotels now also persist that provenance into session runtime state.
When agent-directed work is delivered locally, the hotel decorates it with
delivery context like `delivery_hotel`, `delivery_node_id`, and the concrete
target guest/role, then records that beside `authority_hotel` in session
summary. That makes home ownership vs current execution placement visible in
runtime truth and canonical session snapshots instead of leaving it trapped in
one inbound payload.

Those placement records now also carry explicit marker typing. Runtime
provenance should include at least a `marker_kind`, `marker_source`, and
`marker_strength`, and now an inferred `placement_risk_level`, so the hotel can
distinguish transport continuity markers from role-handoff markers, receptor
ingress markers, or later routing enzymes, and also tell how much placement
authority and remote-execution trust that marker should have. A marker is more
useful when you know not just where it points, but what biological process
expressed it, how strongly it should count, and what risk posture it implies.

That provenance now influences local runtime behavior too. When a session has
no live active incarnation, or its recorded active incarnation is stale, the
hotel may prefer the persisted local `delivery_target_guest_id` for delivery or
materialization before falling back to a generic orchestrator choice. In other
words, placement is no longer just remembered; it is beginning to matter.

This is a freshness-based hint, not a second lease system. Persisted local
placement gets a bounded lifetime and should undergo apoptosis when it goes
stale or is superseded by newer placement truth. The point is not to crown a
new exclusive holder; it is to keep useful local continuity briefly alive
without letting old placement memories haunt the runtime forever.

Different marker classes should not all age like the same tissue. A
`receptor_ingress` marker is usually just a short-lived clue about where an
incoming turn happened to land, so it should have a shorter half-life than a
`transport_continuity` marker that was expressed specifically to carry agent
placement across hotels. `role_handoff` markers can reasonably live a little
longer because they encode an intentional operator or workflow move rather than
an incidental ingress stain.

They should not all resolve conflict the same way either. A fresh
`active_incarnation_id` update should kill older `receptor_ingress` placement
markers immediately, because those are weak local hints. But explicit
`transport_continuity` and `role_handoff` markers can remain valid under that
same conflict and keep steering fallback or parking toward their persisted
guest, because they encode stronger continuity or intentional movement rather
than a transient receptor signal.

Strength should also shape whether a marker can trigger parking or
rematerialization on its own. A weak `receptor_ingress` clue is good enough to
steer delivery to a live local guest, but not good enough to request local
parking/materialization when no such guest is currently expressed. Medium or
strong continuity markers can keep that placement claim longer, because they
are closer to deliberate continuity than incidental ingress.

That posture should now also constrain execution reach, not just placement.
An elevated-risk placement marker should not cause the hotel to advertise
remote execution paths as if the session were fully trusted mesh-local
continuity. Rights do not change, but the projected execution posture can
narrow: elevated-risk sessions can be forced into local-only execution until a
stronger continuity or handoff marker is expressed.

That narrowing is now beginning to split by right class rather than one blunt
switch. A guarded session can still be allowed to use remote model/component
execution while denying remote tool execution and shrinking credential scope.
The hotel still owns grants; posture only narrows how far those grants may
reach in the current session.

Naming note: the fast, posture-derived behaviors should be called
`reflexes`. In runtime projection this now shows up as `effective_reflexes`
with fields like `remote_tool_reflex`, `remote_component_reflex`, and
`credential_scope_reflex`. The older `effective_right_policy` projection can
remain as a transitional compatibility bridge, but reflexes are the intended
operator-facing language for these quick risk/posture responses.

Reflexes should also be governable, not just inferred. The first honest shape
for that is lightweight session-level reflex records:

- `reflex_overrides` for explicit operator or workflow damping/amplification
- `reflex_evaluations` for why a reflex fired, was overridden, or should later
  be revised

Those records do not replace grants. They explain and adjust the fast posture
layer that sits downstream of grants.

The next honest step is to stop treating those records like two detached JSON
organs and give them policy shape. Runtime projection should therefore surface
an `effective_reflex_policy` with ordered layers:

- an inferred `placement_inferred` layer from runtime provenance
- optional hotel-projected `hotel_default` layers from bindings
- optional agent-graph `agent_learned` layers projected from durable learned
  reflex preferences
- optional explicit `reflex_policy_records` for session-scoped override layers
  with `policy_scope`, `policy_source`, `origin_class`, `precedence`, and
  `reflexes`
- a highest-precedence-wins merge rule that projects the final
  `effective_reflexes`

That keeps hotel inference, operator damping, and future agent-learned reflexes
in one governable stack instead of quietly relying on merge order as policy.

The first durable home for learned reflex posture should be the agent graph, not
the hotel. In the current slice that means mesh-synced `reflex_preferences`
records in the agent graph project into session bindings as `agent_learned`
layers inside `effective_reflex_policy`. The hotel still owns grants and
effective key rings; it merely projects the agent-owned learned posture into the
runtime stack it enforces.

Approved routing/reflex refinement should also be able to write back into that
agent-owned layer. The current slice now makes that bridge first-class:

- `routing.policy.propose` remains approval-gated, but now stores a durable
  `routing_policy` record in the hotel graph instead of smuggling routing
  posture through the general `rule` substrate
- the routing-specific record carries explicit `operator_disposition` state and
  durable `evaluations`, so write-back and later governance no longer rely on
  narrative prose alone
- when the approved proposal carries an explicit learned reflex payload, hotel
  IPC writes that payload into agent-graph `reflex_preferences`
- the same turn still records a local `reflex_evaluations` event, but the
  routing-policy artifact now also keeps its own durable audit trail

That is still not the whole governance story, but it turns approved refinement
into actual durable agent posture plus durable routing-policy provenance rather
than a very literate backlog item.

The next necessary refinement after first-class storage is real later-life
governance. Routing-policy records should not be frozen at birth approval.
The current slice therefore adds:

- hotel control-plane listing of agent-scoped routing-policy records
- explicit later disposition updates such as `approved` -> `rejected`
- durable evaluation append on each disposition change so operator review leaves
  the same kind of antigen marker as write-back outcomes

That keeps routing policy governance from collapsing into “whatever happened at
tool execution time must remain true forever,” which is emotionally relatable
but architecturally terrible.

The next step after later-life control is enforcement. A rejected routing-policy
artifact linked to a learned reflex preference should actually inhibit that
`agent_learned` layer during hotel binding assembly. The current slice now does
that:

- hotel snapshot assembly checks the latest routing-policy disposition for each
  linked `learned_reflex_preference_key`
- rejected disposition suppresses that `agent_learned` reflex layer from
  `effective_reflex_policy`
- suppression leaves a visible marker in bindings so the inhibition is
  inspectable instead of mysteriously biochemical

In practice, a fresher active-incarnation update is the `p53` check here: if
the runtime has newer local evidence that the session now belongs on a
different active guest, older delivery provenance should die immediately rather
than waiting out its full TTL.

This is dogfood: the mesh sync story for agent portability is proven by the
same infrastructure everything else depends on. No second sync protocol.

Sync semantics: Last-Writer-Wins at the entity level, agent identity as the
authority scope. Hotel CG and agent graph use the same mesh transport with
distinct authority domains and write access controls.

---

## Component 5: Two-Tier Graph Authority

Two graphs. Two authority domains. Both mesh-synced.

| Property | Hotel CG | Agent Graph |
|---|---|---|
| Owner | Hotel / operator | Agent |
| Write authority | Hotel processes and hotel-authorized actions | Agent (via agent.graph.* tools) and hotel (for grants/revocations) |
| What lives here | Rights, provisioning grants, routing table, resource instances, agent identity records, mesh-visible state | Cognitive harness, resource grants, preferences, local knowledge, role config, experience traces |
| Mesh sync scope | Hotel-wide; visible to mesh peers | Agent-scoped; follows the agent |
| Substrate | Hotel SQLite via `GraphStorage` trait | `AgentGraphStorage` trait (extends `GraphStorage`), SQLite backend first, swappable |

### Authority invariants

- The hotel CG is the canonical source for **what an agent is allowed to do**.
  The agent graph cannot grant itself new rights.
- The agent graph is the canonical source for **how the agent configures itself
  within its granted rights**. The hotel does not prescribe cognitive assembly.
- Grant records in the agent graph are **copies** of hotel-issued grants.
  Revocation flows from the hotel; the agent graph copy becomes stale on
  revocation, not authoritative.
- When the hotel CG and agent graph disagree on a grant, the hotel CG wins.

### Shared catalogs vs effective grants

This proposal now assumes a third conceptual layer beside the two write
authorities above:

- a **shared catalog knowledge layer** for models, tools, skills, rights,
  compatibility edges, and policy templates
- an **agent overlay layer** in the agent graph for local preferences,
  learned posture, and cognitive configuration
- a **hotel effective-state layer** for what is actually granted, bound,
  materialized, and enforceable right now

The shared catalog layer should not quietly become hotel-owned mutable state
just because the hotel happens to project parts of it.

The hotel owns the effective key ring:

- which tools/skills/rights are active for this session
- which bindings are currently projected
- which scoped credentials or grants are usable right now
- which runners/controllers are allowed to execute on the agent's behalf

That means lower routing/execution layers consume an already-authorized
envelope. They do not mint rights, widen grants, or invent new effective
capabilities mid-turn.

Transitional note:

- current session bindings may temporarily carry projections derived from both
  hotel state and agent-owned overlay state
- that relay path does not make the hotel the conceptual owner of the overlay
- it also does not make downstream routers the owner of rights
- the first enforcement slice is now live: hotel snapshots project an explicit
  `effective_rights` key ring for tools, skills, and component capabilities, and
  lower tool/component assembly paths consume that key ring instead of widening
  visibility just because a runner or route exists
- the first shared rights catalog slice is now live too: rights are becoming
  shared reference knowledge in their own right, instead of existing only as
  strings projected into session bindings

---

## Component 6: Router-Observable Throughput and Training Data

The hotel router is the central message bus for all IPC. Rather than requiring
individual components to emit training signals, the router itself is observable.
A **router-listener** subscribes to the throughput stream and serializes events
into a structured training trace store.

### Why at the router, not at the component

- The router already sees everything; no new coupling required
- Components remain unaware of training concerns
- The listener can project and filter post-hoc; no pre-labeling required
  at write time
- The trace store is append-only and separate from the event ledger

### Router-listener as hotel system resource

The router-listener is a hotel-level system resource, not an agent-tenant
resource. The hotel always runs it when the hotel is up. Agents do not request
it; operators may configure its retention and filtering policy.

### Serialization contract

**Every message type flowing through the hotel router must carry sufficient
context for training reconstruction.** This is a first-class design constraint,
not an afterthought. Required context fields on routed messages:

```
RoutedMessage {
  // Identity context
  agent_id:           AgentId,
  hotel_id:           HotelId,
  session_id:         SessionId,
  active_role:        Option<RoleRef>,

  // Message body (the actual IPC payload)
  payload:            IpcPayload,

  // Outcome context (attached by router-listener on result)
  outcome:            Option<OutcomeClassification>,
  approval_decision:  Option<ApprovalDecision>,
  timestamp:          Timestamp,
}
```

### Training record schema

```
ExperienceTrace {
  trace_id:           TraceId,
  agent_id:           AgentId,
  session_ref:        SessionId,
  role_at_time:       Option<RoleRef>,

  // The decision or action
  event_type:         ToolCall | ResourceRequest | ModelInvocation | ApprovalDecision,
  input:              SerializedPayload,
  output:             SerializedPayload,

  // Outcome signal for RL
  outcome:            Success | Failure | ApprovedByOperator | DeniedByOperator
                    | Timeout | Revoked,
  outcome_detail:     Option<String>,

  timestamp:          Timestamp,
}
```

### FunctionsGemma and the RL flywheel

The local FunctionsGemma ONNX controller (ONNX runner Slice 3, see
`LOCAL_ONNX_INFERENCE_PROPOSAL.md`) is trained on the trace store produced by
the router-listener. This closes the RL loop:

```
Agents operate → router observes → traces recorded
  → FunctionsGemma training pipeline reads traces
  → RL reward signal derived from outcome classifications
  → Updated model deployed as ONNX resource
  → Agents request FunctionsGemma controller as a resource
  → Better tool-use decisions → richer traces → tighter loop
```

The FunctionsGemma controller is a resource in the broker model. Agents that
require local function-calling inference request it by type. The hotel
materializes one instance, agents register as tenants.

---

## Implementation Sequence

These seams should be implemented in order. Each seam is a coherent slice that
leaves the system in a working state.

### Seam 1: `agent-resource-broker`

- `ResourceRequest` / `ResourceGranted` / `ResourceDenied` / `ResourceMaterializing`
  / `ResourceReleased` / `ResourceRevoked` as IPC message types in
  `ansible-mesh-core`
- Resource registry in `aiua` (separate struct from `GuestManager`)
- Rights check stub (permissive for now, enforced in a later seam)
- Routing table (resource→tenants, agent→resources)
- Static `ResourceTypeRecord` for the initial known types

### Seam 2: `demand-derived-materialization`

- `static_resource_declarations` field on agent ODS records
- Boot reconciliation loop: read agents → issue synthetic requests → derive
  guest set
- Suspend vs. remove distinction in agent lifecycle
- Teardown on zero tenants (configurable policy)

### Seam 3: `agent-graph-toolrunner`

- `AgentGraphStorage` trait in `ansible-mesh-core` — extends `GraphStorage`
  with agent-graph-specific domain operations (`get_resource_grants`,
  `upsert_tool_preference`, `record_experience_trace`, etc.)
- `SqliteAgentGraphStorage` impl — reuses existing SQLite infrastructure,
  backend swap via trait when warranted
- `AgentGraph` resource type registered with the broker
- Tool-runner variant that holds `Arc<dyn AgentGraphStorage>` and exposes the
  `agent.graph.*` tool surface
- Hotel materializes agent graph tool-runner before agent is considered ready
- Smoke: agent can read/write its own graph through tool calls

### Seam 4: `agent-graph-mesh-sync`

- Export surface on `AgentGraphStorage` trait: snapshot → serialized payload
- Import surface: apply incoming snapshot with LWW conflict resolution
- Wire into existing mesh CRDT transport (no new sync protocol)
- Opportunistically carry `agent_graph_snapshot` on transported agent-directed
  task payloads when the source hotel knows the owning agent, but only attach
  that snapshot when the source hotel is the agent's `authority_hotel`; routed
  payloads should carry both `agent_id` and `authority_hotel`, and the
  receiving hotel hydrates the snapshot before local delivery
- Agent portability proof: agent routed to remote hotel, graph follows via mesh
- Two-tier authority invariant tests: hotel CG wins on grant conflicts

### Seam 5: `router-training-tap`

- Router observability surface (subscribe/tap pattern)
- `RoutedMessage` context fields on existing IPC types (backwards-compatible
  additions)
- Router-listener as hotel system resource
- Append-only training trace store
- `ExperienceTrace` schema and serialization

### Seam 6: `functions-gemma-onnx`

- FunctionsGemma as ONNX runner Slice 3 (see `LOCAL_ONNX_INFERENCE_PROPOSAL.md`)
- FunctionsGemma registered as a requestable resource type
- RL training pipeline reads from trace store
- Hot-swap support: updated model deployed without hotel restart

---

## Open Questions

1. **Agent graph substrate**: The agent graph tool-runner manages its own
   store. Does it use an embedded SQLite per agent, or a namespaced partition
   within a shared store? Own store favors portability; shared store is simpler
   operationally.

2. **Experience trace retention policy**: How long are traces kept? Who has
   authority to prune or export them? Should agents be able to query their own
   trace history through `agent.graph.recall`?

3. **Rights enforcement seam timing**: Seam 1 stubs the rights check as
   permissive. When does the actual rights graph integration land? This is a
   dependency on the operator control plane seam.

4. **Resource request at definition vs. runtime**: Static declarations drive
   boot-time materialization. Runtime requests are dynamic. Is the agent
   allowed to issue a runtime request for a resource type not in its static
   declarations, or must all requested types be pre-declared?

---

## Related Entry Points

- [ARCHITECTURE_STATUS.md](ARCHITECTURE_STATUS.md) — active seam registry
- [ARCHITECTURE.md](ARCHITECTURE.md) — durable system reference
- [LOCAL_ONNX_INFERENCE_PROPOSAL.md](LOCAL_ONNX_INFERENCE_PROPOSAL.md) — FunctionsGemma / ONNX Slice 3
- [TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md](TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md) — tool routing and execution
- [MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md](MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md) — mesh sync placement
- [GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md](GOVERNED_WORKFLOW_SKILLS_PROPOSAL.md) — skill lifecycle (sits on top of this model)
- [docs/task.md](../../docs/task.md) — active execution surface
