---
title: "External Agent And Event Membranes Proposal"
doc_type: proposal
domain: membrane-transport
status: proposed
last_updated: 2026-03-13
tags:
  - membrane
  - a2a
  - nostr
  - security
  - perimeter
related_docs:
  - ARCHITECTURE_STATUS.md
  - MEMBRANE_COMPONENT_PROPOSAL.md
  - TELEGRAM_INTEGRATION_PROPOSAL.md
  - HOTEL_PERIMETER_TRUST_PROPOSAL.md
  - PERIMETER_EGRESS_CONTROL_PROPOSAL.md
  - CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: external-agent-event-membranes
implements: []
implemented_by: []
active_seams:
  - a2a-membrane-contract
  - nostr-membrane-contract
  - transport-edge-trust-gates
  - membrane-sentinel-checks
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
  - ARCHITECTURE.md
---

# External Agent And Event Membranes Proposal

## Goal

Define how Philotic should expose external agent and decentralized event transports without confusing them with the hotel's internal mesh or session authority.

This proposal covers:

- `A2A` as an external agent interoperability membrane
- `Nostr` as a decentralized event-native membrane
- the security and trust boundaries required before either transport is treated as production-worthy
- how membrane-edge defense, scanning, and perimeter trust should cooperate without collapsing into one vague "security layer"

## Core Recommendation

Treat `A2A` and `Nostr` as membrane-facing transport implementations or transport capabilities, not as replacements for Philotic's internal hotel-to-hotel routing model.

Recommended architecture:

- Philotic mesh remains the internal hotel authority and placement plane
- `membrane.a2a` exposes Philotic to external agent ecosystems
- `membrane.nostr` exposes Philotic to decentralized event/relay networks
- both normalize into the same internal hotel/session/task contracts used by other membranes
- perimeter trust, egress control, and admin surfaces remain hotel/control-plane concerns rather than being quietly re-owned by the membrane

In short:

- mesh is how Philotic hotels coordinate with Philotic hotels
- membrane is how outside systems speak to Philotic
- session graph is where the canonical conversation/work truth lives

If `A2A` or `Nostr` starts deciding hotel placement, durable session truth, or internal routing policy, the membrane has torn a hole in the wall and started calling itself architecture.

## Disposition

`proposed`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Current Slice

This slice is document-first.

It does not claim:

- `A2A` support exists today
- `Nostr` support exists today
- the perimeter trust model is fully implemented

It does claim the boundary direction should be:

- external agent and decentralized event transports belong under the membrane component model
- internal hotel routing should stay Philotic-native
- security posture must be designed up front instead of being bolted on after the first "it connected!" demo

## Why This Is A Membrane Question

[MEMBRANE_COMPONENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_COMPONENT_PROPOSAL.md) already defines membrane as the translator, guard, and delivery provider between outside systems and the internal Philotic world.

`A2A` and `Nostr` fit that role well:

- both introduce outside identities
- both introduce outside trust and abuse surfaces
- both need transport-native ingress and egress behavior
- neither should become a second canonical owner of internal session or routing state

This means the right question is not:

- "should A2A replace the Philotic mesh?"

The right question is:

- "how should A2A and Nostr enter Philotic through a membrane boundary while preserving Philotic's existing runtime authority split?"

## Boundary Model

### Membrane-owned responsibilities

`membrane.a2a` and `membrane.nostr` should own:

- transport connectivity and protocol conformance
- external identity parsing and normalization
- transport-edge authentication checks
- ingress shaping, dedupe, replay defense, and rate policy
- session-binding lookup requests
- outbound rendering into transport-native replies, receipts, or events

### Hotel-owned responsibilities

The hotel should remain authoritative for:

- session creation and durable session state
- guest materialization and liveness
- internal task routing and placement
- policy objects for trust, authorization, and approvals
- canonical security findings and audit persistence

### Control-plane-owned responsibilities

