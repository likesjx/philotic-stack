# The Philotic Stack

A Service-Oriented Architecture (SOA) implementation mapping to the "Ansible" & "Philotic Web" metaphors from Speaker for the Dead.

## Architecture

This mesh connects independently compiled intelligent nodes over UDP/IPC:

- **ansible**: The Host Daemon & Hotel Manager. Materializes capabilities and maintains the Context Graph.
- **hegemon**: The Gateway. Listens to Telegram/edge protocols and translates to Philotic tasks.
- **agent-core**: The Persona. Materialized agent intelligence running a REPL loop.
- **model-router**: The Mind. Processes and routes context to cloud APIs (Gemini).

## Getting Started

1. Copy `mesh-config.example.json` to `mesh-config.json` and provide your API keys.
2. Run the Ansible daemon:

```bash
cargo run -p ansible -- --load-config mesh-config.json
```
