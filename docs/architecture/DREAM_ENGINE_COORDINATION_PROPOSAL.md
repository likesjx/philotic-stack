---
title: Dream Engine — Lifecycle Coordination in Aiua Guest Shutdown/Restart
doc_type: proposal
domain: cognitive-plane
status: proposed
last_updated: 2026-04-09
proposal_id: dream-engine-coordination
tags:
- memory
- dream-engine
- muninndb
- philote
- aiua
- cognitive-loop
- lifecycle
related_docs:
- MEMORY_ENRICHMENT_RUST_PORT_PROPOSAL.md
- COGNITIVE_LOOP_PROPOSAL.md
implements: []
implemented_by: []
active_seams:
- dream-engine-lifecycle
---

# Dream Engine — Lifecycle Coordination in Aiua Guest Shutdown/Restart

## Status

Proposed. No implementation blockers — can begin after worktree merge.

## Context

The current shutdown path is mechanical: hotel sends `GracefulShutdown { drain_timeout_secs: 30 }` to all
registered IPC subscribers; philote drains active turns and exits; hotel waits for PIDs to go dark.
The restart path is equally bare: philote spawns, receives `MuninnConfig` from the first IPC exchange,
and immediately begins accepting turns. No session continuity passes across the boundary.

This is fine for stateless turn routing. It is not fine for a cognitive agent that accumulates episodic
memory across sessions. Three gaps:

1. **No session close marker**: When philote exits, the last session's engrams are in MuninnDB but there
   is no timestamped "session ended" anchor. Subsequent recall cannot distinguish "memories from an hour
   ago in the same session" from "memories from a previous day."

2. **No memory activation sweep on shutdown**: MuninnDB tracks Hebbian edge weights and decay scores
   per-engram. These only update when `POST /api/engrams/{id}/evolve` is called. Without a shutdown
   sweep, edge weights go stale between sessions — high-frequency concepts in a long session don't
   potentiate correctly until the next access.

3. **No orientation pass on restart**: The first turn after a restart arrives with no context about what
   the agent was doing. The agent must re-derive its state from scratch. If the user picks up mid-task,
   the first response is always worse than it should be.

## Proposal

Two separate concerns — session close (philote-side) and dream sweep (hotel-side) — coordinated by a
new `DreamsPhase` between guest drain and hotel shutdown.

```
Ctrl-C / SIGTERM received
  ↓
Phase 1 (existing): Send GracefulShutdown to all guest subscribers
  ↓
Philote:  drain active turns → write session wrap-up memory → exit
  ↓
Phase 2 (NEW): Hotel DreamsPhase — runs after guests drain, before shutdown_tx
  ↓
  For each active agent vault:
    1. Recall recent session engrams (GET /api/recall?vault=X&q=session&limit=20)
    2. Embed each engram content  →  ONNX sidecar :11435 POST /api/embeddings
    3. Cluster by cosine similarity (threshold 0.82)
    4. For each cluster ≥ 2 engrams:
         Ollama :11434 POST /v1/chat/completions (gemma4:e4b) → merged_content
         MuninnDB POST /api/consolidate { ids, merged_content }
    5. Evolve remaining (non-consolidated) engrams  →  POST /api/engrams/{id}/evolve
  ↓
Phase 3 (existing): Hotel broadcasts shutdown_tx, clears hotel PID
```

**Infrastructure already in place**:
- ONNX sidecar runs at **:11435** (Ollama-compat `POST /api/embeddings`) — live via `model-router`
- Ollama at **:11434** with OpenAI-compat `/v1/chat/completions` — no API key, local-only
- `OllamaProvider` now wired into `model-router` (added in this workstream) — used by philote at
  runtime; dream sweep uses direct HTTP to avoid IPC during shutdown
- `POST /api/consolidate` live in MuninnDB REST — requires `ids` (≥2) + `merged_content`

### On Restart

