# `aiua` — Philotic Hotel Daemon

The `aiua` binary is the authoritative runtime process for a Philotic hotel node.
It is the first process that starts, owns the Context Graph database, materializes
all guest processes, and acts as the routing hub for every interaction inside the hotel.

Source-of-truth note: this README is a convenience overview. For current implemented architecture and transitional seam status, prefer [docs/architecture/ARCHITECTURE_STATUS.md](../../docs/architecture/ARCHITECTURE_STATUS.md). When this file disagrees with code on socket paths, transport ownership, or transitional boundaries, code wins.

## Responsibilities

| Area                      | Detail                                                                           |
| ------------------------- | -------------------------------------------------------------------------------- |
| **Boot orchestration**    | Reads `aiua_context.db`, bootstraps `NodeCapabilities`, seeds demo guests     |
| **Guest materialization** | Spawns all `is_active=1` guests from the DB via `GuestManager`                   |
| **Guest supervision**     | Reconciliation loop every 5s — resurrects dead processes, reclaims inactive ones |
| **IPC server**            | Unix Domain Socket at the active hotel socket path (`PHILOTIC_HOTEL_SOCKET` for guests; commonly `/tmp/philotic-aiua.sock` or `/tmp/philotic-<hotel>.sock`) — handles all guest↔hotel messages |
| **Mesh beacon**           | UDP server on port 8999 — handles inter-hotel `BeaconMessage` traffic            |
| **Execution transport**   | TCP listener on `mesh_port + 2` (often 9002 for the default hotel) — handles point-to-point inter-hotel execution traffic |
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
| `PHILOTIC_HOTEL_SOCKET`               | derived by hotel           | guest-facing IPC socket path exported to materialized guests |
| `PHILOTIC_ENABLE_RUST_AUTH`           | `0`                        | 1 = enforce HMAC auth on mesh         |
| `PHILOTIC_ENABLE_RUST_DISPATCHER`     | `0`                        | 1 = start outbound inter-hotel dispatcher |
| `PHILOTIC_ENABLE_RUST_TASK_LIFECYCLE` | `0`                        | 1 = start durable event ledger writer |

## Running

```bash
# Standard start from the existing hotel DB
cargo run -p aiua -- --hotel default

# Bootstrap or reseed a hotel from config (guests, identities, and config graph)
cargo run -p aiua -- load --file path/to/config.json --hotel default

# Apply config deltas to a long-running hotel's graph without reseeding guests
cargo run -p aiua -- import-config --file path/to/config.json --hotel default

# Start the transitional Gemini OAuth flow
cargo run -p aiua -- auth google start --provider gemini --client-id YOUR_CLIENT_ID --project-id YOUR_GCP_PROJECT

# Validate the stored Gemini OAuth path with a real Gemini call
cargo run -p aiua -- auth google validate --provider gemini

# Store an OpenAI API key in the hotel vault and validate the configured endpoint
cargo run -p aiua -- auth openai start --provider openai --api-key YOUR_OPENAI_KEY --project-id YOUR_OPENAI_PROJECT
cargo run -p aiua -- auth openai validate --provider openai

# Run the startup text model-controller smoke through the hotel
cargo run -p aiua -- --hotel startup-test-hotel --test text-roundtrip --test-text "hello model controller"

# Run the startup Gemini OAuth smoke through the materialized model-controller guest.
# This harness seeds a temporary vaulted bearer token and talks to a local fake Gemini endpoint,
# so it proves the guest-path OAuth contract without depending on live Google.
cargo run -p aiua -- --hotel startup-test-hotel --test gemini-oauth-roundtrip --test-text "oauth-guest-ok"

# Run the startup OpenAI key smoke through the materialized model-controller guest.
# This harness seeds a temporary vaulted OpenAI API key and talks to a local fake OpenAI endpoint,
# so it proves the guest-path key-management contract without depending on live OpenAI.
cargo run -p aiua -- --hotel startup-test-hotel --test openai-roundtrip --test-text "openai-guest-ok"

# Run the startup voice sample through the hotel
cargo run -p aiua -- --hotel startup-test-hotel --test voice-sample --test-output /tmp/aiua-startup-voice-sample.mp3 --test-text "Hello from the startup voice test."

# Run the startup Telegram controller smoke through the hotel
cargo run -p aiua -- --hotel startup-test-hotel --test telegram-roundtrip --test-text "hello telegram controller"
```

On macOS, the hotel now uses a Keychain-backed vault root key automatically and creates one on first use if needed. `PHILOTIC_VAULT_MASTER_KEY` remains a bootstrap fallback for non-macOS environments or explicit operator override. `PHILOTIC_VAULT_KEY_ID` can scope the Keychain item label when you want separate local vault roots.

`mesh-config.json` can be a flat object, a top-level `context_graph` object, or a hotel-structured object. The preferred shape is `hotels.<hotel>.agents.<agent>.telegram` plus optional agent-scoped `model`, `context_graph`, and `import_workspace` sections. `aiua load --file ... --hotel ...` is the bootstrap/provisioning path: it merges shared keys plus the selected hotel's overlay, seeds guest/runtime records, and writes agent identity bundles. `aiua import-config --file ... --hotel ...` is the stable delta path for a long-running hotel: it updates graph config keys and agent identity bundles without reseeding guests or treating startup like configuration management.

Some log/help text and comments still say `Ansible` for historical reasons. The runtime/component naming in the current repo is Philotic, with `aiua` as the hotel authority.

Current inter-hotel transport split:
- UDP mesh beacon remains the control plane for heartbeat, registry gossip, and compact coordination
- routed inter-hotel task execution now uses a point-to-point TCP execution plane once placement resolves a destination hotel

Current startup self-tests include `--test text-roundtrip`, `--test gemini-oauth-roundtrip`, `--test telegram-roundtrip`, and `--test voice-sample`.

For watched local UAT, prefer rebuilding the materialized guest binaries first and clearing old local processes/sockets. The repo now provides:

- `just build-runtime`
- `just kill-local-stack`
- `just start-ansible-clean <hotel>`

That avoids the particular comedy where tests pass on fresh libraries but the hotel is still materializing yesterday's binaries.

## Architecture Reference

See [docs/architecture/ARCHITECTURE.md](../../docs/architecture/ARCHITECTURE.md).
