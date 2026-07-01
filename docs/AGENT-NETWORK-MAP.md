# Agent Network Map

> Single overview of how the Philotic agent network actually runs — both layers.
> Generated as an orientation aid; the authoritative protocol remains `AGENTS.md`.

There are two distinct "agent networks" in this repo, and they should not be confused:

1. **The product mesh** — the Philotic Web itself: `aiua` hotels materializing guest
   agents, linked into a cryptographic mesh. This is what you ship.
2. **The dev orchestration** — the external AI coding tools (Codex, Gemini/Jules,
   Claude Code, Devin, Cursor, Antigravity) that build the product, coordinated through
   a shared protocol. This is how you ship.

This document maps both and shows where they connect (the graph + Muninn).

---

## Layer 1 — The Product Mesh (runtime)

**Metaphor:** a *Hotel* (node) *materializes* AI *Guests* (processes). All state lives in a
SQLite Context Graph owned by the hotel daemon. Hotels link into the *Philotic Web*.

### Crates / binaries

| Binary | Role |
|---|---|
| `aiua` | Hotel daemon — canonical context owner, supervises all guests |
| `philotic-web` (`phil`) | Operator CLI — `phil init / start / status / graph …` |
| `philote` | Agent core — cognitive loop, sessions, roles |
| `membrane` | Telegram / external protocol gateway guest |
| `model-router` | LLM + ElevenLabs inference routing guest |
| `tool-runner` | Sandboxed tool execution guest (seeded/inactive) |
| `graph-intelligence` | Project intelligence graph — scanners, MCP server, PlantUML |
| `ansible-mesh-core` | Shared primitives, storage traits, mesh types |
| `philotic-client` | Guest SDK (IPC client) |

### Communication

- **Intra-hotel (IPC/UDS):** `/tmp/philotic-aiua.sock`, newline-framed JSON.
- **Inter-hotel (Mesh/UDP):** `BeaconMessage` on port 8999 (HMAC-PSK optional).
- **Blob store (HTTP):** large payloads on :9001.

### Live runtime inventory (`~/.philotic`)

Observed profiles, each its own hotel state dir with `aiua.pid` + sockets:

| Profile | Notable state |
|---|---|
| `jane` | `aiua-mac-jane.sock`, `aiua-mbp-jane.sock` (live), agent-graphs, blobs |
| `bjork` | `aiua-mac-jane.sock`, `bjork.db`, context/training DBs |
| `codex-live` | `context.db`, blobs — Codex-driven hotel |
| `default` | `context.db`, `router_traces.db` |

`mesh-config.json` declares agents `vps-jane`, `mbp-jane`, `mac-jane` across nodes.
Profiles select socket paths via `PHILOTIC_PROFILE` → `~/.philotic/<profile>/aiua-<hotel>.sock`.

---

## Layer 2 — The Dev Orchestration (build-time)

External coding agents coordinate through one shared protocol plus per-tool bootstrap files.
`AGENTS.md` is the cross-tool standard read natively by Codex, Cursor, Devin, Zed, and
Antigravity; the per-tool files add only bootstrap + tool-specific deltas.

### Role → tool → config matrix

| Role | Primary tool | Harness / config | Bootstrap file |
|---|---|---|---|
| Architect | Gemini | `.agents/workflows/gemini-architect-harness.md` | `GEMINI.md` |
| Implementer | Codex / Gemini | `.agents/workflows/gemini-implementer-harness.md`, `.codex/config.toml` | `CODEX.md` / `GEMINI.md` |
| Reviewer | Gemini | `.agents/workflows/gemini-reviewer-harness.md` | `GEMINI.md` |
| Verifier | Gemini | `.agents/workflows/gemini-verifier-harness.md` | `GEMINI.md` |
| Agentic IDE | Antigravity | `.agents/workflows/gemini-antigravity-harness.md` → `~/.gemini/antigravity/philotic/harnesses/…` | `GEMINI.md` |
| Interactive | Claude Code | `.claude/` | `CLAUDE.md` |
| Autonomous PR | Devin | `.devin/{rules,skills,workflows}` | `AGENTS.md` |
| Editor | Cursor | *(reads `AGENTS.md` natively — no dedicated harness yet)* | `AGENTS.md` |

