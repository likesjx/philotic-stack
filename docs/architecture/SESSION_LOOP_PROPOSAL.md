---
title: "Philotic Session Management and Agent Logic Proposal"
doc_type: proposal
domain: runtime-sessions
status: implemented
last_updated: 2026-03-13
tags:
  - sessions
  - approvals
  - checkpoints
  - routing
  - current-slice
related_docs:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
  - AGENT_LOOP_PROPOSAL.md
  - APPROVAL_UX_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: session-loop
implements: []
implemented_by:
  - session-checkpoint-approval-slice
active_seams:
  - session-leases
  - session-compaction
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
---

# Philotic Session Management and Agent Logic Proposal

## Goal

Port the useful parts of ZeroClaw's session + tool-call loop into Philotic without dragging the monolith back in through a side door.

The key architectural decision is:

- `ansible` owns generalized session state, routing, leases, participants, and event history.
- `agent-core` owns cognition inside a session: prompt assembly, model invocation, tool planning, tool execution coordination, compaction, and final reply synthesis.
- `membrane` and other edge guests own transport-specific bindings (`telegram chat -> philotic session`) and delivery UX, not reasoning.
- `SyncApartment` remains the agent's checkpoint path back to the Context Graph, but it should be treated as snapshot sync, not as a fine-grained event stream.

This keeps Philotic aligned with the hotel/guest model already described in [CLAUDE.md](/Users/jaredlikes/code/philotic-stack/CLAUDE.md) and [ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md).

## Disposition

Implemented for the current session/checkpoint/approval slice.

Canonical session ownership, snapshot recovery, approval events, and session-envelope behavior are in place.

Remaining related work lives in [task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md) under:

- `WI 1: Session Management`
- `WI 2: Agent Logic`
- `Deferred Design Threads`

## Session As Live Coordination Envelope

Philotic sessions should stay thinner than the old transcript-heavy continuity vessels used by systems like OpenClaw.

The important split is:

- `session` owns live present-tense coordination truth
- `working memory` is the hottest inner layer of that truth
- `memory` owns durable continuity across sessions

So a session is **not** just immediate working memory, and it is **not** the main long-term continuity container either.

### What belongs in session

Session should own the things that answer "what is happening now in this conversation/workstream?":

- participants and bindings
- conversation/exchange identity
- active role/incarnation for this session
- pending approvals, denials, and interrupts
- active delegated work and return paths
- current turn status
- local working memory needed to continue the present exchange
- a bounded recent-turn/control window needed for local continuity
- compact session facts that are still live for this session

### What should not be the session's job

Session should not be the main owner of:

- durable autobiographical memory
- durable user relationship memory
- general long-term topic memory
- giant transcript archives carried forward out of habit
- every historical detail that might someday be relevant

Those belong in memory engines and context projection, not in the live session envelope.

### Session compaction target

Compaction should therefore preserve:

- active commitments and unresolved work
- pending tool or approval state
- role-local working state that is still live
- the smallest recent-turn window needed for local coherence
- compact session facts worth carrying forward inside this same session

Compaction should aggressively avoid preserving:

- stale exploratory residue
- fully-resolved tool chatter
- old turns whose value is now durable memory rather than session actuality
- duplicated information already promoted into memory summaries or durable records

### Cross-session rule

Separate conversations should still get separate sessions even if memory recall can map them to overlapping durable context.

Memory answers:

- what is relevant
- what was learned before
- what continuity might matter

Session answers:

- what is active now
- what is pending now
- what this current conversation presently means

If Philotic lets memory replace session boundaries entirely, it will eventually ask durable memory to impersonate live coordination truth, which is a wonderfully efficient way to build confusion.

## What ZeroClaw Actually Gives Us

The useful legacy pieces are not the old process topology. They are the turn semantics:

- Persistent conversation history with system/user/assistant/tool entries.
- Bounded iterative loop with `max_tool_iterations`.
- Tool execution step between model passes.
- History trimming and optional compaction.
- Memory/context preloading before each turn.
- Approval-aware execution rules.
- Clear finalization rules: either return an answer or fail when the loop exceeds policy.

