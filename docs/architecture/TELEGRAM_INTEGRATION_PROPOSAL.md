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

## Approval UX in Telegram

Telegram should present approval flow as a transport-native operator interaction, not just a generic agent reply.

Near-term recommendation:

- approval request messages should include:
  - approval ID
  - tool or action name
  - argument summary
  - short reason/policy context
- approval resolution messages should include:
  - approval ID
  - decision (`approved` / `denied`)
  - whether execution resumed, redirected, or stopped

Target message shape:

- `Approval required for tool \`glob_search\``
- `Request ID: \`apr-123\``
- `Args: \`{\"pattern\":\"**/*.json\"}\``

and then:

- `Approved pending request \`apr-123\` for this invocation of \`glob_search\`.`

This should later grow into inline Telegram buttons when appropriate:

- `Approve`
- `Deny`
- possibly `Approve + note`

Fallback command path should still exist:

- `/approve <request_id> [note]`
- `/deny <request_id> [redirect]`

This makes approval flows auditable, legible, and much less dependent on the user remembering invisible internal state.

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

## Future Communication Plane Thread: Nostr

Nostr is worth keeping in the active design set as a possible communication-plane component.

Why it is interesting:

- decentralized relay model
- key-based identity instead of platform accounts
- naturally event-oriented transport
- could support agent-to-agent or bot-to-bot communication without central platform dependency

Why it is not an immediate implementation target:

- public-by-default posture is awkward for internal coordination
- encrypted DM ergonomics are still rough
- relay trust, spam, and private topology questions need a security-first answer

Current recommendation:

- keep Nostr pinned as a research/design thread
- revisit it as a possible external or decentralized communication-plane transport after the Telegram and core session/tooling paths are more mature

## Recommendation

- raise slash commands to `hegemon` soon
- keep Telegram-specific delivery and media logic in `hegemon`
- let `agent-core` remain transport-agnostic
- treat streaming and voice handoff as first-class Telegram integration concerns, not afterthoughts
