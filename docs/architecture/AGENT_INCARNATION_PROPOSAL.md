# Agent Incarnation Model Proposal

## Goal

Define a first-class incarnation model for agents so that a single agent identity can operate as multiple concurrent role incarnations with distinct capability postures, separate running contexts, and controlled ownership of the communication plane. Establish the right primitives for role provisioning, membrane routing, memory sharing, handoff, and ephemeral task delegation.

## Disposition

`design review complete — revised model adopted, implementation sequencing next`

## Linked Work Surface

[docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md) — Agent Logic, Personality and Context sections.
[AGENT_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_LOOP_PROPOSAL.md) — prerequisite gap closures.

---

## Core Thesis

An **agent** is an identity: soul, history, relationships, and shared memory. That identity is singular and continuous — it does not fork.

An **incarnation** is a long-lived, named role that the agent plays. Each incarnation has its own capability posture (toolset/skillset), its own role identity addendum layered on top of the base soul, and its own running session context (turn history, working memory). Multiple incarnations can be active concurrently. The **philotic membrane (hegemon)** routes inbound user messages to exactly one incarnation at a time — the active one — but all incarnations can send outbound messages back through the membrane.

**Workers and subagents** are a separate, ephemeral category: short-lived task delegates that have no access to the communication plane. Their results bubble up to the incarnation that spawned them.

The analogy: an agent is a person. Their role incarnations are the hats they wear — developer, architect, researcher. Switching hats doesn't change who they are or what they know. A worker is more like a contractor they've hired for a specific job.

---

## Incarnation Categories

### Category 1: Role Incarnations

Long-lived, named, provisioned through the orchestrator. Each is a persistent process on the hotel.

**Properties:**
- **Identity**: shares the base agent soul + carries a `role_identity_addendum` (extra persona/directive text specific to this role)
- **Context**: own session — separate turn history, working memory, active turn state
- **Toolset**: defined by a named `toolset_profile` (see Role Provisioning)
- **Skillset**: defined by a named skill set in the profile
- **Turn loop config**: per-role — iteration cap, approval policy, model selection, context window policy
- **Memory**: full read/write access to the shared agent memory layer (same as all other incarnations)
- **Membrane**: can own inbound routing; can always send outbound
- **Lifetime**: long-lived, supervised by the hotel, auto-respawned on crash. Subject to **inactive TTL** — if a role has no active session and no pending tasks for longer than its configured idle TTL, the hotel may reclaim the process. The role definition persists in the Context Graph; it is rematerialized on-demand when it next receives a handoff or inbound task.
- **Provisioning**: created and configured via the `agent.configure_role` tool, available to the orchestrator incarnation

**Example: Aria's role incarnations**
```
aria:orchestrator     ← default active; user-facing; spawns/delegates; minimal tools
aria:tech-researcher  ← web search, doc reading, synthesis
aria:scrum-lead       ← project/task management tools
aria:developer        ← workspace.* + shell.* tools; high iteration cap
aria:architect        ← design tools, long-horizon planning; auto-approve reads
aria:env-engineer     ← infrastructure and deployment tools
```

### Category 2: Workers / Subagents

Ephemeral task delegates. Spawned by any incarnation for a specific, bounded task.

**Properties:**
- **No membrane access**: never own inbound routing; never send directly to the user
- **Results bubble up**: all outputs are delivered back to the spawning incarnation as task results
- **Toolset**: explicitly enumerated at spawn time — no profile lookup
- **Context**: receives a context snapshot passed by the spawner at spawn time; no persistent history
- **Memory**: no direct memory access by default; may receive a memory excerpt in the context snapshot
- **Lifetime**: ephemeral — exits after emitting a result or hitting TTL. Caller-configurable TTL with a hard maximum (suggested: 30 minutes). No auto-respawn; failure delivers `FailTask` to the parent.
- **From the loop's perspective**: spawning a worker looks like calling a slow tool — `WaitingTool` phase, result re-enters the model loop as a tool result

---

## Active Membrane Routing

### The `active_incarnation_id` primitive

The Context Graph session record carries `active_incarnation_id`. The hotel's IpcServer reads this field when routing inbound tasks from hegemon.

- Default: the agent's orchestrator/conversational incarnation
- Updated by the hotel in response to handoff signals from incarnations
- If the active incarnation is not registered (race window, crash), fall back to the orchestrator immediately and log

