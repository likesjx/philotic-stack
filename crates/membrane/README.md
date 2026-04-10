# `membrane` — Compatibility Wrapper

`membrane` is a transitional compatibility package that forwards to
`membrane-telegram`.

## Responsibilities

- Preserve compatibility for existing `membrane` binary references during migration

## Boot Sequence

```
membrane::run()
  │
  ├─ PhiloticClient::connect(ansible_port)
  ├─ client.register(GuestIdentity { guest_id: "membrane-telegram-01" })
  ├─ Subscribe to hotel events
  └─ Enter message processing loop
```

## Integration Points

- `membrane-telegram` is the current provider binary that owns the Telegram runtime
- `aiua` should point Telegram hotels at `membrane-telegram`
- Future sibling providers should live beside Telegram, not behind the bare `membrane` name

## Running

`membrane` remains runnable as a compatibility wrapper:

```bash
cargo run -p membrane -- --ansible-port 9000
```

The preferred Telegram provider binary is:

```bash
cargo run -p membrane-telegram -- --ansible-port 9000
```
