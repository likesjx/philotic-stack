# Agent Incarnation Model Proposal

## Goal

Define a first-class incarnation model for agents so that a single agent identity can exist as multiple concurrent instances with different capability postures, lifetimes, and ownership of the communication plane. This closes the design gap between a conversational presence and goal-focused execution, while establishing the right primitives for memory, forking, subagent spawning, and inter-agent communication.

## Disposition

`draft — proposed for design review before implementation`

## Linked Work Surface

[docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md) — Agent Logic, Personality and Context sections.
[AGENT_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_LOOP_PROPOSAL.md) — prerequisite gap closures.

---

## Core Thesis

An **agent** is an identity: a profile with soul, history, relationships, and memory.

An **incarnation** is a running instance of that agent with a specific capability posture, a bounded lifetime, and — at any given moment — possible ownership of the communication plane for a session.

The communication plane (hegemon) should deliver inbound messages to whichever incarnation currently owns the session. Forking is the act of a running incarnation delegating session ownership to a new one. The original incarnation stays alive but goes quiet until control returns.

This maps naturally to how people work: you talk to someone (conversational), they spin up a focused mode to do a task (worker), maybe they delegate a subtask to someone else (subagent). The session tracks who is "on duty."

---

## Incarnation Taxonomy

Three incarnation kinds. All run as `agent-core` binary processes — the kind is a configuration posture, not a different binary.

### `conversational`

- **Purpose**: User-facing. The default presence on the communication plane.
- **Lifetime**: Long-lived. Supervised by the hotel (auto-respawned if it dies). One per agent identity per hotel.
- **Toolset posture**: Minimal. Primary tools are: memory operations, profile query/update, status, and handoff/kickoff actions. No heavy side-effecting tools by default.
- **Memory access**: Full — has working memory (turns), session memory, and long-term memory access.
- **Session ownership**: Owns inbound messages when no worker is active.

### `worker`

- **Purpose**: Goal-focused execution. Spawned by the conversational incarnation for a specific declared goal.
- **Lifetime**: Medium-lived. Exists until the goal is complete, the user abandons it, or a configured idle TTL expires (suggested default: 30 minutes). Not auto-respawned on crash — failure is a terminal event that returns ownership to conversational.
- **Toolset posture**: Pre-configured named profile (e.g., `"codex"`, `"browser"`, `"research"`). Tools are selected at spawn time and do not change during the worker's life.
- **Memory access**: Read-only access to shared long-term memory. Writes go through explicit `UpdateMemory` actions so the conversational agent can review them on handoff-back.
- **Session ownership**: Owns inbound messages from the moment it acknowledges the handoff until it emits HandoffBack or is abandoned.

### `subagent`

- **Purpose**: Single atomic task. Spawned by any incarnation. Not user-facing.
- **Lifetime**: Short. Dies after emitting a result (or a failure) or hitting a hard TTL (suggested default: 5 minutes). Not supervised — no auto-respawn.
- **Toolset posture**: Explicitly enumerated at spawn time. No defaults.
- **Memory access**: None by default. May receive a snapshot of relevant context at spawn.
- **Session ownership**: Never. Subagents do not appear in the communication plane. Their results are delivered to the spawning incarnation as task results.

---

## Session Ownership and Communication Plane Switching

### Current gap

Today the communication plane routes inbound messages using a per-task `final_reply_role` / `final_reply_guest_id`. This is per-turn, not per-session. Hegemon has no session-level concept of which incarnation is "on duty."

### Recommendation

Add `active_incarnation_id` to the session record in the Context Graph.

Before routing an inbound message, hegemon queries the session record to determine which incarnation currently owns it, then routes to that guest. If no session record exists, fall back to the default conversational incarnation for the agent.

The session record is owned by the hotel (Context Graph). Only the hotel updates `active_incarnation_id`, in response to IPC requests from incarnations — not directly by hegemon.

### Handoff Flow

