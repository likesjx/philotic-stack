---
title: Muninn Memory Protocol Proposal
doc_type: proposal
domain: memory-context
status: accepted-current-slice
last_updated: 2026-03-31
tags:
- muninn
- memory
- protocol
- continuity
- active-seam
related_docs:
- ARCHITECTURE_STATUS.md
- MUNINN_CLIENT_MEMORY_PROTOCOL.md
- AGENT_WORKFLOW_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: muninn-memory-protocol
implements: []
implemented_by:
- muninn-helper-and-skill-slice
active_seams:
- wider-client-adoption
- philotic-native-memory-integration
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Muninn Memory Protocol Proposal

## Goal

Define a standard memory protocol for Philotic and other cognitive clients so Muninn can be evaluated as a real continuity substrate instead of a sporadically used sidecar.

This proposal covers:

- the default retrieve/write habit
- the minimum tool contract clients should support
- where client-specific instructions end and shared infrastructure begins
- how to operationalize Muninn across multiple agent clients

Track implementation in [task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Disposition

Accepted for the current slice and pinned as a separate work item.

Implemented so far:

- Muninn MCP is configured for Codex at `http://localhost:8750/mcp`
- the proper MCP handshake has been validated end to end
- global Codex instructions now default to Muninn retrieval/write-back for meaningful work
- a shareable client protocol doc exists
- a repo-local helper script exists to remove handshake ceremony

Still pending:

- Philotic-native integration
- automatic helper usage in every client runtime
- retrieval quality and behavior evaluation over time
- hard fail/approval-gate behavior in every client when Muninn bootstrap is unavailable
- validate whether short atomic memories plus a lightweight tag vocabulary actually improve retrieval quality enough to justify deeper agent-memory use

Progress since acceptance:

- OpenCode integration landed (muninn init wizard now configures OpenCode natively)
- SDK Stage 6 wire-format audit complete — Python, Node, Go SDKs now have full test suites and bug fixes
- `muninn_read` now returns `entities` + `entity_relationships` in response, useful for Philotic entity graph awareness
- `--listen-host` / `--cors-origins` flags landed, enabling binding to `0.0.0.0` for remote agent access — this unblocks distributed deployment
- `muninn exec` one-shot CLI subcommand added (Stage 4) — agents can write/recall without holding a persistent connection
- `muninn.Open()` embedded Go API added (Stage 3) — direct in-process embedding is now viable, changes the native port calculus

Observed reality gap (resolved):

- ~~the current `muninn_remember` / `muninn_decide` responses are echoing back an empty `concept` field even when one is provided~~ — **fixed** in upstream PR #179 (v0.3.15-alpha). Concept field now populates correctly in remember responses.
- ~~`muninn_read` returns numeric state string instead of label~~ — **fixed** in upstream PR #249 (v0.4.2-alpha). State is now a human-readable label (e.g. `"active"`).

This proposal now has three concrete artifacts behind it:

- a shared protocol reference in [MUNINN_CLIENT_MEMORY_PROTOCOL.md](/Users/jaredlikes/code/philotic-stack/docs/reference/MUNINN_CLIENT_MEMORY_PROTOCOL.md)
- a shared helper in [muninn_mcp.py](/Users/jaredlikes/code/philotic-stack/scripts/muninn_mcp.py)
- a shareable skill package in [SKILL.md](/Users/jaredlikes/code/philotic-stack/skills/muninn-memory-protocol/SKILL.md)
- a bootstrap path that should attempt local Muninn recovery before requiring operator approval to continue without memory

## Core Recommendation

Treat Muninn/Mempalace as a shared memory protocol orchestrated entirely by the **`intel-graph` Coding Management Plane**.

The core shift is from **Ceremonial Habits** to **100% Reflexive Operations**:

1. **Coding Agents (Operator/CLI Agents)**: They no longer use an active `muninn-memory-habit` script. Instead, they use IDE-native `Save`/`PreCompact` hooks that reflexively push conversation turns directly to the `intel-graph` REST API. The graph automatically isolates these records into the agent's dedicated Mempalace `Wing`.
2. **Philotic Engine (Internal Agents)**: Internal `philote` processes remain bounded by strict runtime context management. They do not leak their state into the coding agent's semantic history.
3. **Unified Intelligence**: The `intel-graph` serves as the sole broker. When an admin or architect philote needs systemic insight, the graph provides a unified response spanning the deterministic SQLite codebase and the semantic Mempalace histories.

That means:

- the memory habit is completely automatic (100% reflex) for the coding layer.
- the transport plumbing avoids fragmenting storage logic.
- the `intel-graph` API abstracts the Mempalace implementation entirely.

## Memory Triad

Clients should organize retrieval around three questions:

1. Who am I?
- identity
- stable operating posture
- collaboration style

2. Who am I talking to?
- user preferences
- relationship fit
- recurring collaboration patterns

3. What matters about this topic right now?
- active goals
- recent decisions
- relevant constraints
- unresolved seams

This triad is simple enough to share across clients without forcing all of them into the same personality model.

### The Reflexive Write

Manual write-backs via `muninn_remember` and `muninn_decide` are deprecated for coding agents.

Instead, the `intel-graph` hooks into the execution lifecycle via IDE `Save` and `PreCompact` hooks. At the end of every conversation turn, the raw `WorkingTurn` delta is automatically posted to the `intel-graph` API, which pipes it natively into the agent's partitioned Mempalace instance.

This effectively means the memory overhead costs 0 execution tokens locally and requires no explicit tool invocation.

Good write-back candidates:

- architecture decisions
- collaboration preferences
- workflow learnings
- active project pivots
- explicit future reminders

Bad write-back candidates:

- low-signal pleasantries
- raw transcript dumps
- implementation noise with no durable value

### Size Discipline

Muninn memories should stay short enough to remain crisp retrieval artifacts rather than miniature documents.

Recommended starting policy:

- `remember`: 1-3 sentences, ideally under ~300 characters, hard ceiling ~500
- `decide`: concise rationale, ideally under ~500 characters, hard ceiling ~800

If this feels too small for a thought, that is usually a sign the thought should be split into several atomic memories instead.

### Tag Discipline

Tagging should remain minimal and experimental.

Recommended first vocabulary:

- `flesh-out`
- `decision`
- `reality-gap`
- `validation`
- `follow-up`
- `operator-preference`

The experiment is not “can we invent a better taxonomy.”

The experiment is whether a small number of tags improves retrieval enough to help continuity without creating tagging theater.

## Shared Client Contract

Every client that wants to participate should support at least:

- `where_left_off`
- `recall`
- `remember`
- `decide`

In practice, those map to Muninn MCP tools:

- `muninn_where_left_off`
- `muninn_recall`
- `muninn_remember`
- `muninn_decide`

## Why a Helper Is Required

Muninn MCP is local and does not require an auth token, but it still requires a valid MCP session handshake:

1. connect to the SSE endpoint
2. read the returned `sessionId`
3. send `initialize`
4. send `notifications/initialized`
5. only then call tools

That is too much ceremony to expect every client or every session to perform manually.

So the helper is not optional infrastructure polish. It is the minimum layer that turns Muninn from "reachable" into "usable by habit."

## Why the Helper Should Not Live Only Inside a Skill

A skill is client-specific instruction packaging.

The helper is transport logic.

If the helper lives only inside a Codex skill:

- non-Codex clients cannot reuse it cleanly
- the protocol becomes trapped in one client format
- every other agent client has to rediscover the same handshake logic

So the right split is:

- helper script = shared, standalone, versioned artifact
- skill/instruction set = client-specific wrapper around that helper

This keeps the protocol portable and the client experience ergonomic.

It is fine for a client-specific skill to bundle a thin wrapper around the shared helper. The thing we should avoid is making the wrapper the only canonical implementation. That would be a wonderfully efficient way to make a cross-client protocol client-locked.

## Recommended Adoption Pattern

### 1. Shared Helper

Keep one small helper script in a normal repo path.

Responsibilities:

- open the MCP SSE endpoint
- extract `sessionId`
- send the initialize flow
- invoke tools
- print JSON results consistently

### 2. Shared Instruction Set

Create one plain-language protocol doc that any cognitive client can consume.

This is not tied to Codex, Claude, OpenClaw, or Philotic specifically.

### 3. Client Adapters

Each client should then have its own lightweight wrapper:

- Codex skill
- Claude instruction block
- OpenClaw/ZeroClaw bootstrap guidance
- Philotic-native runtime integration later

## Philotic Recommendation

For Philotic specifically:

- deprecate the `muninn-memory-habit` CLI script for agent sessions and migrate to **100% Reflexive Hooks** (`PreCompact` / `Stop`).
- establish the `intel-graph` as the central broker that receives `POST /api/mempalace/turn` events from the agent IDEs.
- use Mempalace's `Wing` / `Room` semantic vector indices backend for coding agents to achieve maximum retrieval performance.
- keep Philotic OS `philote` runtime context strictly decoupled from the chat histories.

### Production Deployment Topology (Graph-Brokered Continuity)

As Philotic moves toward distributed projects, the recommended topology centers around `intel-graph`:

```
jane-vps  →  intel-graph API (The Broker)
              ├─ AIUA SQLite (Hotel State)
              ├─ Project Graph (Proposals, Seams, Nodes)
              └─ Mempalace ChromaDB (Semantic Histories)
                  ├─ Wing: gemini-architect
                  ├─ Wing: claude-implementer
                  └─ Wing: cursor-operator
```

Key decisions:

1. **Graph-Backed Broker** — `intel-graph` unifies the deterministic codebase and semantic memory, providing a single endpoint for all operations.
2. **Wing Partitioning** — the `intel-graph` isolates incoming reflexes into separate Mempalace Wings based on the `agent_id`.
3. **Reflexive IDE Hooks** — Claude Code, Gemini Antigravity, and Cursor utilize invisible bash hooks to post their transcripts to `intel-graph` without manual `muninn` invocation.
4. **Unified Admin Querying** — `intel-graph` enables an admin philote to retrieve a fused context of code facts and conversation histories seamlessly.

## Success Criteria

This experiment is working if we observe:

- better continuity across sessions
- better recall of prior architectural decisions
- better user-fit behavior
- reduced repetition
- useful retrieval without excessive token overhead
- better recall from short atomic memories and a small stable tag vocabulary

It is not working if:

- retrieval is mostly irrelevant
- write-back becomes ritual noise
- clients ignore the helper because it is too awkward
- recalled memory frequently conflicts with observed truth and causes confusion
- memories become long-form note fragments instead of atomic retrieval units
- tag sprawl turns retrieval into taxonomy maintenance

## Near-Term Next Steps

1. ~~Use the helper-backed Muninn protocol in Codex by default.~~ Done.
2. Share the client instruction set with other cognitive clients.
3. ~~Add at least one more client integration path.~~ OpenCode integration landed.
4. Observe whether Muninn materially improves continuity before making it deeper infrastructure.
5. **New**: Stand up muninndb on jane-vps (Docker, Caddy TLS, per-agent vault provisioning) as the production Cortex.
6. **New**: Evaluate `muninn exec` as the default write path for Philotic agent-workers that don't maintain persistent MCP sessions.
7. **New**: Decide vault naming convention before provisioning more than a handful of agents.

## Implementation Recommendation

Implement Muninn adoption in this order:

1. Shared helper first
- one small transport client
- no client-specific assumptions in the transport layer
- include a hard availability gate that fails loudly and requires operator approval before continuing without Muninn

2. Shared protocol second
- one plain-language instruction contract
- all clients retrieve and write back using the same habit

3. Client adapters third
- Codex skill
- Claude/Desktop instructions
- OpenClaw/ZeroClaw bootstrap guidance
- Philotic-native runtime integration later

4. Native port only after behavior proves useful
- first validate continuity gains
- then decide whether the helper remains external or becomes a Rust-native Philotic client
