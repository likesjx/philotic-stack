---
title: Apple Intelligence Plane — Siri, App Schemas, Foundation Models
doc_type: proposal
domain: membrane-transport
status: proposed
last_updated: 2026-07-26
tags:
- apple
- ios
- ipados
- macos
- apple-intelligence
- siri
- app-intents
- foundation-models
- spotlight
related_docs:
- NATIVE_APPLE_APP_PROPOSAL.md
- LIFE_GRAPH_OS_PROPOSAL.md
proposal_id: apple-intelligence-plane-proposal
implements:
- native-apple-app-proposal
implemented_by: []
active_seams:
- apple-entity-index-plane
- apple-schema-intents
- apple-custom-intents
- apple-fm-provider
- apple-fm-local-triage
- apple-intents-testing
- ipados-shell
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- docs/task.md
---

# Apple Intelligence Plane

> Extends `NATIVE_APPLE_APP_PROPOSAL.md` as **Plane 5**. Planes 1–4 make the
> app a good mesh client. Plane 5 makes the *operating system* a client of the
> mesh. It reuses planes 1–4's seams rather than inventing subsystems.

## The One Structural Fact That Shapes Everything

Apple Intelligence exposes **two entirely separate integration surfaces**, and
conflating them produces a design that cannot ship:

| | **App Intents / App Schemas** | **Foundation Models** |
|---|---|---|
| Who calls it | Siri / Spotlight / Shortcuts — the *system*, on the user's behalf | Your own app's code |
| Direction | System → into our app | Our app → into a model |
| Scope | System-wide, cross-app | **App-local only** |
| Buys us | Philotic content + actions reachable by voice anywhere in iOS | Our mesh as the model behind our own AI features |

**A custom `LanguageModel` provider is scoped to the adopting app.** Registering
philotic-stack as a Foundation Models provider does *not* make Siri route through
our mesh, and does not serve other apps' sessions. Apple Intelligence acts as a
first-party agent and reaches third-party apps **only** through App Intents.

Corollary: **we cannot ship a Siri replacement.** The `assistant` schema domain
(side-button launch of a voice conversational app) is **Japan-only** and consists
of a single `activate` intent. Any design premised on "Philote becomes your Siri"
is dead on arrival outside Japan. What we *can* do is make Siri an excellent
front door to philotic content and actions, and use our mesh as the brain inside
our own app.

## Schema Domain Reality Check

Schema-conforming intents get Siri's semantic index and personal-context
reasoning. Non-conforming custom intents get Shortcuts and Spotlight, but not
deep Siri language reasoning. The domain list is fixed and system-defined:

- **Primary** (Apple Intelligence + Siri): audio, calendar, camera, clock, files,
  mail, maps, messages, notes, phone, photos, reminders, system-and-in-app-search
- **Single-purpose**: assistant *(Japan-only)*, visual-intelligence
- **Shortcuts-only** (no Siri reasoning): books, browser, journaling,
  presentation, reader, spreadsheet, whiteboard

There is **no "personal knowledge graph" or "AI agent" domain.** The honest
mapping for philotic-stack:

| Philotic concept | Schema domain | Fit |
|---|---|---|
| LifeGraph node / Muninn memory | `system` (Open) + `IndexedEntity` | **Strong** — this is the load-bearing one |
| Commitments, open loops | `reminders` | Strong |
| Time-bound commitments | `calendar` | Strong (EventKit already planned) |
| Observations, re-entry notes | `notes` | Moderate |
| Agents, whisper, cron, LifeGraph lenses | *(no domain)* | Custom intents only |

## Slice A — Entity Index Plane (do this first)

**Seam: `apple-entity-index-plane`.** Highest value, lowest risk, no new server
work — it rides `lifegraph-read-plane`, which has **shipped** (verified
2026-07-26): `life.view.node` / `life.view.neighborhood` handlers in
`crates/data-memorygraphrag/src/provider.rs:487`, edge-bearer REST routes
registered in `crates/philotic-web/src/serve.rs:696-704`, and a built Swift
`LifeGraphClient` with `fetchLens` / `fetchNode`. The read plane is real, so
entity donation has something to donate on day one.

