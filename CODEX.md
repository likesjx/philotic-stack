# CODEX.md — OpenAI Codex Agent Instructions

This file provides guidance to OpenAI Codex when working with code in this repository.

## Session Bootstrap

**This protocol is mandatory on every session start.**

1. **Read [AGENTS.md](/AGENTS.md)**: Adopt the standing protocol — principles, slice contract, commit discipline.
2. **Query the Project Graph** (optional): If the graph server is running (`just intel-graph-ensure`), use MCP tools or the REST API for orientation. The graph is faster and more complete than reading raw files but is NOT required — agents can work effectively without it by reading raw files and docs.
   - `graph_status` → node/edge counts, proposal pipeline
   - `graph_digest` → compressed domain→proposal→seam→verification overview
   - `graph_next_task` → scored work recommendation with conflict avoidance
3. **Orient and Recall**: Run `just session-start` or `python3 scripts/muninn_mcp.py bootstrap`, then use `$muninn-memory-habit` for the Muninn triad: self, user, topic. If Muninn cannot bootstrap, stop and require explicit operator approval before meaningful work.
   - Trusted local Codex clients may use the `muninn-local` MCP server in `.mcp.json`; it runs `muninn mcp` against the loopback Muninn listener.
4. **Verify Green Status**: Run `just check` and `cargo test --workspace` to confirm the baseline is stable.
5. **Record Decisions**: After completing work, use `graph_decide` for graph audit and `muninn_decide` / `muninn_remember` for the durable memory delta.

## Graph Workflow

When starting work on a specific proposal or seam:

```
1. graph_next_task           → find highest-priority unclaimed work
2. graph_context_for         → load proposal + seams + code + verification + diagram in one call
3. session_start             → claim the work, visible on dashboard
4. ... do the work ...
5. graph_impact              → check blast radius before committing
6. graph_decide              → record what you did and why
7. session_close             → release the claim when done
8. graph_scan                → update graph (auto-persists PlantUML diagrams)
```

MCP endpoint: `http://127.0.0.1:8901/mcp`
REST API: `http://127.0.0.1:8900`
See `skills/graph-intelligence/SKILL.md` for full tool reference.

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

# Intel Graph (managed lifecycle)
just intel-graph-start      # start ONNX sidecar + graph intelligence server
just intel-graph-stop       # stop the intel-graph stack
just intel-graph-status     # check if running
just intel-graph-health     # health check both services

# Parallel workstreams
just workstream-start <slug>    # create sibling worktree
just workstream-status <slug>   # show git status + hot-file overlap
just workstream-overlap <slug>  # show risky overlap vs origin/main
```

## Branch Model

- `develop` — the golden integration edge; all `codex/<slug>` branches PR into `develop`, not `main`.
- `main` — stable; only merged from `develop` when the edge is ready to ship.
- `codex/<slug>` — one per active implementation thread; lives in a sibling worktree.

Never merge feature branches directly to `main`.

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
- `graph-intelligence`: Project intelligence graph — code scanner, doc scanner, MCP server, PlantUML generation.

### Communication

- **Intra-hotel (IPC/UDS):** Over `/tmp/philotic-aiua.sock`. Newline-framed JSON.
- **Inter-hotel (Mesh/UDP):** `BeaconMessage` on port 8999 (HMAC-PSK optional).
- **Blob store (HTTP):** Large payloads over :9001.

## Commit Convention

```
type(scope): short description
```

**Types**: `feat`, `fix`, `chore`, `ops`, `docs`, `refactor`, `test`, `perf`
**Scope**: crate or area — `aiua`, `membrane`, `philote`, `model-router`, `philotic-web`, `ansible-mesh-core`, `phil`, `skills`

Trailers (additive — include what applies):

| Trailer | When |
|---|---|
| `Slice: codex/<slug>` | Always, when on a named workstream |
| `Seam: <seam-id>` | When touching a known seam boundary |
| `Verified: <level>` | Always: `test-green`, `smoke-green`, `watched-live-green`, `check-only` |

## Muninn Memory Delta

At the end of meaningful work, store only durable continuity handles:

- decisions with rationale
- reality gaps between assumption and observed truth
- validation outcomes
- next seams
- stable operator preferences

Do not store transcripts, logs, or task-list churn. Muninn answers "why this matters next time"; repo docs/code answer "what is true."

## Key References

- `AGENTS.md` — standing protocol for all coding agents
- `skills/graph-intelligence/SKILL.md` — full MCP tool reference and agent workflow
- `docs/architecture/ARCHITECTURE.md` — system design & data flows
- `docs/process/WORKFLOW.md` — SVE operating loop
- `docs/task.md` — active execution surface
- `docs/reference/MUNINN_DIRECT_CLIENT_ACCESS.md` — private native Muninn MCP access for trusted clients