```
conversational emits: HandoffToWorker { goal, toolset_profile, workspace_ref?, ttl_s }
  ↓
IpcServer receives HandoffToWorker
  hotel materializes worker incarnation if not already live (with toolset_profile bindings)
  hotel updates session.active_incarnation_id = worker.guest_id
  hotel emits SessionHandoffTask to worker with {goal, session_id, memory_snapshot}
  hotel emits HandoffAck to conversational
  ↓
worker receives SessionHandoffTask → starts loop
next inbound message from hegemon → routes to worker
  ↓
worker emits: HandoffBack { summary, memory_updates? }
  hotel updates session.active_incarnation_id = conversational.guest_id
  hotel delivers summary to conversational as an InboundTask
  hotel applies memory_updates (with validation)
  ↓
next inbound message → routes back to conversational
```

### Abandonment

The user can send `/abandon` to abort the current worker. Hegemon forwards this to the active incarnation. If it is a worker, it terminates and control returns to conversational with a summary of what was in progress.

A worker may also self-abandon with `AbandonSelf { reason }` if it determines the goal is impossible.

---

## Forking Semantics

Forking is not process-level — it is session ownership transfer. The original incarnation is not killed. Multiple incarnations of the same agent can coexist on the hotel, but only one owns the session at a time.

Key rules:

- **Only the active session owner can fork.** Conversational can hand off to a worker; a worker cannot directly promote a subagent to session owner.
- **Forks are sequential, not parallel.** There is one active session owner at all times. Parallel multi-worker execution is a later concern.
- **Forking is scoped to a session.** A hotel can have multiple sessions active (different Telegram chat IDs, different users). Each session has its own `active_incarnation_id`.
- **Workers do not persist across hotel restarts.** When the hotel restarts, all worker incarnations are gone. The conversational incarnation is re-materialized and the session returns to it.

---

## Subagent Spawning

A subagent is spawned via a new `SpawnSubagent` IPC action:

```rust
SpawnSubagent {
    parent_task_id: Uuid,
    goal: String,
    toolset: Vec<String>,
    context_snapshot: Option<String>,  // JSON, agent decides what to pass
    workspace_ref: Option<String>,
    ttl_seconds: u64,
}
```

The hotel:
1. Materializes a short-lived `agent-core` process with `PHILOTIC_AGENT_ROLE=subagent`, `PHILOTIC_AGENT_PARENT_ID=<spawner>`, and the provided toolset bindings.
2. Sends the subagent a single inbound task containing the goal and context snapshot.
3. Waits for the subagent to emit a result (via `CompleteTask` or `FailTask`).
4. Delivers the result to the spawning incarnation as a `tool_result`-shaped InboundTask.
5. Reclaims the subagent process after result delivery.

Subagents are opaque to hegemon and to the user. From the parent's perspective, spawning a subagent looks like invoking a slow tool.

From the agent loop perspective, `SpawnSubagent` is an `AgentAction` kind. The loop handles it the same way as a tool call: `WaitingTool` phase, with subagent result delivered as a tool result that re-enters the model loop.

---

## Memory Architecture

Four tiers, ordered by scope and durability:

### Tier 1: Working Memory (in-process, per turn)

- `recent_turns`: last 8 completed turns (already exists)
- `working_tool_history`: tool call + result pairs for the current in-flight turn (required by AGENT_LOOP_PROPOSAL.md Gap 1)
- Rolling summary: auto-generated summary injected when `recent_turns` window fills, replacing the oldest turns

**Working memory summarization**: when `recent_turns` reaches 8, the conversational incarnation should emit a `SummarizeContext` internal action. This is a model call (short prompt: "Summarize the key facts from this conversation history in 3-5 sentences for future reference.") whose result replaces the oldest half of the turn window and is stored in Tier 2. This should be transparent to the user.

### Tier 2: Session Memory (hotel apartment, per session)

- Session checkpoint: already stored as `short_session:{session_id}` — includes bindings, active turn, recent turns, profile snapshot.
- Session facts: new apartment type `session_facts:{session_id}` — structured facts the agent has written about this session (user preferences learned, decisions made, context established). Written via `UpdateMemory { kind: "session_fact" }`.

### Tier 3: Agent Short-Term Memory (hotel apartment, per agent, cross-session)

- Agent profile: `soul_text`, `identity_text`, `user_context_text`, `memory_summary` — already seeded from config. Can be updated by the agent via `UpdateMemory { kind: "profile" }`.
- Activity log: a rolling summary of recent sessions (what was worked on, outcomes, unresolved threads). Written on session end by the conversational incarnation.
- Cross-session user context: persistent facts about the user that should survive across sessions (preferences, relationships, ongoing projects). Written explicitly by the agent.

