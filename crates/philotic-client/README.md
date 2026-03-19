# `philotic-client` — Guest SDK

The `philotic-client` crate is the SDK that guest processes use to register
with and communicate with their local hotel daemon over Unix Domain Sockets.

## Usage

```rust
use philotic_client::{GuestIdentity, PhiloticClient};

let identity = GuestIdentity {
    guest_id: "my-agent-01".into(),
    role: "agent".into(),
};

// Connect and register with the hotel
let client = PhiloticClient::connect(identity).await?;

// Publish an event into the hotel's durable ledger
client.publish_event(envelope).await?;

// Optimistically sync memory apartment (fire-and-forget, CRDT LWW)
client.sync_apartment("my-agent-01", "short", &json_value).await?;
```

## IPC Message Protocol

All messages are JSON-encoded and exchanged over the hotel's Unix domain socket.
`PhiloticClient` resolves the socket from `PHILOTIC_HOTEL_SOCKET`, defaulting to
`/tmp/philotic-aiua.sock` when the environment variable is absent.

Source-of-truth note: some older crate READMEs and CLI help text still mention
port-oriented `ansible` IPC. The current client contract is socket-path-driven.

### Requests (guest → hotel)

| Variant                                            | Payload              | Description                    |
| -------------------------------------------------- | -------------------- | ------------------------------ |
| `Register(GuestIdentity)`                          | `{ guest_id, role }` | Announce guest identity        |
| `PublishEvent(EventEnvelope)`                      | Full envelope        | Durably append to event ledger |
| `Heartbeat`                                        | —                    | Liveness ping                  |
| `SyncApartment { agent_id, memory_type, content }` | JSON value           | Optimistic memory write (LWW)  |

### Responses (hotel → guest)

| Variant                                                        | Payload | Description                                  |
| -------------------------------------------------------------- | ------- | -------------------------------------------- |
| `Registered`                                                   | —       | Registration confirmed                       |
| `EventAccepted { seq }`                                        | u64     | Durable write confirmed, seq number assigned |
| `HeartbeatAck`                                                 | —       | Liveness confirmed                           |
| `ApartmentUpdate { agent_id, memory_type, canonical_content }` | JSON    | Hotel override push                          |
| `Error(String)`                                                | message | Request failed                               |

## Notes

- `PhiloticClient` keeps the `GuestIdentity` for future IPC headers.
- `sync_apartment` is fire-and-forget — the guest does not wait for the hotel's
  ACK before continuing its cognitive loop.
- `ApartmentUpdate` responses can arrive unprompted when the hotel detects
  a state conflict that requires resolution (CRDT override).
