---
title: Apple Memory Workspace and Open-loop Actions
doc_type: proposal
domain: memory-context
status: accepted-current-slice
last_updated: 2026-10-02
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
implemented on `codex/apple-memory-workspace`; physical-device/UI acceptance
and real EventKit writes are not yet verified. The complete requested workflow
is **not** implemented by this first slice.

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

Endpoint-scoped markers are transitional: switching a hotel alias changes the
scope, and deleting/editing the marker can defeat deduplication. This is not a
durable cross-device sync ledger, and concurrent exports on separate devices
are not guaranteed duplicate-free. EventKit/iCloud synchronization remains
Apple-owned; an export receipt is not proof that another device received it.

## Next Implementation Work

- Add a device-authorized, audited open-loop action endpoint with typed
  `confirm`, `close`, `reopen` commands, original state/version conflict guards,
  atomic before/after receipts and idempotent retry. Do not extend generic
  text editing to write protected lifecycle fields. `life.resolve` currently
  handles conflict resolution; do not mislabel it as a generic close action.
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
Physical graph interaction, VoiceOver, permission revocation, real export,
repeat export and completed-reminder reconciliation still require UI acceptance.

Current truth: [architecture status](ARCHITECTURE_STATUS.md).
Execution: [task board](../task.md#apple-memory-workspace).
