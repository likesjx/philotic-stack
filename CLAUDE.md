This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 🚀 Session Bootstrap

Every session MUST begin with these three steps:
1.  **Read [AGENTS.md](file:///Users/jaredlikes/code/philotic-stack/AGENTS.md)**: Adopt the standing protocol.
2.  **Verify Green Status**: Run `just check` and `just test` (or the relevant smoke) to ensure the baseline is stable before editing.
3.  **Orient and Recall**: Run `just session-start` first. Use the `$muninn-memory-habit` and `$proposal-maintainer` mindset. Retrieve project context via Muninn and align your plan with the current `Disposition` of active architecture proposals and [docs/task.md](file:///Users/jaredlikes/code/philotic-stack/docs/task.md).

If `just session-start` fails, stop and alert the user/operator immediately. Do not continue with meaningful work until explicit approval is given to proceed without Muninn.

## Commands

```bash
# Build & Check
just build                  # cargo build --workspace
just check                  # cargo check --workspace (faster)
just format                 # cargo fmt --all
just session-start          # require Muninn bootstrap before meaningful work

# Test
just test                   # cargo test --workspace
cargo test -p <crate>       # test a single crate

# Run (requires mesh-config.json)
just start-ansible          # build + start hotel daemon
just start-gateway          # cargo run -p membrane
just start-agent            # cargo run -p agent-core
just start-model            # cargo run -p model-router (Gemini/ElevenLabs)

# Parallel workstreams
just workstream-start <slug>    # create sibling worktree
just workstream-status <slug>   # show git status + hot-file overlap
just workstream-overlap <slug>  # show risky overlap vs origin/main
```

The hotel daemon requires `mesh-config.json` in root — copy from `mesh-config.example.json`.

## Parallel Workstreams

Treat a worktree as the unit of an implementation conversation. See [docs/operations/parallel-worktree-runbook.md](docs/operations/parallel-worktree-runbook.md).

- One active conversation -> one sibling worktree.
- Hot files: `ansible/src/main.rs`, `ansible/src/service/ipc.rs`, `agent-core/src/runtime.rs`, `membrane/src/main.rs`, `philotic-client/src/lib.rs`, `docs/task.md`.

## Architecture

The Philotic Stack is a distributed AI agent OS (Rust). Metaphor: **Hotel** (node) **materializes** AI **Guests** (processes). All state lives in a SQLite Context Graph owned by the hotel daemon.

### Crate Map

- `ansible-mesh-core`: Shared primitives, storage traits, mesh types.
- `philotic-client`: Guest SDK (IPC client).
- `ansible`: Hotel daemon (orchestrator).
- `membrane`: Telegram/external protocol gateway guest.
- `agent-core`: Persona/agent cognitive loop guest.
- `model-router`: Model provider routing guest (Gemini/ElevenLabs).
- `tool-runner`: Seeded/inactive tool execution guest.

### Communication

- **Intra-hotel (IPC/UDS):** Over `/tmp/philotic-ansible.sock`. Newline-framed JSON (`IpcRequest` / `IpcResponse`).
- **Inter-hotel (Mesh/UDP):** `BeaconMessage` on port 8999 (HMAC-PSK optional).
- **Blob store (HTTP):** Large payloads over :9001.

### Storage Traits

All consumers hold `Arc<dyn XxxStorage>`:

| Trait | Target |
|---|---|
| `GraphStorage` | `node_config`, `guests`, `apartments` |
| `EventStorage` | `mesh_events` — append-only event ledger |
| `CursorStorage` | `mesh_cursors` — per-node ACK cursors |

### Conventions

- **Guest Lifecycle**: Hotel reads `materialized_guests`, `GuestManager` spawns/supervises as subprocesses. Respawn 5s.
- **Memory Consistency**: Optimistic / Last-Writer-Wins (LWW).
- **Diagnostics**: [docs/philotic-architecture-diagram.html](docs/philotic-architecture-diagram.html) (interactive diagram).

## Reference Docs

- `docs/architecture/ARCHITECTURE.md` — system design & data flows
- `docs/architecture/PORT_BLUEPRINT.md` — migration blueprint
- `README.md` — status overview & diagram gallery
- `AGENTS.md` — standing protocol for coding agents