Inbound routing rule:
```
incoming message for agent X in session S
  → hotel reads session.active_incarnation_id
  → route to that guest via UDS
  → if not registered: route to default orchestrator incarnation
```

### Concurrent roles

Multiple role incarnations can be running concurrently. The `active_incarnation_id` governs inbound only. Any incarnation can emit outbound to hegemon at any time (e.g. a developer role sending a progress update while the orchestrator owns inbound).

This means a user may receive messages from multiple incarnations interleaved. Hegemon should label the sender (role name) in the delivery context so the user has visibility into which role is speaking.

### Membrane switching

The active incarnation changes when:
1. An incarnation emits a `HandoffToRole` or `HandoffBack` IPC signal
2. The user issues a `/role <name>` or `/back` slash command
3. An operator sends a control signal (future)

The hotel processes the switch: updates `active_incarnation_id`, queues inbound for the new owner, notifies both the outgoing and incoming incarnation.

**Race window handling**: the hotel does not update `active_incarnation_id` until the target incarnation's process is registered and responsive. If the target is not yet live, the hotel materializes it, queues the inbound message, and delivers once registration is confirmed.

---

## Handoff Skill

Handoff is not a raw IPC call — it is a **defined skill** that incarnations invoke to signal and execute a role transition. This makes handoff parametric, introspectable, and extensible.

### What a handoff skill defines

A handoff skill specifies:
- **Trigger patterns**: the prompt shapes or content signals that indicate this handoff should occur (used by the model to decide when to invoke it)
- **Handoff instructions**: what context to gather, how to summarize the current state, what to pass to the receiving incarnation
- **Cleanup steps**: what the outgoing incarnation should do before yielding (flush working memory, update session facts, emit a user-facing status message)
- **Target role**: which incarnation should receive the handoff

### Handoff context bundle

The handoff passes a structured bundle to the receiving incarnation:

```rust
HandoffBundle {
    goal: String,               // what the receiving role should accomplish
    context_excerpt: String,    // relevant recent turns / decisions from the sender
    session_id: String,         // shared session ID (both incarnations work within the same session)
    initiating_turn_id: String, // the turn that triggered the handoff
    return_to: Option<String>,  // role to hand back to when complete (defaults to orchestrator)
}
```

The receiving incarnation gets: its own prior session history + the `HandoffBundle`. It does not get the full turn history of the sender — that is the explicit isolation boundary.

### Handoff flow

```
orchestrator decides to hand off to developer
  → invokes handoff.to_developer skill
  → skill builds HandoffBundle from recent context
  → emits HandoffToRole IPC with bundle
  ↓
hotel receives HandoffToRole
  → materializes developer incarnation if not live
  → queues HandoffBundle for delivery on registration
  → updates session.active_incarnation_id = developer.guest_id (after registration)
  → emits HandoffAck to orchestrator
  ↓
orchestrator enters delegating phase (does not start new turns)
developer receives HandoffBundle → starts loop
next inbound user message → routes to developer
  ↓
developer completes goal
  → emits HandoffBack { summary, return_to }
hotel updates active_incarnation_id = orchestrator.guest_id
  → delivers summary to orchestrator as InboundTask
  → orchestrator resumes
```

### Return handoffs

Any incarnation can hand back. The `HandoffBack` carries a summary that the receiving incarnation (usually orchestrator) gets as its next inbound. This is the feedback loop: the orchestrator learns what the developer did.

---

## Role Provisioning

Role incarnations are defined in the **Context Graph**, not in static config. The orchestrator incarnation has access to the `agent.configure_role` tool, which creates or updates a role definition.

### Role record

```rust
RoleIncarnationRecord {
    agent_id: String,
    role_name: String,                    // e.g. "developer"
    role_identity_addendum: String,       // persona/directive text layered on base soul
    toolset_profile: String,              // reference to a ToolsetProfile in the graph
    turn_loop_config: TurnLoopConfig,
    inactive_ttl_seconds: Option<u64>,    // None = no reclaim
    is_active_in_hotel: bool,
}

TurnLoopConfig {
    max_iterations: u32,                  // model round-trips per turn
    approval_policy: ApprovalPolicy,      // preapproved_tools, preapproved_classes
    preferred_model: Option<String>,      // override model selection for this role
    context_window_turns: u32,            // how many recent turns to include in context
}
```

### Toolset profiles

