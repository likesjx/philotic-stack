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
