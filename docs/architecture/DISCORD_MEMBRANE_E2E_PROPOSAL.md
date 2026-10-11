---
title: Discord Membrane End-to-End Proposal
doc_type: proposal
domain: membrane-transport
status: proposed
last_updated: 2026-08-12
tags:
- discord
- membrane
- voice
- full-duplex
- clippy-ratchet
related_docs:
- ARCHITECTURE_STATUS.md
- TELEGRAM_POLL_LEASE_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: discord-membrane-e2e
implements: []
implemented_by: []
active_seams:
- discord-text-e2e
- discord-session-resume
- discord-speaker-attribution
- discord-outbound-voice
- discord-full-duplex
- discord-ratchet-admission
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Discord Membrane End-to-End (`proposal:discord-membrane-e2e`)

Bring `membrane-discord` from "compiles and half-works" to a **watched-live,
full-duplex Discord membrane**: text both ways, voice both ways, sessions that
survive reconnects, and the crate finally admitted to the clippy ratchet
because its dead code became live code (or was honestly deleted).

## Why now

The clippy ratchet (rounds 3–5, PRs #413/#417) deliberately left this crate
ungated: **15 dead-code warnings, 12 of them in the voice path.** Dead code is
evidence. Mapping it against the source gives a precise picture of what is
unfinished, and that map is this proposal's seam list. Blanket-allowing the
warnings would have destroyed exactly the signal this document is built from.

## Current state (evidence, 2026-08-12)

Verified by reading the crate (3,814 lines, 11 modules) and the lint map, not
from memory:

**Works (wired end to end):**
- Gateway WebSocket connect, Identify, heartbeat, event dispatch
  (`gateway.rs`, 573 lines).
- Split-brain prevention via `DiscordGatewayLease` (`lease.rs`, 558 lines) —
  same pattern as the Telegram poll lease.
- Global slash-command registration at startup (`main.rs:132`).
- Inbound text → hotel turn → reply → Discord text channel.
- **Inbound voice**: VoiceStateUpdate/VoiceServerUpdate pairing → voice
  gateway → UDP receive → decrypt → `VoiceBridge` → `VoiceUtteranceEvent` →
  hotel turn. Replies to spoken input land **as text** in the associated text
  channel (`main.rs:199-215`).

**Broken or unfinished (the dead code, itemised):**

| Evidence | Meaning |
|---|---|
| `voice_udp::send_opus_frame` exists; `send_silence_frames`, `send_speaking_state` **never called** | **The bot cannot speak.** The entire outbound audio path — TTS → Opus frames → UDP — is scaffolded and unwired. |
| `voice_gateway` fields `user_id`, `ssrc`, `speaking` never read | Speaker attribution is decoded and dropped: utterances are not attributed to who said them. |
| `session.rs`: `voice_text_associations` never read, `SessionOverrides` never used, session fields dead | Voice/text session correlation is designed but not enforced; a voice session and its text channel drift apart on reconnect. |
| `GatewayEvent::Ready.session_id`/`resume_url` carried, never consumed | No RESUME: every disconnect is a fresh Identify, dropping in-flight voice state (documented in round 5). |
| `envelope.rs` `transport` never read | Envelope metadata is stamped but not consumed downstream. |
| `register_guild_commands` unused | Dev-only nicety; fine to keep or delete. |

**Deployment reality:** the binary ships in `AIUA_BINS` and the release set,
but no memory or graph record shows a Discord guest **materialized on any
hotel**. E2E here must end watched-live on a real server, per
`$verification-ladder`.

## Constraints (learned the hard way, this fleet)

1. **TTS audio must go through the blob store, never inline.** DEF-080's
   aggravator was 8.6MB turns carrying inline `audio_base64` — Telegram's
   voice path bypassing the blob store helped melt aiua. The Discord outbound
   path must carry blob refs from day one (blob store is :9001).
2. **Budgets**: voice turns ride the same turn-budget layering as everything
   else (DEF-079); the bridge must not hold turns open while streaming audio.
3. **Any "X hasn't happened lately" detector needs an upper bound too**
   (S6 lesson) — applies to the silence/idle detection in S4.
4. **Watched-live is the bar** — smoke-green Discord voice has never been the
   gap; a live guild session with a human speaking is.

## Slices

Each slice is independently shippable, PR'd into `develop`, and ends with its
verification rung recorded via `graph_record_test_run`.

- **S1 — Text E2E watched-live (baseline).** Materialize the Discord guest on
  one hotel (mbp-jane suggested; it already runs the busiest membrane mix),
  register the bot in a test guild, prove text round-trip + slash commands
  watched-live. No code expected beyond config; this flushes out deploy
  reality (guest seeding, vault token, lease).
  *Exit: an operator message in Discord answered by an agent, logged turn ids.*

- **S2 — Sessions that survive reconnects.** Implement RESUME using the
  carried `session_id`/`resume_url`; wire `session.rs`'s voice/text
  association so a voice session's replies always land in its paired text
  channel, across reconnects. Deletes the `envelope.transport` dead field or
  starts consuming it.
  *Exit: kill the WS mid-session; conversation continues without a new
  Identify; association test in CI.*

- **S3 — Speaker attribution.** Consume `ssrc`/`user_id`/`speaking` mapping so
  each `VoiceUtteranceEvent` carries who spoke. Prerequisite for multi-user
  channels and for S5's barge-in.
  *Exit: two speakers in a channel produce correctly-attributed turns.*

- **S4 — The bot speaks (outbound voice).** TTS via model-router (Kokoro /
  ElevenLabs already routed there), audio as **blob refs**, Opus-encode, and
  drive the already-written `send_speaking_state` → `send_opus_frame` →
  `send_silence_frames` sequence. Reply routing gains a voice/text decision:
  spoken input gets a spoken reply plus text transcript.
  *Exit: ask a question by voice in a live guild, hear the answer.*

- **S5 — Full duplex + barge-in parity.** Port the barge-in behaviour proven
  on the mac-jane edge client (2026-07-12): operator speech interrupts bot
  playback; silence-as-signal rules from S6 apply with both bounds.
  *Exit: interrupt the bot mid-answer watched-live; it stops within one frame
  budget.*

- **S6 — Ratchet admission.** With the above, the 15 warnings are either live
  code or deleted. Gate the crate (`[lints] workspace = true`); the clippy job
  then keeps it honest.
  *Exit: `membrane-discord` gated, workspace clippy exit 0.*

## Explicit non-goals

- Multi-guild scale-out, stage channels, Discord threads/forums.
- Speaker *verification* (identifying who a voice belongs to biometrically) —
  attribution in S3 is SSRC→user mapping only.
- Replacing the Telegram membrane's role; Discord is additive.

## Verification ladder

S1 watched-live → S2/S3 test-green + smoke → S4/S5 watched-live in a real
guild (voice cannot be meaningfully smoke-tested without ears) → S6
mechanical. Each slice records its run; the proposal is `implemented` only
when S5 is watched-live green.

## Open questions for the operator

1. Which hotel hosts the Discord guest first — mbp-jane (busiest membranes) or
   mac-jane (has the TTS/voice history)?
2. Is there an existing bot application + token in the vault, or does S1 start
   at the Discord developer portal?
3. Latency bar for S4: Telegram voice replies tolerate seconds; live voice
   chat does not. Is "conversational" (<2s to first audio) the target, or is
   this v1 acceptable as push-to-talk-ish?
