# The Philotic Stack

A distributed AI agent operating system built in Rust — modeled after the "Ansible" and "Philotic Web" metaphors from _Speaker for the Dead_.

Each hotel is an autonomous, Rust-powered node that materializes AI guest processes, persists state in a local Context Graph, and communicates with peer hotels over a durable UDP mesh.

## Quick Start

```bash
# 1. Copy and configure
cp mesh-config.example.json mesh-config.json
# Edit mesh-config.json with your API keys and node identity

# 2. Start the hotel daemon (materializes all guests automatically)
cargo run -p ansible -- --load-config mesh-config.json
```

## Architecture Diagrams

### Target Architecture

![Target Architecture](docs/target_architecture.svg)

### Implementation Status

![Implementation Status](docs/implementation_status.svg)

> **Legend** — 🟢 Implemented · 🟡 In progress / flag-gated · 🟠 Scaffolded / blocked · ⚫ Planned / docs only

Interactive HTML version (with hover tooltips): [`docs/philotic-architecture-diagram.html`](docs/philotic-architecture-diagram.html)

## Crates

| Crate                                                     | Role                                                       |
| --------------------------------------------------------- | ---------------------------------------------------------- |
| [`ansible`](crates/ansible/README.md)                     | Hotel daemon — guest materialization, IPC, mesh routing    |
| [`ansible-mesh-core`](crates/ansible-mesh-core/README.md) | Core primitives, traits, event types, storage abstractions |
| [`philotic-client`](crates/philotic-client/README.md)     | Guest SDK — IPC client for hotel communication             |
| [`hegemon`](crates/hegemon/README.md)                     | Telegram/external protocol gateway guest                   |
| [`agent-core`](crates/agent-core/README.md)               | Persona/agent cognitive loop guest                         |
| [`model-router`](crates/model-router/README.md)           | LLM model provider routing guest                           |

## Key Design Principles

- **Hotel = source of truth.** The Context Graph SQLite DB owns all state.
- **Security first, especially at the perimeter.** External communication surfaces should default to minimal trust, narrow authority, and explicit policy.
- **IPC for intra-hotel.** All local communication uses Unix Domain Sockets.
- **Mesh for inter-hotel.** All cross-machine communication is event-based UDP with durable delivery.
- **Storage is swappable.** All persistence goes through `Arc<dyn GraphStorage>` — SQLite today, PebbleDB tomorrow.
- **Guests are crash-safe.** The supervisor loop auto-respawns dead guests every 5s.
- **Memory is eventually consistent.** Guests write optimistically; the hotel resolves conflicts via Last-Writer-Wins CRDT.

## Documentation

- **[Full Architecture Reference](docs/architecture/ARCHITECTURE.md)** — complete system design, data flows, component reference
- **[Port Blueprint](docs/architecture/PORT_BLUEPRINT.md)** — migration plan from legacy OpenClaw Ansible plugin model