The control plane should remain authoritative for:

- trusted peer or relay inventories
- trust class definitions
- revocation and quarantine
- egress allow/deny policy
- operator inspection, override, and incident workflows

### Explicit non-responsibilities

`membrane.a2a` and `membrane.nostr` should not own:

- inter-hotel placement
- internal mesh membership
- generic tool execution policy
- durable memory promotion
- model-router selection logic

That is the critical boundary. Otherwise "external transport support" turns into a stealth second control plane wearing a standards badge.

## Membrane Variants

### `membrane.a2a`

Purpose:

- expose Philotic as an interoperable external cognitive peer
- receive tasks, requests, approvals, or conversational turns from non-Philotic agent systems
- emit bounded responses, progress, and negotiated capability results back to those peers

Default posture:

- treat remote `A2A` peers as external principals, not mesh members
- require explicit trust records before allowing anything beyond low-risk conversational exchange
- keep capability exposure narrow and policy-labeled

### `membrane.nostr`

Purpose:

- receive decentralized social/event traffic from relays
- project Philotic output back into relay-native events or replies
- bind relay/pubkey/thread semantics onto Philotic sessions

Default posture:

- treat relays as transport infrastructure, not trusted authorities
- treat pubkeys as external principals that still require authorization policy
- default to mention/DM/addressed-event scope rather than ambient firehose consumption

## Normalized Internal Contract

Both membranes should normalize inbound work into the same kind of internal envelope shape:

- `transport`
- `transport_principal`
- `transport_conversation_id`
- `transport_message_id`
- `session_binding_hint`
- `message_kind`
- `content_parts`
- `attachments`
- `auth_context`
- `trust_context`
- `raw_transport_event_ref`

Important rules:

- transport-native metadata stays available for audit/debug
- hotel-facing routing stays transport-agnostic
- trust and auth context should be explicit fields, not implied by which membrane happened to deliver the event

Recommended transport-specific identity mapping:

- `A2A`: remote agent identity, remote workspace or tenant if present, remote conversation or task id
- `Nostr`: author pubkey, relay set, event id, thread/root reference, event kind

## Session Binding Rules

Session binding should stay hotel-owned even when the membrane performs the first lookup.

Recommended binding inputs:

- stable external principal
- stable external conversation/thread identifier when available
- optional transport-specific tenant or namespace
- explicit home membrane implementation

Recommended invariants:

- one session binding points back to one owning membrane target
- outbound replies route to that owning membrane target, not to a generic "some membrane somewhere"
- membrane switching is explicit and session-visible, not inferred from whichever transport spoke last

## External Trust Records

External membranes should not invent ad hoc trust blobs per transport.

Recommended first shared record:

```yaml
ExternalPrincipalTrustRecord:
  principal_id: string
  transport: a2a | nostr | telegram | other
  principal_kind: agent_peer | user_pubkey | relay | service
  trust_class: blocked | untrusted | conversational | delegated | privileged
  auth_material_ref: string?
  allowed_capability_classes:
    - conversation
    - status_query
  allowed_membranes:
    - membrane.a2a
  allowed_destinations: []
  rate_policy_ref: string?
  approval_policy_ref: string?
  quarantine_state: active | quarantined | revoked
  observed_endpoints: []
  notes: string?
```

Important rules:

- the record is hotel/control-plane owned, not membrane-owned mutable state
- relays and agent peers should both fit the same trust grammar even if their protocol details differ
- `trust_class` should steer default capability exposure but not replace explicit policy checks

Recommended initial trust classes:

- `blocked`
- `untrusted`
- `conversational`
- `delegated`
- `privileged`

The names can evolve, but the invariant matters more:

- trust class influences what may be requested
- explicit policy still decides whether a given action is allowed

## Trust Protocol

Philotic should use one trust pipeline shape across external membranes:

