# Philotic Stack — Architecture Reference

> **Status:** Living Document | **Last Updated:** 2026-03-06

This document describes the full runtime architecture of the Philotic Stack —
a distributed AI agent operating system built in Rust. It covers the hotel
model, all crates, all in-process components, the IPC and mesh transports,
storage abstractions, and state synchronization.

---

## Table of Contents

1. [Mental Model](#1-mental-model)
2. [Crate Map](#2-crate-map)
3. [The Hotel — `crates/ansible`](#3-the-hotel--cratesansible)
4. [Core Primitives — `crates/ansible-mesh-core`](#4-core-primitives--cratesansible-mesh-core)
5. [Guest Binaries](#5-guest-binaries)
6. [Client SDK — `crates/philotic-client`](#6-client-sdk--cratesphilotic-client)
7. [Intra-Hotel IPC (Unix Domain Sockets)](#7-intra-hotel-ipc-unix-domain-sockets)
8. [Inter-Hotel Mesh (UDP / OTA)](#8-inter-hotel-mesh-udp--ota)
9. [Storage Layer — Traits and Implementations](#9-storage-layer--traits-and-implementations)
10. [State Synchronization & Optimistic Writes](#10-state-synchronization--optimistic-writes)
11. [Guest Lifecycle — Materialization & Supervision](#11-guest-lifecycle--materialization--supervision)
12. [Security Model](#12-security-model)
13. [Environment Flags](#13-environment-flags)
14. [Port Road Map](#14-port-road-map)

---

## 1. Mental Model

```
           ┌──────────────────────────────────────────────┐
           │               HOTEL  (Ansible daemon)         │
           │                                               │
           │  ┌──────────┐   IPC    ┌────────────────────┐ │
           │  │ hegemon  │◄────────►│  ansible (hotel)   │ │
           │  └──────────┘   UDS    │                    │ │
           │                        │  • ContextGraph DB  │ │
           │  ┌──────────┐          │  • GuestManager     │ │
           │  │agent-core│◄────────►│  • IpcServer        │ │
           │  └──────────┘   UDS    │  • BeaconDaemon     │ │
           │                        │  • BlobService      │ │
           │  ┌─────────────┐       │  • Outbound Disp.   │ │
           │  │model-router │◄──────►└────────────────────┘ │
           │  └─────────────┘                               │
           └──────────────────────────────────────────────┘
                        │  UDP Mesh (BeaconMessage)
           ┌────────────▼─────────────────────────────────┐
           │              REMOTE HOTEL                     │
           └──────────────────────────────────────────────┘
```

**Key design constraints:**

- One canonical `ansible` hotel daemon per machine.
- All in-machine communication uses **Unix Domain Sockets** (IPC).
- All cross-machine communication uses **UDP BeaconMessages** (OTA mesh).
- The Context Graph SQLite DB is the canonical source of truth for all hotel state.
- Storage engines are fully pluggable via trait objects (`Arc<dyn GraphStorage>`).

---

## 2. Crate Map

| Crate               | Role                                                   |
| ------------------- | ------------------------------------------------------ |
| `ansible`           | Hotel daemon — orchestration and service host          |
| `ansible-mesh-core` | Shared primitives, traits, mesh types, storage         |
| `philotic-client`   | Guest SDK — IPC client for guests to talk to the hotel |
| `hegemon`           | Telegram/gateway guest binary                          |
| `agent-core`        | Persona/agent guest binary                             |
| `model-router`      | Model provider routing guest binary                    |
| `robot-kit`         | Embedded robotics HAL (separate concern)               |

### 2.1 The Legacy Reference: `legacy-zeroclaw`

The `legacy-zeroclaw` directory is a **pristine reference** cloned from the Zerocode open-source project. It is **not** part of the active Philotic Stack codebase.

**Policy:**

1. **Pristine State**: Do not modify files inside `legacy-zeroclaw`. It will be periodically updated from its upstream origin.
2. **Copy / Mutate / Rewrite**: If logic from the legacy codebase is needed, **copy** it to a relevant crate under `crates/`, **mutate** it to fit the Philotic architecture, or **rewrite** it from scratch.
3. **Isolation**: No active code in `crates/` should depend on `legacy-zeroclaw`. It is excluded from build, test, and coverage reporting.

---

## 3. The Hotel — `crates/ansible`

The `ansible` daemon is the authoritative runtime process for a hotel node.
It starts first, owns the Context Graph database, and materializes all guest
processes.

### 3.1 Boot Sequence

```
main()
  │
  ├─ Parse CLI args (--load-config flag)
  ├─ Read PHILOTIC_* env flags (feature gates)
  ├─ Open SqliteGraphStorage ("ansible_context.db")
  ├─ Optionally seed config from JSON file
  ├─ Load or bootstrap NodeCapabilities
  ├─ Bind BeaconDaemon (UDP mesh, port 8999)
  ├─ Start BlobService (HTTP, port 9001)
  ├─ Materialize all active guests (GuestManager)
  ├─ Start Guest Supervisor loop (reconcile every 5s)
  ├─ Start IpcServer (UDS, /tmp/ansible.sock)
  ├─ [Flag] Start Outbound Mesh Dispatcher (UDP)
  ├─ [Flag] Start Task Lifecycle Ledger Writer
  └─ Enter main inbox loop (BeaconMessage dispatch)
```

### 3.2 In-Process Services

| Service           | File                         | Description                                                                                        |
| ----------------- | ---------------------------- | -------------------------------------------------------------------------------------------------- |
| `IpcServer`       | `service/ipc.rs`             | Unix Domain Socket server. Routes IPC requests from guests to hotel logic.                         |
| `GuestManager`    | `service/guest_manager.rs`   | Materializes and supervises guest OS processes. Consumes `Arc<dyn GraphStorage>`.                  |
| `BlobService`     | `service/blob.rs`            | HTTP server for large payload upload/download via content-addressed SHA-256 IDs.                   |
| `mesh_dispatcher` | `service/mesh_dispatcher.rs` | Outbound OTA dispatcher. Polls the EventLedger and sends UDP BeaconMessage batches to peer hotels. |
| `webrtc_guest`    | `service/webrtc_guest.rs`    | WebRTC transceiver for ephemeral P2P data channels, bypassing the mesh ledger.                     |

### 3.3 `graph.rs` — Legacy ContextGraph

A lightweight wrapper that opens the SQLite DB and initializes the schema.
This is now only used by `LocalProcessMaterializer::reclaim_guest` as an
internal throwaway connection. All production logic goes through the
`GraphStorage` trait implemented by `SqliteGraphStorage`.

---

## 4. Core Primitives — `crates/ansible-mesh-core`

All shared types, traits, and utilities live here. Every other crate depends on this.

### 4.1 Module Index

| Module           | Description                                                                               |
| ---------------- | ----------------------------------------------------------------------------------------- |
| `event`          | `EventEnvelope`, `EventKind`, `EventPayload`, `TerminalErrorCode`                         |
| `storage`        | Abstract traits: `EventStorage`, `CursorStorage`, `GraphStorage`                          |
| `sqlite_storage` | SQLite implementations: `SqliteEventStorage`, `SqliteCursorStorage`, `SqliteGraphStorage` |
| `ledger`         | `EventLedger` — original concrete event log (still used by `mesh_dispatcher`)             |
| `cursor`         | `CursorTracker` — original concrete cursor table                                          |
| `beacon`         | `BeaconDaemon` — the UDP server/client for the OTA mesh                                   |
| `authz`          | HMAC-PSK validation with 5-minute replay window                                           |
| `graph`          | In-memory `MemoryApartment` types                                                         |
| `graph_tools`    | `ContextGraphInvoker` — tool bridge for `memory.read@1` / `memory.write@1`                |
| `materializer`   | `Materializer` trait — spawn/reclaim/check_status guest lifecycle contract                |
| `registry`       | `NodeRegistry` — in-memory map of active mesh nodes and their capabilities                |
| `model_manager`  | `ModelManagerInvoker` — `model.manager.list@1` / `model.manager.route@1`                  |
| `adapter`        | `BeaconAdapter` — thin UDP send/receive helper                                            |
| `heartbeat`      | Periodic heartbeat message generation                                                     |
| `meshops`        | Mesh-level operations (policy, routing decisions)                                         |
| `runtime`        | `AgentInput` / `ToolInvoker` trait definitions                                            |
| `agent`          | `AgentManifest` — agent identity and capability declarations                              |
| `tools`          | Tool definition types                                                                     |
| `webrtc`         | `WebRtcSignalMessage` and `SignalPayload` types                                           |

### 4.2 `EventEnvelope`

The canonical inter-hotel data unit:

```
EventEnvelope {
  event_id:        UUID (globally unique)
  seq:             u64 (monotonic, per source node)
  source_node_id:  String
  source_agent_id: String
  target_agent_id: Option<String>  // None = broadcast
  kind:            EventKind       // TASK_INVOKE | TASK_RESULT | MEMORY_OP | ...
  corr_id:         String          // ties tasks, replies, retries
  attempt:         u32
  created_at:      u64 (ms epoch)
  expires_at:      Option<u64>
  payload:         Inline { data: String }
                 | BlobRef { blob_id, size, mime, source_hotel_ip }
  trace:           Vec<String>     // routing lineage
}
```

### 4.3 `BeaconMessage`

The UDP transport envelope:

```
BeaconMessage {
  version:   u8
  msg_id:    UUID
  src_node:  NodeId
  dest_node: String
  msg_type:  MsgType  // HEARTBEAT | MESH_EVENT_BATCH | MESH_EVENT_ACK | ...
  seq:       u32
  total:     u32      // for fragmentation (1 = unfragmented)
  payload:   Vec<u8>  // JSON-encoded inner payload
  timestamp: u64      // Unix epoch (for replay guard)
  hmac:      Vec<u8>  // HMAC-SHA256 over PSK
}
```

---

## 5. Guest Binaries

Guests are OS child processes spawned by `GuestManager`. They communicate with
the hotel exclusively over the IPC UDS socket using `PhiloticClient`.

| Binary         | Crate                 | Identity                 | Purpose                                                |
| -------------- | --------------------- | ------------------------ | ------------------------------------------------------ |
| `hegemon`      | `crates/hegemon`      | `hegemon-telegram-01`    | Telegram gateway, ingress/egress for external messages |
| `agent-core`   | `crates/agent-core`   | `agent-jane-01`          | Persona runtime, long-running reasoning loop           |
| `model-router` | `crates/model-router` | `model-router-gemini-01` | Routes LLM calls to available model providers via mesh |

### Guest Boot Sequence

```
main()
  │
  ├─ PhiloticClient::connect(ansible_port)
  │    └─ Sends IpcRequest::Register(GuestIdentity)
  │    └─ Hotel responds with IpcResponse::Registered
  │
  ├─ Enter processing loop:
  │    ├─ Fetch tasks / tool calls from hotel
  │    ├─ Execute local logic
  │    ├─ Optionally sync memory: client.sync_apartment(...)
  │    └─ Publish results back to hotel
```

---

## 6. Client SDK — `crates/philotic-client`

`PhiloticClient` is the SDK that every guest uses to talk to its hotel's IPC layer.

### IPC Message Types

```rust
// Requests (guest → hotel)
enum IpcRequest {
  Register(GuestIdentity),
  PublishEvent(EventEnvelope),
  Heartbeat,
  SyncApartment { agent_id, memory_type, content },
}

// Responses (hotel → guest)
enum IpcResponse {
  Registered,
  EventAccepted { seq: u64 },
  HeartbeatAck,
  ApartmentUpdate { agent_id, memory_type, canonical_content },
  Error(String),
}
```

### Key Methods

```rust
// Connect and register
PhiloticClient::connect(port: u16) -> Result<Self>

// Publish an event into the hotel's ledger
client.publish_event(env: EventEnvelope) -> Result<u64>

// Optimistic apartment write (fire-and-forget, CRDT LWW)
client.sync_apartment(agent_id, memory_type, content_json) -> Result<()>
```

---

## 7. Intra-Hotel IPC (Unix Domain Sockets)

All communication between guests and the hotel daemon uses a **Unix Domain Socket**
at `/tmp/ansible.sock` (configurable). The protocol is newline-delimited JSON
over a persistent stream connection.

```
Guest Process                Hotel IpcServer
     │                            │
     │── IpcRequest (JSON) ──────►│
     │                            ├─ match req:
     │                            │    Register → log identity
     │                            │    PublishEvent → EventLedger.append()
     │                            │    SyncApartment → GraphStorage.sync_apartment()
     │◄── IpcResponse (JSON) ─────│
     │                            │
```

The `IpcServer` holds an `Arc<dyn GraphStorage>` and an `mpsc::Sender<LedgerCommand>`
to dispatch durable writes to the serialized DB writer thread.

---

## 8. Inter-Hotel Mesh (Control Plane) and Execution Transport (Data Plane)

### 8.1 BeaconDaemon

`BeaconDaemon` binds on UDP port **8999** and handles control-plane traffic:

- Receiving `BeaconMessage` packets from remote hotels
- HMAC-PSK authentication with 5-minute replay window
- Bubbling validated messages into the hotel's `inbox_rx` channel
- Dispatching outbound control-plane `BeaconMessage`s via UDP send

### 8.2 Outbound Dispatcher And Execution Plane

A background tokio task (`mesh_dispatcher::outbound_dispatcher`) polls the
durable `EventLedger` and sends unacknowledged routed events to known peer hotels
over the point-to-point execution plane:

```
Tick (every 1s)
  │
  ├─ For each target_node_id:
  │    ├─ CursorTracker.get_cursor(target_node_id) → seq_N
  │    ├─ EventLedger.query_unacked_events(seq_N, limit=50) → [events]
  │    ├─ GraphStorage.list_hotels() → execution target
  │    └─ For each event:
  │         └─ Wrap in BeaconMessage(MsgType::MeshEventBatch)
  │         └─ tcp_stream.write_framed(target_execution_addr)
```

Enabled by `PHILOTIC_ENABLE_RUST_DISPATCHER=1`.

Current implementation note:
- mesh events are now filtered by `target_node_id`, and peer addresses are discovered from known hotel records in the Context Graph
- current loopback-only peer resolution is still transitional (`127.0.0.1:<mesh_port>` for control plane and `127.0.0.1:<execution_port>` for data plane), which is enough for multi-hotel local development but not yet a general cross-machine authority story

### 8.3 Execution Plane Listener

The hotel also runs a TCP execution listener on `execution_port` (default dev layout: base + 2).

It handles:

- accepting point-to-point framed `BeaconMessage` connections from peer hotels
- validating the message envelope
- forwarding `MESH_EVENT_BATCH` payloads into the local inbox for durable processing

This is the first real separation between:

- UDP control-plane gossip
- reliable point-to-point execution traffic

### 8.4 Inbound Dispatch (main inbox loop)

Inbound `BeaconMessage`s arrive and are dispatched in `main.rs`:

| `MsgType`          | Handler                                           |
| ------------------ | ------------------------------------------------- |
| `HEARTBEAT`        | Update `NodeRegistry` with peer capabilities      |
| `MESH_EVENT_BATCH` | Deserialize events, commit to inbound ledger, ACK |
| `MESH_EVENT_ACK`   | Advance `CursorTracker` for the remote node       |
| `MEMORY_OP`        | Route to `GraphStorage.sync_apartment`            |
| `MODEL_MANAGER`    | Route to `ModelManagerInvoker`                    |
| `WEBRTC_SIGNAL`    | Route to `WebRtcGuest` for SDP answer generation  |

### 8.5 Message Queueing (Offline Hotels)

The `EventLedger` is an append-only, durable SQLite log. When a peer hotel
goes offline, events accumulate in the ledger. When the peer comes back
online, the outbound dispatcher resumes from the last acknowledged cursor
position — guaranteeing **at-least-once delivery** with **idempotent processing**.

Current implementation note:
- inbound `MESH_EVENT_BATCH` payloads are now delivered into the local role inbox and trigger a real `MESH_EVENT_ACK` reply
- the ACK is emitted from the async inbox loop after enqueueing the inbound batch to the writer thread; that is a transitional approximation of durable receipt, not yet a strictly post-commit acknowledgment boundary

---

## 9. Storage Layer — Traits and Implementations

The storage layer is fully trait-abstracted. All hotel subsystems receive
`Arc<dyn XxxStorage>` — no concrete type needed at call sites.

### 9.1 Trait Definitions (`storage.rs`)

```rust
trait EventStorage: Send + Sync {
  fn append_event(&self, env: &mut EventEnvelope) -> Result<u64>;
  fn delete_event(&self, event_id: &EventId) -> Result<usize>;
  fn query_unacked_events(&self, target_node_id, cursor_seq, limit) -> Result<Vec<EventEnvelope>>;
}

trait CursorStorage: Send + Sync {
  fn get_cursor(&self, consumer_node_id) -> Result<u64>;
  fn advance_cursor(&self, consumer_node_id, acked_seq, ts) -> Result<()>;
}

trait GraphStorage: Send + Sync {
  fn load_node_capabilities(&self) -> Result<Option<NodeCapabilities>>;
  fn save_node_capabilities(&self, caps) -> Result<()>;
  fn list_guests(&self, active_only: bool) -> Result<Vec<GuestRecord>>;
  fn set_guest_pid(&self, guest_id, pid: Option<&str>) -> Result<()>;
  fn seed_guests(&self, guests: &[GuestRecord]) -> Result<()>;
  fn sync_apartment(&self, agent_id, memory_type, content_json) -> Result<()>;
}
```

### 9.2 SQLite Implementations (`sqlite_storage.rs`)

| Trait           | Implementation        | DB / Table                                                                    |
| --------------- | --------------------- | ----------------------------------------------------------------------------- |
| `EventStorage`  | `SqliteEventStorage`  | `mesh_events`                                                                 |
| `CursorStorage` | `SqliteCursorStorage` | `mesh_cursors`                                                                |
| `GraphStorage`  | `SqliteGraphStorage`  | `node_config`, `materialized_guests`, `agent_identities`, `memory_apartments` |

### 9.3 Adding a New Storage Backend

To plug in PebbleDB, RocksDB, or Postgres:

1. Create a new crate (e.g. `ansible-db-pebble`)
2. Implement `GraphStorage + EventStorage + CursorStorage` for your engine
3. In `ansible/main.rs`, swap the one line:

   ```rust
   // Before (SQLite)
   let graph_storage = SqliteGraphStorage::open(db_path)?;

   // After (PebbleDB)
   let graph_storage = PebbleGraphStorage::open(db_path)?;
   ```

`GuestManager`, `IpcServer`, and the outbound dispatcher are unchanged.

---

## 10. State Synchronization & Optimistic Writes

### 10.1 Memory Apartments

Each agent has typed memory "apartments" (`short`, `long`, `episodic`, `semantic`).
These are stored in the `memory_apartments` table in the Context Graph.

The hotel is the **canonical source of truth**. Guests hold a local hot-path
RAM copy for fast cognitive loops.

### 10.2 Optimistic Write Flow

```
Guest cognitive loop
  │
  ├─ Mutate local RAM state
  ├─ client.sync_apartment(agent_id, memory_type, json)  ← fire-and-forget
  │                                   │
  │              (async, non-blocking)│
  │                                   ▼
  │                         Hotel IpcServer
  │                         GraphStorage.sync_apartment()
  │                         → DELETE old row (same agent + type)
  │                         → INSERT new row
  │                         → debug!("Synchronized ...")
  │
  └─ Continue loop immediately (no waiting)
```

### 10.3 Conflict Resolution (LWW)

The current implementation uses **Last-Writer-Wins** — the most recent
`sync_apartment` call wins. The hotel performs an atomic delete+insert
within a single SQLite transaction.

> **Future:** Vector clocks / Hybrid Logical Clocks (HLC) for precise
> causal ordering across multi-hotel apartment mirrors.

### 10.4 Hotel-to-Guest Push (ApartmentUpdate)

When the hotel detects a canonical state that differs from a guest's
optimistic write (e.g., a conflict resolution from a remote hotel sync),
it can push `IpcResponse::ApartmentUpdate` back to the guest's socket —
overriding the local state.

---

## 11. Guest Lifecycle — Materialization & Supervision

### 11.1 Materialization

On boot, `GuestManager::materialize_all()` reads the `materialized_guests`
table and spawns each `is_active=1` guest as an OS child process:

```
1. Check for ghost PID in DB → if found, call reclaim_guest() first
2. Spawn guest via the Materializer (LocalProcessMaterializer → TokioCommand)
3. Record new PID in DB
4. Detach a monitor task that watches for unplanned exit
```

### 11.2 Supervision Loop

`GuestManager::supervise_guests()` runs every **5 seconds** and reconciles
desired state (DB) vs observed state (OS):

| DB State               | OS State  | Action                               |
| ---------------------- | --------- | ------------------------------------ |
| `is_active=1`, has PID | PID alive | ✅ Healthy, no-op                    |
| `is_active=1`, has PID | PID dead  | ⚡ Re-spawn, record new PID          |
| `is_active=1`, no PID  | —         | ⚡ Spawn, record PID                 |
| `is_active=0`, has PID | PID alive | 🔴 Reclaim (kill) process, clear PID |
| `is_active=0`, no PID  | —         | ✅ Correct, no-op                    |

### 11.3 Materializer Trait

```rust
#[async_trait]
trait Materializer: Send + Sync {
  async fn spawn_guest(&mut self, guest_id, config_json) -> Result<String>; // returns active_id
  async fn reclaim_guest(&mut self, guest_id) -> Result<()>;
  async fn check_status(&self, guest_id, active_id) -> Result<bool>;
}
```

Current implementation: `LocalProcessMaterializer` — uses OS `kill -0 <PID>` for health check and `kill -9 <PID>` for reclaiming.

Future implementations: `DockerMaterializer`, `KubernetesMaterializer`.

### 11.4 Transportation Materialization (Live Migration)

To move a guest process from Hotel A → Hotel B:

```
1. HotelA: GuestManager.set_active(guest_id, false) → triggers reclaim
2. HotelA: Emit MESH_EVENT_BATCH with guest bundle to HotelB
3. HotelB: Receive event, insert guest row, set is_active=true
4. HotelB: Supervisor picks up new row → spawns guest
```

---

## 12. Security Model

| Layer        | Mechanism                                                       |
| ------------ | --------------------------------------------------------------- |
| Mesh PSK     | HMAC-SHA256 over `(payload \|\| timestamp)` with pre-shared key |
| Replay guard | ±5 minute timestamp window on all BeaconMessages                |
| IPC          | Unix file-system permissions on the UDS socket                  |
| Future       | Asymmetric PKI / per-hotel keypairs                             |

Set `PHILOTIC_MESH_PSK=<secret>` on all hotels in the same mesh cluster.
Default is `INSECURE_DEV_DEFAULT_PSK` — override before production.

---

## 13. Environment Flags

| Variable                              | Default                    | Effect                                  |
| ------------------------------------- | -------------------------- | --------------------------------------- |
| `PHILOTIC_MESH_PSK`                   | `INSECURE_DEV_DEFAULT_PSK` | Shared mesh authentication key          |
| `PHILOTIC_HOTEL_PORT`                 | `9000`                     | IPC port                                |
| `PHILOTIC_ENABLE_RUST_AUTH`           | `0`                        | Enable Rust-native HMAC auth (`1` = on) |
| `PHILOTIC_ENABLE_RUST_DISPATCHER`     | `0`                        | Enable Rust outbound mesh dispatcher    |
| `PHILOTIC_ENABLE_RUST_TASK_LIFECYCLE` | `0`                        | Enable Rust durable event ledger writer |

---

## 14. Port Road Map

| Phase                         | Status      | Description                                            |
| ----------------------------- | ----------- | ------------------------------------------------------ |
| Guest Supervisor              | ✅ Complete | Reconciliation loop, ghost detection, auto-respawn     |
| OTA Mesh Queueing             | ✅ Complete | Durable event ledger, cursor-tracked UDP dispatcher    |
| Bidirectional State Sync      | ✅ Complete | `SyncApartment` IPC, LWW apartment upsert              |
| Database Agnosticism          | ✅ Complete | `EventStorage`, `CursorStorage`, `GraphStorage` traits |
| Task Lifecycle Engine         | 🔲 Planned  | State machine with invariants (PORT-BP-004)            |
| Auth Exchange                 | 🔲 Planned  | Invite/ticket validation (PORT-BP-006)                 |
| Scaling / Performance Monitor | 🔲 Planned  | Process scale-out/in based on machine metrics          |
| WebRTC P2P Data Channels      | 🔲 Planned  | Full SDP lifecycle + ICE (PORT-BP-008)                 |
| Multi-Hotel Parity Tests      | 🔲 Planned  | Shadow mode + chaos tests (PORT-BP-009)                |
