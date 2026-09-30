---
title: Perimeter Egress Control Proposal
doc_type: proposal
domain: operator-control-plane
status: accepted-current-slice
disposition: accepted-current-slice
last_updated: 2026-07-28
tags:
- egress
- perimeter
- security
- control-plane
- active-seam
related_docs:
- ARCHITECTURE_STATUS.md
- HOTEL_PERIMETER_TRUST_PROPOSAL.md
- MEMBRANE_COMPONENT_PROPOSAL.md
- MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md
- CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
- OUTBOUND_INTEGRATION_FABRIC_PROPOSAL.md
- OUTBOUND_EGRESS_INVENTORY.md
task_refs:
- docs/task.md
proposal_id: perimeter-egress-control
implements: []
implemented_by:
- crates/ansible-mesh-core/src/integration.rs
- crates/egress-http-runner/src/lib.rs
- crates/aiua/src/service/governed_http.rs
- crates/aiua/src/service/model_catalog_sync.rs
- crates/aiua/src/service/ipc.rs
- crates/philotic-web/src/serve.rs
- docs/architecture/outbound-egress-inventory.json
- scripts/check-outbound-egress-inventory.py
active_seams:
- outbound-fleet-enforcement
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Perimeter Egress Control Proposal

## Goal

Define a deterministic outbound egress boundary for Philotic so the system can answer:

- what external HTTP/network destinations a component may reach
- which outbound requests must cross a perimeter-controlled boundary
- which egress classes are explicitly exempted
- how egress policy, audit, and security review stay machine-checkable instead of becoming ambient lore

This proposal exists because "inside the perimeter" is only half the story. If we do not define how traffic leaves the system, security posture becomes a collection of vibes plus whichever crate imported `reqwest` first.

## Disposition

`accepted-current-slice`. The canonical policy, bounded HTTP executor,
MCP-over-HTTP delegation, first hotel-owned general-API migration, and
credential-safe operator OIDC migration are implemented. Fleet enforcement
remains active because named model-provider, communication, local-resource,
mesh, and artifact exceptions have not all moved behind executable host-level
rules.

## Core Recommendation

Introduce a perimeter-controlled egress plane for outbound HTTP and adjacent external calls.

Recommended default:

- outbound HTTP should cross a perimeter-controlled egress boundary
- egress policy should be deterministic and inspectable
- exceptions must be explicit, narrow, and auditable

For the current architecture direction:

- communication egress should be perimeter-controlled
- general tool/API egress should be perimeter-controlled
- model-provider egress may remain an explicit exception for now

This means Philotic should not silently allow every guest to make arbitrary outbound HTTP just because it technically can. The normal rule should be "egress goes through the perimeter plane," not "egress is wherever a dependency graph happened to grow a socket."

## Disposition

Accepted for the current slice. The bounded general-API execution boundary,
content-free audit, direct-client inventory, and first governed migration are
implemented. Specialized exceptions and remaining migration work keep the
broader perimeter enforcement program open.

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Why This Needs Its Own Proposal

This is related to membranes and perimeter trust, but it is not identical to either.

- [MEMBRANE_COMPONENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_COMPONENT_PROPOSAL.md) defines the outside-world communication boundary
- [HOTEL_PERIMETER_TRUST_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HOTEL_PERIMETER_TRUST_PROPOSAL.md) defines hotel identity, membership, and trust
- this proposal defines outbound egress control

If we blur these together too early, we risk either:

- turning `membrane` into a universal "network stuff" bin
- or leaving outbound traffic policy implicit because every relevant rule lives in a different doc

## Current Reality

Today the repo has a hotel-owned policy and HTTP execution boundary plus an
explicit inventory of direct exceptions.

Current proven shape:

- `perimeter-core` defines `EgressPolicy`, destination allow/deny evaluation,
  credential bindings, traffic classes, and exit-placement policy
- `aiua` owns `HotelEgressGateway` and the `CheckEgress` IPC path
- `hotel.egress.check` is authorization-only; it does not return resolved
  credential material
- `egress-http-runner` executes bounded HTTP requests and emits durable
  content-free audit records
- MCP-over-HTTP delegates its wire exchange to that runner while the MCP
  manager retains protocol authority
- the OpenRouter model-catalog poll is the first hotel-owned general-API caller
  migrated to a system binding, with installed watched-live proof from
  `mbp-jane` through the selected `vps-jane-aiua-01` executor
- Philote consumes only the hotel-owned compact catalog and no longer owns a
  direct OpenRouter fallback client
- 32 remaining production direct-client files have machine-checked
  dispositions, while two migrated callers are regression-guarded in
  `outbound-egress-inventory.json`
