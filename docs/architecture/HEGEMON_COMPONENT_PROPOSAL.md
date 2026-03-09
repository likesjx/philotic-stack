# Hegemon Component Proposal

## Goal

Define `hegemon` as a Philotic component type and interface, not as the name of one specific Telegram implementation.

This proposal exists to answer four questions cleanly:

- what a hegemon component is
- what authority it owns
- how it connects to the hotel
- how multiple hegemon implementations can coexist without role confusion

## Core Recommendation

Treat `hegemon` as the component class for Philotic's outside-world membrane.

A hegemon component is the translator, guard, and delivery provider between an external communication surface and the internal Philotic world.

Examples of hegemon implementations:

- `hegemon.telegram`
- `hegemon.https_listener`
- `hegemon.whatsapp`
- `hegemon.imessage`

These are implementations of the same component type, not separate architectural species that just happen to wear similar hats.

## Disposition

Accepted for current slice.

Implemented so far:

- `hegemon` is now explicitly documented as a component type rather than synonymous with Telegram
- the current generic `role = "hegemon"` reply path is explicitly marked transitional
- the hotel IPC layer now supports optional guest-specific local delivery within a shared role
- `agent-core`, `model-router`, and `hegemon` now carry an optional `final_reply_guest_id` so a turn can preserve its owning hegemon incarnation while keeping role-based fallback
- session bindings now persist the transport reply target so new turns inherit their owning hegemon target from session state rather than only from inbound payload scaffolding

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Current Slice

Current repo truth is transitional:

- `crates/hegemon` is a Telegram-oriented guest binary
- the hotel materializes it with `role = "hegemon"`
- agent and model flows still preserve `final_reply_role = "hegemon"` as the fallback reply contract
- agent and model flows can now also preserve an optional hegemon guest target for local delivery
- session bindings now persist the current reply target (`node`, `role`, optional `guest_id`) so the owning hegemon target survives across turns and checkpoint rehydration

This proves the edge-guest idea, but it does not yet define a scalable hegemon interface.

If we keep one generic reply role forever, multiple hegemons will either:

- all receive the same outbound reply
- need hidden side-routing logic
- or force more transport awareness back into `agent-core`

All three are boundary smell.

## What A Hegemon Is

A hegemon is the membrane between Philotic and an external communication environment.

It is responsible for:

- accepting inbound events from a specific external surface
- authenticating and filtering those events according to transport policy
- translating external payloads into normalized Philotic transport events
- binding external identities and chats/threads to Philotic sessions
- projecting Philotic outbound events back into transport-native UX

It is not:

- the owner of agent cognition
- the owner of durable session truth
- the owner of model execution
- a general-purpose arbitrary HTTP gateway
- a place to bury random business logic because "it touches the outside"

## Hegemon Responsibilities

Every hegemon implementation should own the same responsibility categories.

### 1. Ingress Translation

Convert transport-native input into a normalized internal event envelope.

Examples:

- Telegram update -> normalized message/command/callback event
- WhatsApp webhook -> normalized message/media event
- HTTPS listener callback -> normalized approved external event

### 2. Guard / Policy Enforcement

Apply transport-edge checks before a request becomes internal work.

Examples:

- webhook authenticity
- allowlists
- mention/reply policy
- message-type filtering
- body/media size limits
- rate/abuse controls

### 3. Session Binding

Resolve or create the correct Philotic session binding from external identity and conversation context.

Examples:

- Telegram `chat_id` plus topic -> session
- WhatsApp number plus thread context -> session
- HTTPS listener tenant/request channel -> session

### 4. Delivery Projection

Render internal events back into transport-native output.

Examples:

- reply text
- typing indicators
- partial streaming/drafts
- approval cards/buttons
- media/file responses

### 5. Transport-Scoped State

Keep only the minimal transient state needed to operate the transport.

Examples:

- update offsets
- webhook dedupe window
- outbound delivery retry bookkeeping

Canonical truth still belongs elsewhere.

## Non-Responsibilities

Hegemon components should not own:

- the conversation loop itself
- prompt assembly
- provider/model routing strategy
- long-lived memory truth
- generic tool execution
- hotel-wide routing policy

If a hegemon starts deciding how the agent should think, it has crossed the membrane and is trying to annex the utopia.

