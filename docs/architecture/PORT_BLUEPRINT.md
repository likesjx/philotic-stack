# Philotic Web Port Blueprint

Status: Proposed  
Date: 2026-03-05  
Owner: ansible hotel core (Rust)

## 1. Intent

Define exactly what must be ported from the current OpenClaw Ansible plugin model into the Rust-based Philotic Stack, with Ansible as the hotel core, IPC for intra-hotel routing, and mesh transport for inter-hotel communication.

## 2. Architecture Direction

1. Each hotel runs one authoritative `ansible` daemon.
2. Inside a hotel, all components communicate through local IPC.
3. Between hotels, all communication is event-based over the mesh.
4. Shared coordination metadata remains separate from data-plane payload flow.
5. Delivery target is at-least-once transport + idempotent processing.

## 3. Port Scope (What Must Move)

### 3.1 Runtime-critical domains (Rust-owned)

1. Delivery and dispatch engine.
2. Task lifecycle state machine with transition invariants.
3. Message and task persistence (append-only event log + cursor store).
4. Retry scheduler, dead-letter transitions, and dedupe ledger.
5. SLA sweeps and escalation windows.
6. Retention and compaction logic.
7. Auth admission, invite exchange, ticket validation, replay guards.
8. Capability lifecycle gate executor internals.
9. Large Payload blob store and TCP/HTTP chunked file distribution.
10. WebRTC Media Transceiver signaling (SDP bridging).

### 3.2 Control-plane domains (Rust-owned)

1. Node registry and heartbeat.
2. Agent ownership and addressing.
3. Routing policy and capability index.
4. Coordination roles and sweep policy state.

### 3.3 Keep outside core Rust authority initially

1. CLI experience and presentation formatting.
2. Fast-changing operator workflow glue.
3. Any legacy compatibility adapters required during migration.

## 4. Functional Contract Inventory

The port is not complete until these capabilities exist in Rust and pass parity tests.

1. Agent registry: register, disable, enable, rebind, list.
2. Message flow: send, broadcast, read, mark-read, delete (policy-gated), reply.
3. Task flow: create, claim, update, approve, complete, fail.
4. Capability flow: publish, unpublish, health summary, lifecycle evidence.
5. Governance flow: delegation policy set/get/ack, gateway admin policy, distribution policy, backpressure policy.
6. Reliability flow: lock sweep, retention sweep, SLA sweep, dead-letter accounting.
7. Auth flow: node invite/join exchange, external agent token exchange, replay prevention.
8. Observability flow: status, counters, lag metrics, cursor health, incident surfaces.

## 5. Crate Ownership Plan

## 5.1 Existing crates aligned to target roles

1. `crates/ansible`: hotel daemon, orchestration entrypoint.
2. `crates/ansible-mesh-core`: routing, graph, registry, runtime primitives.
3. `crates/philotic-ipc`: IPC transport abstractions.
4. `crates/membrane`: edge gateway/protocol adapters.
5. `crates/agent-core`: local agent runtime integration.
6. `crates/model-router`: model-provider routing service.

## 5.2 New modules to add in this workspace

1. `ansible-mesh-core::event` for canonical envelopes.
2. `ansible-mesh-core::ledger` for dedupe + idempotency records.
3. `ansible-mesh-core::cursor` for per-source acked sequence tracking.
4. `ansible-mesh-core::lifecycle` for task/message transition machines.
5. `ansible-mesh-core::authz` for invite/ticket/nonce validation.
6. `ansible-mesh-core::blob` for TCP/HTTP chunked file distribution and reference hashing.
7. `ansible-mesh-core::webrtc` for SDP signaling bridging and media transceiver guests.
8. `ansible::service` for IPC + admin HTTP surfaces.

## 6. IPC Contract (Intra-hotel)

Transport:

1. Unix Domain Socket on macOS/Linux.
2. Named pipe fallback where UDS unavailable.

Required IPC operations:

1. `publish_message`
2. `create_task`
3. `ack_event`
4. `update_task`
5. `complete_task`
6. `fail_task`
7. `subscribe_inbox`
8. `query_status`
9. `query_timeline`

Response contract:

1. Every response includes `ok`, `code`, `message`, `corr_id`.
2. Error codes are stable and machine-parseable.
3. Mutations are durable before success is returned.

## 7. Mesh Contract (Inter-hotel)

Event envelope fields:

1. `event_id`
2. `seq`
3. `source_node_id`
4. `source_agent_id`
5. `target_agent_id` (optional for broadcast)
6. `kind`
7. `corr_id`
8. `attempt`
9. `created_at`
10. `expires_at` (optional)
11. `payload` or `payload_ref`
12. `trace` (route decision + policy version)

ACK semantics:

1. `accepted`: receiver durably enqueued event.
2. `processed`: receiver reached terminal handling for the event.
3. `failed_terminal`: receiver rejected/failed permanently with reason class.

Cursor semantics:

1. Cursor tracked per `(consumer_node_id, source_node_id)`.
2. `last_seq` must be contiguous.
3. Cursor advances only after durable local write.

## 8. State Machines

### 8.1 Task lifecycle

Valid transitions:

1. `pending -> claimed`
2. `claimed -> in_progress`
3. `in_progress -> completed`
4. `in_progress -> failed`
5. `claimed -> failed`
6. `pending -> failed` (rejected/no_route/expired)

Invariants:

1. No terminal-to-nonterminal transition.
2. Claimer identity immutable after claim unless explicit reassignment event.
3. High-risk tasks require approval artifact before execute/complete.

### 8.2 Delivery lifecycle

1. `attempted`
2. `delivered`
3. `dead_letter`

Invariants:

1. No duplicate local engagement for same `(event_id, target_agent_id)`.
2. Retries increment `attempt` and preserve `corr_id`.
3. Dead-letter records include machine error class.

## 9. Security Model

1. Invite tokens are single-use and short TTL.
2. Exchange endpoint enforces nonce replay protection.
3. Tickets bind to node identity and allowed room/stream scope.
4. External agent auth uses scoped tokens; no gateway-admin grants.
5. All mutating operations are actor-attributed and auditable.

## 10. Migration Plan

### Phase 0: Contract freeze

1. Freeze envelope schemas and error taxonomy.
2. Generate golden fixtures from current behavior.

Exit criteria:

1. Fixtures versioned.
2. Contract tests green.

### Phase 1: Shadow mode

1. Rust computes decisions for messages/tasks in parallel.
2. Existing path remains write-authoritative.
3. Diff logger records mismatches.

Exit criteria:

1. No critical decision drift across soak period.

### Phase 2: Domain cutover

Cutover order:

1. SLA sweep engine.
2. Task lifecycle validator.
3. Delivery dispatcher.
4. Auth exchange path.

Exit criteria:

1. Per-domain rollback flag exists.
2. Soak stable with no P1 regressions.

### Phase 3: Data-plane cutover

1. Move payload traffic fully to mesh event logs.
2. Keep control-plane metadata minimal and payload-free.

Exit criteria:

1. End-to-end task and message flow runs without Yjs payload dependencies.

### Phase 4: Control-plane replacement

1. Replace remaining Yjs coordination dependencies with low-level mesh sync.
2. Retire legacy adapters.

Exit criteria:

1. Legacy dependency removal complete.
2. Rolling upgrade path validated.

## 11. Test and Verification Plan

1. Golden parity tests for command semantics.
2. Property tests for state-machine invariants.
3. Fuzz tests for auth parser and envelope decoding.
4. Chaos tests for duplicate, delayed, and out-of-order events.
5. Soak tests for cursor lag, retry storms, and dead-letter budget.
6. Mixed-version upgrade tests across at least two hotels.

## 12. SLOs and Telemetry

Required metrics:

1. ACK accepted latency p50/p95.
2. ACK processed latency p50/p95.
3. Dead-letter rate by kind.
4. Retry attempts distribution.
5. Cursor lag per source node.
6. Task SLA breach counts by policy class.

Minimum SLO targets (initial):

1. p95 accepted ACK under 2s for healthy mesh links.
2. dead-letter rate under 0.5% for non-expired events.
3. zero duplicate local execution for same `(event_id, target_agent_id)`.

## 13. Execution Backlog Seeds

1. `PORT-BP-001`: define `EventEnvelope` and error code taxonomy.
2. `PORT-BP-002`: implement IPC service in `crates/ansible`.
3. `PORT-BP-003`: implement dedupe ledger + cursor store.
4. `PORT-BP-004`: implement task lifecycle engine with invariant tests.
5. `PORT-BP-005`: implement mesh ACK/processed semantics.
6. `PORT-BP-006`: implement invite exchange with replay guard.
7. `PORT-BP-007`: implement large payload blob extraction + TCP/HTTP chunked fetching index.
8. `PORT-BP-008`: implement WebRTC Ephemeral Transceiver capabilities and SDP mesh signaling.
9. `PORT-BP-009`: shadow-mode parity harness.
10. `PORT-BP-010`: phased cutover flags and rollback drills.

## 14. Definition of Done

This port is done when:

1. Runtime-critical and security-critical domains are Rust-authoritative.
2. Behavior parity is proven against fixtures.
3. Delivery guarantees and SLOs are met in soak.
4. Mixed-version upgrades are safe.
5. Legacy Yjs payload routing is fully retired.