The parts we should not copy directly:

- In-process ownership of all agent state.
- Agent-local session durability.
- Tool/runtime/provider coupling inside one binary.
- Channel loop behavior entangled with agent reasoning.

ZeroClaw's loop is a good *turn engine*. It is a bad fit as Philotic's *system boundary*.

## Current Philotic State

Today Philotic already has some of the right primitives:

- Guests register over UDS and subscribe by role in [ipc.rs](/Users/jaredlikes/code/philotic-stack/crates/ansible/src/service/ipc.rs).
- The graph persists guest materialization metadata and memory apartments in [sqlite_storage.rs](/Users/jaredlikes/code/philotic-stack/crates/ansible-mesh-core/src/sqlite_storage.rs).
- `membrane`, `agent-core`, and `model-router` already exchange routed tasks over IPC.
- `agent-core` is still a stub and has no first-class session lifecycle.

The main missing concept is a durable `session` object that survives guest restarts and can drive turn replay, resumption, multi-channel continuity, and cross-component coordination.

## Proposed Session Model

Introduce session as a hotel-owned, component-general resource in the Context Graph.

This is not just a chat transcript container.

A Philotic session should be a durable coordination envelope that any guest can participate in:

- `membrane` can bind an external user/channel to it
- `agent-core` can own the cognitive state within it
- `model-router` can contribute inference work to it
- approval or streaming guests can observe or extend it
- WebRTC or long-running workflow guests can attach to it later

The agent still owns conversation intelligence. The hotel owns the shared runtime envelope.

### New graph entities

- `sessions`
  - `session_id`
  - `session_kind` (`conversation`, `approval`, `stream`, `workflow`)
  - `primary_agent_id`
  - `channel_kind`
  - `channel_session_key`
  - `status` (`active`, `idle`, `blocked`, `closed`)
  - `lease_owner_component_id`
  - `lease_expires_at`
  - `summary_json`
  - `created_at`, `updated_at`
- `session_participants`
  - `session_id`
  - `component_id`
  - `role` (`owner`, `gateway`, `model`, `observer`, `approver`)
  - `joined_at`, `last_seen_at`
- `session_turns`
  - `turn_id`
  - `session_id`
  - `request_event_id`
  - `user_message_json`
  - `status` (`queued`, `running`, `waiting_tool`, `waiting_approval`, `completed`, `failed`)
  - `response_json`
  - `error_json`
  - `started_at`, `completed_at`
- `session_artifacts`
  - optional per-turn tool transcripts, model reasoning summaries, attachments/blob refs

### Why generalized and hotel-owned

- The hotel can recover after guest crash.
- Edge guests can map external identities to the same session deterministically.
- Multiple guests can coordinate around the same session model without inventing distributed amnesia.
- Replay/debugging belongs beside the durable event ledger, not in one lucky process's RAM.
- A session can outlive one specific agent turn or one specific component implementation.

## Agent State and Apartment Sync

The generalized session does **not** mean the hotel owns all agent cognition.

The split should be:

- generalized session state in the hotel
  - identifiers, bindings, lifecycle, leases, participants, event/timeline metadata
- cognitive conversation state in `agent-core`
  - working history, intermediate tool loop state, compaction logic, prompt assembly
- durable memory checkpoints in the Context Graph
  - written back via `SyncApartment`

### Canonical ownership rule

Session state should have exactly one canonical home:

- canonical session state lives in the hotel/context graph
- apartment checkpoints are derived recovery projections for agent hot-path restore
- `agent-core` may cache and project session state locally, but it should not become a second authority

This avoids split-brain session ownership while still preserving fast local recovery.

### What `SyncApartment` should mean

`SyncApartment` should be treated as a **checkpoint/snapshot write**, not as the only representation of the session.

That means:

- `agent-core` can keep rich local working state during a turn
- it periodically writes a compacted or structured snapshot home
- the hotel does not need every token of transient thinking to be persisted through `SyncApartment`

