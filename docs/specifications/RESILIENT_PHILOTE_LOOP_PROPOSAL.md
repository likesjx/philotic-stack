---
domain: runtime-sessions
status: accepted-current-slice
disposition: accepted-current-slice
last_updated: 2026-06-30
---

# Resilient Philote Cognitive Loop Proposal

**Status**: Proposed
**Domain**: runtime-sessions
**Date**: 2026-05-05
**Motivation**: Three live failures observed 2026-05-04:
  1. Gemini streaming returned no content → batch fallback with no timeout → 27-minute session hang
  2. Network outage → cloud model fails with connection error → not classified as retryable, loop terminates turn immediately
  3. Second voice memo queued behind stuck active turn, never processed

---

## Goal

Make the philote cognitive loop **long-running and self-healing**. A loop iteration that encounters a model failure should exhaust local recovery options before surfacing an error to the user. The loop should remain alive indefinitely under network instability, cloud quota exhaustion, and transient provider errors — falling back gracefully through a tier of providers rather than failing fast.

---

## Current State

### What exists

- `should_attempt_provider_repair`: retries once on `provider_failure` where `retryable=true` and `capability=text.generate`
- `evict_timed_out_turns`: watchdog evicts turns stuck in waiting phases past their timeout
- `fallback_to_text`: voice synthesis falls back to text on TTS failure
- `stuck_turn_first_seen`: tracks when a waiting-phase turn was first observed

### What's missing

| Gap | Consequence Observed |
|---|---|
| No timeout on cloud model HTTP requests | Gemini batch fallback hung for 27 min |
| Connection errors not classified as retriable | Network drop → immediate turn failure, no local fallback |
| No tiered provider fallback for `text.generate` | Cloud outage = user-facing failure, even with local ONNX available |
| No streaming-idle timeout | Zero-token SSE stream held open indefinitely |
| Turn queue has no depth cap or stale eviction | Queued turns pile up behind stuck active turn |
| No network-state-aware routing | Online/offline signal not used for provider selection |

---

## Proposed Architecture

### 1. Error Taxonomy

Classify all model failures at the loop boundary before deciding what to do:

```
ModelFailureKind:
  NetworkError       — reqwest/IO failure, no bytes received (RETRIABLE → local)
  ProviderError      — HTTP 4xx/5xx received from provider (depends on code)
  EmptyResponse      — provider returned 200 but no content (RETRIABLE → batch once, then local)
  ContentError       — malformed payload, tool call parse failure (REPAIR once, then fail)
  TimeoutError       — request exceeded per-capability deadline (RETRIABLE → local)
  RateLimitError     — HTTP 429 (RETRIABLE with backoff → local)
```

The existing `TaskErrorPayload.kind` field is `"provider_failure"` for all of these. Subdivide it with a `sub_kind` field so routing decisions can be precise.

### 2. Per-Request Timeouts

Add a `timeout_secs` field to the model task payload, enforced by the model-controller before firing the HTTP request. Defaults:

| Capability | Streaming timeout (first token) | Total timeout |
|---|---|---|
| `text.generate` | 10s | 120s |
| `voice.transcribe` | — (local ONNX, no HTTP) | 60s |
| `voice.synthesize` | 5s (first audio chunk) | 30s |

The **streaming idle timeout** is the key fix for the 27-minute hang: if the SSE stream opens but produces zero tokens within `streaming_idle_secs`, abort the stream and emit a `TimeoutError` (not `EmptyResponse`). Do not fall back to batch — go straight to the fallback tier.

### 3. Tiered Provider Fallback for `text.generate`

Replace the single `should_attempt_provider_repair` retry with a **fallback ladder**:

```
Tier 0: Primary cloud provider (Gemini)
  → on NetworkError / TimeoutError / RateLimitError: drop to Tier 1
  → on EmptyResponse: retry batch once, then drop to Tier 1
  → on ContentError: repair once (existing logic), then fail turn

Tier 1: Secondary cloud provider (configured per-hotel, e.g. Claude via OpenAI-compat)
  → on any failure: drop to Tier 2

Tier 2: Local model (model.local / ONNX / MLX)
  → on failure: fail turn with user-visible error
```

The ladder is **stateless per turn** — each tier attempt is a fresh model task emission. The session state tracks `fallback_tier: u8` (0–2) so the loop knows which tier a returning result belongs to and where to route failures.

The local tier (Tier 2) is always available when model-controller-onnx is running. It gives the user *something* even when the internet is down.

### 4. Network-State-Aware Routing

`aiua` already broadcasts `NetworkState { online: bool }` to guests on reachability change. Wire this into philote:

- On `NetworkState { online: false }`: set `session_context.network_offline = true`
- On `network_offline = true`: **skip Tier 0 and Tier 1 entirely**, route `text.generate` directly to Tier 2
- On `NetworkState { online: true }`: clear `network_offline`, resume normal ladder

This makes the online→offline transition instant — no failed cloud attempt, no wasted latency. Users on airplane mode get a local response in seconds.

