# `ansible-mesh-core` — Philotic Core Primitives

Shared library crate containing all types, traits, and utilities
used across the Philotic Stack. Every other crate depends on this one.

## Module Overview

| Module           | Key Types                                                         | Description                                                     |
| ---------------- | ----------------------------------------------------------------- | --------------------------------------------------------------- |
| `event`          | `EventEnvelope`, `EventKind`, `EventPayload`, `TerminalErrorCode` | The canonical inter-hotel event model                           |
| `storage`        | `EventStorage`, `CursorStorage`, `GraphStorage`, `GraphAdapter`, `GuestRecord` | DB-agnostic trait contracts                          |
| `sqlite_storage` | `SqliteEventStorage`, `SqliteCursorStorage`, `SqliteGraphStorage` | SQLite implementations of all storage traits                    |
| `ledger`         | `EventLedger`                                                     | Concrete append-only SQLite event log (used by mesh dispatcher) |
| `cursor`         | `CursorTracker`                                                   | Per-node ACK cursor table                                       |
| `beacon`         | `BeaconDaemon`, `BeaconMessage`, `MsgType`                        | UDP mesh server/client                                          |
| `authz`          | `validate_hmac`, `HmacClaim`                                      | HMAC-PSK auth with 5-min replay guard                           |
| `graph`          | `MemoryApartment`, `MemoryEntry`                                  | In-memory apartment types                                       |
| `graph_tools`    | `ContextGraphInvoker`                                             | `memory.read@1` / `memory.write@1` tool bridge                  |
| `materializer`   | `Materializer`                                                    | Guest lifecycle trait (spawn / reclaim / check_status)          |
| `registry`       | `NodeRegistry`, `NodeRecord`                                      | Live map of mesh nodes and capabilities                         |
| `model_manager`  | `ModelManagerInvoker`                                             | `model.manager.list@1` / `model.manager.route@1`                |
| `adapter`        | `BeaconAdapter`                                                   | UDP send/receive helper                                         |
| `heartbeat`      | `HeartbeatEncoder`                                                | Periodic mesh heartbeat payload                                 |
| `meshops`        | Mesh operations                                                   | Policy and routing decision utilities                           |
| `runtime`        | `AgentInput`, `ToolInvoker`                                       | Sync tool invocation contract                                   |
| `agent`          | `AgentManifest`                                                   | Agent identity and declared capabilities                        |
| `tools`          | Tool definitions                                                  | Tool schema types                                               |
| `webrtc`         | `WebRtcSignalMessage`, `SignalPayload`                            | SDP/ICE signal types                                            |
| `lib`            | `NodeCapabilities`, `BeaconMessage`, `MsgType`, `NodeRole`        | Top-level mesh types                                            |

## Storage Traits

The three storage traits are the backbone of the hotel's persistence layer.
They are defined in `storage.rs` and implemented for SQLite in `sqlite_storage.rs`.

```
EventStorage  →  SqliteEventStorage  →  mesh_events table
CursorStorage →  SqliteCursorStorage →  mesh_cursors table
GraphStorage  →  SqliteGraphStorage  →  node_config
                                        materialized_guests
                                        agent_identities
                                        memory_apartments
```

All consumers (`GuestManager`, `IpcServer`, hotel `main.rs`) receive
`Arc<dyn XxxStorage>` — making the backend a pluggable deployment decision.

## Adding a Storage Backend

Implement the three traits in a new crate and drop in at startup:

```rust
// aiua/src/main.rs
let adapter = MyCustomAdapter::open(db_path)?;
let graph_domain = Arc::new(GraphDomain::new(Arc::new(adapter)));
```

Candidates: PebbleDB, RocksDB, TiKV, Postgres.
