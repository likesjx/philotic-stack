# Philotic Apple Companion

## Product direction

One companion, two jobs: **Ask** is the quick agent command center; **Today**
is the personal dashboard. The Mac keeps a quiet, top-centered pill below the
camera/menu exclusion area, expanding when the pointer hovers over or near the
notch (click and keyboard opening also remain available). The iPhone
uses Today, Agents, and Life tabs with the same integration boundaries.

This is an original, notch-inspired SwiftUI surface in the existing app, not
a NotchNook fork or a second backend. The first implementation is transitional:
it does not complete the native-app program or create a remote device tool host.

## First version

| Surface | Implemented behavior | Boundary |
|---|---|---|
| Mac companion | Collapsed pill; expandable Ask/Today; hide and full-app handoff; menu-bar controls; Option-Command-N while app is active | One owned NSPanel, one existing chat session; no global keyboard monitoring |
| Today | Connection status, current LifeGraph lens with explicit load/retry/empty states, Apple connection cards | Reads the existing LifeGraph store, not a parallel personal database |
| Agents | Existing typed/voice chat in full app and Mac companion | Existing Philotic Web edge transport and credentials; selection revisions reject stale history handoffs |
| Health | Existing selected-metric, read-only preview and separately confirmed sharing | No new background access or Health writes |
| Places | Existing manual sharing plus an opt-in MapKit view of the last acknowledged snapshot | Uses already-shared precision, not live tracking; map content comes from Apple |
| Reminders | Explicit local preview, at most 50 incomplete items due in the next seven days including overdue items | Apple requires full access, but adapter exposes reads only; no sync, writes, or agent upload |
| App Intents | Open Today, Agents, or LifeGraph in the foreground | Navigation only; no sends, capture, indexing, donations, or remote commands |
| Apple Intelligence | Summarize a user-typed note locally with Foundation Models on supported devices | Maximum 4,000 input characters; availability/error states; no cloud fallback, tools, or automatic sharing |

Reminders previews are discarded on dismissal/backgrounding; stale completions
cannot repopulate them. Native fetches have a 20-second timeout. Health retains
its existing exact-preview consent and invalidation rules. Local AI text and
results are discarded when its sheet closes. The collapsed Mac pill contains
no health, location, reminder, or conversation preview.

## Mac notch hover — 2026-09-15

The hover target includes the physical camera gap, an 18-point horizontal
margin, and the collapsed pill. Non-notched displays use a bounded top-center
target, not the whole menu bar. The panel prefers a notched display when present.
A 150 ms dwell opens it; a continuous region from notch to expanded panel plus
16 points of padding and a 450 ms exit grace prevent flicker during pointer
transfer. Manual collapse suppresses reopening until the pointer leaves and
returns. Keyboard opening remains available until the pointer visits the panel.

Hover never calls `makeKey` or activates the application. Editing, menu tracking,
mouse-button drags and active voice capture keep an already-open panel from
auto-closing. The content remains mounted while collapsed to preserve drafts.
Frame transitions respect Reduce Motion. A controller-owned 20 Hz timer samples
only the current `NSEvent.mouseLocation`; it stores no pointer history and
requests no Accessibility/Input Monitoring grant. Sampling stops when hidden,
the display sleeps, or the user session resigns active; observers/timer are
released with the controller. Screen sleep and session activity are separate
gates so one resume event cannot override the other.

All 38 Mac tests pass. Timing, suppression, interaction guards, notch-to-panel geometry, offset screen
coordinates and non-notched fallback have deterministic tests. Real pointer
hover, focus retention in another app, draft retention, sleep/lock and external
display transitions still require operator validation; policy tests are not
physical pointer-event proof. No iPhone behavior changes in this increment.

## Authority and Philotic Web

`ChatSessionManager` remains the application session owner. The notch and full
window project that same session; `CompanionRouter` carries only foreground
navigation. LifeGraph remains the canonical destination for deliberately
shared observations through the existing `/api/edge/lifegraph/observe` route.
Local chat history remains the existing transitional store, not newly claimed
server-canonical history. Reminders and local AI are deliberately not connected
to remote agent execution yet. Apple permission is not consent to upload.

The existing exact-host development ATS exception and Keychain-backed edge
credentials are preserved. No general cleartext exception is added. Production
TLS, health-specific access/retention rules, and App Store readiness remain
separate requirements.

## Verification and reality gaps — 2026-09-14

- macOS (30 passing) and iOS Simulator (29 passing) hosted tests cover routing, safe foreground intents,
  reminder state/invalidation, packaged permission text, and Mac panel geometry.
  Shared PhiloticKit tests also pass (95 passed, one skipped).
- The running Mac app's Ask/Today panel, Today dashboard, Reminders disclosure,
  and Apple Intelligence screens were inspected. The iPhone simulator Today
  layout was inspected and its connection strip moved above the content to
  avoid covering the system tab bar. A synthetic note was successfully summarized by the
  on-device model. No personal Reminders permission was granted or data read.
- Physical build 4 was installed/started on September 12; the operator reported
  the LifeGraph fix working. That does not prove this companion version is on
  the phone. The old development profile expired September 12; the September 14
  signing attempt found no Xcode account/profile for renewal. Phone unavailable.
- Actual Siri/Shortcuts launch, real Reminders permission/revocation, MapKit
  rendering after a newly approved share, phone companion UI, lock-screen and
  multiple-display behavior, and newest server observation/agent recall are
  still unverified. Geometry unit tests are not display-switch proof.
- The Mac test session is not enrolled; screenshots of disconnected states are
  not evidence of a live agent conversation or authenticated LifeGraph read.
- Closeout: graph registration began during verification rather than initial
  implementation; no harness trial telemetry was collected. No memory was
  written. Rust-wide checks were omitted because this change is Swift/UI-only.
  Graph scan is deferred to integration to avoid rewriting another checkout's
  existing generated-document changes.

## Next increments

1. Renew signing and validate this build on the phone; test permissions with
   the operator, then independently check the latest approved observation.
2. Add a real dashboard summary backed by bounded LifeGraph lens data and a
   steward review inbox, with freshness and provenance rather than sample data.
3. Design narrowly scoped EventKit actions with approval, idempotency, and audit
   before enabling agent-created reminders or Calendar/LifeGraph synchronization.
4. Add opt-in Spotlight entities, richer App Intents, widgets/Live Activities,
   and device handoff only after privacy/redaction/retirement rules are defined.
5. Explore local Foundation Models triage as an optional provider, keeping
   unsupported-device behavior explicit and cloud processing separately approved.

## Build and references

Use `script/build_and_run.sh` for the Mac companion; `--verify` checks that the
new process launches. The Codex Run action uses this script. XcodeGen's
`PhiloticApp/project.yml` is the project source. Signed device builds require a
current development profile and trusted, reachable phone.

- [Native Apple program](../../docs/architecture/NATIVE_APPLE_APP_PROPOSAL.md)
- [Active work](../../docs/task.md#native-apple-companion)
- [Apple EventKit access](https://developer.apple.com/documentation/eventkit/accessing-the-event-store)
- [Apple Foundation Models availability](https://developer.apple.com/documentation/foundationmodels/systemlanguagemodel)
- [Apple App Intents](https://developer.apple.com/documentation/appintents/creating-your-first-app-intent)
- [Apple MapKit](https://developer.apple.com/documentation/mapkit/map)
- [Apple camera exclusion geometry](https://developer.apple.com/documentation/AppKit/NSScreen/auxiliaryTopLeftArea-uglc)
