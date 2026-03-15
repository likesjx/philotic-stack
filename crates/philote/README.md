# `philote` — Persona Agent Guest

`philote` is a materialized guest process that runs a persistent AI agent
(persona) within a hotel. It represents the cognitive loop of a named agent identity.

## Responsibilities

- Register with the hotel (identity: `agent-jane-01`)
- Receive task invocations from the hotel ledger
- Execute local reasoning (tool calls, model invocations)
- Reply with `TaskResult` events via `publish_event`
- Sync memory to the hotel via `sync_apartment` (short, long, episodic, semantic)

## Memory Apartment Pattern

```rust
// After updating local RAM state:
client.sync_apartment(
    "agent-jane-01",
    "short",   // short | long | episodic | semantic
    &json!({ "recent_context": [...] })
).await?;
// Non-blocking — hotel will LWW-upsert in the background
```

## Running

Spawned automatically by `GuestManager`. For development:

```bash
cargo run -p philote -- --ansible-port 9000
```
