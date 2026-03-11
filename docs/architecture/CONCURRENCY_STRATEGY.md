# Philotic Stack — Concurrency Strategy

> **Status:** Living Document | **Last Updated:** 2026-03-10

---

## Current State Audit

The stack uses Tokio's multi-threaded runtime (`rt-multi-thread` feature) on all binaries. That means the async executor is already using all CPU cores. The gaps are not in the runtime choice — they're in how work is scheduled onto it, and in several places where sequential `await` loops serialize work that is naturally parallel.

### What is already concurrent

| Component | Mechanism | Notes |
|-----------|-----------|-------|
| IPC accept loop | `tokio::spawn` per connection | Each guest connection gets its own task |
| Per-connection write path | Dedicated `tokio::spawn` write task | Reader and writer split on each UDS stream |
| DB event writes | Dedicated `std::thread` + `blocking_recv` | Keeps blocking SQLite writes off the async runtime |
| Hotel services | All started as independent `tokio::spawn` tasks | Beacon, blob, supervisor, dispatcher, inbox loop are concurrent |
| Heartbeat | Dedicated spawned task | |
| Execution plane listener | Dedicated spawned task | |
| Shutdown broadcast | `tokio::sync::broadcast` | Clean fan-out |

### What is sequential and shouldn't be

These are the actual bottlenecks, ordered by impact.

---

## Bottleneck Analysis

### 1. Guest materialization on boot — SEQUENTIAL `for` loop

**File:** `crates/ansible/src/service/guest_manager.rs` — `materialize_all()`

```rust
for rec in guest_records {
    let mut mat = self.materializer.lock().await;
    mat.spawn_guest(&rec.guest_id, &config).await   // <-- blocks next guest
}
```

Each guest is spawned one at a time. Guests are independent OS processes with no ordering dependency. With 5 guests, boot time is the sum of all spawn delays rather than the maximum.

**Fix:** `JoinSet`. Spawn all guests concurrently, await completion.

---

### 2. Mesh dispatcher peer fan-out — SEQUENTIAL `for` loop

**File:** `crates/ansible/src/service/mesh_dispatcher.rs` — tick loop

```rust
for (target_node_id, target_addr) in &targets {
    dispatch_for_target(...).await  // TCP connect + write, blocks next peer
}
```

Each peer gets a full TCP roundtrip before the next peer is started. With N peer hotels, dispatch latency is O(N × RTT) when it should be O(max RTT).

**Fix:** `JoinSet`. Dispatch to all peers concurrently per tick.

---

### 3. Membrane Telegram update processing — SEQUENTIAL `for` loop

**File:** `crates/membrane/src/main.rs` — getUpdates result handling

```rust
for update in result {
    // media download → blob upload → IPC publish  (all awaited inline)
}
```

A slow media download (voice note, photo) blocks all subsequent updates in the same long-poll batch. A user sending a photo followed immediately by a text message will have their text message wait until the photo fully uploads to blob storage.

**Fix:** `tokio::spawn` per update. Offset advancement must happen before spawn (already computed from `update_id`), so ordering of ACKs is safe. Each update is processed independently.

---

### 4. SQLite: no WAL mode, single `Arc<Mutex<Connection>>` per storage

**File:** `crates/ansible-mesh-core/src/sqlite_storage.rs`

All three storage types (`EventStorage`, `CursorStorage`, `GraphStorage`) each hold a single `Arc<Mutex<Connection>>`. Reads and writes serialize through the same lock. No WAL mode is configured, so SQLite's default journal mode is in use — this prevents any concurrent readers even when the writer is idle.

**Fix (immediate):** Enable WAL mode and set `busy_timeout` on connection open:

```rust
conn.execute_batch("
    PRAGMA journal_mode=WAL;
    PRAGMA busy_timeout=5000;
    PRAGMA synchronous=NORMAL;
")?;
```

This is a one-line-per-connection change with no API impact. WAL allows multiple concurrent readers + one writer. The existing `Arc<Mutex<Connection>>` write lock remains correct — it just stops blocking reads.

**Fix (later):** Separate read connections from the write path. Maintain a small pool of read-only connections (`Connection::open_with_flags(..., OpenFlags::SQLITE_OPEN_READ_ONLY)`). Writes still go through the serialized writer thread. Reads no longer contend with writes at all.

---

### 5. Main inbox loop — sequential per-message dispatch

**File:** `crates/ansible/src/main.rs`

```rust
while let Some(msg) = inbox_rx.recv().await {
    match msg.msg_type {
        MeshEventBatch => { /* DB write + ACK reply */ }
        Heartbeat => { /* NodeRegistry update */ }
        ...
    }
}
```

