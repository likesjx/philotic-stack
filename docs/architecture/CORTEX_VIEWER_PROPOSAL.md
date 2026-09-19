---
title: Cortex Viewer for the Apple Apps
doc_type: proposal
domain: memory-context
status: accepted-current-slice
last_updated: 2026-09-19
tags: [cortex, muninn, apple, observability, read-only]
related_docs:
  - MUNINN_MEMORY_CORE_PROPOSAL.md
  - NATIVE_APPLE_APP_PROPOSAL.md
  - HOTEL_USER_IDENTITY_AND_OPERATOR_AUTH_PROPOSAL.md
  - ARCHITECTURE_STATUS.md
task_refs: [docs/task.md]
proposal_id: cortex-viewer
active_seams: [muninn-admin-observability-plane]
source_of_truth_targets: [ARCHITECTURE_STATUS.md]
---

# Cortex Viewer

## Goal

Give the operator one Cortex view of Philotic's durable memory on Mac and
iPhone. Machine selection is not the organizing model. Vault, agent, tags,
memory state, and time are filters inside the unified collection.

## Core Recommendation

Use a native read-only browser backed by an operator-authorized, hotel-owned
Cortex adapter. Keep Muninn credentials on the server. Do not grant all-vault
access merely because a device enrolled, and do not expose Muninn's raw admin
API or inject device tokens into WebKit.

The first screen should show Cortex identity, inventory freshness and coverage,
then recent memories and search. Details show full content, vault, source,
timestamps and tags. Related-memory navigation follows after stable identity
and pagination are proven. Editing, forgetting, consolidation and replica
repair are out of scope for the first viewer.

## Disposition

Accepted for the data-contract foundation. No viewer, HTTP endpoint or live
Cortex inventory is implemented by this first change.

## Current Slice

`PhiloticKit/CortexSnapshot.swift` defines a proposed read response contract,
with tests for incomplete inventories, unavailable counts, explicit exclusions,
unknown states and vault-qualified memory identities. It has no transport and
cannot widen access. The backend adapter must adopt or revise this contract
before the app connects to it.

### Coverage is evidence, not a label

- `cortex_id`, `observed_at`, `catalog_complete`, per-vault availability and
  counts are explicit. Missing or denied data must not turn into zero.
- A complete inventory refers only to the authorized Cortex collection, not
  every state store in Philotic. Search relevance is not an inventory count;
  a page ending does not mean every vault was queried.
- Current `is_cortex_routable_vault` includes shared/user/self vaults but excludes
  `session_*` scratch. Historical synchronization is not proof of current
  completeness. Show exclusions and unsynchronized/unknown coverage honestly.
- LifeGraph remains the authority for its nodes and edges; hotel session ledgers
  and intel graphs are not silently reclassified as Muninn memory. Link to these
  surfaces rather than copy them into a competing store.

### Implementation sequence

1. Add the operator-authorized read adapter at the existing hotel management
   boundary, using the configured Cortex route and server-held vault credentials.
   Authorize every inventory, page and detail request; reject client-supplied
   upstream URLs and arbitrary vault access. Do not treat ordinary device bearer
   authentication as administrator authorization.
2. Inventory authorized vaults directly at Cortex; implement bounded pagination
   and detail reads. Retain unavailable/denied states and bounded content-free
   access audit. No implicit local-observer fallback claiming to be Cortex.
3. Connect a shared SwiftUI Cortex screen on Mac/iPhone. Clear content on logout
   or authority change; no persistent full-memory cache in the first release.
4. Prove anonymous/non-admin/revoked access fails; compare paginated UI inventory
   with Cortex; test partial outage and empty-vault cases; install and exercise
   the resulting app on physical devices.

## Verification

Contract tests are only foundation evidence. Installed UI, authorization,
pagination completeness, live inventory and deployment remain unproven.
See [current status](ARCHITECTURE_STATUS.md) and [execution work](../task.md).