Conform LifeGraph nodes and Muninn memories to `AppEntity` + `IndexedEntity`,
and donate them via `indexAppEntities` on cache fill / `LifeGraphChange` apply,
`deleteAppEntities` on retire. Result: **philotic memory becomes searchable from
Spotlight with attribution, and enters Siri's personal-context reasoning.** Ask
Siri about something you told a philote three weeks ago and it can surface it.

Governance carries over unchanged: only nodes already permitted through
`life.view.*` projections are ever indexed, provenance chips survive into the
entity's display representation, and zoning/validation-state filtering happens
server-side before anything reaches the index. Nothing raw-Cypher, nothing
unzoned. Index only what the device is already allowed to cache.

## Slice B — Schema Intents

**Seam: `apple-schema-intents`.**

- **`system` domain** — adopt **`Open`** (open a LifeGraph node / memory /
  conversation by reference). Note: the domain's `Search` intent is **marked
  deprecated**; do not build on it. Discovery comes from `IndexedEntity` +
  Spotlight instead, which is where Apple has moved the semantic burden.
- **`reminders` / `calendar`** — conform the Attention Steward's commitments
  and open loops. `device-tool-plane` and `eventkit-lifegraph-sync` already
  plan EventKit work, so schema conformance is incremental on top of it, not
  a new subsystem. This makes the sync bidirectional in a system-legible way.
- **`notes`** — observations and re-entry context, if slices A/B prove out.

## Slice C — Custom Intents (state the ceiling honestly)

**Seam: `apple-custom-intents`.** Everything philotic-specific has no schema:
select/talk-to agent, run a LifeGraph lens, trigger whisper, inspect cron,
capture an observation. These ship as ordinary `AppIntent`s + an
`AppShortcutsProvider`.

They get: Shortcuts, Spotlight, widgets, Controls, Action Button, Siri
invocation **by app-shortcut phrase**. They do *not* get: free-form Siri
language reasoning. Write the phrases deliberately; that's the whole interface.

## Slice D — Foundation Models Inside the App

Two independent uses, both app-scoped.

**D1 — On-device triage (`apple-fm-local-triage`), ship first.** Use Apple's
`SystemLanguageModel` for work that should never leave the device or wait on the
mesh: summarizing a conversation for the Today view, drafting a commitment title
from a voice capture, classifying whether an utterance is a task/observation/
question, generating lens summaries. `@Generable` guided generation gives typed
structs with no parsing. Works offline, zero inference cost, no mesh round-trip.

**D2 — Philotic as a model provider (`apple-fm-provider`).** Implement
`LanguageModel` + `LanguageModelExecutor` in an SPM package (`PhiloticFM`)
that proxies to `model-router` over the existing edge WS. This is a
genuinely clean fit with our architecture:

- `LanguageModelCapabilities([.toolCalling, .guidedGeneration, .reasoning])`
  maps directly onto what model-router already does.
- Guided generation backs tool calling, so the model can never emit an invalid
  tool name — the same invariant our tool-grant work enforces server-side.
- The framework caches executors by configuration, enabling KV-cache reuse and
  minimal network churn — which matches our per-session philote model.
- Map our failures onto built-in `LanguageModelError` cases
  (`contextSizeExceeded`, `rateLimited`, `refusal`, `timeout`).
- Auth via token provider + Keychain, **not** a plain key string; App Attest
  for device attestation. Our edge bearers are already hard-scoped to
  `/api/edge/*`, so this reuses enrollment rather than adding a credential.

Payoff: one `LanguageModelSession` API for app AI features, with the *same
call site* served by Apple's on-device model when offline/cheap and by the
philotic mesh (with full agent context, LifeGraph access, and mesh tools) when
connected. Provider selection becomes a policy decision, not a rewrite.

