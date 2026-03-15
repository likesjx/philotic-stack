---
title: "Philotic Telegram Integration Proposal"
doc_type: proposal
domain: membrane-transport
status: accepted-current-slice
last_updated: 2026-03-12
tags:
  - telegram
  - membrane
  - transport
  - multimodal
  - current-slice
related_docs:
  - ARCHITECTURE_STATUS.md
  - TELEGRAM_POLL_LEASE_PROPOSAL.md
  - MEMBRANE_COMPONENT_PROPOSAL.md
  - SLASH_COMMANDS_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: telegram-integration
implements: []
implemented_by:
  - telegram-normalized-ingress-slice
  - telegram-media-startup-smoke-slice
active_seams:
  - webhook-security-contract
  - watched-live-telegram-validation
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# Philotic Telegram Integration Proposal

## Goal

Define Telegram as a real outside-world transport boundary for Philotic, with explicit decisions for:

- inbound transport ownership
- webhook security posture
- multimodal normalization
- slash-command elevation
- streaming and approval UX
- the boundary between transport behavior and agent cognition

This proposal is intentionally security-first. Telegram can be the outside-world interface, but that only helps if the outside world is not also our unauthenticated load tester.

This proposal assumes the broader membrane boundary defined in [MEMBRANE_COMPONENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_COMPONENT_PROPOSAL.md): Telegram is one membrane implementation, not the definition of membrane itself.

Telegram poller ownership is defined separately in [TELEGRAM_POLL_LEASE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_POLL_LEASE_PROPOSAL.md), because "how Telegram updates are normalized" and "who may hold the one real polling cursor" are adjacent but not the same authority question.

## Core Recommendation

Keep `membrane` as the canonical Telegram transport boundary.

More precisely for long-term architecture:

- `membrane` is the component type
- Telegram is one membrane implementation
- the current `crates/membrane` binary is a transitional Telegram-oriented implementation of that type

For the first coherent slice:

- keep polling as the default production ingress
- design webhook ingress now, but gate it behind a stricter security contract than polling
- raise Telegram-native control behavior into `membrane`
- normalize Telegram updates into a transport-neutral envelope before handing them to `philote`
- keep `philote` transport-agnostic and focused on cognition

In other words:

- `membrane` owns Telegram semantics
- `philote` owns the conversational/agent loop
- the context graph owns durable session truth

## Disposition

Accepted for current slice.

Accepted here means:

- polling remains the default ingress until webhook security gates are implemented
- webhook support is in scope as a follow-on transport capability, not the first thing we trust with a public attack surface
- active work should be tracked in [task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)

## Current Slice

Current repo truth:

- `crates/membrane` is a long-polling Telegram guest
- it now normalizes one canonical inbound path for:
  - `message.text`
  - captioned media messages
  - attachment-only media/file messages
  - `callback_query`
- it emits a normalized inbound transport envelope into `philote` with:
  - `transport`
  - `session_id`
  - `turn_id`
  - `chat_id`
  - `thread_id`
  - `sender_id`
  - `sender_username`
  - `message_kind`
  - `content`
  - `attachments`
  - `command`
  - `callback_data`
  - `raw_transport_event`
- it replies with `sendMessage`
- it now preserves an optional `final_reply_guest_id` and session-level transport reply target so the owning local membrane guest survives beyond a single turn without relying only on shared-role fan-out
- the startup smoke now simulates text, photo, and voice-note Telegram updates against local fake Telegram and fake Gemini APIs, so the blob-backed media path has a real regression harness even though watched live Telegram validation is still pending

That is a useful membrane slice, but it is not yet a richer Telegram controller:

- slash commands still execute in `philote`
- attachment handling now resolves `file_id` values through Telegram `getFile`, downloads bytes, and uploads them into the hotel blob service
- blob-backed Telegram attachments now have an initial downstream path through `philote` and `model-router` as `media.analyze`, so supported photos, audio/voice notes, and documents can reach Gemini for first-pass interpretation
- membrane/agent identity is now guest-configurable, so separate hotels can materialize separate Telegram pollers and separate agent identities without hardcoding Jane everywhere
- specialized voice transcription, richer vision workflows, and watched live validation of the blob-backed media path are still follow-on work
- polling still does not cover the full Telegram update surface beyond `message` and `callback_query`

## Current Reality

Today the implementation is narrower than some repo docs imply:

- the live code uses `getUpdates`, not webhooks
- the live code currently requests `allowed_updates = ["message", "callback_query"]`
- the live code normalizes text, captions, callback data, and common media/file metadata, but not full media retrieval
- there is no Telegram webhook verification path in Philotic yet

This matters because transport docs that describe a webhook path as though it already exists are not harmless color. They increase the odds that we accidentally design around inferred behavior instead of proven behavior.

## Inbound Transport Options

### Option A: Long Polling

Characteristics:

- no inbound public port required
- lower external attack surface
- simpler local and home-lab deployment
- one active poller per bot token
- less natural fit for high-throughput fan-in than webhooks

Security posture:

- stronger by default because the runtime initiates outbound connections only
- still requires token handling, sender allowlists, update dedupe, and media limits

Operational tradeoff:

- easier to make safe early
- less flexible for horizontally scaled ingress

### Option B: Webhooks

Characteristics:

- requires reachable HTTPS endpoint
- better fit for prompt delivery and direct request-response webhook handling
- introduces a real public ingress boundary
- requires explicit callback authenticity and replay/idempotency handling

Security posture:

- meaningfully riskier than polling if implemented casually
- must not be treated as "just another HTTP route"

Operational tradeoff:

- more capable
- more exposed
- more likely to blur gateway ownership unless we name the boundary clearly

## Security Review

### Review Scope

This review covers:

- the current Telegram implementation in `crates/membrane`
- existing Telegram and deployment docs
- the likely security requirements for adding Telegram webhook ingress

### Findings

#### 1. High: repo docs currently overstate webhook reality

Some docs describe Telegram webhook delivery as if it already exists, while the running implementation is long-polling only.

Why this matters:

- operators may expose an ingress path that the code does not actually secure
- future work can inherit stale assumptions about callback authenticity, TLS, or delivery semantics
- security review gets weaker when architecture prose outruns the code

Examples:

- [crates/membrane/src/main.rs](/Users/jaredlikes/code/philotic-stack/crates/membrane/src/main.rs#L74) shows `getUpdates` polling as the actual ingress path.
- [docs/walkthrough.md](/Users/jaredlikes/code/philotic-stack/docs/walkthrough.md#L26) still says "Telegram Webhook hits the independent `membrane` process."
- [docs/implementation_plan.md](/Users/jaredlikes/code/philotic-stack/docs/implementation_plan.md#L31) says the Telegram listener "listens to webhooks".

Disposition:

- treat webhook as proposed, not implemented
- do not expose a Telegram webhook endpoint until the security contract below is built and verified

#### 2. High: a naive webhook port would create a new public authority boundary with no current auth design

Polling currently avoids public ingress. A webhook implementation would not.

If we add webhook support without an explicit authentication model, the first public POST handler becomes a de facto owner of:

- source authenticity
- replay handling
- update dedupe
- request sizing and timeouts
- denial-of-service posture

That is too much authority to leave implicit.

Disposition:

- require a transport-level webhook verification contract before any Telegram webhook endpoint is enabled

#### 3. Medium: the current ingress normalization is too text-specific to be safely extended ad hoc

The current code immediately assumes `message.text` and packages a very small task payload.

Why this matters for webhook work:

- multimodal updates will arrive with more shapes than a text field
- callback queries, commands, media, and topic/thread metadata need a stable normalized envelope
- extending this one field at a time encourages partial transport truth scattered across `membrane` and `philote`

Disposition:

- define one normalized Telegram ingress envelope before broadening update coverage

#### 4. Medium: webhook retries require idempotent session/update handling that is not yet explicitly designed

Telegram retries unsuccessful webhook deliveries. That is useful for reliability and annoying for systems that pretend every POST is unique.

We need an explicit rule for:

- deduping by `update_id`
- acknowledging only after durable handoff
- avoiding duplicate turn creation during retries or restarts

Disposition:

- make update idempotency part of the transport contract, not an afterthought

## Webhook Security Contract

If Telegram webhook ingress is added, it should only ship with all of the following:

### 1. Callback authenticity

- configure `setWebhook` with Telegram's `secret_token`
- require `X-Telegram-Bot-Api-Secret-Token` on every inbound request
- reject missing or mismatched values with `401`
- do not rely on a secret URL path as the primary control

Telegram's own docs recommend `secret_token`, and the Bot API includes it specifically for webhook authenticity checks.

### 2. Network exposure discipline

- prefer `127.0.0.1` bind plus tunnel/proxy over direct public bind
- if direct bind is ever supported, make it an explicit opt-in equivalent to `allow_public_bind`
- terminate TLS in a clearly owned layer
- document whether TLS is terminated by Philotic, a reverse proxy, or a tunnel

### 3. Request shaping and resource limits

- enforce content-type and body-size limits before JSON parse
- bound request concurrency
- bound webhook handler time
- fail closed on malformed payloads

### 4. Update idempotency

- record the last accepted `update_id` or dedupe window per bot/session scope
- make webhook handling safe under retries and restarts
- ensure duplicate webhook deliveries do not create duplicate turns

### 5. Sender and chat authorization

- keep channel allowlists enforced after webhook authenticity succeeds
- separate "Telegram delivered this" from "this sender is allowed to drive the agent"
- keep group mention/trigger policy explicit

### 6. Auditability

- log inbound webhook acceptance/rejection without leaking secrets
- log why requests were rejected: missing header, bad token, oversized payload, unauthorized sender
- capture transport decisions in a way that later approval/security work can inspect

### 7. Verification ladder

- unit tests for secret-token enforcement and payload validation
- integration tests for webhook routing and dedupe
- smoke tests through a real HTTPS/tunnel path
- watched live run before calling webhook ingress trustworthy

## Telegram Transport Boundary

`membrane` should own:

- inbound Telegram update ingestion
- webhook or polling transport specifics
- slash-command parsing
- callback-query parsing
- attachment/media normalization
- Telegram reply projection
- streaming/draft behavior
- approval-card formatting

`philote` should own:

- conversational reasoning
- tool/approval orchestration
- context and session use
- model requests

This keeps Telegram quirks out of the agent loop.

## Normalized Ingress Envelope

Before broadening beyond plain text, define a normalized Telegram event envelope with fields like:

- `transport = "telegram"`
- `update_id`
- `chat_id`
- `thread_id` or topic identifier when present
- `sender_id`
- `sender_username`
- `message_kind`
- `text`
- `attachments`
- `command`
- `callback_data`
- `raw_update_ref` or retained raw payload blob when needed

This envelope should become the one object `membrane` emits into the rest of the system.

Current implementation note:

- the defined envelope now exists in `agent-core::protocol::InboundTaskPayload`
- the Telegram polling path now populates it for text, captioned media, attachment-only media/file messages, and `callback_query`
- attachment normalization now includes first-hop media transport into blob storage; downstream interpretation remains follow-on work

## Outbound Formatting

Telegram gives us three real outbound text-formatting paths:

- `parse_mode = MarkdownV2`
- `parse_mode = HTML`
- explicit formatting entities instead of parse-mode string parsing

Observed tradeoff:

- `MarkdownV2` is available, but escaping rules are fussy enough that it turns innocent model output into a reliability tax
- `HTML` is usually easier to project into clean Telegram messages from model-authored Markdown-like text
- explicit entities are the strongest long-term projection boundary, but they require us to represent formatting structurally instead of as one decorated string

Recommendation:

- near-term: keep model output transport-neutral and let `membrane` translate a supported Markdown subset into Telegram-safe HTML
- medium-term: define an outbound rich-text contract above Telegram so `membrane` can project to explicit Telegram entities without teaching `philote` transport-specific markup trivia
- respect Telegram-specific limits when projecting:
  - normal message text length after formatting parse
  - caption length limits for media messages

Current implementation note:

- `membrane` now projects outbound `sendMessage` replies through a Markdown-subset to Telegram HTML formatter
- the current supported subset includes headings, bold, italic, strikethrough, inline code, fenced code blocks, links, blockquotes, and simple lists
- explicit Telegram entities and length-aware chunking/fallback are still follow-on work

## Slash Commands

### Recommendation

Elevate slash-command parsing into `membrane`.

Flow:

1. Telegram update arrives
2. `membrane` normalizes it
3. `membrane` detects a deterministic `/command`
4. `membrane` emits a structured control payload or handles a transport-local action
5. session metadata remains consistent

### Near-Term Commands

- `/ping`
- `/status`
- `/new`
- `/pause`
- `/resume`
- `/approve`
- `/deny`
- `/preapprove`
- `/approval status`
- `/approval reset`

## Streaming and Delivery UX

Telegram now supports partial draft delivery, which makes it a much better fit for agent-style interaction than the older "wait and dump" model.

### Current reality

The current implementation is atomic and silent:

1. Telegram update arrives; membrane emits `EmitTask` to agent-core (fire and forget)
2. Agent-core processes the full turn — model call, tool evaluation, response assembly
3. Agent-core emits final `InboundTask` back to membrane
4. Membrane calls `sendMessage` once with the final formatted reply

The user sees nothing until the full reply is ready. No typing indicator. No partial progress. No delivery signal while tools are running. This is the "wait and dump" model we want to move away from.

### Design principle

Keep canonical turn state in the context graph. Let `membrane` project partial progress into Telegram delivery UX. Do not leak Telegram-specific draft/edit behavior into `philote`.

`philote` should emit turn lifecycle signals. `membrane` should own the projection of those signals into Telegram-native UX behaviors.

### Layer 1: Typing indicator (no protocol changes required)

The simplest improvement and the most valuable first step.

Telegram's `sendChatAction` with `action = "typing"` shows a typing indicator to the user. The indicator lasts approximately 5 seconds before it expires. Re-sending refreshes it.

Implementation:

1. When `membrane` emits a task to `philote`, record the active turn in a local in-memory map keyed by `(chat_id, thread_id)`.
2. Immediately send `sendChatAction(chat_id, "typing")` before dispatching to `philote`.
3. Spawn a background tokio task that re-sends `sendChatAction(chat_id, "typing")` every 4 seconds while the turn is active.
4. When the final reply `InboundTask` arrives, cancel the typing refresh task and deliver the reply.

This requires no IPC protocol changes and no changes to `philote`. The turn map is internal to `membrane`.

Action variants by turn phase (future extension, same pattern):

| Turn Phase | `sendChatAction` value |
|---|---|
| Waiting for model | `typing` |
| Processing a tool | `typing` |
| Uploading media reply | `upload_photo`, `upload_voice`, `upload_document` |
| Recording voice reply | `record_voice` |

### Layer 2: Progressive delivery (edit-based streaming)

Progressive delivery simulates streaming by sending an initial placeholder message and then editing it in place as content arrives. Telegram supports editing messages via `editMessageText`.

This requires `philote` to emit intermediate partial reply signals back to `membrane` over IPC.

Proposed IPC protocol extension:

```
InboundTask with kind = "partial_reply":
  - chat_id
  - turn_id
  - partial_content    (text fragment so far)
  - is_final           (false = more coming, true = done)
```

`membrane` behavior:

1. On first `partial_reply` for a turn: call `sendMessage` with the partial content; store the returned `message_id` keyed by `(chat_id, turn_id)`.
2. On subsequent `partial_reply`: call `editMessageText` with the updated partial content using the stored `message_id`.
3. On `is_final = true`: make one last `editMessageText` call with the final formatted reply. Cancel the typing refresh task.
4. If no `message_id` is stored (first partial never arrived, or `sendMessage` failed): fall back to the atomic path — send the final content as a new message.

Constraints:

- Telegram rate-limits `editMessageText`. Do not edit on every token. Batch partial content at reasonable intervals (e.g., every 500ms or every completed sentence/paragraph boundary).
- Telegram enforces a minimum edit interval. Edits faster than approximately 1/second may be silently dropped or trigger 429 errors.
- Do not edit a message after more than 48 hours (Telegram API limit). Not relevant for turn timescales.
- `editMessageText` requires `parse_mode` to be re-specified on every edit. The HTML formatter must be called on each partial update.

### Layer 3: Turn lifecycle signals

For the typing indicator to accurately reflect what is happening, `philote` should emit structured turn lifecycle events rather than just final replies.

Proposed lifecycle events:

| Event | Description |
|---|---|
| `turn.started` | Agent received the task and is beginning the loop |
| `turn.model_requested` | Model call is in flight |
| `turn.tool_requested` | A tool call is being evaluated |
| `turn.approval_pending` | Turn is paused, waiting for operator approval |
| `turn.partial_reply` | Intermediate text content is available |
| `turn.final_reply` | Turn is complete, full content is available |
| `turn.failed` | Turn failed with an error |

`membrane` maps these to Telegram delivery behaviors:

| Lifecycle event | Telegram action |
|---|---|
| `turn.started` | Send `sendChatAction(typing)`, start refresh loop |
| `turn.model_requested` | No change (still typing) |
| `turn.tool_requested` | No change (still typing) |
| `turn.approval_pending` | Stop typing refresh; send approval card (see Approval Card UX section) |
| `turn.partial_reply` | Edit message in place with latest partial content |
| `turn.final_reply` | Final edit or new `sendMessage`; cancel typing refresh |
| `turn.failed` | Cancel typing refresh; send error message |

Near-term: implement typing indicator and final reply with no lifecycle events (Layer 1 only).
Medium-term: add `turn.partial_reply` / `turn.final_reply` distinction (Layer 2).
Long-term: add full lifecycle events when `philote` has a proper turn state machine (Layer 3).

### Interruption handling

Telegram users may send a new message while a turn is still in flight. This creates a conflict: the agent is in the middle of reasoning about a prior message, and now there is a new input.

Three options:

**Option A: Queue and process sequentially (recommended near-term)**

- Accept the new message normally.
- Do not dispatch it to `philote` until the active turn for that `(chat_id, thread_id)` completes.
- The typing indicator naturally covers both turns from the user's perspective.
- Simple to implement with a per-session pending queue in `membrane`.

**Option B: Cancel and restart**

- When a new message arrives during an active turn: signal `philote` to abandon the current turn, then dispatch the new message immediately.
- Requires a `CancelTurn` IPC request type and `philote` to support graceful cancellation.
- More complex but more responsive for long-running turns.
- Not recommended until `philote` has a proper cancellation boundary.

**Option C: Reject and notify**

- Reject the new message with a transport-level reply ("I'm still thinking about your last message...").
- Simple but poor UX; not recommended.

Recommended default: Option A (queue). Add Option B (cancel) when `philote` has a cancellation mechanism.

### Thread and chat scoping

Interruption policy, typing indicators, and partial delivery are scoped to `(chat_id, thread_id)`. Concurrent turns in different chats or different threads within the same chat should be fully independent.

`membrane` should track active turns in a `HashMap<(chat_id, thread_id), ActiveTurn>` where `ActiveTurn` holds:

- the current `turn_id`
- the typing refresh task handle
- the last `message_id` (if a placeholder was sent for progressive delivery)
- a pending message queue for Option A interruption handling

### Message length and chunking

Telegram enforces a 4096-character limit on `sendMessage` text. The current implementation does not enforce this.

Required behavior:

- Before sending any text reply, check byte length after HTML formatting.
- If the reply exceeds 4096 characters, split at paragraph boundaries and send as sequential messages.
- For `editMessageText`, the same 4096-character limit applies.
- Do not split mid-sentence or mid-word. Prefer paragraph or section boundaries.

This is independent of progressive delivery and should be handled in a shared `send_formatted_text(chat_id, thread_id, text)` helper in `membrane` that both the atomic and streaming paths use.

### Wiring to agent-core

`TurnPhase` already exists in `agent-core/src/loop.rs` with the full state machine:

```
Queued → LoadingContext → WaitingModel → Thinking → WaitingTool → WaitingApproval → WaitingVoice → Completed / Failed
```

Every phase transition is already called via `set_active_turn_phase(...)` in `runtime.rs`. None of these transitions are currently emitted back to membrane — they are only reflected to the context graph via `UpdateTask`. Membrane receives exactly one IPC message per turn: the final `EmitTask` with `action: "send_reply"` from `deliver_text_reply`.

#### New protocol type: TurnEventPayload

Add `TurnEventPayload` to `agent-core/src/protocol.rs`:

```rust
pub struct TurnEventPayload {
    pub action: &'static str,        // always "turn_event"
    pub event: String,               // TurnPhase::as_str()
    pub session_id: String,
    pub turn_id: String,
    pub chat_id: String,
    pub partial_content: Option<String>,
}
```

This travels the same `EmitTask` → `InboundTask` path already used for `FinalReplyPayload`. No new IPC primitives are needed — it's a different `action` value in the task JSON.

#### New helper: emit_turn_event

Add a private `emit_turn_event` helper to `AgentRuntime`:

```rust
async fn emit_turn_event(&mut self, session_id: &str, event: &str, partial_content: Option<String>)
```

It reads `final_reply_to`, `final_reply_role`, `final_reply_guest_id`, `turn_id`, and `chat_id` from the active session and fires `IpcRequest::EmitTask` with the `TurnEventPayload`. If there is no active turn (e.g., session not found), it logs a warning and returns without error.

#### Emission points in runtime.rs

Call `emit_turn_event` after these existing `set_active_turn_phase` calls:

| Call site (approx. line) | Phase | event string | Membrane action |
|---|---|---|---|
| line 459 — before model dispatch | `WaitingModel` | `"waiting_model"` | maintain typing |
| line 774 — before tool dispatch | `WaitingTool` | `"waiting_tool"` | maintain typing |
| line 670 — approval branch | `WaitingApproval` | `"waiting_approval"` | stop typing, queue approval card |
| line 1124 — `deliver_text_reply` | `Completed` | `"completed"` | redundant with `send_reply`; omit |
| line 1206 — failure path | `Failed` | `"failed"` | stop typing, send error |

`Completed` is redundant because `deliver_text_reply` already sends `action: "send_reply"` which membrane uses as the delivery trigger. Do not emit a separate `turn_event` for `Completed`.

`Queued`, `LoadingContext`, and `Thinking` transitions are high-frequency internal state; emit them only if membrane has a meaningful differentiation for them (currently it does not).

#### Membrane-side ActiveTurn tracking

Add to membrane's main loop state:

```rust
struct ActiveTurn {
    turn_id: String,
    chat_id: String,
    thread_id: Option<String>,
    typing_task: tokio::task::JoinHandle<()>,
    draft_message_id: Option<i64>,    // set after first sendMessage for progressive delivery
}

active_turns: HashMap<String, ActiveTurn>    // keyed by session_id
```

When dispatching a new inbound update to agent-core:

1. Create `ActiveTurn` for the session.
2. Call `sendChatAction(chat_id, "typing")` immediately.
3. Spawn the typing heartbeat as a tokio task (re-sends every 4 seconds).
4. Insert into `active_turns`.

When receiving `InboundTask`:

- `action = "turn_event"` with `event = "waiting_tool"` or `"waiting_model"` → no change (typing continues)
- `action = "turn_event"` with `event = "waiting_approval"` → abort typing task; send approval card placeholder
- `action = "turn_event"` with `event = "failed"` → abort typing task; send error message; remove from `active_turns`
- `action = "partial_reply"` (future, Layer 2) → send or edit draft message; update `draft_message_id`
- `action = "send_reply"` (existing) → abort typing task; deliver final message; remove from `active_turns`

The typing heartbeat task itself should be cancellation-safe. Use a `CancellationToken` or a channel-based approach rather than relying on `JoinHandle::abort` alone, to avoid leaving a dangling HTTP request in flight.

#### Scope boundary

`philote` emits turn lifecycle events. `membrane` maps them to Telegram UX behaviors. `philote` does not know or care about Telegram-specific typing actions, message IDs, or edit behavior.

### Implementation order

1. Typing indicator heartbeat (Layer 1) — `membrane`-only change, no IPC protocol changes
2. Message length chunking — `membrane`-only change, covers the safety gap immediately
3. `TurnEventPayload` in `agent-core/src/protocol.rs` + `emit_turn_event` helper in `AgentRuntime` — wires `WaitingTool`, `WaitingApproval`, `Failed` to membrane
4. Membrane `ActiveTurn` map + turn event dispatch (Layer 2 foundation)
5. `turn.partial_reply` signal from model streaming once model-router supports chunked output
6. Edit-based progressive delivery (Layer 2) — builds on step 5
7. Full approval card suspension + approval card UX (Layer 3) — builds on step 4

## Media and Voice

Telegram should support multimodal ingress and egress, but Telegram itself is not the speech engine.

Recommendation:

- `membrane` normalizes voice notes, audio, photos, and documents
- media analysis and speech generation/transcription route to dedicated model or voice components
- `membrane` remains the transport adapter, not the media-processing owner

## Transport Recommendation

### Recommended default

Polling remains the default ingress for the first real Telegram controller slice.

Why:

- lower attack surface
- faster route to proven multimodal and command behavior
- avoids making public ingress the first place we discover our session/update contract is underspecified

### Recommended webhook stance

Implement webhook support only after:

- secret-token verification exists
- request limits exist
- update dedupe exists
- tunnel/proxy deployment is documented
- tests and a watched live run prove the callback path

## Recommended Work Item

Start with a transport-hardening slice, not public webhook enablement.

Scope:

- prove the normalized Telegram ingress envelope on the current text polling path
- expand polling ingestion beyond text-only
- elevate deterministic slash commands into `membrane`
- add delivery primitives for typing/partial/final responses
- write the webhook security contract into config/docs/tests without enabling public webhook by default

Out of scope:

- public webhook rollout
- Mini Apps
- inline mode
- business bot flows
- full voice machine integration

## Next Seam

After the first slice proves the normalized Telegram transport boundary, the next highest-value seam is:

- secure webhook ingress as an opt-in deployment mode

That keeps the order honest:

1. prove the Telegram controller boundary
2. prove multimodal/command behavior
3. then open a public callback surface