### 5. Turn Queue Health

- **Queue depth cap**: reject new inbound tasks when `queue_depth >= 3`; emit a "busy" reply to the user instead of silently queuing
- **Stale queue eviction**: any queued turn older than `QUEUE_STALE_SECS = 120` is dropped with a logged warning on the next watchdog tick
- **Active turn deadline**: extend `evict_timed_out_turns` to cover the `Processing` phase (currently it only covers waiting phases). A turn stuck in `Processing` for more than `PROCESSING_STUCK_SECS = 90` is evicted and re-routed to the next fallback tier (not simply failed)

### 6. Streaming Idle Timeout Implementation

In `model-router/src/providers/gemini.rs`, the streaming path:

```rust
// Current: reads SSE until stream closes, no idle check
// Proposed:
let idle_timeout = Duration::from_secs(config.streaming_idle_secs);
let token_stream = timeout_stream(sse_stream, idle_timeout);
// If first token doesn't arrive within idle_timeout → TimeoutError
```

The `timeout_stream` wrapper polls the SSE reader and fires `TimeoutError` if no event arrives within the window. This is the minimal fix for the 27-minute hang — everything else in this proposal builds on top.

---

## Implementation Slices

### Slice 1 — Streaming Idle Timeout (immediate, high value)
- Add `streaming_idle_secs: u64` to Gemini provider config (default: 8s)
- Wrap SSE reader with idle timeout in `gemini.rs`
- Emit `sub_kind: "streaming_timeout"` in `TaskErrorPayload`
- **Fixes**: 27-minute hang

### Slice 2 — Error Taxonomy + Request Timeouts
- Add `sub_kind` to `TaskErrorPayload`
- Add `timeout_secs` to model task payload; enforce in model-controller base runtime
- Classify reqwest errors into `NetworkError` vs `ProviderError` vs `EmptyResponse`
- **Enables**: precise routing decisions in Slices 3 and 4

### Slice 3 — Tiered Fallback Ladder
- Add `fallback_tier: u8` to `WorkingTurn`
- Add `TurnLoopConfig.fallback_tiers: Vec<FallbackTierConfig>` (ordered list of model roles)
- On `NetworkError` / `TimeoutError`: increment tier, re-emit model task to next role
- On `RateLimitError`: increment tier with jitter delay
- **Fixes**: cloud outage → local response

### Slice 4 — Network-State-Aware Routing
- Handle `NetworkState` IPC message in philote runtime
- Track `network_offline` on `AgentRuntime` (not per-session — it's hotel-wide)
- Route `text.generate` directly to Tier 2 when `network_offline`
- **Fixes**: airplane-mode / network drop → instant local response

### Slice 5 — Turn Queue Health
- Add `queue_depth` cap (reject at 3) with user-visible "busy" reply
- Stale queue eviction in `evict_timed_out_turns`
- `Processing` phase watchdog with re-route to next fallback tier
- **Fixes**: queued turn pileup behind stuck active turn

### Slice 6 — Observability
- Structured log field `fallback_tier` on every model dispatch
- `phil hotel status` surfaces: current tier per session, queue depth, network state
- Metric counters: `fallback_tier_0_failures`, `fallback_tier_1_failures`, `fallback_tier_2_used`

---

## Configuration Shape

```toml
[turn_loop]
streaming_idle_secs = 8
request_timeout_secs = 120
processing_stuck_secs = 90
queue_depth_cap = 3
queue_stale_secs = 120

[[turn_loop.fallback_tiers]]
role = "model"              # Gemini (Tier 0)

[[turn_loop.fallback_tiers]]
role = "model.openai"       # Claude / OpenAI-compat (Tier 1, optional)

[[turn_loop.fallback_tiers]]
role = "model.local"        # ONNX / MLX (Tier 2)
response_prefix = "[local] "  # optional marker so user knows they got local inference
```

Per-hotel config (in `mesh-config.json` or via graph secrets). Tier 1 is optional — if unconfigured, Tier 0 failures drop directly to Tier 2.

---

## Non-Goals

- Streaming TTS fallback (covered by existing `fallback_to_text`)
- Cross-hotel load balancing (separate proposal: Mesh Visibility And State Placement)
- Model quality routing / smart dispatch (deferred — pick best model for task type)
- Voice transcription fallback (Whisper is local; no network dependency)

---

## Open Questions

1. **Tier 1 provider on bjork**: currently no secondary cloud provider configured. Default to Tier 0 → Tier 2 (skip Tier 1) for now, add Tier 1 when a second API key is available.
2. **Context truncation for local model**: ONNX text.generate has a small context window. When falling back to Tier 2, the loop may need to truncate the conversation history. Truncation policy TBD.
3. **User transparency**: should the user be told they got a local response? Configurable `response_prefix` proposed above; default off.
4. **Queue rejection UX**: "busy" reply text needs to be persona-aware (Bjork should say it in character, not expose a system error string).
