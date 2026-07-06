# `membrane` — Membrane Runtime SDK

`membrane` is an SDK-only library crate. It provides the base primitives for
protocol gateway guests (`MembraneRuntime`, `MembraneContext`, envelope and
lease helpers) and is consumed by the concrete gateway binaries.

This crate no longer ships a standalone `membrane` binary. The dead
compatibility wrapper was retired once the unified `membrane-telegram` gateway
passed live operator-turn and approval-button verification.

## Responsibilities

- Expose `MembraneRuntime` / `MembraneContext` and the `MembraneGuest` trait
- Provide shared envelope (`InboundEnvelope`, `OutboundReply`, `SenderInfo`) and
  lease primitives for gateway guests

## Consumers

- `membrane-telegram` — the live Telegram gateway binary (materialized by every hotel)
- `membrane-mcp` — MCP protocol gateway
- `membrane-discord` — Discord protocol gateway

## Usage

Import the crate and implement `MembraneGuest` to build a new protocol gateway:

```rust
use membrane::{MembraneRuntime, MembraneContext};
```

To run the live Telegram gateway:

```bash
cargo run -p membrane-telegram -- --ansible-port 9000
```
