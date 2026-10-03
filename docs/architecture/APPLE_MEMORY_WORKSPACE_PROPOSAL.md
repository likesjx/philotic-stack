---
title: Apple Memory Workspace and Open-loop Actions
doc_type: proposal
domain: memory-context
status: accepted-current-slice
last_updated: 2026-10-03
tags: [apple, graphical-ux, lifegraph, cortex, reminders]
related_docs:
- ARCHITECTURE_STATUS.md
- NATIVE_APPLE_APP_PROPOSAL.md
- CORTEX_VIEWER_PROPOSAL.md
- NATIVE_OPERATOR_SIGNIN_PROPOSAL.md
task_refs:
- docs/task.md#apple-memory-workspace
active_seams:
- lifegraph-lens-ui
- muninn-admin-observability-plane
- device-tool-plane
---

# Apple Memory Workspace

## Goal

Explore LifeGraph relationships and Cortex memory visually on iPhone and Mac,
and act on open loops without treating a list of database rows as the product.

## Core Recommendation

- LifeGraph: lens-based entry cards, focused stored one-hop graph, fit/zoom,
  accessible relationship list, and existing audited text editing in details.
- Cortex: vault tiles, explicit-tag topic filters and readable memory cards.
  Preserve vault-qualified identity, loaded-page coverage, ephemeral native
  sign-in and background clearing. Tags are not learned association edges.
- Open loops: explicit title-only export into an operator-chosen writable
  Reminders list. Search linked items across lists, including completed items,
  before creating one. No background export, notes upload or automatic closure.

## Disposition

Accepted for current slice. Initial graphical navigation and linked export are
implemented on `codex/apple-memory-workspace`, followed by audited lifecycle
actions. The graphical build was installed and launched on the new iPhone on
October 2; physical UI acceptance, real EventKit writes and the new lifecycle
actions' deployment remain unverified. The complete requested workflow is
**not** implemented by these slices.

## Current Slice

1. Promote the existing LifeGraph graph from a buried detail section into the
   default Explore screen; retain Records and navigable node details.
2. Replace Cortex row browsing with vault/topic/memory tiles using existing
   authorized inventory/page/detail endpoints. Filtering stays loaded-only.
3. Add a reviewed Reminders export from an OpenLoop detail. The linking marker
   hashes the selected hotel endpoint and canonical node ID; it contains no
   credentials or content. A process-wide exporter serializes local writes.
   Export copies only the title and marker; no dates, status or provenance.
4. Update permission language to describe explicit creation accurately.
5. Add explicit confirm/close/reopen sheets, original-snapshot conflict guards,
   retry-safe audit receipts, device-only gateway and atomic runner writes.

### Audited lifecycle contract

`POST /api/edge/lifegraph/node/:node_id/action` accepts `request_id`, typed
`action`, `before` and optional-note text (required key, empty allowed). The
gateway derives the actor from device enrollment; shared bearers and
client-supplied actor/id/arbitrary changes are rejected. The internal
`life.loop.action` command is not in the model-visible tool catalog.

- Confirm updates validation and confirmation time, not resolution.
- Close writes canonical `status=resolved`, server time and resolution note,
  without changing validation, confidence or source provenance.
- Reopen writes `status=open`, removes legacy `loop_status` and resolution
  fields, and preserves validation and source provenance. Retired nodes and
  unknown validation states are not revived through this surface.

The runner requires exactly one matching OpenLoop and compares all ten
displayed text/lifecycle properties, including explicit null originals and
`loop_action_revision`. The same Cypher statement changes the node and creates
`LifeLoopAction` with before/after JSON, actor, note and server time. Revision
increments prevent close/reopen ABA overwrites through this API. Existing
other lifecycle writers do not increment this revision; their current values
are guarded, but an exact legacy-writer ABA is not detected yet.

Retry IDs are scoped to node and enrolled actor. Repeating the immutable
request returns its original audit; reusing its ID for changed input conflicts.
The sheet freezes its note after an attempt and validates node/action/request
identity in the receipt. A transport/database write conflict can require an
explicit retry. No optimistic success or Reminders completion is performed.

Endpoint-scoped markers are transitional: switching a hotel alias changes the
scope, and deleting/editing the marker can defeat deduplication. This is not a
durable cross-device sync ledger, and concurrent exports on separate devices
are not guaranteed duplicate-free. EventKit/iCloud synchronization remains
Apple-owned; an export receipt is not proof that another device received it.

## Next Implementation Work

- Merge/deploy both action boundaries, install the updated app, and verify
  live enrolled-device save/read-back plus persisted audit on a disposable
  loop. Verify native UI confirm/close/reopen and stale-screen rejection.
- Extend lifecycle revision discipline to other writers before claiming a
  graph-wide revision guarantee; keep generic text editing narrow.
- Add reviewable reconciliation: reminder completed / loop still open,
  loop closed / reminder incomplete, missing item, edited title and conflict.
  Applying either direction is an explicit user decision, never silent.
- Add a hotel-owned sync ledger keyed by stable hotel identity and node ID
  before claiming robust multi-device two-way sync.
- Extend the scoped Cortex gateway to expose authorized stored associations
  and global search. Preserve edge origin/weight and explicit vs learned
  distinction; never infer edges from proximity or silently merge LifeGraph
  truth with Muninn association evidence.

## Verification

Shared-code tests cover topic membership, cross-vault identity, stable ordering,
hotel-scoped reminder markers and credential rejection. App compilation checks
the native views and EventKit adapter. No test touches the operator's Reminders.
October 2: 119 PhiloticKit tests (one skipped, zero failures), 30 shared app
tests (zero failures), Mac build and generic iOS Simulator build pass.
October 3 lifecycle slice: 228 library and 57 runner tests pass; three gateway
authority tests pass; 123 PhiloticKit tests (one skipped, zero failures) and
30 app tests pass; Mac and generic iOS Simulator builds pass. An opt-in test
against disposable Memgraph 3.5.0 proves actual audit persistence, protected
property preservation, stale/ABA rejection, exact retry replay, concurrent
retry deduplication, legacy alias clearing, wrong-label and duplicate-ID
rejection. This is integration/test-green, not deployed or phone-action proof.
Physical graph interaction, VoiceOver, permission revocation, real export,
repeat export and completed-reminder reconciliation still require UI acceptance.

Current truth: [architecture status](ARCHITECTURE_STATUS.md).
Execution: [task board](../task.md#apple-memory-workspace).
