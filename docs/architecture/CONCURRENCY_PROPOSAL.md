# Concurrency Proposal

## Goal

Eliminate the sequential bottlenecks in the hotel daemon, membrane, and SQLite storage layer that prevent the stack from saturating available CPU/IO concurrency. The Tokio multi-threaded runtime is already in place — this proposal closes the gap between the runtime's capability and how work is actually scheduled onto it.

## Disposition

`proposed — not yet started`

---

## Background

The async runtime (`rt-multi-thread`) uses all CPU cores on every binary. The problems are not in the runtime choice. They are seven specific sequential `for`+`await` loops and two structural issues that serialize naturally parallel work or create latent deadlock risk. Full audit lives in [CONCURRENCY_STRATEGY.md](CONCURRENCY_STRATEGY.md).

---

## Change 1: SQLite WAL Mode + `busy_timeout`

**Files:** `crates/ansible-mesh-core/src/sqlite_storage.rs` — three `Connection::open` call sites (`SqliteEventStorage`, `SqliteCursorStorage`, `SqliteGraphStorage`)

**Problem:** All three storage implementations use a single `Arc<Mutex<Connection>>` for both reads and writes. SQLite's default journal mode serializes all access — a read cannot proceed while a write is in progress, even if the write is queued. Under any concurrent guest activity, reads back up behind the write lock.

**Fix:** Add a one-time PRAGMA batch immediately after each `Connection::open`:

```rust
conn.execute_batch(
    "PRAGMA journal_mode=WAL;
     PRAGMA busy_timeout=5000;
     PRAGMA synchronous=NORMAL;"
)?;
```

WAL (Write-Ahead Logging) allows multiple concurrent readers to proceed independently of the writer. `busy_timeout` replaces `SQLITE_BUSY` errors with a 5-second retry window — preventing spurious errors during legitimate write contention. `synchronous=NORMAL` is safe with WAL and recovers correctly after a crash.

**No API or schema changes required.** The existing `Arc<Mutex<Connection>>` write serialization remains correct and unchanged.

**Scope:** 3 sites, ~3 lines each.

---

## Change 2: Fix `block_on` in Sync `ToolInvoker` Impls

**Files:** `crates/ansible-mesh-core/src/model_manager.rs:101`, `crates/ansible-mesh-core/src/graph_tools.rs:73`

**Problem:** Both `ToolInvoker` impls call `tokio::runtime::Handle::current().block_on(...)` inside a synchronous trait method. This blocks a Tokio worker thread for the duration of the async call. On a runtime with N worker threads, N concurrent tool invocations will exhaust all threads. The runtime stalls — no new tasks can be scheduled until a blocked thread returns. This is a latent deadlock, not a theoretical one.

**Fix:** Make `ToolInvoker` an async trait. Replace `block_on(async { ... })` with `.await` at each call site.

```rust
// Before
fn invoke(&self, tool: &str, args: Value) -> Result<Value> {
    tokio::runtime::Handle::current().block_on(async {
        self.send_request(tool, args).await
    })
}

// After
async fn invoke(&self, tool: &str, args: Value) -> Result<Value> {
    self.send_request(tool, args).await
}
```

All callers of `invoke` are already in async contexts. The trait definition change propagates cleanly.

**Scope:** 2 impl files + 1 trait definition + callers in `ipc.rs`.

---

## Change 3: Parallel Guest Materialization

**File:** `crates/aiua/src/service/guest_manager.rs` — `materialize_all()`

**Problem:** Guests are booted sequentially:

```rust
for rec in guest_records {
    let mut mat = self.materializer.lock().await;
    mat.spawn_guest(&rec.guest_id, &config).await;  // waits for each
}
```

With 5 guests, boot time is the sum of all spawn latencies. Guests are independent OS processes — there is no ordering dependency between them.

**Fix:** Replace with `JoinSet`:

```rust
let mut set = JoinSet::new();
for rec in guest_records {
    let mat = self.materializer.clone();
    let config = config.clone();
    set.spawn(async move {
        let mut mat = mat.lock().await;
        mat.spawn_guest(&rec.guest_id, &config).await
    });
}
while let Some(result) = set.join_next().await {
    if let Err(e) = result? { warn!("Guest spawn error: {}", e); }
}
```

Boot time becomes O(slowest single guest) instead of O(sum of all guests).

The ghost reclamation phase (the `if let Some(pid)` check before spawn) can also be parallelized in the same pass — reclamation is a `kill -9` and DB write, also independent per guest.

**Scope:** ~30 lines in `materialize_all`. `supervise_guests` / `reconcile_all` should receive the same treatment in a follow-up (lower priority — the 5s tick makes sequential status checks acceptable for now).

---

## Change 4: Parallel Mesh Dispatcher Fan-Out

**File:** `crates/aiua/src/service/mesh_dispatcher.rs` — tick loop

**Problem:** On each tick, the dispatcher fans out to all known peer hotels sequentially:

```rust
for (target_node_id, target_addr) in &targets {
    dispatch_for_target(...).await   // TCP connect + framed write
}
```

