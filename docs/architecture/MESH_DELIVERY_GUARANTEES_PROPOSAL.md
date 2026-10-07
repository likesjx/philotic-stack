---
title: Mesh Delivery Guarantees — Make the Mesh Loud Before Making It Bigger
doc_type: proposal
domain: mesh-placement
status: in-progress
last_updated: 2026-10-07
tags:
- mesh
- delivery
- dead-letter
- heal-queue
- observability
- reliability
related_docs:
- FLEET_SUPERVISION_PROPOSAL.md
- SUBSTRATE_HARDENING_PROPOSAL.md
- MESH_SYNC_AND_TRANSPORT_BOUNDARIES_PROPOSAL.md
- AGENT_FRONTDOOR_PROPOSAL.md
- INTER_HOTEL_ROUTING_PROPOSAL.md
proposal_id: mesh-delivery-guarantees
implements:
- aiua
- ansible-mesh-core
- philotic-primitives-mesh
implemented_by: []
active_seams:
- mesh-loud-inbound
- mesh-gossip-budget
- mesh-doctor-checks
- mesh-ack-what-you-delivered
- mesh-task-expiry
- mesh-untargeted-envelopes
- life-observe-write-confirmation
---

# Mesh Delivery Guarantees — Make the Mesh Loud Before Making It Bigger

Origin: Philotic Stack Atlas (2026-09-30), next seam #1. Facts re-checked
against `origin/develop` @ `f170cb88` on 2026-10-05.

## Problem

Cross-hotel work is store-and-forward. The sender appends to `mesh_events`,
the dispatcher pushes batches over TCP, the receiver delivers or parks, then
acks, and the sender advances its cursor. That loop works, but several of its
failure paths are **silent**, and the receiver acks things it never delivered.

