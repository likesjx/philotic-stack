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

This proposal assumes the broader hegemon boundary defined in [HEGEMON_COMPONENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HEGEMON_COMPONENT_PROPOSAL.md): Telegram is one hegemon implementation, not the definition of hegemon itself.

## Core Recommendation

Keep `hegemon` as the canonical Telegram transport boundary.

More precisely for long-term architecture:

- `hegemon` is the component type
- Telegram is one hegemon implementation
- the current `crates/hegemon` binary is a transitional Telegram-oriented implementation of that type

For the first coherent slice:

- keep polling as the default production ingress
- design webhook ingress now, but gate it behind a stricter security contract than polling
- raise Telegram-native control behavior into `hegemon`
- normalize Telegram updates into a transport-neutral envelope before handing them to `agent-core`
- keep `agent-core` transport-agnostic and focused on cognition

In other words:

- `hegemon` owns Telegram semantics
- `agent-core` owns the conversational/agent loop
- the context graph owns durable session truth

## Disposition

Accepted for current slice.

Accepted here means:

- polling remains the default ingress until webhook security gates are implemented
- webhook support is in scope as a follow-on transport capability, not the first thing we trust with a public attack surface
- active work should be tracked in [task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)

## Current Slice

Current repo truth:

- `crates/hegemon` is a long-polling Telegram guest
- it now normalizes one canonical inbound path for:
  - `message.text`
  - captioned media messages
  - attachment-only media/file messages
  - `callback_query`
- it emits a normalized inbound transport envelope into `agent-core` with:
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
- it now preserves an optional `final_reply_guest_id` and session-level transport reply target so the owning local hegemon guest survives beyond a single turn without relying only on shared-role fan-out

That is a useful membrane slice, but it is not yet a richer Telegram controller:

- slash commands still execute in `agent-core`
- attachment handling now resolves `file_id` values through Telegram `getFile`, downloads bytes, and uploads them into the hotel blob service
- blob-backed Telegram attachments now have an initial downstream path through `agent-core` and `model-router` as `media.analyze`, so supported photos, audio/voice notes, and documents can reach Gemini for first-pass interpretation
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

- the current Telegram implementation in `crates/hegemon`
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

- [crates/hegemon/src/main.rs](/Users/jaredlikes/code/philotic-stack/crates/hegemon/src/main.rs#L74) shows `getUpdates` polling as the actual ingress path.
- [docs/walkthrough.md](/Users/jaredlikes/code/philotic-stack/docs/walkthrough.md#L26) still says "Telegram Webhook hits the independent `hegemon` process."
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
- extending this one field at a time encourages partial transport truth scattered across `hegemon` and `agent-core`

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

`hegemon` should own:

- inbound Telegram update ingestion
- webhook or polling transport specifics
- slash-command parsing
- callback-query parsing
- attachment/media normalization
- Telegram reply projection
- streaming/draft behavior
- approval-card formatting

`agent-core` should own:

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

This envelope should become the one object `hegemon` emits into the rest of the system.

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

- near-term: keep model output transport-neutral and let `hegemon` translate a supported Markdown subset into Telegram-safe HTML
- medium-term: define an outbound rich-text contract above Telegram so `hegemon` can project to explicit Telegram entities without teaching `agent-core` transport-specific markup trivia
- respect Telegram-specific limits when projecting:
  - normal message text length after formatting parse
  - caption length limits for media messages

Current implementation note:

- `hegemon` now projects outbound `sendMessage` replies through a Markdown-subset to Telegram HTML formatter
- the current supported subset includes headings, bold, italic, strikethrough, inline code, fenced code blocks, links, blockquotes, and simple lists
- explicit Telegram entities and length-aware chunking/fallback are still follow-on work

## Slash Commands

### Recommendation

Elevate slash-command parsing into `hegemon`.

Flow:

1. Telegram update arrives
2. `hegemon` normalizes it
3. `hegemon` detects a deterministic `/command`
4. `hegemon` emits a structured control payload or handles a transport-local action
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

Recommendation:

- keep canonical turn state in the graph
- let `hegemon` project partial progress into Telegram delivery UX
- prefer one transport-specific projection layer instead of leaking edits/drafts into `agent-core`

Near-term behaviors:

- `sendChatAction` for typing/upload cues
- partial delivery via draft/update methods when available
- final message commit when a turn completes
- interruption policy scoped to same sender and same chat/thread

## Media and Voice

Telegram should support multimodal ingress and egress, but Telegram itself is not the speech engine.

Recommendation:

- `hegemon` normalizes voice notes, audio, photos, and documents
- media analysis and speech generation/transcription route to dedicated model or voice components
- `hegemon` remains the transport adapter, not the media-processing owner

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
- elevate deterministic slash commands into `hegemon`
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
