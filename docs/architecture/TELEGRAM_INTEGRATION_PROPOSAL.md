# Philotic Telegram Integration Proposal

## Goal

Define the next-stage Telegram integration so it becomes a proper transport boundary instead of just a text ingress/egress shim.

This proposal focuses on:

- Telegram-side slash-command elevation
- streaming and delivery UX
- media-aware session handling
- clean separation between transport behavior and agent cognition

## Disposition

Proposed and pinned for near-term work.

The current Telegram path works for plain text turns, but Telegram-side `/commands` elevation is still pending and should be treated as a practical testing enabler, not only a UX nicety.

Track active work in [task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Core Recommendation

Keep `hegemon` as the Telegram transport boundary and elevate transport-native behavior there before the normal agent loop.

That means `hegemon` should own:

- Telegram update parsing
- slash-command parsing
- message/media ingress normalization
- Telegram-specific delivery behavior
- streaming/chunked reply UX

`agent-core` should continue to own cognition, not Telegram semantics.

## Why This Needs Its Own Proposal

Telegram is no longer just "text in, text out."

We already need to think about:

- `/commands`
- approval UX
- eventual streaming
- TTS/STT/voice handoff
- media attachments
- latency and interruption behavior

If all of that gets stuffed into the generic turn path, we will eventually end up debugging transport quirks inside the agent loop, which is both rude and avoidable.

## Telegram Slash Commands

### Recommendation

Elevate slash-command parsing into `hegemon`.

Flow:

1. Telegram update arrives
2. `hegemon` detects `/command`
3. `hegemon` emits a structured command payload over IPC
4. deterministic commands bypass the full model loop
5. session/turn metadata still remain consistent

### Why Do This Now

- faster operational testing
- less latency for deterministic commands
- cleaner approval and session control UX
- better separation between transport control and cognition

### Near-Term Commands To Raise

- `/ping`
- `/status`
- `/pause`
- `/resume`
- `/approve`
- `/deny`
- `/preapprove`
- `/approval status`
- `/approval reset`

## Streaming and Delivery UX

Telegram integration should eventually support more than final-message delivery.

Near-term design questions:

- whether partial text should stream to Telegram incrementally
- how edits vs follow-up messages should be used
- how approval waits should appear mid-turn
- how to cancel or supersede stale partials

Recommendation:

- keep the canonical turn state in the graph
- let `hegemon` project partial progress into Telegram UX
- do not let Telegram-specific delivery mechanics leak into `agent-core`

## Media and Voice Handoff

Telegram should be able to hand voice/media work off to a dedicated voice component rather than forcing `hegemon` or `agent-core` to own audio pipeline details.

That means Telegram integration should be designed to normalize:

- text messages
- voice notes
- audio attachments
- future richer media inputs

and then route them into either:

- normal text session flow
- voice machine flow
- hybrid text+voice flow

## Recommendation

- raise slash commands to `hegemon` soon
- keep Telegram-specific delivery and media logic in `hegemon`
- let `agent-core` remain transport-agnostic
- treat streaming and voice handoff as first-class Telegram integration concerns, not afterthoughts