- model providers, communications, local resources, mesh, and artifacts remain
  named specialized exceptions; operator auth is a temporary exception

## Recommended Egress Taxonomy

Philotic should distinguish at least three outbound classes:

### 1. Communication Egress

Examples:

- Telegram `sendMessage`
- WhatsApp replies
- webhook callbacks
- operator notifications

Recommendation:

- route through the perimeter egress boundary by default

This includes outbound protocol-native delivery from membrane implementations such as Telegram today and potential `A2A` / `Nostr` membranes later. A transport-specific membrane may shape the request, but it should not silently self-authorize the network exit.

### 2. General HTTP / Tool Egress

Examples:

- API calls made by tools
- external documentation fetches
- MCP-over-HTTP or service-backed tool runners
- non-model outbound service integrations

Recommendation:

- route through the perimeter egress boundary by default

### 3. Model / Provider Egress

Examples:

- LLM API calls
- TTS/STT provider requests
- embedding provider calls

Recommendation:

- treat as an explicit exception class for now
- do not assume the exception is permanent
- keep the exception visible in docs/policy, not hidden in implementation accidents

This lets Philotic start controlling the majority of outbound HTTP without forcing the entire model stack through a perimeter refactor in one slice.

## Deterministic Policy Model

The egress plane should be policy-driven and machine-checkable.

Minimum policy dimensions:

- caller component type / guest role
- agent or persona scope when relevant
- destination class
- destination allowlist or named trust class
- method / protocol class
- credential handling requirements
- audit requirement
- enforcement mode
  - allow
  - deny
  - allow+audit
  - require approval

The important point is not that every outbound request needs human review. The important point is that the system can explain why the request was allowed, denied, or exempted.

## Suggested Runtime Shape

Do not redefine the current Telegram-oriented `membrane` binary into a universal egress god-object.

Instead, define a perimeter egress boundary that may later be implemented by:

- a dedicated egress-control component
- a membrane-hosted egress service
- or another bounded perimeter runtime

The architecture should preserve these boundaries:

- communication membranes own transport semantics
- the egress plane owns outbound policy and audit
- model-router owns model/provider invocation semantics

Those can cooperate closely without becoming the same thing.

## Deterministic Findings And Cognitive Review

Perimeter egress control should produce structured findings that can later feed a cognitive review loop.

Deterministic findings:

- unauthorized destination attempt
- policy mismatch
- missing exemption for direct provider call
- unexpected guest-originated HTTP
- stale or overbroad allowlist
- unusual egress volume or destination spread

Then a later cognitive/security cycle may:

- summarize findings
- correlate patterns
- rank operator attention
- propose remediation

But the cognitive layer should interpret deterministic facts, not replace them as the source of truth.

## Current Slice

The coherent implementation slices now are:

1. **Implemented, test-green:** define traffic classes and exit-placement
   decisions; make checks authorization-only and keep credentials out of model
   tool results.
2. **Implemented, smoke-green:** inventory current direct outbound HTTP call
   sites by component class.
3. **Implemented:** classify current egress paths as:
   - perimeter-controlled already
   - temporary direct exceptions
   - violations of the intended future model
4. **Implemented and watched-live-green for selected `vps-jane` placement:** add
   the bounded hotel-owned HTTP executor defined by
   [OUTBOUND_INTEGRATION_FABRIC_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OUTBOUND_INTEGRATION_FABRIC_PROPOSAL.md).
5. **Implemented, smoke-green:** route the hotel-owned OpenRouter catalog sync
   through that executor.
6. **Implemented, smoke-green:** route operator OIDC token and userinfo
   back-channel exchange through a typed local-only binding; keep client
   secrets and access/refresh tokens inside the execution hotel, return only
   allowlisted identity claims, and audit both legs separately.
7. Keep model/provider egress as an explicit documented exception until a
   later decision.

## Open Questions

- The first implementation is a dedicated `egress-http-runner` selected and
  mediated by the hotel; membranes retain transport semantics.
- The implemented audit payload records target, status, size, duration,
  placement, credential reference, and disposition without request or response
  content. Revisit only when an operator use case proves that insufficient.
- Which outbound classes should support approval-gated release versus strict deterministic allow/deny?
- When should model/provider egress stop being an exception?
- How does this intersect with future perimeter health / membrane supervision checks?

## Links

- [docs/architecture/MEMBRANE_COMPONENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_COMPONENT_PROPOSAL.md)
- [docs/architecture/HOTEL_PERIMETER_TRUST_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HOTEL_PERIMETER_TRUST_PROPOSAL.md)
- [docs/architecture/TOOL_MANAGEMENT_PLANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_MANAGEMENT_PLANE_PROPOSAL.md)
- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
