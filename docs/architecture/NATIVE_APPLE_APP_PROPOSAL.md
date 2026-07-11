---
title: Native Apple App — Edge Client Program
doc_type: proposal
domain: membrane-transport
status: accepted-current-slice
last_updated: 2026-07-11
tags:
- apple
- ios
- macos
- edge-mesh
- lifegraph
- voice
- device-tools
- eventkit
- attention-steward
related_docs:
- LIFE_GRAPH_OS_PROPOSAL.md
- MCP_COORDINATION_ENDPOINT_PROPOSAL.md
- DISTRIBUTED_CRON_PROPOSAL.md
- OPERATOR_IDENTITY_AND_DANGEROUS_ACTION_CEREMONIES_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: native-apple-app-proposal
implements: []
implemented_by: []
active_seams:
- edge-sessions-bridge
- edge-cursor-ledger
- lifegraph-read-plane
- lifegraph-lens-ui
- steward-review-ui
- device-tool-plane
- eventkit-lifegraph-sync
- edge-push-notification
- streaming-stt-uplink
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- docs/task.md
---

# Native Apple App — Edge Client Program

> Provenance note: the original proposal lived only as graph-owned content
> (`doc:native-apple-app-proposal`, no backing .md) and was dropped by a graph
> rescan. This file recreates and extends it as a file-backed proposal so it
> survives rescans like every other active proposal.

## Goal

Grow the shipped iOS/macOS edge client from "streaming chat + two-way voice
with one philote at a time" into the operator's primary living surface for the
Philotic mesh:

1. **Conversations** — durable, multi-philote conversation management with
   server-canonical history across devices.
2. **LifeGraph** — visualize, interact with, and steward the Life Graph from
   the device: lenses, node detail with provenance, confirm/retire, conflict
   and patch review.
3. **Device capabilities** — the iPhone/Mac becomes a first-class tool runner
   in the mesh: Reminders, Calendar (EventKit), and later HealthKit/Shortcuts,
   invocable by philotes under approval policy.
4. **Attention** — the Attention Steward gets its operator surface: a Today
   view fed by commitments, open loops, and re-entry context, with humane
   nudge delivery.

## What Is Already True (shipped baseline, PR #178 / #217 / #224)

- `philotic-edge-protocol` v1: versioned envelope (`v/seq/ack`), opaque resume
  cursor, enrollment; Swift mirrors byte-checked against golden fixtures.
- Wire types **already defined** for everything this proposal needs:
  `LifeGraphChange`, `ToolInvoke`/`ToolResult`, `CapabilitiesUpdate`,
  `ApprovalRequest`/`ApprovalResolve` — all currently stubbed (logged,
  unrouted, or never emitted).
- philotic-web edge surface: enroll (invite-gated), `GET /api/edge/ws`
  (bearer), `GET /api/edge/agents` (mesh-wide agent directory with authority
  hotels pre-resolved), `POST /api/edge/blob`. Edge bearers are hard-scoped to
  `/api/edge/*`.
- Streaming turns + streaming two-way voice (mic chunk uplink → Scribe STT;
  sentence-pipelined ElevenLabs TTS → gapless chunked playback), reconnect
  resilience on both layers, in-memory replay ring (256) with cursor resume.
- App: `PhiloticKit` (EdgeClient actor, EndpointSelector, enrollment, blob),
  `PhiloticApp` (NavigationSplitView: agent list ‖ chat, voice controls,
  Keychain-backed identity). Local `ConversationStore` is the current source
  of truth; `HistoryHydrator` calls a server route that does not exist.
- Server side live: philotic-web systemd unit on vps-jane (Tailscale :7700);
  mac-jane runs the voice-capable binaries.

## Core Recommendation

Build outward along four planes, in dependency order, reusing the stubs the
protocol already carries instead of inventing new subsystems:

1. **Sessions plane** — make the hotel canonical for conversation history by
   bridging the existing `ListOperatorSessions` / `ListSessionTurns` IPC to
   edge-bearer REST routes; demote the app's `ConversationStore` to a cache
   with optimistic sends. Durable replay via the already-named
   `edge-cursor-ledger` seam.
2. **LifeGraph read plane** — never expose raw Cypher to devices. Add bounded,
   policy-filtered read tools in `data-memorygraphrag` (`life.view.*`),
   bridge them as `/api/edge/lifegraph/*` REST, and finally emit
   `LifeGraphChange` frames for live updates. The app renders **lenses**
   (keyed to the five named retrieval strategies) before any node-link canvas.
