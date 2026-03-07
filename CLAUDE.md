# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
# Build
just build                  # cargo build --workspace
just check                  # cargo check --workspace (faster, no artifacts)
just format                 # cargo fmt --all

# Test
just test                   # cargo test --workspace
cargo test -p <crate>       # test a single crate
cargo test <test_name>      # run a specific test by name

# Run
just start-ansible          # build + start hotel daemon (requires mesh-config.json)
just start-gateway          # cargo run -p hegemon
just start-agent            # cargo run -p agent-core
just start-model            # cargo run -p model-router
```

The hotel daemon requires `mesh-config.json` in the project root — copy from `mesh-config.example.json` and fill in credentials before running.

## Architecture

The Philotic Stack is a distributed AI agent OS built in Rust. The metaphor: a **hotel** is a node that **materializes** AI **guests** (processes). All state lives in a SQLite Context Graph owned by the hotel daemon.

### Crate Dependency Order

```
ansible-mesh-core       (shared primitives — everything else depends on this)
philotic-client         (guest SDK — IPC client only, no hotel internals)
ansible                 (hotel daemon — imports both above)
hegemon / agent-core / model-router / robot-kit  (guests — import philotic-client)
```

### Communication Layers

- **Intra-hotel (UDS):** All guest↔hotel communication uses Unix Domain Sockets at `/tmp/philotic-ansible.sock` (overridable via `PHILOTIC_HOTEL_SOCKET`). Protocol is newline-framed JSON using `IpcRequest` / `IpcResponse` from `philotic-client`.
- **Inter-hotel (UDP):** Cross-machine communication uses `BeaconMessage` envelopes on port 8999. HMAC-PSK auth is opt-in via `PHILOTIC_ENABLE_RUST_AUTH=1`.
- **Blob store (HTTP):** Large payloads served content-addressed over HTTP on port 9001.

### Storage Abstractions (`ansible-mesh-core/src/storage.rs`)

Three pluggable traits — all consumers hold `Arc<dyn XxxStorage>`:

| Trait | SQLite Impl | Backs |
|---|---|---|
| `EventStorage` | `SqliteEventStorage` | `mesh_events` — durable append-only event ledger |
| `CursorStorage` | `SqliteCursorStorage` | `mesh_cursors` — per-node ACK cursors |
| `GraphStorage` | `SqliteGraphStorage` | `node_config`, `materialized_guests`, `memory_apartments` |

To add a new backend, implement the three traits and inject at startup in `ansible/src/main.rs`.

### Guest Lifecycle

1. `ansible` reads `materialized_guests` (where `is_active=1`) from the Context Graph on boot.
2. `GuestManager` + `LocalProcessMaterializer` spawn each guest as an OS subprocess.
3. A supervisor loop runs every 5s — dead guests are auto-respawned; inactive guests are reclaimed.
4. Each guest calls `PhiloticClient::connect()` which registers over UDS; the hotel tracks the live PID.

### Key Environment Variables

| Variable | Default | Purpose |
|---|---|---|
| `PHILOTIC_HOTEL_SOCKET` | `/tmp/philotic-ansible.sock` | IPC socket path |
| `PHILOTIC_MESH_PSK` | `INSECURE_DEV_DEFAULT_PSK` | UDP mesh auth key |
| `PHILOTIC_HOTEL_PORT` | `9000` | IPC listen port |
| `PHILOTIC_ENABLE_RUST_AUTH` | `0` | `1` = enforce HMAC on mesh |
| `PHILOTIC_ENABLE_RUST_DISPATCHER` | `0` | `1` = start outbound UDP dispatcher |
| `PHILOTIC_ENABLE_RUST_TASK_LIFECYCLE` | `0` | `1` = start durable event ledger writer |

### Notable Conventions

- `BeaconMessage` is the UDP envelope; `MsgType` identifies payload semantics. Payload is `Vec<u8>` (JSON, MsgPack, or CBOR).
- Memory apartment writes are **optimistic / LWW** — guests call `SyncApartment` IPC and the hotel resolves conflicts on upsert.
- `legacy-zeroclaw/` is a pristine reference clone — do not modify it or treat it as active code.
- The `robot-kit` crate is a separate embedded robotics HAL concern, not part of the hotel/guest model.

### Reference Docs

- `docs/architecture/ARCHITECTURE.md` — full system design and data flows
- `docs/architecture/PORT_BLUEPRINT.md` — migration plan from legacy plugin model
- `crates/ansible/README.md` — hotel daemon service inventory
- `crates/ansible-mesh-core/README.md` — module-by-module primitive reference