### The standard development loop (from `AGENTS.md` + bootstrap files)

Every agent, every session, in order:

1. **Read `AGENTS.md`** — adopt the standing protocol.
2. **Query the graph** (optional) — `graph_status`, `graph_digest`, `graph_next_task`.
3. **Orient & recall** — `just session-start` → Muninn triad (self / user / topic).
   If Muninn can't bootstrap: **stop** and get operator approval.
4. **Verify green** — `just check` + `just test` before editing.
5. **Work the slice** — smallest honest slice; one worktree per thread.
6. **Record** — `graph_decide` (audit) + `muninn_remember` (durable delta only).

### Handoff / branch flow

```
codex/<slug>  (one per worktree)  ──PR──▶  develop  ──when stable──▶  main
```

- One active implementation thread → one `codex/<slug>` branch → one sibling worktree.
- Never merge feature branches directly to `main`.
- Worktree helpers: `just workstream-start|status|overlap <slug>`.

### Shared connective tissue (where Layer 1 meets Layer 2)

- **Project graph** — `phil graph` / intel-graph MCP at `http://127.0.0.1:8901/mcp`,
  REST at `:8900`. Structural truth: code, proposals, seams, sessions, decisions.
- **Muninn** — cognitive memory (the triad). Learned context, not a task tracker.
- **MCP servers** (`.mcp.json`): `intel-graph` (http), `graphify` (stdio).
- **Skills** — `skills/graph-intelligence/SKILL.md` (full MCP tool reference) and repo-local skills.

---

## File topology — who owns what (source-of-truth map)

| File | Intended role | Should contain |
|---|---|---|
| `AGENTS.md` | **Protocol source of truth** | Principles, slice contract, commit discipline, parallel-workstream rules. Cross-tool. |
| `CLAUDE.md` | **Canonical repo map + commands** (AGENTS.md §1 points here) | Crate map, command inventory, architecture, branch model. |
| `CODEX.md` | Codex bootstrap | Codex-specific session start + deltas; *defer* map/convention to AGENTS.md/CLAUDE.md. |
| `GEMINI.md` | Gemini bootstrap | Gemini-specific session start + deltas; *defer* map/convention. |
| `.agents/workflows/*` | Role harnesses | Architect / implementer / reviewer / verifier / antigravity profiles. |
| `.devin/`, `.codex/`, `.claude/` | Per-tool local config | Tool-native settings, rules, projects. |
| `mesh-config.json` | Runtime mesh | Hotels, agents, models, fallback tiers. |

---

## Tightening backlog (drift found 2026-06-25)

1. **[fixed] `CLAUDE.md` crate map was missing `graph-intelligence`** — it is the
   canonical map (AGENTS.md §1 delegates the repo map to it), yet it listed 7 crates
   while `CODEX.md`/`GEMINI.md` list 8. Corrected to 9 (added `graph-intelligence`,
   plus the already-present `ansible-mesh-core`/`philotic-client`).
2. **`CODEX.md` and `GEMINI.md` are ~95% identical and duplicate** the crate map,
   architecture, communication, and commit-convention sections that `AGENTS.md` intends
   to live in `CLAUDE.md`/`AGENTS.md`. Triplicated content is the drift engine that
   produced #1. **Proposal:** slim both to bootstrap + tool-specific deltas + a pointer
   to `CLAUDE.md`/`AGENTS.md` for the shared map and convention.
3. **Command-inventory drift** — `CLAUDE.md` documents `intel-graph-ui` /
   `intel-graph-agent`; `CODEX.md`/`GEMINI.md` reference `intel-graph-ensure` (only
   documented in `AGENTS.md`). Consolidate the command list in one place.
4. **Cursor and OpenClaw are unwired** — Cursor reads `AGENTS.md` natively but has no
   role harness; OpenClaw (your chat-driven dispatcher) isn't a documented entry point
   for kicking off agent runs. Out of scope for this pass; noted for later.
5. **Stale worktrees** — `codex/fix-toolset-profile-test-fixture` and
   `codex/life-recall-feedback-rollout` show as `prunable`. Prune or land to `develop`.