## Interface To The Hotel

The hegemon-hotel boundary should be explicit and narrow.

### Hotel-owned concerns

The hotel owns:

- component materialization
- component registry and liveness
- canonical session graph
- routing of internal tasks/events between components
- durable config/secrets retrieval

### Hegemon-owned concerns

The hegemon owns:

- transport connectivity
- transport-specific validation
- transport event normalization
- outbound transport rendering

### Required interactions

A hegemon should be able to ask the hotel to:

- fetch component config and secrets
- resolve or create a session binding
- emit normalized inbound work toward `agent-core` or another target
- receive outbound events intended for its session bindings
- record transport events or delivery outcomes when needed

## Recommended Internal Contract

The internal contract should be phrased in terms of normalized events, not Telegram/WhatsApp-specific payloads.

Inbound from hegemon to hotel:

- `ResolveSessionBinding`
- `EmitInboundTransportEvent`
- `RecordTransportCheckpoint` when needed

Outbound from hotel to hegemon:

- `DeliverOutboundTransportEvent`
- `DeliverApprovalRequest`
- `DeliverApprovalResolution`
- `DeliverProgressUpdate`

These names are directional guidance, not a claim that the IPC types already exist in this exact form.

## Component Identity And Materialization

Components are blueprints for materialization.

That means the graph should eventually distinguish:

- component type: `hegemon`
- implementation: `telegram`, `https_listener`, `whatsapp`, `imessage`
- incarnation: the concrete materialized instance on a hotel/environment

Example mental model:

- component type: `hegemon`
- capability: `transport.telegram`
- implementation: `telegram`
- incarnation: `hegemon-telegram-01`

Or:

- component type: `hegemon`
- capability: `transport.whatsapp`
- implementation: `whatsapp`
- incarnation: `hegemon-whatsapp-01`

The exact naming can be refined, but the three layers should stay distinct.

## Routing Recommendation

Do not keep one forever-generic reply target of `hegemon`.

Instead, outbound routing should resolve to the hegemon implementation that owns the session binding.

Recommended direction:

- session binding records the owning hegemon component/incarnation or routable transport target
- `agent-core` emits transport-agnostic outbound events
- the hotel routes those events to the correct hegemon implementation

This avoids teaching `agent-core` whether a session belongs to Telegram, WhatsApp, or a constrained HTTPS listener.

## Why The Current Generic Role Is Transitional

The current `final_reply_role = "hegemon"` path is useful scaffolding, but it does not scale honestly.

Problems it creates once we have multiple hegemons:

- ambiguous fan-out
- wrong-destination replies
- hidden transport coupling
- pressure to reintroduce transport logic into cognitive components

So the rule should be:

- generic `hegemon` role is acceptable as a current-slice shortcut
- guest-specific hegemon targeting is the first implemented bridge away from pure role fan-out
- transport-specific hegemon routing is the real destination architecture

## HTTPS Listener Clarification

An HTTPS listener hegemon should not mean a universal raw webhook sink for everything.

It should mean a deliberate hegemon implementation for one bounded outside-world contract.

Examples:

- `hegemon.https_listener.approvals`
- `hegemon.https_listener.partner_events`
- `hegemon.https_listener.operator_ingress`

That keeps the membrane strong:

- bounded protocol
- bounded auth model
- bounded routing semantics

Not "POST anything here and we will figure out which civilization it belongs to later."

## Relationship To The Telegram Proposal

The Telegram proposal should be read as one hegemon implementation proposal.

Telegram-specific items belong there:

- polling vs webhook
- Telegram slash commands
- Telegram streaming/draft UX
- Telegram media normalization

This proposal defines the more general architectural frame those Telegram choices must live inside.

## Recommended Next Slice

Before broadening Telegram itself further, establish the hegemon component boundary in docs and routing language.

That slice should:

- define `hegemon` as a component type
- mark the generic `role = "hegemon"` reply path as transitional
- define how session bindings route outbound events back to the owning hegemon
- prepare for transport-specific hegemon implementations

## Next Seam

After this boundary is accepted, the next highest-value seam is:

- move reply routing from generic `hegemon` role delivery toward binding-owned hegemon targets

That is the point where the architecture stops talking about membranes poetically and starts actually honoring them.