Tier 3 is stored in `memory_apartments` under agent-scoped keys (not session-scoped). Hotel enforces size limits.

### Tier 4: Long-Term Memory (external service)

Initially: **Muninn** accessed as a hotel-mediated tool. The conversational agent can call `memory.search`, `memory.store`, and `memory.relate` tools that the hotel routes to a local Muninn HTTP endpoint. This is the same pattern as the current `model.manager.*` tools — a hotel-side invoker that wraps the external service.

Later: **Philotic-native memory guest** (`muninn-core` crate) materializing as a hotel guest with its own UDS registration, exposing the same tool surface but without the HTTP hop. Whether this is a Rust rewrite of Muninn internals or a port of the protocol is a separate decision to make after Muninn's value is demonstrated in production.

The decision of whether Muninn stays external or becomes native should be driven by whether cross-hotel memory federation is needed. If memory is hotel-local, a native guest is clean. If memory needs to be shared across hotels, Muninn's existing graph API may be the right substrate to keep.

### Memory Update Safety

The agent can propose memory updates via a new `UpdateMemory` action:

```rust
UpdateMemory {
    kind: MemoryUpdateKind,  // SessionFact | Profile | ActivityLog | UserContext
    content: String,
    merge_strategy: MergeStrategy,  // Append | Replace | Patch
}
```

Hotel-side enforcement:
- `SessionFact`: always allowed, size-limited.
- `Profile` (`identity_text`, `soul_text`): rate-limited. Max 1 profile update per session. Content size-limited. Operator-configurable: can require `/approve` before applying.
- `UserContext`: allowed but rate-limited. Size-limited.
- `ActivityLog`: only on session end signal; not mid-session.

The conversational incarnation is the only one allowed to write Tier 3 memory. Workers write to Tier 1/2 only, and propose Tier 3 updates via `HandoffBack.memory_updates`. Conversational reviews and applies (or discards) those proposals.

---

## Inter-Agent Communication

Agents can communicate with each other as peers via existing `EmitTask` IPC. The key gap is discovery: an agent needs to know what other agents are available.

### Recommendation

The session snapshot returned by the hotel should include a `known_peers` list:

```json
"known_peers": [
  { "agent_id": "agent-aria-01", "role": "agent", "hotel_id": "aria-architect-hotel" }
]
```

An incarnation can then emit a task to a peer agent by `target_guest_id = "agent-aria-01"`. The hotel routes it via the normal `EmitTask` dispatch (local UDS if same hotel, inter-hotel mesh if remote).

Peer task results return via the normal `InboundTask` path to the sending incarnation. From the loop's perspective, a peer task call looks like a slow tool call.

A `DelegateToPeer` action is cleaner than a raw `EmitTask`:

```rust
DelegateToPeer {
    peer_agent_id: String,
    goal: String,
    context_snapshot: Option<String>,
}
```

The hotel wraps this in a proper `EmitTask` and wires up the reply routing.

Deferred: streaming replies from peer agents; broadcast to all agents of a role.

---

## Incarnation Lifetime Policy

| Kind | Supervised | Auto-respawn | Idle TTL | Death on failure |
|---|---|---|---|---|
| `conversational` | Yes | Yes | None (persistent) | No — supervisor respawns |
| `worker` | No | No | 30 min (configurable) | Yes — returns to conversational with error summary |
| `subagent` | No | No | 5 min (hard) | Yes — delivers FailTask to parent |

Workers and subagents are tracked in the Context Graph under `materialized_guests` with `incarnation_kind` field. The supervisor loop skips auto-respawn for non-conversational kinds.

On hotel restart:
- Conversational incarnations are re-materialized (existing behavior).
- Worker and subagent guests are marked `is_active=0` on restart (they had ephemeral state that is now gone).
- Sessions that were owned by a worker revert to conversational on next inbound message (the hotel resolves this via `active_incarnation_id` fallback logic).

---

## New IPC Actions Required

The following new `IpcRequest` variants are needed:

| Action | Direction | Purpose |
|---|---|---|
| `HandoffToWorker { goal, toolset_profile, workspace_ref, ttl_s }` | guest → hotel | Fork session to a worker incarnation |
| `HandoffBack { summary, memory_updates }` | guest → hotel | Return session to conversational |
| `AbandonSelf { reason }` | guest → hotel | Worker/subagent self-terminates |
| `SpawnSubagent { ... }` | guest → hotel | Materialize a short-lived subagent |
| `UpdateMemory { kind, content, merge_strategy }` | guest → hotel | Propose a memory/profile update |
| `DelegateToPeer { peer_agent_id, goal, context_snapshot }` | guest → hotel | Delegate a goal to another agent |

The following new `IpcResponse` variants are needed:

| Response | Direction | Purpose |
|---|---|---|
| `HandoffAck { worker_incarnation_id }` | hotel → guest | Worker spawned and session transferred |
| `HandoffBackAck` | hotel → guest | Session returned to conversational |
| `SubagentResult { task_id, result }` | hotel → guest | Subagent completed |
| `MemoryUpdateAck { applied: bool, reason? }` | hotel → guest | Memory update applied or rejected |

---

## Slash Commands

New slash commands for the communication plane:

| Command | Effect |
|---|---|
| `/worker <goal>` | Request handoff to a worker incarnation for the stated goal |
| `/abandon` | Abandon the current worker; return to conversational |
| `/break` | Finish current worker iteration, yield session to conversational, keep worker idle for `/resume` |
| `/worker status` | Show current session owner and worker state |
| `/memory show` | Display current Tier 2/3 memory summary |
| `/memory reset` | Reset session facts for this session |

Worker switching is a **conversation plane action**, not a configuration action. `/worker <goal>` is the canonical switch mechanism — either typed directly or triggered by a Mini App button that sends the command on the user's behalf. There is no parallel control plane for runtime switching; the session plane owns it.

---

## Telegram Mini App — Configuration Surface

The Telegram Mini App is the right surface for *configuration* of the incarnation model, not for runtime switching.

### What belongs in the Mini App

- Toolset profile catalog — browse available profiles, assign tools, set execution modes
- Incarnation status — view active workers, how long they have been running, idle vs. active state
- Agent profile editing — soul, identity, user context, memory summary (rich text, not chat commands)
- Approval management — pending approvals with full context, preapproval policy configuration
- Hook registry — view what listeners are registered at each lifecycle point
- Memory browser — Tier 2/3 session facts, ability to delete specific entries
- Vault credential onboarding — the secure path for entering credentials without plaintext in chat

### What does not belong in the Mini App

Runtime session switching. A "switch to worker" button in the Mini App is acceptable only if it sends a `/worker <goal>` message into the conversation plane — it is a shortcut, not a separate control mechanism.

### Security model

The BlobService (port 9001) must **not** host Mini App assets. It is internal hotel infrastructure with no access control beyond content-addressed IDs. Repurposing it as an external-facing web server conflates two services with incompatible trust requirements.

Mini App architecture:

- Assets hosted externally (static host or CDN) — no hotel HTTP exposure for assets
- Mini App calls a dedicated hotel API endpoint, not the blob service
- That endpoint sits **behind hegemon** as the perimeter, not directly internet-facing
- Telegram Mini App auth is well-specified: `initData` signed by Telegram, backend verifies HMAC-SHA256 with the bot token — a real auth mechanism, not hand-rolled
- HTTPS is required by Telegram for Mini App web app URLs — if a hotel-local endpoint is needed, a TLS proxy (e.g. Caddy) fronts it, same pattern as the Muninn Claude Desktop solution

### BlobService security note

The BlobService has no access control today — SHA-256 content IDs are unguessable but are not credentials. For production / VPS deployment:

- Bind blob service to localhost only
- External model providers (real Gemini cloud API) cannot fetch `http://localhost:9001/blob/<sha>` — this is a latent bug in the current media analysis path. Resolution: inline base64 encoding for small media, or signed temporary URLs for large blobs, rather than raw localhost URLs passed to cloud APIs.
- Blob service security contract must be resolved before the stack is deployed beyond local development.

---

## Implementation Order

This proposal has hard dependencies. Recommended sequencing:

1. **AGENT_LOOP_PROPOSAL.md gaps first.** The re-entry loop (Gap 1) is required before worker incarnations can do multi-step work. Tool catalog (Gap 3) is required before toolset profiles are meaningful.