1. discovery
2. identity proof
3. transport admission
4. authorization
5. session binding
6. ongoing trust renewal
7. revocation or quarantine

### 1. Discovery

Examples:

- configured relay inventory
- configured `A2A` peer registry
- operator-approved invite or endpoint record

Discovery grants no authority by itself.

### 2. Identity proof

Examples:

- protocol-native signatures
- signed challenge responses
- API credentials or mTLS where applicable
- timestamp and nonce validation for replay resistance

### 3. Transport admission

Examples:

- protocol/version checks
- message size and attachment limits
- rate and burst policy
- relay or endpoint allowlist checks

### 4. Authorization

Examples:

- allowed transport classes
- allowed remote principals
- allowed capability classes
- approval requirements for privileged requests

### 5. Session binding

Only after identity and authorization checks pass should the membrane ask the hotel to resolve or create a session binding.

### 6. Ongoing trust renewal

Examples:

- relay health checks
- remote peer certificate or key rotation
- trust record freshness
- anomaly thresholds

### 7. Revocation or quarantine

Examples:

- temporarily quarantine a relay
- deny a remote `A2A` peer
- freeze outbound replies while preserving audit evidence
- force re-authentication or re-approval

## Security Posture

### Membrane edge gates

Before an inbound event becomes internal work, the membrane should perform deterministic edge checks for:

- authentication validity
- replay and dedupe
- size and attachment policy
- protocol conformance
- rate and abuse thresholds
- destination addressing validity
- trust class lookup

These checks should return structured denial reasons.

### Sentinel

Introduce a first-class `sentinel` concept for membrane-edge defense.

Recommended role:

- a deterministic security observer and policy-check pipeline attached to membrane ingress and egress
- emits structured findings, counters, and trust events
- can block, quarantine, or require review depending on policy
- does not become the owner of session truth, routing truth, or cognitive interpretation

The sentinel is best treated as a security/control-plane function that membranes call into or emit findings for, not as a replacement for the membrane itself.

Recommended sentinel finding classes:

- auth failure
- replay or nonce violation
- relay drift or endpoint mismatch
- schema/protocol violation
- suspicious burst or spam pattern
- oversized or disallowed attachment
- unauthorized capability request
- disallowed outbound destination
- trust downgrade or revoked principal

### Scanning

Scanning should stay layered and mostly deterministic at first.

Recommended first scanning classes:

- attachment MIME/type and size inspection
- URL and destination classification
- payload schema validation
- dedupe/replay scans
- simple content safety or abuse heuristics where required by transport
- trust-policy cross-checks against current inventories

Later cognitive/security review may summarize or correlate findings, but deterministic scanners should remain the first source of truth. Security theater loves replacing explicit checks with vibes and a dashboard.

## Sentinel Finding Contract

The first coherent implementation should define one structured finding schema that can be emitted by any membrane-edge sentinel path.

Recommended minimum shape:

```yaml
SentinelFinding:
  finding_id: string
  occurred_at: timestamp
  membrane_id: string
  transport: a2a | nostr | telegram | other
  principal_id: string?
  conversation_id: string?
  severity: info | low | medium | high | critical
  category: auth | replay | rate | schema | attachment | destination | capability | trust | anomaly
  enforcement_mode: allow | allow_audit | deny | quarantine | require_review
  decision_reason: string
  evidence:
    event_id: string?
    relay_or_endpoint: string?
    request_fingerprint: string?
  session_id: string?
  policy_refs: []
```

Recommended invariants:

- findings should be append-only audit artifacts
- denial and quarantine should point to the same finding format as softer audit-only cases
- findings should be useful to both operators and later automated summarization without requiring raw payload dumps

Recommended first enforcement modes:

- `allow`
- `allow_audit`
- `deny`
- `quarantine`
- `require_review`

This keeps enforcement explicit instead of baking policy outcomes into one giant boolean called `safe`, which would be efficient in the same way that unlabeled wires are efficient.

### Perimeter defense