### Delta strategy

Do not overload `SyncApartment` with patch semantics first.

Recommended model:

- use small session/turn events for lightweight deltas and progress
- use `SyncApartment` for periodic state checkpoints
- keep apartment contents structured so checkpoints are not giant opaque blobs

In practice:

- deltas
  - assistant drafted text
  - tool calls requested
  - tool results returned
  - approval wait entered/resolved
- snapshots
  - short-term memory
  - conversation summary
  - outstanding commitments/tasks
  - compact recent-context window if needed

This is simpler and safer than trying to make `SyncApartment` a JSON patch protocol on day one.

### Recommended compact session contents

The first compact session payload should converge on these fields:

- `session_identity`
  - `session_id`, channel/session key, primary agent, participants
- `live_status`
  - turn status, approval state, active waits, active delegated tasks
- `active_role`
  - current role/incarnation and role-local posture refs
- `working_state`
  - active goal, active constraints, current hypotheses, pending tool/result state
- `recent_local_window`
  - bounded recent turn/control history for coherence
- `session_facts`
  - compact still-live facts for this session only

Everything else should justify itself before joining the payload instead of squatting there because transcripts are cheap and context windows are apparently free in fairy tales.

### `CompactSessionEnvelope`

The first explicit compaction target should be a named structured payload.

Recommended first shape:

- `session_identity`
  - `session_id`
  - `session_kind`
  - `primary_agent_id`
  - `channel_kind`
  - `channel_session_key`
  - `participants`
- `live_status`
  - `session_status`
  - `current_turn_status`
  - `approval_state`
  - `active_waits`
  - `active_delegations`
- `active_role`
  - `role_name`
  - `active_incarnation_id`
  - `role_addendum_ref`
  - `toolset_profile_ref`
  - `skillset_profile_ref`
- `working_state`
  - `active_goal`
  - `active_constraints`
  - `current_hypotheses`
  - `pending_tool_call`
  - `pending_tool_results`
  - `pending_return_contract`
- `recent_local_window`
  - bounded ordered turn/control entries still needed for local coherence
- `session_facts`
  - compact still-live facts and commitments for this session only
- `checkpoint_metadata`
  - `captured_at`
  - `captured_by`
  - `compaction_version`
  - `source_turn_ids`

### Envelope rules

- `CompactSessionEnvelope` is session-local, not a memory artifact
- it should preserve live coordination truth, not durable continuity
- it should be cheap enough to checkpoint and restore without becoming a second transcript archive
- fields should prefer structured state over long freeform blobs

### Compaction policy implications

When compacting into `CompactSessionEnvelope`:

- summarize only what is still live
- convert resolved activity into event history or memory write-back rather than keeping it hot in session
- preserve the minimum bounded local window needed for coherent continuation
- keep role-local working state explicit so posture switches and rematerialization can restore without guessing

## Session Binding Rules

External transports should resolve a stable session key before creating work.

Examples:

- Telegram DM: `telegram:<chat_id>:agent-jane-01`
- Telegram group thread: `telegram:<chat_id>:thread:<thread_id>:agent-jane-01`
- CLI ephemeral: `cli:<tty-or-client-id>:agent-jane-01`
- WebRTC: `webrtc:<remote-peer-id>:agent-jane-01`

`membrane` should resolve or create a session through the hotel and receive:

- `session_id`
- current status
- primary `agent_id`
- whether a turn is already running

That keeps channel guests stateless apart from transport cursors.

## Proposed Turn Loop in Philotic

### 1. Intake

`membrane` receives a user message and asks `ansible` to:

- resolve or create session
- append an inbound event / queued turn
- route work to the assigned agent guest

### 2. Lease

Before processing a turn, `agent-core` acquires a session lease from `ansible`.

Rules:

- only one active turn per session by default
- leases are renewable heartbeat-style
- expired leases let the hotel requeue work

This prevents duplicate replies when guests restart or supervisors respawn.

### 3. Context Load

