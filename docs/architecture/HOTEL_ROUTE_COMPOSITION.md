---
title: Hotel Route Composition
doc_type: reference
domain: runtime-sessions
status: transitional
last_updated: 2026-10-09
tags: [routing, fallback, migration]
related_docs: []
---

# Hotel route composition

The pure `ansible_mesh_core::route_composition` API computes an ordered effective
route from hotel-resolved candidates. It performs no dispatch, persistence,
credential acquisition, or access grant. Existing runtime routing remains in
place until the hotel integration is explicitly implemented and adopted.

`RoutePolicy` is the reviewable policy input. A missing version means
`legacy_exclusive_v1`: a nonempty agent list excludes the hotel waterfall.
Explicit `preferences_then_hotel_v2` appends the hotel waterfall after agent
preferences. An empty agent list uses the hotel waterfall in either version.
Unknown versions and unknown top-level fields fail deserialization.

A direct override precedes preferences. `prefer_with_fallback` retains the
remaining plan; `strict_pin` permits only the chosen candidate. A denied or
unknown strict pin yields an empty plan, never an implicit fallback. Existing
`/model` strict-pin behavior has not been reinterpreted.

Each candidate requires request-scoped hotel admission. Privacy denial, access
denial, and unknown admission all fail closed, including overrides. Shared oracle
hard gates enforce capability, context capacity, trust ceiling, tools, structured
responses, availability, and health cooldown without reordering operator intent.
The API returns the specific first restriction. Diagnostics name requested aliases
and their source, inclusion, rejection, or first deduplicated candidate.

Deduplication follows alias resolution and compares provider, model, endpoint,
credential scope, hotel, incarnation, and policy scope exactly. Endpoint and
credential fields must be opaque handles; no credential values belong in plans
or diagnostics. Identity/provider-profile mismatches fail closed. Registry and
admission snapshots must be refreshed and admission rechecked at dispatch.

Source default tiers are OpenRouter then Ollama. Gemini remains selectable in
explicit ladders, overrides, and pins. Existing persisted nonempty ladders and
hotel default overrides are retained. Migration must not automatically relabel
historically exclusive role configurations as preferences.

## Remaining integration

The hotel must resolve aliases to exact dispatch identities, obtain admission
from the canonical privacy/access owner, persist versioned policy, and expose
inspect/edit controls through existing authorized routing surfaces. Philote must
consume one hotel-produced plan for initial dispatch and escalation, with request
snapshot consistency and dispatch-time admission revalidation. Until those owners
are wired, this API is a tested composition contract, not runtime authority.

The operator control write is a separate phase. `role.configure` currently
requires a registered agent caller, while web role PATCH omits routing ladders
and model bindings. Broad `aiua load` is not a narrow setter because it also
touches secrets, guests, catalog, and identity. Add a narrow authenticated
operator route-policy API/CLI using the existing operator authorization checks,
with read-only effective-route diagnostics and a version-checked policy update.
It must not impersonate an agent, bypass identity checks, or persist new grants.
The temporary `/model model.openrouter` session pin remains strict and disables
automatic fallback. No live configuration is changed by this document or API.

## Fallback assessment

The live loop already owns `NoResponseClass`, capped oracle expansion, sticky
fallback overrides, and origin-tier recovery probes. Its working tool history is
the source of committed results. Those mechanisms were not replaced here.

Robust fallback needs a coordinated next change: structured provider failures
including retry-after; cooldown keys scoped to model/endpoint/credential/placement
instead of broad provider exclusion; bounded jitter; one attempt, deadline, and
token ledger covering ladder and oracle dispatches; cancellation carried across
IPC; and recovery that retains committed tool receipts without replaying tools.
Provider error transport and request lifecycle ownership must support these
before the loop can enforce them. No unused resilience facade was added.

No deployment or real-provider validation is authorized for this source-only
work. Unit evidence does not establish installed or live routing behavior.

## Protected runtime explain consumer

`LocalAuthorityRpc::with_route_catalog` explicitly installs a hotel-owned catalog.
The append-only `explain_model_route` protected request uses the existing Unix
peer-authenticated RPC path. It accepts only an immutable authority envelope and
an expected catalog revision. Actor preferences, aliases, override mode, hotel
waterfall, endpoint classifications and request projection stay server-owned.
Every otherwise eligible candidate is resolved through canonical task authority
against its actual endpoint; catalog access denials remain denied. Missing
installation, stale catalog, unsupported projection, missing actor policy and
absence of any admitted endpoint fail closed. Successful diagnostics contain no
source closure, provider credentials or resource policies. Policy revision and
payload digest bind the explain result; it is not a dispatch grant. Dispatch
must re-resolve the chosen endpoint immediately before each provider call.

This is an opt-in runtime RPC consumer, not production installation. The context
owner must supply the complete bound-payload projection and canonical catalog
installation. A concrete dispatch adapter and fallback attempt/time/token budget
remain dependencies. Legacy dispatch is unchanged. No production authority,
provider calls or configuration changes are claimed.

Validation for this integration: rustfmt and git diff --check only. New wire
correlation/injection and diagnostic round-trip tests are authored but not run;
local builds were explicitly prohibited. The earlier 3251 passing tests cover
phase-one code on develop 86e154c8, not this privacy-base integration.

## Diagnostic SDK consumer

`PhiloticClient::explain_local_model_route` uses the existing protected RPC and
server-owned catalog. The launch owner supplies the trusted hotel kernel peer;
the caller supplies expected catalog revision and candidate identities solely
for reply comparison. These expectations cannot grant access or change policy.
Replies must match the request ID, task, immutable payload digest, catalog
revision and full candidate identities. Duplicate identities and expanded strict
pins are rejected. Binding/type failures disconnect; an authenticated `Denied`
returns an error without a legacy fallback. Whole-operation timeouts include
backpressured writes and force reconnection before reuse.

The returned `BoundRouteExplanation` contains diagnostics and revision metadata,
not an authenticated processing context or dispatch receipt. No controller or
provider invocation consumes it automatically. Catalog installation, catalog
comparison metadata delivery, complete outgoing-payload authorization and guarded
retry/fallback installation remain separate runtime dependencies. The merged
context bridge is preserved without duplication. No management writes, grants or
live activation are introduced.

Synthetic SDK tests cover valid/empty-strict explanations, wrong request/task/
digest/catalog revision, all endpoint identity dimensions, duplicates, reply-type
confusion, replay and stale unsolicited replies, denied admission, wrong hotel
peer, truncated/closed transport, read timeout and backpressured-write deadline.
Local builds are prohibited; hosted exact-head CI supplies execution evidence.
