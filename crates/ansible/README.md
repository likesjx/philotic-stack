# `ansible` — Philotic Hotel Daemon

The `ansible` binary is the authoritative runtime process for a Philotic hotel node.
It is the first process that starts, owns the Context Graph database, materializes
all guest processes, and acts as the routing hub for every interaction inside the hotel.

## Responsibilities

| Area                      | Detail                                                                           |
| ------------------------- | -------------------------------------------------------------------------------- |
| **Boot orchestration**    | Reads `ansible_context.db`, bootstraps `NodeCapabilities`, seeds demo guests     |
| **Guest materialization** | Spawns all `is_active=1` guests from the DB via `GuestManager`                   |
| **Guest supervision**     | Reconciliation loop every 5s — resurrects dead processes, reclaims inactive ones |
| **IPC server**            | Unix Domain Socket at `/tmp/ansible.sock` — handles all guest↔hotel messages     |
| **Mesh beacon**           | UDP server on port 8999 — handles inter-hotel `BeaconMessage` traffic            |
| **Execution transport**   | TCP listener on port 9002 — handles point-to-point inter-hotel execution traffic |
| **Outbound dispatch**     | Polls `EventLedger`, ships unacked routed events to peer hotels over TCP         |
| **Blob service**          | HTTP server on port 9001 — content-addressed large payload store                 |
| **State sync**            | `SyncApartment` IPC → LWW upsert into `memory_apartments`                        |

## Key Services (`src/service/`)

- **`ipc.rs`** — `IpcServer` — Unix socket server, routes to `GraphStorage`
- **`guest_manager.rs`** — `GuestManager` + `LocalProcessMaterializer` + supervision loop
- **`blob.rs`** — `BlobService` — SHA-256 content-addressed HTTP store
- **`mesh_dispatcher.rs`** — Outbound inter-hotel routed-event dispatcher
- **`execution_transport.rs`** — TCP execution-plane listener and point-to-point sender
- **`webrtc_guest.rs`** — WebRTC transceiver for SDP signaling

## Environment Variables

| Variable                              | Default                    | Description                           |
| ------------------------------------- | -------------------------- | ------------------------------------- |
| `PHILOTIC_MESH_PSK`                   | `INSECURE_DEV_DEFAULT_PSK` | Mesh auth key                         |
| `PHILOTIC_HOTEL_PORT`                 | `9000`                     | IPC listen port                       |
| `PHILOTIC_ENABLE_RUST_AUTH`           | `0`                        | 1 = enforce HMAC auth on mesh         |
| `PHILOTIC_ENABLE_RUST_DISPATCHER`     | `0`                        | 1 = start outbound inter-hotel dispatcher |
| `PHILOTIC_ENABLE_RUST_TASK_LIFECYCLE` | `0`                        | 1 = start durable event ledger writer |

## Running

```bash
# Standard start
cargo run -p ansible

# Load initial config
cargo run -p ansible -- --load-config path/to/config.json

# Start the transitional Gemini OAuth flow
cargo run -p ansible -- auth google start --provider gemini --client-id YOUR_CLIENT_ID --project-id YOUR_GCP_PROJECT

# Validate the stored Gemini OAuth path with a real Gemini call
cargo run -p ansible -- auth google validate --provider gemini

# Run the startup text model-controller smoke through the hotel
cargo run -p ansible -- --hotel startup-test-hotel --load-config mesh-config.json --test text-roundtrip --test-text "hello model controller"

# Run the startup Gemini OAuth smoke through the materialized model-controller guest.
# This harness seeds a temporary vaulted bearer token and talks to a local fake Gemini endpoint,
# so it proves the guest-path OAuth contract without depending on live Google.
cargo run -p ansible -- --hotel startup-test-hotel --test gemini-oauth-roundtrip --test-text "oauth-guest-ok"

# Run the startup voice sample through the hotel
cargo run -p ansible -- --hotel startup-test-hotel --load-config mesh-config.json --test voice-sample --test-output /tmp/ansible-startup-voice-sample.mp3 --test-text "Hello from the startup voice test."

# Run the startup Telegram controller smoke through the hotel
cargo run -p ansible -- --hotel startup-test-hotel --test telegram-roundtrip --test-text "hello telegram controller"
```

On macOS, the hotel now uses a Keychain-backed vault root key automatically and creates one on first use if needed. `PHILOTIC_VAULT_MASTER_KEY` remains a bootstrap fallback for non-macOS environments or explicit operator override. `PHILOTIC_VAULT_KEY_ID` can scope the Keychain item label when you want separate local vault roots.

`mesh-config.json` can be a flat object, a top-level `context_graph` object, or a hotel-structured object. The preferred shape is `hotels.<hotel>.agents.<agent>.telegram` plus optional agent-scoped `model`, `context_graph`, and `import_workspace` sections. On startup, Ansible merges top-level shared keys, then `hotels.default`, then the selected hotel's overlay, flattens nested agent telegram/model settings into the current runtime config keys, and uses `import_workspace` to seed the selected agent identity bundle from an OpenClaw-style workspace.

Current inter-hotel transport split:
- UDP mesh beacon remains the control plane for heartbeat, registry gossip, and compact coordination
- routed inter-hotel task execution now uses a point-to-point TCP execution plane once placement resolves a destination hotel

Current startup self-tests include `--test text-roundtrip`, `--test gemini-oauth-roundtrip`, `--test telegram-roundtrip`, and `--test voice-sample`.

## Architecture Reference

See [docs/architecture/ARCHITECTURE.md](../../docs/architecture/ARCHITECTURE.md).