A `ToolsetProfile` is a named bundle of tools and skills stored in the Context Graph as a `toolset_profile` node:

```rust
ToolsetProfileRecord {
    profile_name: String,
    tools: Vec<String>,     // direct tool grants by abstract_tool name
    skills: Vec<String>,    // skill grants (each expands to implied_tools at assembly time)
}
```

Built-in profiles seeded at hotel startup alongside the tool catalog:

| Profile | Tools | Notes |
|---|---|---|
| `"orchestrator"` | `session.status`, `agent.configure_role`, `handoff.*`, `memory.search` | Default for conversational role |
| `"codex"` | `workspace.read`, `workspace.list`, `workspace.write`, `workspace.search` | Software dev |
| `"browser"` | `browser.navigate`, `browser.read`, `browser.screenshot` | Web research |
| `"research"` | `workspace.read`, `workspace.search`, `memory.search` | Doc/memory synthesis |
| `"utility"` | `echo`, `session.status` | Minimal capability |

### Skill catalog

Skills are defined in the graph as `abstract_skill` nodes (parallel to `abstract_tool`):

```rust
AbstractSkillRecord {
    skill_name: String,
    description: String,           // model-facing description of what this skill represents
    implied_tools: Vec<String>,    // tools implicitly granted when skill is active
}
```

Skills serve dual purpose: a model prompt hint (`"Current skill posture: codex"`) and a tool grant mechanism. Assigning a skill to a role implicitly adds its `implied_tools` to the effective toolset at assembly time.

---

## Inactive TTL and On-Demand Materialization

Role incarnations that are not the active session owner and have no pending tasks may be reclaimed by the hotel after their `inactive_ttl_seconds` has elapsed.

**Reclaim**: the hotel sends a graceful shutdown signal, waits for the incarnation to flush its in-memory state to the shared memory layer, then terminates the process and marks `is_active_in_hotel = false` in the Context Graph.

**Rematerialization**: when the reclaimed role receives a handoff or inbound task, the hotel re-materializes it from its role record, delivers the pending task, and resumes normal operation. The role restores its session context from the shared memory layer.

This makes long-tail role incarnations cheap: they don't consume a running process when idle, but they retain continuity because their session state is durably stored.

The supervisor loop's existing 5s check handles this: add a TTL check alongside the liveness check. Active membrane owner is never reclaimed.

---

## Shared Memory

All role incarnations share the same memory layer. There is no per-role memory isolation. This is the continuity guarantee: every incarnation operates on the same agent knowledge, regardless of which role is active.

### Memory tiers

**Tier 1 — Working memory (in-process, per turn)**
- `recent_turns`: last N completed turns for the current incarnation's session
- `working_tool_history`: tool call + result pairs for the current in-flight turn (Gap 1 of AGENT_LOOP_PROPOSAL)
- Rolling summary: auto-generated on window overflow, stored to Tier 2

**Tier 2 — Session memory (hotel apartment, per session)**
- Session checkpoint: existing `short_session:{session_id}` — bindings, active turn, recent turns, profile snapshot
- Session facts: `session_facts:{session_id}` — structured facts written during the session. List of fact records (not a blob) so the hotel can enforce count limits and the agent can target-delete entries.

**Tier 3 — Agent memory (hotel apartment, per agent, cross-session)**
- Agent profile: `soul_text`, `identity_text`, `user_context_text`, `memory_summary` — seeded from config, updatable via `UpdateMemory`
- Activity log: rolling summary of recent sessions (outcomes, unresolved threads) — written on session end

All role incarnations read and write the same Tier 2/3 apartments. Changes during a user turn are immediately visible to all incarnations reading memory — the LWW push/pull semantics of `SyncApartment` are the consistency model. No additional synchronization is needed.

**Tier 4 — Long-term memory (external service)**
Initially: **Muninn** accessed as a hotel-mediated tool. The active incarnation can call `memory.search`, `memory.store` tools that the hotel routes to the local Muninn endpoint. Memory retrieval is automatic at prompt build time: before building the model prompt, if Muninn is configured, the hotel runs `memory.search` with the user message as the query and injects top-N results into the `[Knowledge]` projection section. This is transparent to the model.

If Muninn is unavailable, the hotel returns empty results with a logged warning — not a turn failure.

Future: a **philotic-native memory guest** combining MuninnDB and HippoGraph. The substrate decision (external vs. native, graph structure) should be driven by demonstrated production value from the Muninn experiment, not pre-committed in this proposal.

