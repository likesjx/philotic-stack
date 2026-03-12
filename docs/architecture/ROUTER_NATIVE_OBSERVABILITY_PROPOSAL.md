# Router-Native Observability Proposal

## Goal

Define an observability model where structured events flow through the router/event plane and are consumed by attachable listeners, instead of relying on a privileged native logger subsystem as a second authority.

## Disposition

`proposed`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Current Slice

- pin router-native observability as the preferred architecture
- keep a minimal emergency sink outside the router for bootstrap and fatal failures
- define the first workstream for event envelopes, listeners, and durable sinks in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)

## Core Recommendation

Philotic should treat observability as an **event distribution problem**, not as a pile of unstructured log strings sprayed directly to sinks.

Recommended model:

1. components emit structured events
2. events travel through the router/event plane
3. listeners subscribe by filter
4. sinks decide what to persist, display, analyze, or ignore

This keeps observability aligned with the rest of the architecture:

- one transport/event plane
- multiple pluggable consumers
- no special logger god-object quietly becoming the real truth owner

## What This Should Support

- console/dev listeners
- archival/file sinks
- graph/event-store sinks
- live TUI/web streams
- audit streams
- evaluation / reinforcement datasets
- metrics extraction

The same event plane should be able to power:

- debugging
- admin inspection
- incident review
- future model reinforcement or training traces

without inventing a second telemetry universe later.

## Why This Is Better

This approach gives Philotic:

- structured filtering by role, component, session, capability, severity, or event kind
- replayable traces
- attachable/detachable listeners
- a single source for runtime observation and later analysis

It also keeps “logging” from quietly becoming architecture by accident.

## Important Caveat

Do **not** make the system totally silent before the router is healthy.

Philotic still needs a minimal emergency sink for:

- fatal startup failures
- pre-router bootstrap failures
- crash-path diagnostics

Recommended boundary:

- router-native observability is the primary model
- stderr / ring-buffer emergency sink is the bootstrap safety rail

Otherwise the irony is savage: the router dies before it can route the logs about why it died.

## Event Shape Recommendation

Prefer structured events over prose-first logs.

Example kinds:

- `component.started`
- `component.failed`
- `task.routed`
- `task.failed`
- `provider.error`
- `membrane.delivery.sent`
- `turn.phase.changed`
- `approval.requested`
- `approval.resolved`

Each event should be able to carry:

- event kind
- severity
- component / hotel / node identity
- session and turn identity when relevant
- capability / route context
- structured payload
- trace metadata

## Listener Model

Listeners should be attachable by filter rather than globally hardcoded.

Examples:

- all `provider.error` events
- only one session
- only `membrane` and `agent-core`
- only one hotel
- only validation traces

This naturally supports future reinforcement/eval pipelines without forcing every runtime path to know about them directly.

## First Slice Recommendation

1. define the first structured observability event envelope
2. add a listener registration/filter model
3. route a small first set of component events through it
4. keep a tiny bootstrap/fatal emergency sink outside the router
5. prove one console listener and one persistent sink

## Relationship To Other Proposals

- [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md)
  - admin surfaces should consume structured observability instead of scraping strings
- [MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md)
  - multi-hop execution needs auditable event trails
- [HOTEL_PERIMETER_TRUST_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HOTEL_PERIMETER_TRUST_PROPOSAL.md)
  - trust rejections and perimeter decisions should be structured, filterable events
- [PROPOSAL_ORGANIZATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PROPOSAL_ORGANIZATION_PROPOSAL.md)
  - observability is one of the first domains that will benefit from explicit proposal grouping, backlinks, and lightweight tags