```
Philote starts → receives MuninnConfig from hotel IPC
  ↓
Orientation pass: GET /api/recall?vault=self_{agent_id}&q=recent+session&limit=5
  ↓
Cache orientation_summary on Runtime struct
  ↓
First turn: inject orientation block into system prompt
```

---

## Detailed Design

### A. Session Wrap-Up Memory (Philote, Shutdown)

**Where**: `crates/philote/src/runtime.rs` — `GracefulShutdown` handler, immediately after drain loop exits
(current line ~740: `info!("Graceful shutdown drain complete; philote exiting.")`).

**What to write**:

```rust
// After drain loop, before return Ok(())
if let Some(engine) = self.memory_engine_for(&self.agent_id, &self.agent_id) {
    let session_count = self.sessions.len();
    let turn_count: usize = self.sessions.values()
        .map(|s| s.turn_count())
        .sum();
    let _ = engine.remember(
        MemoryScope::SelfOnly,
        &format!("session:end:{}", chrono::Utc::now().format("%Y-%m-%dT%H")),
        &format!(
            "Session ended cleanly. {} session(s), {} turn(s) completed this process lifetime.",
            session_count, turn_count
        ),
        vec!["session".into(), "session:end".into()],
    ).await;
}
```

**Why this format**:
- `session:end:{date}` slug is sortable by concept prefix — recall queries can find the most recent
  session end without full-text search.
- Turn count gives the orientation pass meaningful signal (long session vs. quick check-in).
- Non-fatal: `let _ =` — a failed memory write at shutdown must not block exit.

**Requires**: `SessionState` to expose `turn_count()` (a simple field or `completed_turns.len()`).
Check `crates/philote/src/session.rs` for the right accessor.

---

### B. Dream Sweep — Embed, Cluster, Consolidate, Evolve (Hotel, DreamsPhase)

**Where**: `crates/aiua/src/main.rs` — between Phase 2 (wait for guests to drain) and Phase 3
(broadcast shutdown_tx). New async block after line ~4970.

```rust
// DreamsPhase: semantic consolidation + Hebbian sweep across all agent vaults.
if let Some(muninn_cfg) = &muninn_config {
    dream_sweep(muninn_cfg, &graph_domain_arc, &hotel_name).await;
}
let _ = shutdown_tx.send(());
```

**`dream_sweep` function** (new file: `crates/aiua/src/dream.rs`):

```rust
pub async fn dream_sweep(
    muninn_cfg: &MuninnConfig,   // per-vault token, API endpoint
    graph: &GraphDomain,
    hotel_name: &str,
) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_default();

    let vault_names = collect_agent_vault_names(graph, hotel_name);

    for vault_name in &vault_names {
        let token = match vault_token_for(graph, vault_name) {
            Some(t) => t,
            None => { tracing::debug!(vault=%vault_name, "DreamsPhase: no token — skipping"); continue; }
        };

        // ── Step 1: Recall recent session engrams ─────────────────────────
        let engrams = match recall_recent(
            &client, &muninn_cfg.endpoint, &token, vault_name, 20
        ).await {
            Ok(e) => e,
            Err(_) => continue,
        };
        if engrams.is_empty() { continue; }

        // ── Step 2: Embed via ONNX sidecar (:11435) ───────────────────────
        // Direct HTTP — avoids IPC routing plane which is shutting down.
        let embedded = embed_engrams(&client, &engrams).await;

        // ── Step 3: Cluster by cosine similarity (threshold 0.82) ─────────
        let clusters = cosine_cluster(&embedded, 0.82_f32);

        // ── Step 4: Consolidate each cluster via Ollama + MuninnDB ────────
        for cluster in clusters.iter().filter(|c| c.len() >= 2) {
            let merged = match ollama_merge(&client, cluster).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!("DreamsPhase: Ollama merge failed — {e}");
                    continue;
                }
            };
            let ids: Vec<&str> = cluster.iter().map(|e| e.id.as_str()).collect();
            let _ = consolidate(&client, &muninn_cfg.endpoint, &token, vault_name, &ids, &merged).await;
        }

        // ── Step 5: Evolve non-consolidated engrams ───────────────────────
        let consolidated_ids: std::collections::HashSet<&str> = clusters
            .iter()
            .filter(|c| c.len() >= 2)
            .flat_map(|c| c.iter().map(|e| e.id.as_str()))
            .collect();
        for engram in engrams.iter().filter(|e| !consolidated_ids.contains(e.id.as_str())) {
            let _ = evolve_engram(&client, &muninn_cfg.endpoint, &token, vault_name, &engram.id).await;
        }

        tracing::info!(vault=%vault_name, total=engrams.len(), "DreamsPhase: complete");
    }
}
```