| Defect | Status 10-05 | What happens | Where |
|---|---|---|---|
| DEF-182 | open, high | A batch that fails `Vec<EventEnvelope>` deserialization is dropped with no `else`. Unknown `EventKind`/`EventPayload` variants have no `#[serde(other)]`. Cross-hotel "no subscriber … task dropped" only `warn!`s and is still acked. Gossip parse failures are silent. | `aiua/src/service/mesh_runtime.rs:430-432`, `:407`, `:418`, `:712`; `role_materialization.rs:1264-1269`, `:1105-1114`; `ansible-mesh-core/src/beacon.rs:241,372,393,439`; `philotic-primitives-mesh/src/event.rs:51,109` |
| DEF-184 | open (logged once per kind) | `target_node_id: None` envelopes are skipped by the ledger writer. The cron broadcast emitters behind them never reach anyone. | `aiua/src/main.rs:8044-8078`; `cron_ticker.rs:691,728,764`; dead handlers at `mesh_runtime.rs:473-494` |
| DEF-059 | open | Cross-hotel `life.observe` is acked when it lands in the inbox, not when it completes. A runner restart loses it while the operator already heard "captured". | `mesh_runtime.rs:640`; `main.rs:8143`; parked tasks in memory only (`ipc.rs:1311`, TTL 720 s) |
| DEF-192 | open, high | mac-jane's `HotelStateSync` is 7,873 B against a 9,000 B budget and a 9,216 B datagram limit. Over budget it only warns and ships anyway. The roster includes **inactive** guests. | `ansible-mesh-core/src/heartbeat.rs:28,177-245`; `main.rs:8309-8363` (`list_guests(.., false)`) |
| DEF-211 | partly fixed (#601) | Inside the 15 s peer TTL, a frontdoor `execute_tool` is queued with `created_at: 0, expires_at: None`. It can execute after the caller already saw a 30 s timeout. | `ipc.rs:6807-6822`; `membrane-mcp/src/server.rs:34` |
| DEF-185 | nonce part **fixed in code** | The bounded `NonceTracker` replaced the SQLite nonce stores. Not yet verified live. | `ansible-mesh-core/src/authz.rs:12,105-223` |

**Cross-cutting gaps:**
- There is no dead-letter concept anywhere in the repo.
- `DeliveryClaims` (`ipc.rs:1348-1389`) is in-memory only.
- `deliver_event_envelope_or_park` returns a `bool` that callers discard (`mesh_runtime.rs:450-460`).
- The inbound ack's `max_seq` is computed **before** filtering (`:434`).
- The dispatcher never increments `attempt` and never checks `expires_at` (`mesh_dispatcher.rs:261-356`).
- `phil doctor` has no mesh delivery checks.

## Goal

Every envelope that enters the mesh ends in exactly one **observable**
terminal state: delivered, parked, dead-lettered with a reason, or expired
with an error reply to the originator. Nothing is acked that wasn't one of
those. An operator can see backlog, dead letters and gossip budget from
`phil doctor` and the heal queue.

### Non-goals

- Exactly-once semantics or a durable claim store. Idempotent receivers plus
  at-least-once delivery is the target.
- DEF-187 (HMAC field coverage), which needs a key-id and dual-accept flag
  day. It belongs to `PERIMETER_ENFORCEMENT_PROPOSAL.md`.
- DEF-192 slice B (a paged roster protocol) and the DEF-185 hot-path
  performance work. Both are deferred until after this proposal.

## Design principles

1. **Loud first, correct second.** The first slices change only logging and
   heal-queue pushes, need no wire changes, and are safe to roll out
   hotel by hotel.
2. **Receiver-side changes before sender-side contract changes.** The ack
   semantics change only on the receiver, which is safe in a mixed-version
   fleet.
3. **Use the heal queue as the alarm bus.** Use
   `HealQueueStorage::push_classified(guest_id, raw_text, severity, pattern_tag)`
   (`ansible-mesh-core/src/heal_queue.rs:524`), which already collapses floods
   over 60 s. Register each new tag in `heal_action_for_pattern_tag` (`:443`)
   under `escalate`. Follow the model in `report_unserved_local_role`
   (`ipc.rs:4207-4245`).
4. **Use `TerminalErrorCode` as the dead-letter reason taxonomy.** It is
   defined but unused (`philotic-primitives-mesh/src/event.rs:126`).

## Slices

### L1 — Loud inbound (S, no wire change)

1. Add `heal_queue: Option<Arc<dyn HealQueueStorage>>` to `MeshRuntimeContext`
   (`mesh_runtime.rs:66-100`), wired from `heal_queue_arc` (`main.rs:8167`).
2. In the inbound loop (`mesh_runtime.rs:428-707`):
   - parse the payload as `Vec<serde_json::Value>`, then each element as
     `EventEnvelope`;
   - on failure, `warn!` with `src_node`, raw `kind` and `event_id` when present;
   - push tag `mesh_event_undecodable`.
   Extract the parse step into a pure helper,
   `fn decode_inbound_batch(&[u8]) -> (Vec<EventEnvelope>, Vec<UndecodableEvent>)`,
   so it can be unit-tested.
3. Add throttled `else` branches, at most once per minute per `(peer, kind)`,
   with tag `mesh_gossip_undecodable`, to these gossip parses:
   - `mesh_runtime.rs:407`, `:418`, `:712`
   - `beacon.rs:241`, `:372`, `:393`, `:439`
4. Change `deliver_event_envelope_or_park` (`role_materialization.rs:1018`) to
   return an enum instead of discarding a `bool`:
   ```rust
   enum DeliveryOutcome { Delivered, Parked, Rescued, AlreadyClaimed, NotMine, Dropped(DropReason) }
   ```
   On `Dropped`, which covers "no subscriber for role" (`:1264`) and a failed
   `memory.write_forward` (`:1105`):
   - push tag `mesh_task_dropped`;
   - emit an error `TaskResult` / `datasource_response` to the envelope's
     `final_reply_*` route, mirroring `report_unserved_local_role`.
5. Add the new tags to `heal_action_for_pattern_tag` and its tests
   (`heal_actions_for_delivery_hardening_patterns`, `heal_action_for_pattern_tag_matrix`).

**Tests:**
- `decode_inbound_batch` handles a good batch, one bad element, and a whole
  non-JSON payload. Put these next to `mod sender_binding_tests`
  (`mesh_runtime.rs:891`).
- The `deliver_event_envelope_or_park` outcome cases extend the existing tests
  at `role_materialization.rs:4584` and `:4742`.
- The heal-tag matrix.

**Live check:**
- Run the SUBSTRATE_HARDENING S4 chaos smoke with an injected malformed batch.
  Use a debug-only `PHILOTIC_MESH_CHAOS_GARBLE=1` on the sender, or
  `scripts/chaos-smoke.sh mesh-garble`.
- A `mesh_event_undecodable` row must appear on the receiver within one dispatch tick.

**Rollout:** any order.

**Status (2026-10-07): implemented** on `codex/mesh-l1-loud-inbound`
(test-green; live chaos check pending a deploy).
- `decode_inbound_batch` + `report_undecodable_event` in `mesh_runtime.rs`.
  The ack formula is unchanged (max seq of the decoded events); L4 owns
  ack-what-you-delivered.
- Gossip alarms use `ansible_mesh_core::mesh_alarm::GossipParseAlarm`, at
  `mesh_runtime` heartbeat / capability sync / event ack and at `beacon.rs`
  catalog sync / hotel state. Beacon's own heartbeat and capability-sync
  parses are not alarmed separately: beacon forwards those messages to the
  runtime inbox unconditionally, where the same parse is alarmed.
- `DeliveryOutcome` / `DropReason` in `role_materialization.rs`. Drops also
  cover a refused operator-surface handoff (heal only, no reply) and a
  failed `memory.write_forward` (heal only: fire-and-forget, no waiting turn).
- `dropped_task_error_reply` picks the reply by the dropped action:
  - `execute_tool` → `tool_result`
  - a model request → `model_response`
  - a dotted capability → `datasource_response`
  - anything else → `send_reply` to `final_reply_*`

  `error` is always a `TaskErrorPayload` object; a bare string would fail
  philote's parse. A dropped reply is never answered (loop guard).
- Reply routing rules (no EmitTask in the path, so its safeguards are
  re-applied by hand):
  - a request reply goes out only when the return route names a caller guest;
  - a `send_reply` carries `reply_owner_agent_id` (DEF-166) from the dropped
    task's `agent_id`, or a pinned `final_reply_guest_id`, else nothing is sent;
  - nothing is sent to this hotel itself.
- Ack consequence until L4:
  - an undecodable element below the batch's highest decoded seq is acked,
    so it is lost loudly;
  - one above it is re-sent every tick (its alarm is throttled per event id).
- **Live check needs a hook that does not exist yet:** neither
  `PHILOTIC_MESH_CHAOS_GARBLE` nor `scripts/chaos-smoke.sh mesh-garble`
  exists. Add one (release-safe, operator-gated) before claiming
  watched-live.

### L2 — Loud gossip budget (S, independent of L1)

1. Gossip **active guests only**: change `list_guests(&hotel_name, false)` at
   `main.rs:8310` to `true`. First confirm that no receiver relies on
   `active:false` entries (grep `HotelStateSyncGuest.active` consumers in
   `registry.rs` and the operator roster views).
2. Add a budget alarm in `chunk_hotel_state_payloads` (`heartbeat.rs:177`):
   - over 75% of `MAX_HOTEL_STATE_WIRE_BYTES` (6,750 B) → tag `hotel_state_budget`, severity warning;
   - over 100% → severity critical.
3. When `emit_hotel_state_sync` fails (EMSGSIZE or any send error), push
   tag `hotel_state_send_failed` instead of only `warn!`-ing
   (`mesh_runtime.rs:147-165`).

**Tests:**
- Add `hotel_state_budget_alarm_threshold` beside
  `hotel_state_single_datagram_when_small` (`heartbeat.rs:626`).
- Add a test that inactive guests are excluded from the snapshot builder.

**Live check:**
- mac-jane's measured roster size drops. Log the wire length once per change.
- No `hotel_state_budget` row appears at steady state.

### L3 — Mesh doctor checks (S, after L1 and L2)

Add the following to `crates/philotic-web/src/doctor.rs`, which reads the
hotel SQLite directly (the pattern of `mesh.orphan-hotel-node`, `:2536`):

- `mesh.ledger-backlog`: per `target_node_id`, the row count and the age of
  the oldest row in `mesh_events`. Warn over 5 min, fail over 30 min.
- `mesh.hotel-state-budget`: the last-sent roster size as a fraction of the
  budget. The sender must persist `hotel_state.last_wire_bytes` via `SetConfig`.
- `mesh.dead-letters`: count and newest reason. Ships as "n/a" until L4.
- `mesh.nonce-store`: confirms the legacy `nonces.db` and `mesh_nonces` are
  gone. This is the DEF-185 live proof.

**Live check:** run `phil doctor` on all three hotels and attach the output to
the slice PR.

### L4 — Ack what you delivered, plus dead letters (M, depends on L1)

1. Extract the ledger writer loop body (`main.rs:8033-8155`) into
   `fn apply_ledger_command(storage, cmd)` so it can be unit-tested.
2. Add a dead-letter table to the hotel DB, with a TTL vacuum (7 days, same as
   the heal queue) and an index on `received_at`:
   ```sql
   mesh_dead_letters(event_id TEXT PRIMARY KEY, source_node TEXT, kind_raw TEXT,
                     reason TEXT, raw_json TEXT, received_at INTEGER)
   ```
3. Inbound, per event:
   - outcome `Delivered | Parked | Rescued | AlreadyClaimed | NotMine` →
     eligible for ack;
   - outcome `Dropped(reason)` or undecodable → write a dead letter, which is
     then also eligible for ack;
   - compute `acked_seq` as the highest contiguous eligible seq. Acks are
     cumulative, so dead-lettering is what lets the cursor move without losing
     data.
4. Stop committing events that failed the sender check, or are addressed to
   another node, into `CommitInboundBatch` (`mesh_runtime.rs:441-448`, `:634`).
5. Add `#[serde(other)] Unknown` to `EventKind`. Stop
   `sqlite_storage.rs:226` from turning unknown stored kinds into `TaskInvoke`:
   dead-letter them on the sender instead.
6. Add `phil mesh dead-letters [--since] [--replay <event_id>]`. Replay
   re-appends the raw envelope locally, for operator-driven recovery.

**Tests:**
- Unit tests for `apply_ledger_command` covering `ProcessAck` deletion and
  `CommitInboundBatch` local-only deletion.
- The ack computation with a dead letter in the middle of a batch.
- `serde(other)` round trip, in a new `#[cfg(test)]` module in
  `philotic-primitives-mesh/src/event.rs`.
- `phil mesh dead-letters` listing and replay.

**Mixed version:**
- Receiver-only, so it is safe to roll out.
- An old sender still sends cumulative acks.
- An old receiver still drops unknown kinds. Senders must not emit new kinds
  until a heartbeat feature check exists (see L5 step 1).

### L5 — Task expiry and attempt counting (M, depends on L4)

1. Add `features: Vec<String>` to `NodeCapabilities`, next to `build_version`
   (`ansible-mesh-core/src/lib.rs:106`).
   - Advertise `"mesh.expiry"` and `"mesh.dead_letter"`.
   - Senders consult this list before relying on a peer's behavior. This
     follows the `supports_continuity` pattern.
2. Stamp `created_at` and `expires_at` (**milliseconds**, as the `event.rs`
   docs require) on every `EmitTask` envelope (`ipc.rs:6807-6822`).
   - Default TTL for `execute_tool`: 25 s, under the frontdoor's 30 s
     `DISPATCH_TIMEOUT`. This closes the rest of DEF-211.
   - Default for other kinds: none.
3. Change the dispatcher (`mesh_dispatcher.rs:261-356`) to:
   - increment `attempt` per send;
   - skip expired events, dead-letter them locally with
     `TerminalErrorCode::Expired`, and send an error `TaskResult` to the local
     originator;
   - stop truncating `seq` with `as u32` at `:323` (it fails past 2^32).
     Carry a u64; the receiver accepts both.
4. The receiver refuses expired envelopes as defense in depth: dead-letter
   them and ack.

**Tests:**
- Dispatcher tests (`mesh_dispatcher.rs:358`) for expired events and attempt
  increments.
- An `ipc.rs` test asserting `expires_at` is set for `execute_tool`, next to
  the `TARGET_NODE_UNREACHABLE` tests (`:24660`, `:24774`).

**Live check (with the frontdoor):**
1. Put mac-jane to sleep and call `graph_status` via `/agent/mcp`.
2. Expect an error within at most 25 s, not a 30 s timeout.
3. Wake the Mac and confirm the call does **not** execute late. Nothing should
   appear in intel-graph's session log.

### L6 — DEF-184 untargeted envelopes (S or M, operator decision)

- **Option (a), recommended (S):** delete the dead cron broadcast emitters
  (`cron_ticker.rs:691`, `:728`, `:764`) and their unreachable handlers
  (`mesh_runtime.rs:473-494`). Cron stays hotel-local. The duplicated builders
  at `ipc.rs:12,871-12,939` go too.
- **Option (b) (M):** explicit per-peer fan-out, as in
  `command_manifest.rs:133`, behind a per-job `replicate: bool`. This carries
  DEF-166 duplicate-brief risk and turns on paths that have never run live.
- **Either way:** until the option ships, push tag `ledger_untargeted_envelope`
  once per kind in the ledger writer.

**Status (2026-10-07): option (a) implemented** on
`codex/mesh-l6-delete-cron-broadcast` (operator approved the recommended
option). The following are deleted:
- the `CronFired` and `CronJobSync` emitters and `startup_sync` in
  `cron_ticker.rs`;
- the duplicate builders `broadcast_cron_sync_{upsert,remove}` in
  `ipc/mod.rs`, plus their four call sites in the cron handlers (which also
  lose their now-unused `local_node_id` / `dispatcher_tx` parameters);
- the `mesh_runtime` dispatch arms;
- the `main.rs` handlers and `strip_replicated_cron_policy`.

Kept:
- the `EventKind` variants, for wire compatibility;
- the per-hotel fire-time guards (`enabled_locally`, own-job-id checks),
  because a job record can still arrive in a copied or restored DB;
- the ledger's once-per-kind untargeted-envelope warning, which still covers
  any other emitter.

Behavior is unchanged, since the ledger already dropped every one of these
envelopes. Closes IPC_DISPATCH_SPLIT P-d.

### L7 — `life.observe` write confirmation (M, independent)

Redelivery is safe:
- observe writes are `MERGE` on a stable id (`data-memorygraphrag/src/cypher.rs:436`);
- re-observing the same id bypasses the duplicate guard (`provider.rs:1235-1247`);
- autocapture uses `stable_capture_node_id` (`philote/src/life_capture.rs:342`).

1. The philote stops reporting "captured" until the runner's response
   arrives. If the result times out or errors, the tool result says
   *not confirmed*, and the model and the operator see that.
2. After a not-confirmed result, retry once, idempotently, with the same
   `observation_id` and node id.
3. Push tag `life_observe_unconfirmed` if the retry also fails.
4. Make the runner stop claiming new work on SIGTERM. Add a drain to
   `crates/datasource/src/runtime.rs`, which today has none.

**Tests:** extend the `life_capture.rs` tests (`:1069+`) and the philote
`life_observe_parse_failed` heal tests (`runtime.rs:10553`, `:11204`).

**Later:** ack-on-complete for datasource `TaskInvoke`. That is size L,
needs both ends plus the `features` flag from L5, and is tracked as
FLEET_SUPERVISION S2.

## Ordering

```
L1 ─┬─► L3 ─► L4 ─► L5
L2 ─┘           │
L6 (decision) ──┘ (any time)
L7 (any time)
```

L1, L2, L6(a) and L7 can run in parallel worktrees. L1 and L4 both touch
`mesh_runtime.rs` and `role_materialization.rs`, so run them sequentially.
**Coordinate with `IPC_DISPATCH_SPLIT_PROPOSAL.md`:** L5 touches the EmitTask
arm. Land L5 before the `emit` family extraction, or after it, but never
while that extraction is in flight.

## Verification ladder

| Slice | Test-green | Smoke | Watched-live |
|---|---|---|---|
| L1 | unit + heal matrix | chaos garble on mac↔vps | one real undecodable event observed and classified |
| L2 | budget tests | roster size logged | mac-jane under 75% for 7 days |
| L3 | doctor unit tests | `phil doctor` on 3 hotels | — |
| L4 | ledger + ack tests | dead-letter + replay drill | zero silent drops for 7 days (dead letters all explained) |
| L5 | dispatcher tests | Mac-asleep frontdoor drill | DEF-211 closed |
| L7 | capture tests | runner-restart drill | DEF-059 closed |

## Risks

- **Wider acks.** If a dead-letter write fails, the receiver must **not** ack.
  Treat a dead-letter write failure as "not eligible", so the batch is
  redelivered.
- **Heal-queue noise.** A peer stuck on an old version could flood
  `mesh_event_undecodable`. The 60 s flood collapse plus the per-(peer, kind)
  throttle bound it, and the H0 stale sweep (#596) closes repeats.
- **Inactive-guest roster change (L2).** The operator roster view
  (`GetMeshRoster`) may show fewer remote guests. That is intended; document it
  in the PR.

## Definition of done

- DEF-182, DEF-184, DEF-192 (slice A), DEF-211 and DEF-059 are closed, and
  DEF-185 is verified live.
- `phil doctor` reports mesh backlog, dead letters and roster budget on all
  three hotels.
- A garbled-batch chaos drill and a Mac-asleep frontdoor drill are both
  recorded as watched-live-green in the Intel Graph.

## Operator decisions needed

1. DEF-184: option (a), delete the cron broadcast paths, or option (b),
   per-peer replication? **Approved 2026-10-07: (a).**
2. Dead-letter retention: 7 days is proposed.
3. Should `phil mesh dead-letters --replay` be operator-only (an admin
   ceremony)?
