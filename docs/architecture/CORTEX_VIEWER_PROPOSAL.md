---
title: Cortex Viewer for the Apple Apps
doc_type: proposal
domain: memory-context
status: accepted-current-slice
last_updated: 2026-09-21
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

Accepted for the native read-only viewer. The adapter is deployed and live
reads are verified. The Mac connection screen is running; authenticated native
UI and physical iPhone viewer validation remain pending.

## Current Slice

`GET /api/cortex` requires a fresh hotel-issued administrator session on every
request and rechecks it after the read. Device enrollment is insufficient.
The exact local management adapter identity invokes `ReadCortex`; the hotel
uses its existing server-held Muninn administrator credential. Upstream URLs,
methods and paths are not caller-controlled. Responses are bounded to 4 MiB,
pages to 50 entries and inventory to 512 vaults; the hotel read has a 35-second
deadline. Redirects are disabled. Access logging records session ID and status,
not memory content or tokens; a dedicated durable access-audit ledger is deferred.

Rollout must inspect the canonical server and set `cortex_viewer_endpoint` to
the JSON string matching `muninn_endpoint`. Missing/mismatched attestation or
a remote write route fails closed. This attestation is transitional, not a
live cluster leadership proof; migration must revoke/update it.

The shared SwiftUI Cortex tab lists vaults, loads pages and displays full memory
details. Filtering covers loaded rows only, not a global or semantic search.
Offset pagination is not a stable snapshot under concurrent writes. The client
pins the operator-approved `http://100.64.212.8:7700` Tailscale origin, rejects
redirects and retains tokens/content only in memory. Backgrounding or logout
clears them. An existing operator session must be entered manually; integrated
native operator sign-in remains deferred. Public desktop routing is unchanged.

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

PR #578 merged as `9ef26207`; Linux build `35539749776` at `6ba0b372` was
installed on vps-jane. Both running executable hashes match the CI artifacts.
Nine Swift tests, two hotel adapter tests and all 205 web tests pass. Mac and
signed iPhone builds pass; the installed Mac Cortex connection screen was
observed with the existing Desktop tab preserved.

On September 21, following explicit operator approval, the live probe verified
the upstream primary/leader and attested its existing endpoint through hotel
IPC. The deployed endpoint returned **13 vaults, zero unavailable, 1,464
memories**. Inventory, first page, full detail and a second page succeeded;
the two pages had no duplicate identities. Anonymous access returned 401.
The temporary bootstrap-issued admin session was logged out through the normal
route; replaying its token returned 401. No credential was printed or saved
locally. This is watched-live evidence for the backend, not exhaustive inventory
or replica-completeness proof.

Authenticated native UI, partial-outage behavior, full pagination coverage and
the new viewer's physical iPhone deployment remain unproven. Installation hit a
connection reset; the phone subsequently reported unavailable. Native sign-in
still requires an existing operator session entered manually.
See [current status](ARCHITECTURE_STATUS.md) and [execution work](../task.md).