3. **Steward surface** — the app is the missing "patch review UX" the Life
   Graph OS proposal calls for: Today view + review inbox driven by
   `life.patch.list`, `life.conflict`/`life.resolve`, SIL entries, and the
   existing `ApprovalRequest`/`ApprovalResolve` wire pair.
4. **Device tool plane** — persist advertised `EdgeCapabilities`, add an
   `edge_device` tool execution route so philote `EmitTask` tool calls are
   delivered over the edge WS as `ToolInvoke` and correlated back from
   `ToolResult`; ship a Swift `ToolHost` with an EventKit toolpack
   (Reminders + Calendar) as the first real device tools.

## App Architecture: Kits and Targets

Introduce a domain layer between transport and UI so iOS and macOS share
everything except shell chrome:

```text
PhiloticKit        (exists)  — wire protocol, EdgeClient actor, enrollment,
                               endpoint selection, blob client. No domain logic.
PhiloticCore       (new)     — @Observable domain stores, platform-agnostic:
  SessionRepository          — server-canonical sessions/turns, optimistic sends,
                               reconciliation, offline queue
  AgentDirectory             — roster + presence, per-philote metadata
  LifeGraphStore             — lens snapshots, node cache, LifeGraphChange
                               application, pending-action queue
  StewardInbox               — approvals, patch proposals, conflicts, SIL
  ToolHost                   — device tool registry + executors (EventKit first),
                               capability advertisement, invocation lifecycle
PhiloticApp (iOS)            — TabView shell: Talk / Life / Today / Settings
PhiloticApp (macOS)          — NavigationSplitView shell + menu-bar extra
                               (quick capture, steward nudges, push-to-talk)
```

Rules: `PhiloticCore` depends only on `PhiloticKit`; views depend only on
`PhiloticCore`; `ToolHost` executors are the only place system frameworks
(EventKit, later HealthKit) are touched, each behind its own capability flag
and OS permission prompt.

## Plane 1 — Conversations With Philotes

### Server

- `GET /api/edge/sessions?agent_id=&limit=` → bridges
  `IpcRequest::ListOperatorSessions` → `OperatorSessionView` JSON.
- `GET /api/edge/sessions/{session_id}/turns?limit=&before_turn_id=` →
  bridges `IpcRequest::ListSessionTurns` → `SessionTurnView` JSON. This is the
  exact route `HistoryHydrator` already speculatively calls — implement it,
  edge-bearer-scoped, and the app's hydration path lights up.
- `edge-cursor-ledger` (existing TODO in `serve/edge.rs`): move the replay
  ring onto `EventStorage`/`CursorStorage` with ledger-minted opaque cursors
  so replay survives philotic-web restarts. Prerequisite for trusting the
  server as history authority.
- Fold minimal presence into `/api/edge/agents`: authority hotel reachability
  and last-seen, so the picker can show which philotes are actually awake.

### App

- `SessionRepository` reconciliation: server list is truth; local store holds
  unsent/optimistic turns keyed by client-generated turn nonce; on reconnect,
  hydrate + replay-diff. Offline sends queue and flush on reconnect.
- Conversation list becomes philote-centric: one thread per philote (session
  key remains `operator-chat:edge:{node}:{agent}`), with resumable prior
  sessions listed per philote.
- Cross-hotel is already transparent (`target_node_id` carries the authority
  hotel; park/materialize handles delivery). The app only needs to render
  hotel provenance, not manage it.
- **Deferred, named**: multi-party rooms (several philotes in one thread).
  The mesh has no room primitive; do not fake one client-side. Cross-philote
  awareness arrives via the Today view instead.

## Plane 2 — LifeGraph Visualization and Interaction

### Principle

Devices get **governed projections, never raw Cypher**. `graph.query` stays
IPC/operator-only. All device reads flow through `data-memorygraphrag` so
zoning, validation-state filtering, and provenance stamping apply uniformly.

### Server

New read-only tools in `data-memorygraphrag` (template-compiled Cypher,
bounded result sizes, provenance envelope included on every node):

- `life.view.lens { lens, params }` — the five named retrieval strategies
  (`open_loops_by_context`, `goals_and_next_actions`,
  `commitments_approaching`, `re_entry_context`, plus generic recall) as
  render-ready packets: nodes + typed edges + evidence paths + scores.
