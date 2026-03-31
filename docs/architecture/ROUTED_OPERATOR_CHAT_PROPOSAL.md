---
title: Routed Operator Chat Proposal
doc_type: proposal
domain: operator-control-plane
status: proposed
last_updated: 2026-03-31
tags:
- operator-chat
- router
- membrane
- telegram-parity
- graph-runner
related_docs:
- OPERATOR_MEMBRANE_PLUGIN_BOUNDARY_PROPOSAL.md
- DESKTOP_MEMBRANE_PROPOSAL.md
- TELEGRAM_INTEGRATION_PROPOSAL.md
- INTER_HOTEL_ROUTING_PROPOSAL.md
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md
proposal_id: routed-operator-chat
implements: []
implemented_by: []
active_seams:
- operator-membrane-plugin-boundary
- desktop-membrane-boundary
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Routed Operator Chat Proposal

## Goal

Define operator chat as a router-resolved agent conversation surface so the desktop membrane can talk to local or remote agents with the same underlying authority path as Telegram, while leaving backing data ownership free to move between hotel-local state and an agent-centric `graph-runner`.

## Core Recommendation

Treat operator chat as a **membrane ingress into the canonical agent conversation plane**, not as a separate admin RPC family.

Recommended rule:

1. operator chat to agents should use the same underlying conversation/session path as Telegram
2. the membrane should hand the turn to the local router, not handcraft target-hotel choreography itself
3. the router should resolve whether the target authority is:
   - local hotel-owned session/runtime state
   - a remote hotel/router path
   - a future agent-centric `graph-runner` authority
4. backing data placement should be a router/configuration concern, not a membrane contract concern
5. operator control-plane queries like `operator.targets.*` remain sibling surfaces, not disguised chat turns

Put differently: Telegram and desktop should be two membranes over one conversation plane. If we let desktop chat become a second special control path, the router has failed at the exact job it was hired for.

## Disposition

`proposed`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Current Slice

This slice now has a first implementation foothold.

Current truth:

- the reusable operator target surfaces now exist for `list`, `status`, `guests`, and `agents`
- routed remote operator reads now use generic `OperatorSurfaceQueryHandoff`
- desktop target routes are adapters over the generic operator seam
- operator chat now has a first thin routed contract:
  - shared IPC exposes `SendOperatorChatTurn`
  - `aiua` submits the turn into the canonical agent conversation path via routed `EmitTask`
  - `philotic-web` exposes `POST /api/mesh/targets/:target_node_id/agents/:agent_id/chat` as a thin desktop adapter
  - `SendOperatorChatTurn` observes `turn_event` messages on that reply path before the first final reply and returns them in the reply envelope
  - the current desktop adapter now returns `202 Accepted`, then streams in-flight `operator_chat:turn_event`, `operator_chat:partial_reply`, `operator_chat:reply`, and `operator_chat:error` updates over the existing `/ws` channel while the routed turn is in flight
  - the lower conversation/model path can now optionally carry `partial_replies` in `model_result.result.partial_replies`, which `philote` turns into real `partial_reply` frames before the final reply
  - default providers still emit no partial chunks unless they explicitly supply them, so progressive delivery is now possible without pretending every provider is already streaming

This still leaves the broader operator-chat seam intentionally incomplete: richer session continuity, provider-native incremental generation instead of optional batch partials, and watched live remote-hotel proof remain follow-on slices.

## Why This Needs Its Own Contract

The desktop membrane is now mesh-aware enough to inspect targets and agents across hotels.

The natural temptation is to say: “great, now add a chat panel and route some messages.”

That temptation is precisely where architecture develops a sense of irony.

If operator chat is added as:

- desktop-specific IPC actions
- desktop-specific remote delivery workers
- desktop-specific session materialization rules
- desktop-specific storage expectations

then we will have rebuilt a second conversation plane beside the one Telegram already uses.

That would be worse than duplication. It would create conflicting truth about what a conversation with an agent even is.

## Shared Conversation Rule

When the operator chats with an agent from the desktop membrane, the system should behave like Telegram in every semantically important way:

