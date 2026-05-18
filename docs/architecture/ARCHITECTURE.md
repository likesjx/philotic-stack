---
title: Philotic Stack Architecture Reference
doc_type: reference
domain: runtime-sessions
status: active
last_updated: 2026-03-31
tags:
- runtime
- reference
- hotel
- ipc
- mesh
related_docs:
- README.md
- ARCHITECTURE_STATUS.md
- PORT_BLUEPRINT.md
task_refs:
- docs/task.md
tracks_domains:
- runtime-sessions
- membrane-transport
- mesh-placement
- tooling-execution
- deployment-distribution
---

# Philotic Stack — Architecture Reference

> **Status:** Living Document | **Last Updated:** 2026-03-12

This document describes the full runtime architecture of the Philotic Stack —
a distributed AI agent operating system built in Rust. It is built around a powerful and intuitive **Hotel & Guest** metaphor. It covers The Hotel daemon (the orchestrator), all crates, all materialized Guest processes (the agents and gateways), the IPC and mesh transports,
storage abstractions, and state synchronization.

This is a durable hierarchy reference, not the live work queue. The graph is
canonical for current state, domain ownership, seam ownership, and active
work. For a legacy/transitional snapshot of current implementation status, use
[ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md).

Generated UML/PlantUML diagrams for the graph-visible hierarchy live under
`docs/architecture/generated/` and should be treated as derived views.

---

## Table of Contents