2. **Session ownership in Context Graph.** Add `active_incarnation_id` to session records. Hotel reads it during IpcServer `EmitTask` routing. This is the load-bearing primitive for everything else.

3. **Conversational toolset restriction.** Configure the conversational incarnation with a minimal default toolset. Gate heavy tools behind worker-only posture. This is achievable by configuration before any new IPC actions exist.

4. **`HandoffToWorker` / `HandoffBack` IPC.** Once session ownership exists, implement the handoff protocol and worker materialization.

5. **Memory Tier 2 (session facts) and `UpdateMemory`.** These are safe to add before Tier 4 (Muninn), and they establish the memory write contract that Tier 4 can plug into later.

6. **Muninn tool surface (Tier 4).** Add `memory.search` / `memory.store` as hotel-mediated tools pointing at the local Muninn endpoint.

7. **`SpawnSubagent`.** Can land after HandoffToWorker since the infrastructure is similar.

8. **Inter-agent communication (`DelegateToPeer`).** Land after subagents, since it uses the same result-routing pattern.

---

## Open Questions

- **Worker naming**: should toolset profiles be named strings (e.g., `"codex"`) that the hotel resolves from a catalog, or should the caller pass the full toolset list? Named profiles are cleaner for the user-facing UI; explicit lists are more general. Likely both: named profiles with an override list.

- **Conversational agent identity continuity across worker periods**: while the worker owns the session, can the user break through to the conversational agent with a special command? Or is the session fully transferred? Recommendation: `/conversational` as an escape hatch that pauses the worker and lets the user speak directly to the conversational agent.

- **Memory update operator review**: for profile updates, should approval always be required, or should there be a `trust level` on the agent that controls this? An untrusted agent should always require approval before profile changes.

- **Muninn vs. native memory backend**: this decision should be driven by a concrete evaluation of whether Muninn's value is demonstrated through Codex/Claude use (the current experiment). If yes, port the protocol. If the evaluation is inconclusive, defer. Do not commit to a Rust rewrite before the value is clear.

- **Multi-hotel session ownership**: what if the conversational agent is on hotel A and the worker should run on hotel B? The `active_incarnation_id` model works if both hotels share the session record, which they currently do not. This is deferred to after the inter-hotel mesh is more mature.

- **Parallel workers**: can a session ever have two active workers simultaneously? This proposal says no — sequential only. Parallel execution is addressed via subagents (which are not session owners) rather than multiple session owners.

---

## Addendum: Design Review Analysis

*Added after initial proposal draft. Records confidence levels, identified risks, and recommended revisions per design review.*

### Confidence Summary

| Component | Confidence | Key Risk |
|---|---|---|
| Taxonomy (conversational/worker/subagent) | 8/10 | Blurry toolset boundary, toolset profile catalog unspecified |
| `active_incarnation_id` primitive | 8/10 | Race window during handoff, IpcServer routing change scope |
| Handoff flow (HandoffToWorker/Back) | 5/10 | Worker readiness, conversational behavior during delegation |
| Forking semantics | 8/10 | Solid; `/break` is the correct escape hatch, not `/conversational` |
| Subagent spawning | 5/10 | One-shot runtime mode unspecified, hotel result routing should be async |
| Memory Tiers 1–3 | 7/10 | Summarization failure path, session_facts granularity |
| Memory Tier 4 (Muninn) | 4/10 | Context injection protocol unspecified, external dependency fragility |
| Inter-agent communication | 3/10 | Tool-call abstraction breaks for multi-turn exchange; cross-hotel is the primary use case but unaddressed |
| Mini App — config surface | 7/10 | HTTPS/TLS requirement, hegemon-proxied API design needed |
| Mini App — worker switching | resolved | Not a Mini App concern; conversation plane owns runtime switching |
| BlobService security | open risk | No access control, localhost-only required, latent bug with cloud Gemini URLs |

---

### Section-by-Section Notes

#### Taxonomy

The conversational/worker/subagent split is right. The binary-with-posture approach (same process, different configuration) avoids build and deploy complexity.

**Gotcha — toolset boundary is blurry.** The conversational agent's restricted toolset forces hand-off by making certain things literally impossible without tools. But the model still has to decide when to hand off versus answer directly, and it will get this wrong in both directions — spawning unnecessary workers for lightweight requests, or attempting complex tasks it doesn't have tools for. The restriction should be soft and configurable, not so tight that conversational is useless for routine requests.