- the operator addresses a real target agent
- the turn enters canonical session/turn machinery
- tool use, approval posture, and memory projection follow the normal agent path
- local and remote delivery both resolve through the router
- replies come back through the same routed conversation path

The membrane layer may differ in presentation:

- Telegram renders transport-specific message UX
- desktop renders windows, panes, agent pickers, richer context, and live status

But those are membrane concerns, not conversation-authority concerns.

## Router As Placement Resolver

The router should answer the question:

“Where does the authoritative conversation/session/context owner live for this target agent right now?”

That resolution may point to:

- the local hotel runtime
- a remote hotel router
- a future graph-runner-backed authority

The membrane should not care which of those is true beyond attribution and error reporting.

That gives us the durable property we want:

- moving agent/session/context ownership from hotel-local ODS/graph storage to a graph-runner does not require new membrane contracts
- it changes routing/configuration and authority resolution, not membrane semantics

## Surface Split

Keep these planes distinct:

### Operator surface queries

Examples:

- `operator.targets.list`
- `operator.targets.status`
- `operator.targets.guests`
- `operator.targets.agents`

These are control-plane queries. They ask the system to report operator-visible truth.

### Agent conversation surfaces

Examples:

- `operator.chat.start`
- `operator.chat.send_turn`
- `operator.chat.observe`
- `operator.chat.close`

These are conversation-plane interactions. They address a target agent and participate in the canonical turn/session lifecycle.

The desktop UI may render both in one workspace, but they are not the same protocol wearing different fonts.

## Proposed Routed Operator Chat Envelope

The first routed operator chat handoff should be explicit and router-resolved.

Suggested fields:

- `handoff_kind`
- `conversation_surface`
- `request_id`
- `source_hotel`
- `target_hotel`
- `target_node_id`
- `target_agent_id`
- `caller_kind`
- `caller_id`
- `operator_session_id`
- `conversation_id`
- `message_id`
- `intent`
- `payload`
- optional `trace`

Recommended initial semantics:

- `handoff_kind = "operator_chat_turn"`
- `conversation_surface` names the membrane-to-agent conversation action
- `payload` carries the operator utterance plus bounded rendering metadata
- router resolution determines where the turn is fulfilled

This handoff should be transport-agnostic and authority-oriented.

It is not a browser payload, not a WebSocket frame schema, and not a desktop-only convenience object.

## Conversation Semantics

### Local target

When the target agent is local:

- the local router may hand the turn directly to the local conversation authority
- the result should still be recorded through the same canonical turn/session model

### Remote target

When the target agent is remote:

- the local membrane hands the turn to the local router
- the local router hands off across the mesh
- the remote router/hotel delivers into the target agent's canonical conversation path
- replies route back through the same path

The membrane should not need a separate remote-agent chat transport family.

### Future graph-runner-backed authority

If a future graph-runner owns more of the canonical agent/session/context truth:

- router resolution may direct the handoff there
- the membrane contract stays the same
- the operator still experiences one stable conversation surface

This is the whole point of the router: backing data location becomes configuration at best, not membrane ceremony.

## Non-Goals

This proposal does **not** recommend:

- collapsing operator control-plane queries into chat
- making the desktop membrane the canonical conversation owner
- exposing raw graph-runner or hotel storage details to membranes
- inventing a desktop-only chat protocol that later needs Telegram parity retrofitted onto it

## First Implementation Slice

The first implementation slice should be intentionally modest:

1. define the routed operator-chat contract and envelope
2. map it onto the same canonical conversation/session path used by Telegram
3. support local and remote target-agent delivery through router resolution
4. return explicit attribution for source/target authority and delivery path
5. leave desktop UI as a thin adapter over that contract

## Reality Gap

What is already proven:

- reusable operator query surfaces can be extracted from desktop-specific naming
- the router handoff pattern works for remote operator reads
- desktop can be a membrane adapter instead of a privileged local file browser

What is not yet proven:

- that desktop operator chat can reuse the Telegram-grade conversation path without sneaking UI-specific orchestration into daemon core
- that router resolution can hide a future shift from hotel-local conversation truth to graph-runner-backed authority without membrane churn