### Memory update safety

```rust
UpdateMemory {
    kind: MemoryUpdateKind,       // SessionFact | Profile | ActivityLog
    content: String,
    merge_strategy: MergeStrategy,  // Append | Replace | Patch
}
```

Hotel-side enforcement:
- `SessionFact`: always allowed, size-limited, count-limited per session
- `Profile` (`identity_text`, `soul_text`): rate-limited (max 1 per session), size-limited, operator-configurable approval gate
- `ActivityLog`: only on session end signal; not mid-session

All role incarnations may write to Tier 2. Tier 3 profile writes are rate-limited regardless of which incarnation emits them.

---

## Workers / Subagents (detail)

A worker is spawned via `SpawnSubagent` IPC:

```rust
SpawnSubagent {
    parent_task_id: Uuid,
    goal: String,
    toolset: Vec<String>,           // explicit list; no profile lookup
    context_snapshot: Option<String>,
    ttl_seconds: u64,               // caller-specified; hard max enforced by hotel
}
```

The hotel:
1. Materializes a short-lived `agent-core` process with `PHILOTIC_AGENT_MODE=subagent` and the provided toolset bindings
2. Sends the subagent a single inbound task (goal + context snapshot) once the process registers
3. Records the parent association (`parent_task_id → parent_guest_id`)
4. When the subagent emits `CompleteTask` or `FailTask`, delivers the result to the parent as an `InboundTask` (fully async — the hotel does not block)
5. Reclaims the subagent process after result delivery

The `agent-core` runtime in subagent mode: after emitting `CompleteTask` or `FailTask`, exit rather than continue listening.

Subagents cannot spawn subagents by default. Nested spawning, if later allowed, must cap child TTL to parent's remaining TTL.

---

## Inter-Agent Communication

Agents communicate as peers via `EmitTask` IPC to a known peer's `guest_id`. The session snapshot includes a `known_peers` list scoped to the local hotel. Cross-hotel peer communication uses the inter-hotel mesh (after routing gaps are resolved).

**First slice**: same-hotel peer task emission via existing `EmitTask`. Validate in live use before generalizing.

**Deferred**: `DelegateToPeer` abstraction, multi-turn peer exchange, cross-hotel result routing.

---

## Incarnation Lifetime Summary

| Category | Supervised | Auto-respawn | Inactive TTL | On failure |
|---|---|---|---|---|
| Role incarnation | Yes | Yes | Configurable (default: none for orchestrator, 30 min for others) | Respawn; session stays assigned |
| Active membrane owner | Yes | Yes | Never reclaimed while active | Respawn; buffer inbound |
| Worker / subagent | No | No | Hard max (caller-specified) | Deliver `FailTask` to parent |

On hotel restart:
- Role incarnations are re-materialized (same as current guest supervisor behavior)
- Worker/subagent guests are marked `is_active=0`; in-flight tasks are failed to their parents on reconnect
- Sessions retain their `active_incarnation_id`; if the active role is not yet re-materialized, the hotel buffers inbound until it registers

---

## New IPC Actions Required

| Action | Direction | Purpose |
|---|---|---|
| `HandoffToRole { role_name, handoff_bundle }` | guest → hotel | Signal membrane switch to a named role incarnation |
| `HandoffBack { summary, return_to? }` | guest → hotel | Return membrane to calling or specified role |
| `AbandonSelf { reason }` | guest → hotel | Worker/subagent self-terminates; delivers FailTask to parent |
| `SpawnSubagent { ... }` | guest → hotel | Materialize an ephemeral subagent |
| `UpdateMemory { kind, content, merge_strategy }` | guest → hotel | Propose a memory update |
| `ConfigureRole { role_record }` | guest → hotel | Create or update a role incarnation definition (orchestrator only) |

| Response | Direction | Purpose |
|---|---|---|
| `HandoffAck { incarnation_id }` | hotel → guest | Role active, membrane transferred |
| `HandoffBackAck` | hotel → guest | Membrane returned |
| `SubagentResult { task_id, result }` | hotel → guest | Subagent completed |
| `MemoryUpdateAck { applied: bool, reason? }` | hotel → guest | Memory update applied or rejected |
| `RoleConfigAck { role_name, is_new: bool }` | hotel → guest | Role definition persisted |

---

## Slash Commands