**Gap — the toolset profile catalog is unspecified.** `HandoffToWorker` takes a `toolset_profile` string like `"codex"` or `"browser"`. Where does this catalog live? Who configures it? How does the conversational model know what profiles exist? This is a blocking gap for the HandoffToWorker implementation. The catalog should live in the hotel config (mesh-config.json) as named toolset bundles the hotel can resolve at spawn time.

#### `active_incarnation_id`

The right primitive. One field, no new infrastructure, enables everything else.

**Risk 1 — Race window during handoff.** The hotel updates `active_incarnation_id = worker.guest_id`, then spawns the worker process. The worker takes 500ms–2s to fork, exec, connect UDS, and register. If a message arrives from hegemon during this window, routing reads the new ID but the worker's socket isn't registered yet — the message fails or drops. Two options: (a) buffer inbound messages and replay them once the worker registers, or (b) don't update `active_incarnation_id` until the worker sends its registration acknowledgment. Option (b) is cleaner — conversational might receive one extra message before the handoff completes, which is acceptable.

**Risk 2 — IpcServer routing change.** The proposal says "hegemon queries the session record," but hegemon doesn't read the Context Graph. It's the hotel's IpcServer that routes inbound tasks. The IpcServer routing logic needs to change from "route by target_role" to "for agent-type tasks, look up active_incarnation_id from the session record and route to that guest." This is more involved than the proposal implies and should be called out explicitly.

#### Handoff Flow

**Gap 1 — Worker readiness.** The flow shows the hotel sending `SessionHandoffTask` to the worker immediately after spawning it. The worker isn't connected yet when that send happens. The handoff task must be queued and delivered upon worker registration, not on spawn.

**Gap 2 — Conversational behavior during delegation.** After emitting `HandoffToWorker` and receiving `HandoffAck`, what does the conversational agent do? It should enter a delegating phase that routes any new user messages to the worker rather than processing them directly. The protocol for this is unspecified. A simple approach: conversational transitions to a `delegating` turn phase and refuses to start new turns, relying on `active_incarnation_id` routing to send messages to the worker anyway.

**Gap 3 — `/break` vs `/conversational` mechanics.** The escape hatch is mentioned but not specified. What does "pause the worker" mean concretely? A cleaner model: `/break` causes the worker to finish its current iteration, emit a status summary, and yield session control back to conversational while remaining live and idle. Conversational can then issue `/resume` to hand back to the same worker. True abandonment is `/abandon`.

**Gap 4 — Stale worker state on reuse.** "Hotel materializes worker if not already live" implies reusing an idle worker. But an idle worker's in-memory session state may be stale from a prior task. The hotel must send a fresh `SessionHandoffTask` to any worker being reused, and the worker must reinitialize its session state on handoff receipt regardless of prior state.

#### Subagent Spawning

**Problem 1 — agent-core has no one-shot execution mode.** The current runtime loops forever on `recv_task()`. A subagent needs: receive one task, execute it (possibly with a tool loop), emit result, exit. This requires a runtime mode flag (`PHILOTIC_AGENT_MODE=subagent`) that changes loop behavior: after emitting `CompleteTask` or `FailTask`, exit rather than continue listening. This is a non-trivial runtime change.

**Problem 2 — Hotel result routing must be async.** The proposal says the hotel "waits for the subagent to emit a result." The hotel cannot block its IPC handler thread synchronously on a subprocess. The correct model: hotel fires off the spawn, records the parent-child association (parent_task_id → parent_guest_id), and when the subagent later emits `CompleteTask`/`FailTask`, the IpcServer looks up the parent association and delivers the result as an `InboundTask` to the parent. This is fully async and matches the existing IPC model.

**Problem 3 — TTL.** The "5 min (hard)" TTL in the lifetime table conflicts with the caller-configurable `ttl_seconds` in the SpawnSubagent struct. Resolution: caller-configurable with a hard maximum (suggest 30 minutes). The 5-minute default is too aggressive for any multi-step reasoning task.

**Problem 4 — Nested subagents.** Unaddressed. Recommend: subagents cannot spawn subagents by default. If nested spawning is later allowed, child TTL must not exceed parent's remaining TTL.