With N peer hotels, each tick takes O(N × RTT). With 10 peers at 20ms RTT each, one dispatch tick takes 200ms. At the 1-second tick interval, this occupies 20% of the cycle in serial I/O.

**Fix:** `JoinSet` — all peers dispatched concurrently per tick:

```rust
let mut set = JoinSet::new();
for (target_node_id, target_addr) in targets {
    let ledger = ledger.clone();
    let tracker = tracker.clone();
    let local = local_node_id.clone();
    set.spawn(async move {
        dispatch_for_target(&ledger, &tracker, &local, &target_node_id, &target_addr).await
    });
}
while let Some(r) = set.join_next().await {
    if let Err(e) = r? { error!("Dispatch error: {}", e); }
}
```

Tick latency becomes O(max single peer RTT) regardless of peer count.

**Scope:** ~20 lines in the tick body.

---

## Change 5: Parallel Telegram Update Processing

**File:** `crates/membrane/src/main.rs` — getUpdates result loop

**Problem:** Inbound Telegram updates are processed sequentially:

```rust
for update in result {
    // media download → blob upload → IPC publish (all awaited inline)
}
```

A single voice note or photo (media download → blob upload) can take 500ms–2s. Any text messages that arrived in the same long-poll batch wait behind it. From the user's perspective: they send a photo and a follow-up text message, and the text response is delayed by the photo upload latency.

**Fix:** Spawn a task per update. The offset must be advanced *before* spawning (since `offset = update_id + 1` is computed from the update's ID, not its processing result), so correctness is maintained:

```rust
for update in result {
    let update_id = update.get("update_id")...;
    offset = update_id + 1;  // advance before spawn — safe

    let ctx = handler_ctx.clone();
    tokio::spawn(async move {
        if let Err(e) = process_update(ctx, update, update_id).await {
            error!("Update {} processing failed: {}", update_id, e);
        }
    });
}
```

The `process_update` function should encapsulate the current inline logic: `telegram_inbound_envelope` construction, attachment media download, blob upload, and IPC publish.

**Note on ordering:** Telegram's Bot API guarantees updates are delivered in order within a single `getUpdates` response. Spawning tasks means delivery to the agent may arrive slightly out of order if two updates complete at different times. For the current use case (chat messages to a single agent) this is acceptable. If strict ordering is later required, a bounded `mpsc` queue per chat_id can serialize within a conversation while parallelizing across conversations.

**Scope:** ~50 lines extracted into `process_update`, spawn loop replaces inline loop.

---

## Change 6: Inbox Loop — Spawn Slow Message Handlers

**File:** `crates/aiua/src/main.rs` — main inbox loop

**Problem:** The hotel's main inbox processes one `BeaconMessage` at a time:

```rust
while let Some(msg) = inbox_rx.recv().await {
    match msg.msg_type {
        MeshEventBatch => { /* DB insert loop — can be slow */ }
        Heartbeat => { /* NodeRegistry write — fast */ }
        ...
    }
}
```

A large `MESH_EVENT_BATCH` (50 events, each requiring a DB insert through the writer channel) occupies the inbox loop for its entire duration. Heartbeats from other peers queue up behind it. Delayed heartbeat processing causes stale `NodeRegistry` state, which affects routing decisions.

**Fix:** Spawn `MeshEventBatch` handling as a task. Heartbeats, ACKs, and control messages remain inline (they're fast):

```rust
MeshEventBatch => {
    let dispatcher_tx = dispatcher_tx.clone();
    // ... clone other Arcs
    tokio::spawn(async move {
        // batch processing logic
    });
}
Heartbeat => { /* inline — fast */ }
```

This is lower priority than Changes 1–5 because current event volumes are low. Implement when batch sizes or peer count grows.

**Scope:** ~30 lines in the inbox match arms.

---

## Deferred: SQLite Read Connection Pool

Not in scope for this proposal. WAL mode (Change 1) eliminates the immediate read/write contention. A read pool (`r2d2`, `deadpool-sqlite`, or a hand-rolled `Vec<Connection>` behind a semaphore) is only warranted if profiling shows read contention after WAL is enabled.

---

## Implementation Order

```
Change 1 (WAL)           ← immediate, trivial, no risk
Change 2 (block_on fix)  ← immediate, low risk, prevents future deadlock
Change 3 (guest boot)    ← next sprint, small
Change 4 (dispatcher)    ← next sprint, small
Change 5 (membrane)      ← next sprint, medium
Change 6 (inbox spawn)   ← when scale warrants it
```

Changes 1 and 2 should land together in one PR. Changes 3, 4, and 5 can land in a single concurrency pass PR. Change 6 is deferred.

---

## What Is Explicitly Not Changed

- **Per-connection IPC request ordering** — requests within a single guest's UDS stream are handled sequentially by design. The protocol is ordered; parallelizing within a connection would require request multiplexing.
- **DB writer thread** — the serialized `std::thread` + `blocking_recv` for event ledger appends is correct. SQLite allows one writer; the channel absorbs bursts. WAL reduces its impact on readers without changing the writer.
- **Hotel pre-boot sequence** — DB open → schema init → node capabilities → service bind → guest spawn. The ordering is a hard dependency constraint. Sequential is correct here.