**Do not count on free PCC.** Foundation Models on Private Cloud Compute is
free for Apple **Small Business Program** apps (<2M downloads), but that
program is an App Store commission program requiring paid enrollment *and*
App Store distribution. An operator-personal app installed on our own devices
is almost certainly **not** eligible. Treat PCC as unavailable-by-default; if
we ever ship to the App Store, revisit. This does not affect D1 (on-device is
unconditionally free) or D2 (our own mesh is the backend).

## Slice E — Free Wins

Standard SwiftUI text views in the chat composer get **Writing Tools** for
free. **Genmoji** and **Image Playground** are near-zero-effort adds. Take them;
don't design around them.

## Slice F — Verification

**Seam: `apple-intents-testing`.** `AppIntentsTesting` (WWDC26) validates Siri /
Shortcuts / Spotlight integration **through real system pathways** rather than UI
automation. This is exactly this repo's "watched-live, not merely smoke-green"
standard, expressed as a test framework — so intent adoption gets the same
evidentiary bar as the rest of the stack. Adopt it alongside slice A, not after.

## Platform Shells (iPadOS is currently missing)

`apps/philotic-apple` ships iOS + macOS targets. iPadOS is **not** a separate
target — but "runs on iPad" is not the same as "designed for iPad."

Recommendation: keep one SwiftUI codebase; make the shell adaptive rather than
adding a third app. iPad is the natural **LifeGraph canvas and Steward review**
surface — the node-link canvas from Plane 2 and the review inbox from Plane 3
are cramped on iPhone and idle on Mac. Concretely: size-class-driven
`NavigationSplitView`, pointer/keyboard support, Stage Manager multi-window
(canvas and chat side by side), and Apple Pencil for canvas annotation later.

Per-platform intent emphasis:

- **iOS** — voice capture, Action Button → capture-observation intent, Live
  Activities for long-running philote turns, Controls in Control Center.
- **iPadOS** — canvas + review inbox, multi-window, keyboard shortcuts.
- **macOS** — menu-bar quick capture, push-to-talk, Spotlight-first entity
  index (the Mac is where the semantic index earns the most).

## Build Order

1. **A** — `IndexedEntity` on LifeGraph/Muninn entities → Spotlight. Highest
   value, rides an existing seam.
2. **F** — `AppIntentsTesting` harness, alongside A.
3. **B** — `system.Open`, then `reminders`/`calendar` on the EventKit work.
4. **D1** — on-device `SystemLanguageModel` triage for Today/capture.
5. **C** — custom intents + App Shortcuts phrases.
6. **E** — Writing Tools / Genmoji / Image Playground.
7. **D2** — `PhiloticFM` provider package. Last: highest effort, and A–D1
   deliver value without it.

## Hardware Constraint on Verification

Apple Intelligence requires **A17 Pro minimum (8GB)**; the top-tier on-device
model introduced in iOS 27 requires **12GB unified memory** (iPhone Air /
17 Pro / 17 Pro Max, M4+ iPad, M3+ Mac with 12GB, M5 Vision Pro).

An iPhone 13 Pro (A15) **cannot run Apple Intelligence at all** — no on-device
iPhone verification of any slice here is possible on it. Development and
Simulator work on an Apple silicon Mac is unaffected; only device verification
is gated. See session notes for the September 2026 hardware decision.

**Signing constraint on the dev loop**: a free Apple Account (Xcode "Personal
Team") issues provisioning profiles that **expire 7 days** after creation — the
installed app simply stops launching and must be rebuilt from Xcode. Paid
Apple Developer Program profiles last a year. Any slice whose verification
target is *watched-live on a real device over days* (which is most of Plane 5,
since Spotlight indexing and Siri reasoning need lived-in data) effectively
requires the $99 membership to avoid weekly re-signing.

## Explicitly Out of Scope

- Replacing Siri (`assistant` domain is Japan-only, single `activate` intent).
- Routing system Siri through the philotic mesh (custom providers are app-local).
- Any raw graph access from device (Plane 2's governed-projection rule holds).
- Indexing unzoned or unvalidated LifeGraph content into Spotlight.