#### Memory Architecture

**Tier 1** — Solid. One concern: the rolling summarization model call triggers on a fixed turn count (every 8 turns), adding a model round-trip at the worst moment (mid-conversation). Prefer lazy triggering (when the window would overflow) rather than eager. Also: the summary should be stored alongside `recent_turns`, not replacing them — so you can fall back to raw turns if the summary is absent or clearly wrong.

**Tier 2** — Solid. Recommend structuring `session_facts` as a list of fact records rather than a single JSON blob, so the hotel can enforce count-based size limits and the agent can target-delete specific facts.

**Tier 3** — Medium. The "cross-session user context" flat text field is the weakest part. In practice, agents write noise here, miss what matters, or write things that age poorly. Muninn (Tier 4) is the right long-term home for this kind of structured knowledge. Tier 3 should stay as profile metadata (soul, identity, memory_summary) rather than accumulating free-form user facts.

**Tier 4 (Muninn)** — Most speculative. Two critical gaps:

1. **Context injection is unspecified.** The agent calls `memory.search` as a tool and gets back N memories as a tool result. Then what? Does it inject them automatically into the next prompt? Does it re-submit to the model? Does the model have to explicitly request memory retrieval? The proposal needs to specify: before building the model prompt, if Muninn is configured, automatically run `memory.search` with the user message as the query, and inject top-N results into the `[Knowledge projection]` section. This makes retrieval transparent to the model rather than requiring the model to "think to use memory." This is a system design choice, not a tool design choice.

2. **External dependency fragility.** If Muninn is down, the hotel-mediated invoker must return empty results with a logged warning — not fail the turn. Fallback behavior must be explicitly specified and tested.

Do not commit to this integration until real usage reveals whether agents naturally reach for memory tools or whether it requires heavy system prompt engineering. The answer determines whether Muninn is a first-class feature or a later optimization.

#### Inter-Agent Communication

This section needs the most revision.

**Problem 1 — Tool-call abstraction doesn't support multi-turn exchange.** "Peer task call looks like a slow tool call" works for fire-and-forget delegation. It breaks for agent collaboration that requires back-and-forth: Jane delegates to Aria, Aria sends a clarifying question, Jane responds. This cannot be modeled as a tool call. A "peer sub-session" abstraction is needed for that use case — one that's out of scope for the first slice.

**Problem 2 — `known_peers` needs a policy.** Unspecified. Starting point: all active guests with `role=agent` on the local hotel. Cross-hotel peers are a separate problem.

**Problem 3 — Cross-hotel is the primary use case.** Jane and Aria are on different hotels. `DelegateToPeer` across hotels requires inter-hotel result routing that currently has acknowledged gaps (ACK boundary, loopback-only addressing). Designing `DelegateToPeer` without resolving those gaps first produces a feature that works locally and fails in production.

**Recommendation:** Remove `DelegateToPeer` from this proposal. Replace with a narrower first slice: same-hotel agent-to-agent task emission using existing `EmitTask` + `known_peers` in the session snapshot. Validate that this works in live use. Generalize to cross-hotel once the mesh result routing is solid.

---

### Revised Minimum Viable Slice

The components below have the highest confidence and deliver the core value without the high-risk pieces:

1. **`active_incarnation_id` in Context Graph + IpcServer routing update.** Don't update until worker registers (option b above).
2. **Soft conversational toolset restriction.** Configurable via hotel config, not hard-coded. Default: exclude workspace and execution tools; include memory, status, and handoff actions.
3. **Toolset profile catalog in hotel config.** Named bundles (e.g. `"codex": [workspace.list, workspace.read, echo]`) resolvable at HandoffToWorker time.
4. **Worker readiness protocol.** Buffer inbound messages between spawn and registration; deliver on registration.
5. **`HandoffToWorker` / `HandoffBack` IPC** with the delegating phase on conversational and a `HandoffAck` that includes the worker incarnation ID.
6. **Liveness fallback on session routing.** If `active_incarnation_id` points to an unregistered guest, fall back to the default conversational incarnation immediately and log the miss.

Everything after this — subagent one-shot mode, Muninn context injection, inter-agent communication — builds on this foundation and should be sequenced based on what the live system reveals about actual usage patterns.