**Key implementation notes**:

- **ONNX sidecar** (`POST http://localhost:11435/api/embeddings`): direct HTTP, Ollama-compat format.
  The sidecar is served by `model-router` on startup; it may still be running during the drain window
  since it's not an IPC guest. Add a 3s grace wait before the sweep if needed.

- **Ollama** (`POST http://localhost:11434/v1/chat/completions`, model `gemma4:e4b`): prompt template:
  ```
  You are a memory consolidation engine. Merge these related memories into one concise engram.
  Output only the merged memory text, no commentary.

  Memories:
  - {concept}: {content}
  - {concept}: {content}
  ```
  Response is the merged_content string passed directly to `POST /api/consolidate`.

- **Cosine clustering**: pure Rust, no external call. Build pairwise similarity matrix over the
  embedding vectors, greedily merge pairs above threshold 0.82. Clusters ≥ 2 go to consolidation;
  singletons go to evolve-only.

- **Per-vault token**: read from vault registry via `resolve_secret()` — same path used in
  `muninn_provision.rs`. Do NOT re-use admin session; each vault has its own scoped token.

- **Non-fatal discipline**: every step in the loop is `let _ = ...` or wrapped in `match ... Err =>
  continue`. A failed vault must not abort other vaults. A failed Ollama call means evolve-only for
  that cluster.

- **Sidecar availability**: if the ONNX sidecar is unreachable (`:11435` returns error), fall back
  to evolve-only (skip clustering entirely). Log at `warn` level, not `error`.

---

### C. Orientation Pass on Restart (Philote, Startup)

**Where**: `crates/philote/src/runtime.rs` — after `muninn_config` is set from IPC response
(line ~573 `self.muninn_config = Some(cfg);`), add an async orientation recall.

**What to do**:

```rust
// After setting muninn_config, fetch orientation summary.
self.orientation_summary = self.load_orientation_summary().await;
```

**New method on `PhiloteRuntime`**:

```rust
async fn load_orientation_summary(&self) -> Option<String> {
    let engine = self.memory_engine_for(&self.agent_id, &self.agent_id)?;
    let recent = engine
        .recall(MemoryScope::SelfOnly, "recent session end decision", 5)
        .await
        .ok()?;
    if recent.is_empty() {
        return None;
    }
    // Format: one line per recalled engram, most recent first.
    let lines: Vec<String> = recent
        .iter()
        .map(|e| format!("- {}: {}", e.concept, e.content))
        .collect();
    Some(format!(
        "[Orientation — what you were working on before this session:]\n{}",
        lines.join("\n")
    ))
}
```

**Injection point**: `orientation_summary` is added as `Option<String>` to the `PhiloteRuntime`
struct. The first time `assemble_system_prompt()` (or equivalent) is called, if
`orientation_summary.is_some()`, append it at the end of the system prompt and then clear it
(`take()`), so it fires once per process startup, not every turn.

**Why once**: The orientation block gives the model context for the first response. Repeating it
every turn pollutes the context window and increases token cost. Clear it after first use.

---

## IPC Changes

None. The dream engine runs entirely within the hotel after the drain window. No new IPC messages
are needed. `GracefulShutdown` is sufficient to trigger the philote-side session wrap-up.