1. [Mental Model](#1-mental-model)
2. [Crate Map](#2-crate-map)
3. [The Hotel — `crates/ansible`](#3-the-hotel--cratesansible)
4. [Core Primitives — `crates/ansible-mesh-core`](#4-core-primitives--cratesansible-mesh-core)
5. [Guest Binaries](#5-guest-binaries)
6. [Client SDK — `crates/philotic-client`](#6-client-sdk--cratesphilotic-client)
7. [Intra-Hotel IPC (Unix Domain Sockets)](#7-intra-hotel-ipc-unix-domain-sockets)
8. [Inter-Hotel Mesh (Control Plane) and Execution Transport (Data Plane)](#8-inter-hotel-mesh-control-plane-and-execution-transport-data-plane)
9. [Storage Layer — Traits and Implementations](#9-storage-layer--traits-and-implementations)
10. [Session Authority And Derived State Sync](#10-session-authority-and-derived-state-sync)
11. [Guest Lifecycle — Materialization & Supervision](#11-guest-lifecycle--materialization--supervision)
12. [Security Model](#12-security-model)
13. [Environment Flags](#13-environment-flags)
14. [Port Road Map](#14-port-road-map)

---

## 1. Mental Model

```
           ┌──────────────────────────────────────────────┐
           │               HOTEL  (aiua daemon)            │
           │                                               │
           │  ┌──────────┐   IPC    ┌────────────────────┐ │
           │  │ membrane  │◄────────►│  aiua (hotel)      │ │
           │  └──────────┘   UDS    │                    │ │
           │                        │  • ContextGraph DB  │ │
           │  ┌──────────┐          │  • GuestManager     │ │
           │  │ philote  │◄────────►│  • IpcServer        │ │
           │  └──────────┘   UDS    │  • BeaconDaemon     │ │
           │                        │  • BlobService      │ │
           │  ┌─────────────┐       │  • Outbound Disp.   │ │
           │  │model-router │◄──────►└────────────────────┘ │
           │  └─────────────┘                               │
           └──────────────────────────────────────────────┘
                 │ UDP control plane / TCP execution plane
           ┌─────▼────────────────────────────────────────┐
           │              REMOTE HOTEL                    │
           └──────────────────────────────────────────────┘
```

**Key design constraints:**

- One canonical `aiua` hotel daemon per machine.
- All in-machine communication uses **Unix Domain Sockets** (IPC).
- Cross-machine coordination uses **UDP BeaconMessages** on the control plane plus a framed point-to-point execution transport for routed work.
- The Context Graph SQLite DB is the canonical source of truth for all hotel state.
- Canonical session truth lives in the graph; apartment sync is a derived recovery/checkpoint path, not a competing session authority.
- Storage engines are fully pluggable via trait objects (`Arc<dyn GraphStorage>`).

---

## 2. Crate Map

| Crate               | Role                                                         |
| ------------------- | ------------------------------------------------------------ |
| `aiua`           | Hotel daemon — orchestration and service host                |
| `ansible-mesh-core` | Shared primitives, traits, mesh types, storage (Legacy monolith) |
| `philotic-primitives-*` | Extracted primitives (mesh, hotel, agent, data, model, tool) |
| `philotic-client`   | Guest SDK — IPC client for guests to talk to the hotel       |
| `membrane-*`        | Protocol gateway guests (Telegram, Discord, MCP)             |
| `philote`           | Persona/agent cognitive loop guest binary                    |
| `model-router`      | Shared LLM inference routing SDK                             |
| `philotic-web`      | Desktop operator surface (Next.js)                           |
| `tool-runner`       | Workspace tool executor guest                                |
| `graph-datasource`  | Autonomous graph partition management                        |
| `graph-intelligence`| Project intelligence graph + MCP server                      |
| `robot-kit`         | Embedded robotics HAL (separate concern, not hotel/guest)    |

### 2.1 The Legacy Reference

The `legacy-zeroclaw` submodule was removed from this repository. Historical reference docs are preserved under `docs/legacy/`. Consult the original `zeroclaw` repository directly if legacy implementation context is needed. No active code in `crates/` depends on the legacy codebase.

---

## 3. The Hotel — `crates/ansible`

The `aiua` daemon is the authoritative runtime process for a hotel node.
It starts first, owns the Context Graph database, and materializes all guest
processes.

### 3.1 Boot Sequence

```
main()
  │
  ├─ Parse CLI args (--load-config flag)
  ├─ Read PHILOTIC_* env flags (feature gates)
  ├─ Open SqliteGraphStorage ("aiua_context.db")
  ├─ Optionally seed config from JSON file
  ├─ Load or bootstrap NodeCapabilities
  ├─ Bind BeaconDaemon (UDP control plane, port 8999)
  ├─ Start BlobService (HTTP, port 9001)
  ├─ Start execution-plane listener (default dev layout: base + 2)
  ├─ Materialize all active guests (GuestManager)
  ├─ Start Guest Supervisor loop (reconcile every 5s)
  ├─ Start IpcServer (UDS, /tmp/philotic-aiua.sock)
  ├─ [Flag] Start Outbound Mesh Dispatcher
  ├─ [Flag] Start Task Lifecycle Ledger Writer
  └─ Enter main inbox loop (BeaconMessage dispatch)
```

### 3.2 In-Process Services

| Service           | File                         | Description                                                                                        |
| ----------------- | ---------------------------- | -------------------------------------------------------------------------------------------------- |
| `IpcServer`       | `service/ipc.rs`             | Unix Domain Socket server. Routes IPC requests from guests to hotel logic.                         |
| `GuestManager`    | `service/guest_manager.rs`   | Materializes and supervises guest OS processes. Consumes `Arc<dyn GraphStorage>`.                  |
| `BlobService`     | `service/blob.rs`            | HTTP server for large payload upload/download via content-addressed SHA-256 IDs.                   |
| `mesh_dispatcher` | `service/mesh_dispatcher.rs` | Outbound routed-task dispatcher. Polls the EventLedger and sends framed `BeaconMessage` batches to peer hotels over the execution plane. |
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
| `beacon`         | `BeaconDaemon` — the UDP server/client for control-plane mesh traffic                     |
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

| Binary                      | Crate                 | Role identity                    | Purpose                                                    |
| --------------------------- | --------------------- | -------------------------------- | ---------------------------------------------------------- |
| `membrane-telegram`         | `crates/membrane-telegram` | `membrane-telegram-01`       | Telegram gateway, ingress/egress for external messages     |
| `membrane-discord`          | `crates/membrane-discord`  | `membrane-discord-01`        | Discord gateway                                            |
| `membrane-mcp`              | `crates/membrane-mcp`      | `membrane-mcp-01`            | MCP gateway                                                |
| `membrane`                 | `crates/membrane`      | compatibility wrapper             | Transitional wrapper over shared membrane runtime during provider extraction |
| `philote`                | `crates/philote`      | `agent-{persona}-01`             | Persona runtime, long-running reasoning loop               |
| `model-controller-*`        | `crates/model-router` | `model-router-01`                | Multi-provider LLM/TTS routing guest (Gemini, ElevenLabs)  |
| `tool-runner`               | `crates/tool-runner`  | `{hotel}:tool-runner`            | Workspace tool executor                                    |
| `philotic-web`              | `crates/philotic-web` | `desktop-operator-01`            | Desktop/web operator surface                               |

The `model-router` crate now acts as shared SDK/runtime infrastructure. The two `model-controller-*` binaries are separate materialized guests for their respective providers.

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

### IPC Message Families

The IPC surface has evolved well beyond the original minimal register/publish
shape. The important current families are:

- guest lifecycle and liveness:
  - `Register`
  - `Heartbeat`
- routed work and result flow:
  - `EmitTask`
  - `UpdateTask`
  - `AckEvent`
- session and runtime coordination:
  - session snapshot/config requests
  - approval/session status updates
  - Telegram poll-lease acquire/renew/release
- recovery and derived state sync:
  - `SyncApartment`
  - push-style `ApartmentUpdate`
- secret/config access:
  - `GetConfig`
  - `GetSecret`

Use the actual types in code as canonical contract truth. This section is a
family-level reference so it does not quietly freeze an obsolete enum snapshot.

### Key Methods

```rust
// Connect and register
PhiloticClient::connect(port: u16) -> Result<Self>

// Publish an event into the hotel's ledger
client.publish_event(env: EventEnvelope) -> Result<u64>

// Read current runtime/session/config state
client.send_request(IpcRequest::GetConfig { ... }) -> Result<IpcResponse>

// Derived recovery/checkpoint sync
client.sync_apartment(agent_id, memory_type, content_json) -> Result<()>
```

---

## 7. Intra-Hotel IPC (Unix Domain Sockets)

All communication between guests and the hotel daemon uses a **Unix Domain Socket**
at `/tmp/philotic-aiua.sock` (overridable via `PHILOTIC_HOTEL_SOCKET`). The protocol is newline-delimited JSON
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
| `GraphStorage`  | `SqliteGraphStorage`  | `node_config`, `materialized_guests`, `agent_identities`, `memory_apartments`, session graph entities, route/identity records |

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

## 10. Session Authority And Derived State Sync

### 10.1 Canonical Session Truth

Philotic session truth is graph-owned.

That includes:

- session identity and bindings
- participants
- turns
- session events/timeline
- approval/session status
- route-affecting metadata

Apartment state still exists, but it is not the canonical session envelope.

### 10.2 Memory Apartments As Derived Recovery State

Each agent still has typed memory "apartments" (`short`, `long`, `episodic`,
`semantic`) stored in the Context Graph.

The hotel is the durable owner of those records, while guests may hold a local
hot-path RAM copy for cognitive loops and checkpoint it back through
`SyncApartment`.

### 10.3 Optimistic Apartment Write Flow

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

### 10.4 Conflict Resolution (LWW)

The current implementation uses **Last-Writer-Wins** — the most recent
`sync_apartment` call wins. The hotel performs an atomic delete+insert
within a single SQLite transaction.

> **Future:** Vector clocks / Hybrid Logical Clocks (HLC) for precise
> causal ordering across multi-hotel apartment mirrors.

### 10.5 Hotel-to-Guest Push (ApartmentUpdate)

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
| Mesh PKI     | WireGuard-inspired Ed25519 node identities and ephemeral X25519 ECDH session keys |
| Legacy Mesh PSK | HMAC-SHA256 over `(payload \|\| timestamp)` with pre-shared key (migrating out) |
| Replay guard | ±5 minute timestamp window on all BeaconMessages                |
| IPC          | Unix file-system permissions on the UDS socket                  |
| Future       | Automated certificate rotation                                  |

Set `PHILOTIC_MESH_PSK=<secret>` on all hotels in the same mesh cluster for fallback.
Default is `INSECURE_DEV_DEFAULT_PSK` — override before production.

---

## 13. Environment Flags

| Variable                              | Default                          | Effect                                              |
| ------------------------------------- | -------------------------------- | --------------------------------------------------- |
| `PHILOTIC_HOTEL_SOCKET`               | `/tmp/philotic-aiua.sock`     | IPC Unix domain socket path                         |
| `PHILOTIC_MESH_PSK`                   | `INSECURE_DEV_DEFAULT_PSK`       | Shared mesh authentication key                      |
| `PHILOTIC_HOTEL_PORT`                 | `9000`                           | IPC listen port                                     |
| `PHILOTIC_BIN_DIR`                    | (none — uses `PATH`)             | Directory where guest binaries are resolved         |
| `PHILOTIC_BLOB_BASE_URL`              | `http://127.0.0.1:<blob_port>`   | Base URL injected into guests for blob access       |
| `PHILOTIC_ENABLE_RUST_AUTH`           | `0`                              | Enable Rust-native HMAC auth (`1` = on)             |
| `PHILOTIC_ENABLE_RUST_DISPATCHER`     | `0`                              | Enable Rust outbound mesh dispatcher                |
| `PHILOTIC_ENABLE_RUST_TASK_LIFECYCLE` | `0`                              | Enable Rust durable event ledger writer             |

---

## 14. Port Road Map

| Phase                         | Status      | Description                                            |
| ----------------------------- | ----------- | ------------------------------------------------------ |
| Guest Supervisor              | ✅ Complete | Reconciliation loop, ghost detection, auto-respawn     |
| Control Plane Mesh            | ✅ Complete | Durable event ledger, cursor-tracked control-plane gossip |
| Execution Plane               | ✅ Complete | Point-to-point framed transport for routed work        |
| Session Graph Model           | ✅ Complete | Graph-owned sessions, participants, turns, and events  |
| Derived Apartment Sync        | ✅ Complete | `SyncApartment` IPC, LWW apartment upsert              |
| Database Agnosticism          | ✅ Complete | `EventStorage`, `CursorStorage`, `GraphStorage` traits |
| Task Lifecycle Engine         | 🔲 Planned  | State machine with invariants (PORT-BP-004)            |
| Auth Exchange                 | 🔲 Planned  | Invite/ticket validation (PORT-BP-006)                 |
| Scaling / Performance Monitor | 🔲 Planned  | Process scale-out/in based on machine metrics          |
| WebRTC P2P Data Channels      | 🔶 In Progress | Signal/SDP types exist; `WebRtcGuest` started but ICE/lifecycle incomplete |
| Multi-Hotel Parity Tests      | ✅ Complete | Mesh-visible capability routing and remote model proof |

### Current Transitional Notes

- control-plane peer discovery and execution-plane addressing are still carrying transitional local-development assumptions in some paths
- apartment sync remains a derived checkpoint path and should not be confused with canonical session truth
- role incarnation routing direction is established, but the full graph-backed role/toolset/handoff system is still a live implementation seam