This is a single-threaded dispatch loop. A slow `MESH_EVENT_BATCH` (100 events, each requiring a DB insert) blocks heartbeat processing, which blocks ACK replies. For the current load (few peer hotels, low event rate) this is acceptable. It becomes a bottleneck at scale.

**Fix (when needed):** Spawn per-message type or per-sender. Heartbeats and ACKs are fast and should never wait behind batch processing. At minimum, move `MESH_EVENT_BATCH` processing into a `tokio::spawn` so the inbox loop can continue draining.

---

### 6. `block_on` inside sync trait impls — DANGER

**Files:** `crates/ansible-mesh-core/src/model_manager.rs:101`, `crates/ansible-mesh-core/src/graph_tools.rs:73`

```rust
tokio::runtime::Handle::current().block_on(async { ... })
```

These calls block a Tokio worker thread inside a sync `ToolInvoker` impl. On a runtime with `N` worker threads, calling this from `N` concurrent tasks simultaneously will deadlock or starve the runtime. This is not theoretical — it will happen under concurrent tool execution load.

**Fix:** Make `ToolInvoker` an async trait (using `async-trait` or Rust 1.75+ native async traits) and `.await` instead of `block_on`.

---

### 7. Guest supervision reconciliation — SEQUENTIAL `for` loop

**File:** `crates/ansible/src/service/guest_manager.rs` — `reconcile_all()`

```rust
for rec in all_guests {
    mat.check_status(...).await   // kill(pid, 0) — fast but still serialized
    mat.spawn_guest(...).await    // slow if a guest needs respawn
}
```

`check_status` is a `kill -0` syscall — microseconds each. But `spawn_guest` involves `Command::spawn` and a DB write. Respawning 3 crashed guests happens sequentially. Low priority but worth fixing.

**Fix:** `JoinSet` with a clone of the materializer per guest, or split the check phase (parallel) from the act phase (parallel per action).

---

## Implementation Priority

| Priority | Change | Effort | Impact |
|----------|--------|--------|--------|
| **P1** | SQLite WAL mode + `busy_timeout` on all connections | Trivial (3 lines × 3 files) | Eliminates read/write contention immediately |
| **P1** | Fix `block_on` in `model_manager` and `graph_tools` | Small | Prevents potential deadlock under load |
| **P2** | Parallel guest materialization (`JoinSet` in `materialize_all`) | Small | Faster boot with multiple guests |
| **P2** | Parallel mesh dispatcher fan-out (`JoinSet` in tick loop) | Small | O(max RTT) dispatch latency instead of O(N × RTT) |
| **P3** | Parallel Telegram update processing (`spawn` per update) | Medium | Prevents media downloads from blocking text messages |
| **P4** | Spawn inbox loop message handling for slow message types | Medium | Needed at scale, not urgent now |
| **P5** | SQLite read connection pool | Medium | Only needed when read contention is observed in profiling |

---

## What NOT to Parallelize

### IPC per-request loop within a guest connection

Each guest's connection is handled by one task (`handle_client`). Requests within a single connection are processed sequentially. This is intentional: a guest's request/response stream is ordered, and a guest shouldn't need to send a second request before receiving the first response. Parallelizing within a connection would require request multiplexing and complicate the protocol.

### DB writer thread

The serialized `std::thread` writer for event ledger appends is correct. SQLite allows one writer at a time, and the channel queues burst writes gracefully. Adding parallelism here would require transactions or batching — more complexity for no benefit over what WAL mode already provides.

### Hotel boot sequence before guest materialization

The hotel must complete: DB open → schema init → node capabilities load → services bind → before spawning guests. This ordering is a hard constraint (guests need the IPC socket to exist). The sequential pre-boot phase is correct.

---

## Quick Reference: Canonical Patterns

### Parallel guest fan-out with `JoinSet`

```rust
use tokio::task::JoinSet;

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
    if let Err(e) = result? { warn!("Guest spawn failed: {}", e); }
}
```

### Parallel peer dispatch

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
    if let Err(e) = r? { error!("Dispatch failed: {}", e); }
}
```

### Telegram update fan-out

```rust
for update in result {
    let update_id = ...; // extract before spawn
    offset = update_id + 1;
    let ctx = ctx.clone(); // Arc-wrapped handler context
    tokio::spawn(async move {
        if let Err(e) = process_update(ctx, update).await {
            error!("Update {} failed: {}", update_id, e);
        }
    });
}
```

### SQLite WAL pragma (add to every `Connection::open`)

```rust
conn.execute_batch(
    "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA synchronous=NORMAL;"
)?;
```