Perimeter defense should combine:

- authenticated transport ingress
- trust inventories and trust classes
- bounded capability exposure
- deterministic egress policy
- audit trails and structured findings
- quarantine and revocation controls
- admin inspection surfaces

This proposal therefore depends on close coordination with:

- [HOTEL_PERIMETER_TRUST_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HOTEL_PERIMETER_TRUST_PROPOSAL.md)
- [PERIMETER_EGRESS_CONTROL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERIMETER_EGRESS_CONTROL_PROPOSAL.md)
- [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md)

## Transport-Specific Security Notes

### `A2A` security notes

Recommended default controls:

- explicit remote peer records
- signed request validation or equivalent authenticated channel requirement
- remote capability exposure allowlist
- per-peer rate and concurrency ceilings
- approval-gated access for privileged tool or mutation classes
- structured audit of remote requests, delegated actions, and outbound responses

Important warning:

an external agent interoperability protocol should not automatically inherit internal peer trust. A remote agent can be legitimate and still be outside the perimeter.

### `Nostr` security notes

Recommended default controls:

- relay allowlist or trust-classed relay inventory
- event signature verification
- event id dedupe and replay handling
- mention/DM/addressed-event gating by default
- pubkey allowlist, denylist, or trust-class policy
- attachment and link restrictions
- outbound posting rules tied to explicit session or operator policy

Important warning:

relays are delivery infrastructure, not identity authorities. "It came from a relay we know" is not the same statement as "the sender is trusted to invoke high-risk capabilities."

## Capability Exposure Model

External membranes should expose capability classes, not arbitrary internal internals.

Recommended initial classes:

- conversation
- bounded task request
- approval exchange
- status query
- artifact delivery

Avoid exposing by default:

- arbitrary tool execution
- internal graph mutation
- hotel admin operations
- mesh placement controls

Higher-risk classes should require:

- stronger trust class
- explicit operator policy
- possibly action-grant or approval semantics

## Outbound Egress Model

Outbound replies from `membrane.a2a` and `membrane.nostr` are still outbound network activity and should not dodge the perimeter egress story merely because they are "just communication."

Recommended rule:

- transport-native outbound posting belongs to the membrane
- policy and audit for whether that outbound traffic is allowed belongs to the perimeter egress plane

This preserves the useful split:

- membrane knows how to speak the protocol
- perimeter policy decides whether the message should be allowed to leave

## First Slice Recommendation

The first coherent slice should:

1. define the normalized transport envelope for external membranes
2. define the trust record shape for external principals, relays, and `A2A` peers
3. define the first sentinel finding schema and enforcement modes
4. implement one narrow membrane transport in a constrained mode
5. keep privileged capability exposure out of scope

Recommended order:

- start with the contract
- start with narrow trust defaults
- start with deterministic finding generation
- broaden only after audit and operator surfaces exist

Recommended candidate v1s:

- `membrane.nostr` in addressed-event/DM-only ingress mode with relay allowlists and conversational/status capability classes only
- or `membrane.a2a` with one explicit trusted peer record and bounded conversational/task intake only

Recommended non-goal for v1:

- proving both transports at once

## Explicitly Out Of Scope For The First Slice

- replacing Philotic inter-hotel routing with `A2A`
- ambient relay ingestion from arbitrary `Nostr` traffic
- unrestricted remote tool execution
- automatic trust of any external agent that can authenticate
- treating membrane-edge scanning as a substitute for approval policy or admin review

## Open Questions

- should `A2A` and `Nostr` be separate membrane guests or one multi-transport membrane runtime with separate implementation modules?
- what is the minimum useful trust record for an external principal?
- should sentinel enforcement live hotel-side, membrane-side, or as a shared policy service?
- which outbound `A2A` or `Nostr` operations need approval-class semantics versus static allow/deny?
- when should relay reputation or behavioral scoring be introduced, if ever?