- `life.view.node { id }` — one node, full provenance envelope, bounded
  1-hop typed neighbors, linked evidence/passages.
- `life.view.neighborhood { id, depth<=2, edge_types? }` — bounded expansion
  for the canvas view.

Bridged as edge-bearer REST (same EmitTask/SubscribeInbox round-trip pattern
the chat path uses): `GET /api/edge/lifegraph/lens/{name}`,
`GET /api/edge/lifegraph/node/{id}`, `GET /api/edge/lifegraph/neighborhood/{id}`.

Live updates: emit `LifeGraphChange` at last. The life-graph-runner's write
handlers (`life.observe/commit/resolve/patch.*`) publish a change event;
philotic-web's existing per-node retainer translates it into the retained
`LifeGraphChange` frame (`change_kind`, `node_id`, `label`, `summary`).
The app applies changes to `LifeGraphStore` caches and badges the Life tab.

### App

Lens-first, canvas-second — the graph is for acting, not admiring:

- **Life tab** opens on lens cards: Open Loops, Commitments (due-ordered),
  Goals → Next Actions tree, Re-entry ("where was I on X?"). Each lens is one
  `life.view.lens` call, renders as native lists/trees, and every row is a
  node with provenance chips (validation state, confidence, source membrane,
  last-confirmed).
- **Node detail**: provenance envelope, typed edges grouped by relation,
  evidence passages, and actions — Confirm (`life.commit`), Retire
  (validation-state transition), Add follow-up (creates `NEEDS_FOLLOWUP`
  via `life.observe`), Rate recall (`life.recall.feedback`, which feeds the
  retrieval flywheel).
- **Canvas** (second slice): SwiftUI Canvas force-directed neighborhood view
  seeded from a lens row or search hit, `depth<=2`, typed-edge styling,
  tap-through to node detail. Explicitly not a whole-graph hairball.
- Writes queue in `LifeGraphStore` and reconcile against `LifeGraphChange`
  confirmations, so the UI is optimistic but truthful.

## Plane 3 — Steward Surface (Today + Review Inbox)

The Life Graph OS proposal's open items — "patch review UX", "operator
confirmation gate", SIL reinforcement — land here.

- **Today view** (iOS tab / macOS sidebar section): composed from
  `commitments_approaching` + `open_loops_by_context` + `re_entry_context`
  lenses plus pending review items. One glance = what matters now. Tone
  follows the anti-nagging policy: no red badges for rest days, celebrate
  closures, never present inferred goals as commitments.
- **Review inbox**: patch proposals (`life.patch.list` → approve/defer →
  `life.patch.apply` for low-risk, proposal-only display for high-risk),
  conflicts (`life.conflict` handoffs → `life.resolve`), and SIL entries
  (reinforce / dampen / retire — the confirmations that unlock active
  steward interruptions after 5 confirmed entries).
- **Approvals**: `ApprovalRequest` frames finally get UI — actionable cards
  in Today and as notifications, resolved via `ApprovalResolve`. This also
  serves tool-plane approval gates (Plane 4).
