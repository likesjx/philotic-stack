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
| **Outbound dispatch**     | Polls `EventLedger`, ships unacked events to peer hotels over UDP                |
| **Blob service**          | HTTP server on port 9001 — content-addressed large payload store                 |
| **State sync**            | `SyncApartment` IPC → LWW upsert into `memory_apartments`                        |

## Key Services (`src/service/`)

- **`ipc.rs`** — `IpcServer` — Unix socket server, routes to `GraphStorage`
- **`guest_manager.rs`** — `GuestManager` + `LocalProcessMaterializer` + supervision loop
- **`blob.rs`** — `BlobService` — SHA-256 content-addressed HTTP store
- **`mesh_dispatcher.rs`** — Outbound UDP event dispatcher
- **`webrtc_guest.rs`** — WebRTC transceiver for SDP signaling

## Environment Variables

| Variable                              | Default                    | Description                           |
| ------------------------------------- | -------------------------- | ------------------------------------- |
| `PHILOTIC_MESH_PSK`                   | `INSECURE_DEV_DEFAULT_PSK` | Mesh auth key                         |
| `PHILOTIC_HOTEL_PORT`                 | `9000`                     | IPC listen port                       |
| `PHILOTIC_ENABLE_RUST_AUTH`           | `0`                        | 1 = enforce HMAC auth on mesh         |
| `PHILOTIC_ENABLE_RUST_DISPATCHER`     | `0`                        | 1 = start outbound UDP dispatcher     |
| `PHILOTIC_ENABLE_RUST_TASK_LIFECYCLE` | `0`                        | 1 = start durable event ledger writer |

## Running

```bash
# Standard start
cargo run -p ansible -- --hotel local-hotel

# Load initial config
cargo run -p ansible -- --hotel local-hotel --load-config path/to/config.json
```

### Startup Smoke: Hotel-Routed ElevenLabs Voice Sample

```bash
cargo run -p ansible -- \
  --hotel startup-test-hotel \
  --load-config mesh-config.json \
  --test voice-sample \
  --test-output /tmp/ansible-startup-voice-sample.mp3 \
  --test-text "Hello from the Philotic startup test."
```

### Startup Smoke: Hotel-Routed Text Round-Trip

```bash
cargo run -p ansible -- \
  --hotel startup-test-hotel \
  --load-config mesh-config.json \
  --test text-roundtrip \
  --test-text "hello model controller"
```

### Startup Smoke: Telegram Controller Round-Trip

```bash
cargo run -p ansible -- \
  --hotel startup-test-hotel \
  --load-config mesh-config.json \
  --test telegram-roundtrip \
  --test-text "hello telegram controller"
```

`--load-config` accepts either a flat JSON object for backward compatibility or a
top-level `context_graph` object whose keys are injected into `node_config`. The
structured form is preferred for secrets like `telegram_bot_token`,
`gemini_api_key`, `elevenlabs_api_key`, and `elevenlabs_voice_id`.

`--test text-roundtrip`, `--test telegram-roundtrip`, and `--test voice-sample`
are startup self-tests. They build the required guest binaries, boot the hotel,
route a task through the hotel IPC plane, verify the reply, and then shut the
hotel down. The Telegram smoke points `hegemon` at a local fake Telegram API and
asserts that the controller sends the expected `sendMessage` reply. The voice
test also forces inline audio for the `model.elevenlabs` guest and writes the
returned MP3.

## Architecture Reference

See [docs/architecture/ARCHITECTURE.md](../../docs/architecture/ARCHITECTURE.md).