**Future consideration**: If dream consolidation eventually requires model-router, a
`IpcRequest::TriggerDream { vault: String, engram_ids: Vec<String> }` message could be sent to
philote before shutdown — philote has the model-router handle. But do not add this now; the evolve
sweep is model-free and sufficient.

---

## Changes Required

### `crates/philote/src/runtime.rs`
- Add `orientation_summary: Option<String>` field to `PhiloteRuntime`
- Add `load_orientation_summary()` method
- Inject orientation into first system prompt (find `assemble_system_prompt` or equivalent)
- Add session wrap-up memory write in `GracefulShutdown` handler after drain loop

### `crates/philote/src/session.rs`
- Add `turn_count()` accessor (count of completed turns this session)

### `crates/aiua/src/main.rs`
- Add `DreamsPhase` block after Phase 2 (guest drain wait), before `shutdown_tx.send()`
- Call `dream_sweep(muninn_cfg, &graph_domain_arc, &hotel_name).await`

### `crates/aiua/src/dream.rs` (new file)
- `pub async fn dream_sweep(muninn_cfg, graph, hotel_name)` — 5-phase pipeline above
- Internal helpers: `embed_engrams()` (ONNX sidecar :11435), `cosine_cluster()` (pure Rust),
  `ollama_merge()` (Ollama :11434), `consolidate()` (MuninnDB), `evolve_engram()` (MuninnDB)

### `crates/aiua/src/memory.rs`
- Expose `vault_token_for(graph, vault_name) -> Option<String>` helper for use from `dream.rs`

### `crates/model-router/src/providers/ollama.rs` ✅ (added in this workstream)
- `OllamaProvider` — OpenAI-compat TextGenerate provider for local Ollama

### `crates/model-router/src/providers/mod.rs` ✅ (updated in this workstream)
- Exports `OllamaProvider`

### `crates/model-router/src/controller.rs` ✅ (updated in this workstream)
- `ProviderConfigs` gains `ollama_base_url` and `ollama_model`

### `crates/model-router/src/main.rs` ✅ (updated in this workstream)
- `OllamaProvider` registered as last-priority provider

---

## Non-Goals

- **Cross-session entity graph**: Enrichment (entities, relationships) is the
  `memory-enrichment-rust-port` proposal's domain. Dream engine does not duplicate it.
- **Dream scheduling during idle hours**: Phase 0/1 is shutdown-triggered. Cron-based quiet-hours
  dream runs are a Phase 2 feature (use `CronJob` once established).
- **Ollama routing through model-router IPC**: Dream sweep uses direct HTTP to Ollama during
  shutdown. The `OllamaProvider` in `model-router` is for runtime cognitive tasks (philote turns),
  not dream consolidation.

## Model-Router Integration (added in this workstream)

`OllamaProvider` is now wired into `model-router`:

- **File**: `crates/model-router/src/providers/ollama.rs`
- **TaskKind**: `TextGenerate` only — Ollama is a fallback after Gemini in the provider registry
- **Config**: `PHILOTIC_OLLAMA_BASE_URL` (default `http://localhost:11434`),
  `PHILOTIC_OLLAMA_MODEL` (default `gemma4:e4b`), or via mesh-config keys `ollama_base_url` /
  `ollama_model`
- **Priority**: Last in provider list — Gemini handles `TextGenerate` when configured; Ollama
  handles it when Gemini is unavailable (no key, quota exhausted, or offline)

ONNX embeddings continue to be handled by `OnnxProvider` → sidecar at `:11435`.

---

## Implementation Order

1. **Session wrap-up memory** (philote) — quickest, highest value, self-contained
2. **Orientation pass** (philote) — requires session wrap-up to be live so there's something to recall
3. **DreamsPhase evolve sweep** (hotel) — last; requires knowing per-vault tokens are accessible from `dream.rs`

Do NOT implement 3 before 1 — the evolve sweep potentiates engrams that don't exist yet without the
session wrap-up markers.