`agent-core` asks the hotel for a `SessionSnapshot`:

- system/persona prompt inputs
- session summary
- recent turn transcript window
- relevant memory apartments
- pending approvals
- attached artifacts/blob refs
- participant/session metadata when useful

This replaces ZeroClaw's purely agent-local `history` vector with a rebuildable working set.

### 4. Run the cognitive loop

Inside `agent-core`, keep a loop very close to legacy ZeroClaw:

1. Build provider messages from the snapshot plus current working turn history.
2. Invoke model.
3. If no tool calls are returned, finalize answer.
4. If tool calls are returned:
   - validate against policy
   - request approval if required
   - execute tools sequentially or in parallel depending on policy
   - append tool results to working history
   - continue until final answer or iteration cap

The difference is that each state transition is visible through session events, while apartment sync remains checkpoint-oriented instead of being spammed with whole-history rewrites every time the model sneezes.

### 5. Checkpoint after each step

After each model/tool phase, `agent-core` emits turn/session progress:

- current loop iteration
- tool calls requested
- tool results
- partial assistant text
- approval waits
- compacted history delta when checkpointing

Checkpointing cadence should be coarse enough to stay cheap, but frequent enough for crash recovery:

- after initial context load
- after each assistant tool-call decision
- after each batch of tool results
- on explicit apartment checkpoint
- on final answer
- on failure / cancellation

### 6. Finalize

When the loop completes, `ansible`:

- marks the turn completed
- updates session summary / recent transcript cache
- accepts memory apartment checkpoint updates
- emits outbound event for `membrane`

Then `membrane` sends the transport reply.

## Recommended Communication Shape

Keep the communication plane general.

Near term, avoid adding a giant parade of session-specific IPC commands if the existing generic plane is enough.

Recommended approach:

- session and turn envelopes are modeled in the graph
- guest-to-hotel communication continues to use general IPC task/event operations
- payloads carry `session_id`, `turn_id`, `component_id`, `kind`, and structured JSON
- `SyncApartment` remains the explicit checkpoint path for apartment state

This preserves a general communication plane while still making session a first-class concept in storage and routing.

## Recommended `agent-core` Internal Structure

Implement `agent-core` as a small runtime with explicit loop stages.

### Core structs

- `SessionSnapshot`
- `WorkingTurn`
- `LoopPolicy`
- `ToolPlan`
- `TurnCheckpoint`
- `TurnOutcome`

### Core modules

- `session.rs`
  - session envelope + snapshot helpers
- `context.rs`
  - transform `SessionSnapshot` into provider prompt input
- `loop.rs`
  - bounded reasoning/tool loop
- `tools.rs`
  - tool execution orchestration
- `compaction.rs`
  - summarize old turns into apartment/session checkpoints
- `approval.rs`
  - wait/resume semantics for supervised tools

### Important boundary

Do not let `agent-core` become responsible for:

- owning SQLite directly
- talking to Telegram/Discord directly
- inventing durable IDs locally
- managing remote model provider routing policy globally

That way lies monolith nostalgia.

## History and Memory Strategy

ZeroClaw used a mutable in-memory transcript and optional compaction. In Philotic, split state into three layers:

- session summary
  - generalized shared session facts and state
- recent turns window
  - exact last N user/assistant/tool exchanges
- memory apartments
  - agent-owned semantic/episodic/long-term stores already aligned with Philotic's graph model

Recommended read path for each turn:

1. session summary
2. recent turns
3. memory apartment recall
4. channel/tool-specific context

Recommended write path after each completed turn:

1. append exact turn/session event record
2. update short-term apartment checkpoint
3. periodically compact older turns into summary/checkpoint
4. optionally promote durable facts into semantic/episodic apartments

This gives us ZeroClaw-quality continuity without pretending RAM is a database.

## Approval Model

Keep approval enforcement hotel-visible even if execution stays guest-local.

Why:

- approvals are session state
- waiting for approval is a resumable turn status
- channels may need to surface approval prompts

Design:

- `agent-core` detects approval-required tools before execution
- `ansible` records `waiting_approval` on the turn
- `membrane` or another UX guest delivers the approval request
- approval response is written back through hotel IPC
- `agent-core` resumes from checkpoint

This preserves ZeroClaw's useful approval semantics while fitting Philotic's multi-guest reality.

## Failure and Recovery Rules

The session loop should be restart-safe.

### Recoverable failures

- agent guest crashes mid-turn
- hotel restarts after turn creation but before completion
- tool execution timeout
- approval wait exceeds SLA

### Recovery behavior

- if lease expired and turn is `running`, requeue from latest checkpoint
- if no checkpoint exists beyond intake, rerun turn from original input
- if tool side effects are non-idempotent, mark turn `failed_requires_operator`
- if final response was produced but not delivered, let `membrane` retry outbound delivery idempotently

This argues for storing a generalized session/turn state machine explicitly instead of treating conversation as one opaque blob.

## Work Item Split

This work should be split into two independent but coordinated work items.

### WI 1: Session Management

Scope:

- generalized session model in the graph
- session participants, lifecycle, leases, and timeline events
- transport-to-session binding in `membrane`
- recovery semantics and approval/session visibility
- generic event envelopes carrying `session_id` / `turn_id`

Non-goals:

- detailed agent prompt logic
- tool-call loop behavior
- memory compaction policy details

Deliverables:

- graph schema/entities for sessions
- session routing and persistence
- session snapshots for consumers
- tests for recovery, lease behavior, and participant binding

### WI 2: Agent Logic

Scope:

- ZeroClaw-style bounded turn loop in `agent-core`
- context assembly from session snapshot + apartments
- tool-call execution and approval-aware control flow
- checkpoint policy for `SyncApartment`
- compaction from working state into snapshots

Non-goals:

- transport binding rules
- generalized session lifecycle rules outside the agent

Deliverables:

- `agent-core` loop/runtime modules
- local working session state
- apartment checkpoint writes
- tests for loop progression, compaction, and recovery from snapshot

## Implementation Plan

### Phase 1: Session substrate

- Add graph entities and adapter methods for sessions, participants, and turns
- Add `membrane` session resolution before emitting agent work
- Keep the IPC plane general by carrying session identifiers in existing task/event payloads

### Phase 2: Single-turn durable loop

- Refactor `agent-core` into `SessionSnapshot -> WorkingTurn -> TurnOutcome`
- Implement bounded tool-call loop with session events plus apartment checkpoints
- Persist final answer and recent transcript

### Phase 3: Recovery and approval

- Add session leases and heartbeat renewal
- Add resumable approval waits
- Add crash recovery on supervisor restart

### Phase 4: Compaction and quality

- Add recent-turn window + summary compaction
- Add session metrics
- Add replay/debug tooling

## Recommended MVP Scope

For the first working version, do not build everything.

Build only:

- one active turn per session
- deterministic channel-to-session mapping
- durable turn records
- lease acquisition
- legacy-style bounded tool loop in `agent-core`
- final response delivery back through `membrane`

Defer for later:

- multi-agent collaboration on one session
- speculative parallel subturns
- full timeline query UI
- fancy artifact graphing
- cross-node session migration

## Concrete Recommendation

The cleanest Philotic implementation is:

- `ansible` becomes the source of truth for generalized session and turn lifecycle
- `agent-core` ports the ZeroClaw cognitive loop as a resumable worker inside that session
- `membrane` becomes a transport adapter that binds users to sessions and renders replies/approvals
- memory apartments remain the agent checkpoint path back to the graph
- `SyncApartment` stays snapshot-oriented while smaller session events carry deltas/progress

If we follow that split, we get:

- ZeroClaw's battle-tested turn behavior
- Philotic's crash recovery and materialization model
- a general session envelope usable by more than just chat agents
- a path to multi-channel and multi-node sessions later

If we do not follow that split, we will accidentally rebuild a monolith inside `agent-core`, only this time with more IPC and less honesty.
