This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 🚀 Session Bootstrap

**This protocol is mandatory on every session start — including sessions that resume from a summary or continue mid-task. A context summary is not a substitute for bootstrap. Do not skip these steps.**

Every session MUST begin with these steps in order:
1.  **Read [AGENTS.md](file:///Users/jaredlikes/code/philotic-stack/AGENTS.md)**: Adopt the standing protocol.
2.  **Query the Project Graph**: If the graph server is running (`just intel-graph-ensure`), use MCP tools (`graph_status`, `graph_digest`) for a complete picture of what's in flight. The graph is faster and more complete than reading raw files but is NOT required — agents can work effectively without it. See `$graph-intelligence` skill.
3.  **Orient and Recall**: Run `just session-start` to bootstrap Muninn. Use `$muninn-memory-habit` for cognitive context. The graph gives you structural facts; Muninn gives you learned context.
4.  **Verify Green Status**: Run `just check` and `just test` (or the relevant smoke) to confirm the baseline is stable before editing.
5.  **Record Decisions**: After completing work, use `graph_decide` (MCP) or `phil graph decide` to record what you did and why. This creates the audit trail.

When starting work on a specific proposal or seam, use the graph workflow:
- `graph_next_task` → find the highest-priority unclaimed work
- `graph_context_for` → load proposal + seams + code + verification + diagram in one call
- `session_start` → claim the work so other agents see it on the dashboard
- `graph_impact` → check blast radius before committing
- `session_close` → release the claim when done

If `just session-start` cannot recover Muninn, stop and alert the user/operator immediately. Do not continue with meaningful work until explicit approval is given to proceed without Muninn.

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
just start-aiua             # build + start hotel daemon
just start-gateway          # cargo run -p membrane
just start-agent            # cargo run -p philote
just start-model            # cargo run -p model-router (Gemini/ElevenLabs)

# Parallel workstreams
just workstream-start <slug>    # create sibling worktree
just workstream-status <slug>   # show git status + hot-file overlap
just workstream-overlap <slug>  # show risky overlap vs origin/main

# Project Graph (context engine)
phil graph scan               # scan code, docs, git into the graph
phil graph serve               # start graph server (REST :8900, MCP :8901)
phil graph status              # orientation — counts and proposal pipeline
phil graph proposals           # all proposals with current status
phil graph seams               # all registered seams
phil graph skeleton <crate>    # PlantUML diagram for a crate
phil graph search "<text>"     # full-text search across code and docs

# Intel Graph (managed lifecycle)
just intel-graph-start         # start ONNX sidecar + graph intelligence server
just intel-graph-stop          # stop the intel-graph stack
just intel-graph-status        # check if running
just intel-graph-health        # health check both services
just intel-graph-ui            # open the web UI (http://127.0.0.1:8900)
just intel-graph-agent 60      # start with auto-shutdown after N minutes
```

The hotel daemon requires `mesh-config.json` in root — copy from `mesh-config.example.json`.

## Branch Model

- `develop` — the golden integration edge; all `codex/<slug>` branches PR into `develop`, not `main`.
- `main` — stable; only merged from `develop` when the edge is ready to ship.
- `codex/<slug>` — one per active implementation thread; lives in a sibling worktree.

## Parallel Workstreams

Treat a worktree as the unit of an implementation conversation.

- one active implementation thread -> one `codex/<slug>` branch
- one `codex/<slug>` branch -> one sibling worktree
- do not continue multiple active implementation conversations in the same checkout
- PRs target `develop`; do not merge feature branches directly to `main`

Before touching hot runtime files in a worktree:

```bash
just workstream-status <slug>
```

Before opening a PR from a worktree:

```bash
just workstream-overlap <slug>
```

Hot files include `crates/aiua/src/main.rs`, `crates/aiua/src/service/ipc.rs`, `crates/philote/src/runtime.rs`, `crates/membrane/src/main.rs`, `crates/model-router/*`, `crates/philotic-client/src/lib.rs`, `crates/aiua/README.md`, `docs/task.md`.

## Architecture

The Philotic Stack is a distributed AI agent OS (Rust). Metaphor: **Hotel** (node) **materializes** AI **Guests** (processes). All state lives in a SQLite Context Graph owned by the hotel daemon.

### Crate Map

- `ansible-mesh-core`: Shared primitives, storage traits, mesh types.
- `philotic-client`: Guest SDK (IPC client).
- `aiua`: Hotel daemon (orchestrator).
- `membrane`: Telegram/external protocol gateway guest.
- `philote`: Persona/agent cognitive loop guest.
- `model-router`: Model provider routing guest (Gemini/ElevenLabs).
- `tool-runner`: Seeded/inactive tool execution guest.

### Communication

- **Intra-hotel (IPC/UDS):** Over `/tmp/philotic-aiua.sock`. Newline-framed JSON (`IpcRequest` / `IpcResponse`).
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
- `skills/graph-intelligence/SKILL.md` — full MCP tool reference and agent workflow
- `docs/process/WORKFLOW.md` — SVE operating loop
