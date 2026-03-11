# `membrane` — Telegram/External Gateway Guest

`membrane` is a materialized guest process that acts as the external gateway
for the Philotic hotel — currently handling Telegram bot integration and
inbound message routing.

## Responsibilities

- Register with the hotel via `PhiloticClient`
- Accept inbound Telegram/external messages
- Route them as `EventEnvelope`s into the hotel ledger
- Handle model routing requests for conversation responses
- Periodically sync session memory apartments

## Boot Sequence

```
main()
  │
  ├─ PhiloticClient::connect(ansible_port)
  ├─ client.register(GuestIdentity { guest_id: "membrane-telegram-01" })
  ├─ Subscribe to hotel events
  └─ Enter message processing loop
```

## Integration Points

- Talks to `ansible` hotel via local IPC (UDS/UDP loopback)
- May publish events for `agent-core` or `model-router` via the hotel
- Uses `client.sync_apartment()` to persist conversation state

## Running

Spawned automatically by `GuestManager` when `is_active=1` in the Context Graph.
Can also be run standalone for development:

```bash
cargo run -p membrane -- --ansible-port 9000
```