- **Delivery gap, named honestly**: everything above only reaches the device
  while the WS is connected. Steward nudges that must land when the app is
  closed require APNs. That is the `edge-push-notification` seam: a thin
  relay (philotic-web → APNs, content-free "check in" pushes that wake the
  app to fetch over the WS — no life content transits Apple's servers).
  Deferred until the steward has confirmed SIL entries worth pushing.

## Plane 4 — Device Tool Plane (Reminders, Calendar, then more)

### Server

1. **Persist capabilities**: store `EdgeCapabilities.tools` from
   `Hello`/`CapabilitiesUpdate` per enrolled node (today they're dropped).
2. **Execution route**: new `execution_mode: edge_device` in philote's
   `ToolExecutionRoute` resolution. aiua delivers the `ToolExecutionPayload`
   to philotic-web (which holds the operator-surface lease and the WS);
   edge.rs mints `ToolInvoke { invocation_id, tool_ref, args_json }`,
   correlates the returned `ToolResult` by `invocation_id`, and emits the
   result back along the philote's `ReturnRoute`. Timeout + device-offline →
   tool `availability_state: unavailable`, so philotes degrade gracefully
   ("your phone is offline — I'll queue it via cron instead").
3. **Policy**: device tools are approval-classed via the existing
   `ToolPolicyAnnotation` machinery; writes (create reminder/event) default
   to `ApprovalRequest` until the operator trusts them per-tool.

### App — Swift `ToolHost` + EventKit toolpack

First advertised tool refs:

- `os.apple.reminders.create@1` / `list@1` / `complete@1`
- `os.apple.calendar.create_event@1` / `list_events@1` / `availability@1`

Executors run on-device with OS permission prompts; results return as
`ToolResult`. Now "Jane, remind me to call the pharmacy when I get home"
ends as a real EKReminder with a geofence, created by the philote through
the operator's own device.

### Reminders/Calendar ↔ LifeGraph ownership split

Two canonical owners, one mapping — no third source of truth:

| Concern | Owner |
|---|---|
| Meaning: what was promised, to whom, why, evidence | LifeGraph `Commitment` node |
| OS-level alarm/alert delivery, snooze, device sync | EventKit (`EKReminder`/`EKEvent`) |
| Mapping | `ek_identifier` property on the `Commitment` node + `philotic_id` in the EK item's notes/URL field |

`eventkit-lifegraph-sync` (app-side worker, explicit per-source toggles):

- EK → graph: completed reminders and new calendar events emit `life.observe`
  (`Commitment` confirmed-complete; `Event` nodes with
  `source_membrane: edge:{ios|macos}` provenance).
- Graph → EK: `Commitment` nodes with due dates and no `ek_identifier`
  offer "put this on my Reminders" (approval-gated bulk, or per-item).
- Server-scheduled prompts stay on the existing cron plane (`cron.register`,
  Chronos/paracrine heartbeats); EventKit is for alarms that must fire even
  if every hotel is asleep.

## Security Posture

- Edge tier keeps its two-token model (shared or per-device bearer), scoped
  to `/api/edge/*`; new routes inherit that gate. No backbone PSKs on device.
- LifeGraph reads are projection-only; zoning + validation-state filters
  apply server-side in `data-memorygraphrag` before anything reaches wire.
- Mutations from device are limited to the governed `life.*` verbs; risk
  tiers per the Life Graph OS patch policy; high-risk = proposal-only from
  the app, ceremony per the operator-identity proposal.
- EventKit/HealthKit data leaves the device only through explicit per-source
  observe toggles; capability advertisement is itself operator-configurable.
- APNs (when built) carries wake signals only, never content.

## Disposition

`accepted for current slice`

## Slices (dependency-ordered)

| # | Seam | Content | Verification target |
|---|---|---|---|
| 1 | `edge-sessions-bridge` | `/api/edge/sessions*` REST over existing IPC; `SessionRepository` in app; ConversationStore → cache | smoke-green (two devices converge on one history) |
| 2 | `edge-cursor-ledger` | EventStorage/CursorStorage-backed replay, opaque durable cursors | test-green + restart smoke |
| 3 | `lifegraph-read-plane` | `life.view.*` tools + `/api/edge/lifegraph/*` + `LifeGraphChange` emission | smoke-green against vps-jane Memgraph |
| 4 | `lifegraph-lens-ui` | Life tab lenses, node detail w/ provenance + confirm/retire/feedback | watched-live-green (operator session) |
| 5 | `steward-review-ui` | Today view, review inbox (patches/conflicts/SIL), ApprovalRequest UI | watched-live-green |
| 6 | `device-tool-plane` | capability persistence, `edge_device` route, ToolInvoke/Result correlation, Swift ToolHost + EventKit toolpack | watched-live-green (philote creates a real reminder) |
| 7 | `eventkit-lifegraph-sync` | Commitment↔EKReminder mapping + observe worker | watched-live-green |
| 8 | `edge-push-notification` | APNs wake relay for steward nudges | deferred until SIL maturity |

Parallel-safe pairs: 1+3 (different routes/crates), 4 tracks 3, 5 tracks 3.
Slice 6 is the largest and should be its own workstream.

## Open Questions

- Canvas rendering: SwiftUI Canvas vs SpriteKit for the force layout at
  ~200 visible nodes — prototype in slice 4, decide by feel on device.
- Should `life.view.lens` results carry pre-computed layout hints (server
  knows the graph shape) or is client-side layout sufficient at lens scale?
- Per-device capability policy: does the operator gate tool advertisement
  per device (Mac may create calendar events, phone may not)?
- Widget/App Intents surface (Siri "what's on my plate?" → `re_entry_context`
  lens) — after slice 4, likely cheap and high-leverage.
- Does the steward's nudge tone policy live in SIL only, or does the app
  keep a local quiet-hours override the graph never sees?