| Command | Effect |
|---|---|
| `/role <name>` | Request membrane switch to named role incarnation |
| `/back` | Hand membrane back to orchestrator |
| `/abandon` | Terminate current worker/subagent; return membrane to orchestrator |
| `/roles` | List configured role incarnations and active membrane owner |
| `/memory show` | Display current Tier 2/3 memory summary |
| `/memory reset` | Reset session facts for this session |

---

## Telegram Mini App — Configuration Surface

The Telegram Mini App is the right surface for configuration — not runtime role switching (that belongs to the conversation plane).

**What belongs in the Mini App:**
- Role incarnation catalog — browse, edit, create roles with identity addendum and toolset profile
- Toolset profile browser — tools, skills, implied grants
- Active incarnation status — which role owns the membrane, idle roles and their TTL status
- Memory browser — Tier 2/3 session facts, delete specific entries
- Approval management — pending approvals with full context, preapproval policy
- Vault credential onboarding — secure path for entering credentials

**Security model** (unchanged from prior review):
- Assets hosted externally, not on blob service
- Mini App calls a hotel endpoint behind hegemon as the perimeter
- `initData` HMAC-SHA256 verification against bot token
- HTTPS required; TLS proxy (Caddy) for hotel-local endpoints
- BlobService bound to localhost only; no access control; latent bug with cloud Gemini URLs receiving localhost blob URLs

---

## Implementation Order

Dependencies are real and must be respected:

1. **AGENT_LOOP_PROPOSAL gaps first.** Re-entry loop (Gap 1) and approval granularity (Gap 4) before role incarnations do meaningful multi-step work. Tool catalog (Gap 3, done) and toolset profiles before profile-based role provisioning is meaningful.

2. **Skill catalog + toolset profiles in the Context Graph.** `abstract_skill` nodes and `toolset_profile` nodes. Seed built-in profiles at hotel startup. This closes the tool assignment gap and establishes the profile resolution path.

3. **Role incarnation records in the Context Graph.** `RoleIncarnationRecord` persisted and queryable. `agent.configure_role` IPC action. Hotel can now read role definitions at materialization time.

4. **Session bindings seeded from role profile.** When a session is initialized for a role incarnation, the hotel seeds `SessionBindings` from the role's `toolset_profile`. No more `["echo"]` default.

5. **`active_incarnation_id` in session records + IpcServer routing update.** Hotel reads this field before routing inbound tasks. Default to orchestrator. Don't update until target is registered.

6. **`HandoffToRole` / `HandoffBack` IPC + handoff skill scaffolding.** Implement the membrane switch protocol. Define the first handoff skill shape.

7. **Inactive TTL + on-demand rematerialization.** Add TTL check to the supervisor loop. Restore session context from memory on rematerialization.

8. **`SpawnSubagent` IPC + subagent runtime mode.** One-shot execution mode in `agent-core`. Async result routing in the hotel.

9. **Memory Tier 2 (`session_facts`) and `UpdateMemory` IPC.** Establish the memory write contract before Tier 4 integration.

10. **Muninn tool surface (Tier 4).** `memory.search` / `memory.store` as hotel-mediated tools. Auto-injection into prompt context.

---

## Open Questions

- **Turn loop heterogeneity**: should different roles be able to run structurally different loop variants (e.g. a planning loop that builds a structured plan before tool use, vs. a reactive loop), or is per-role TurnLoopConfig sufficient? Start with config-only; defer structural loop variants unless a concrete use case requires it.

- **Role identity addendum injection**: where in the prompt does the role addendum land? Recommended: injected between base `[Identity]` and `[Knowledge]` projections, labeled `[Role: developer]`. The model sees the full persona stack in order.

- **Concurrent outbound messages**: when two role incarnations send messages in the same user turn, what does the user experience? Hegemon should deliver them in emission order, labeled by role. The user should be able to tell which role spoke.

- **Memory sync on rematerialization**: when a reclaimed role is rematerialized, it restores from Tier 2/3. Is there a risk it has stale in-memory state from a prior life? Resolution: on materialization, the hotel sends a session snapshot that the incarnation uses to initialize its working memory. Prior in-process state is gone by definition (new process).

- **`agent.configure_role` trust**: should any incarnation be able to configure roles, or only the orchestrator? Recommendation: hotel enforces orchestrator-only by checking the requesting guest's role field. Non-orchestrator configure attempts are rejected with an error.
