---
title: Perimeter Egress Control Proposal
doc_type: proposal
domain: operator-control-plane
status: proposed
disposition: accepted-current-slice
last_updated: 2026-08-26
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
task_refs:
- docs/task.md
proposal_id: perimeter-egress-control
implements: []
implemented_by:
- crates/exec-guard/src/net_egress.rs
- crates/ansible-mesh-core/src/mcp_upstream.rs
- crates/tool-runner/src/main.rs
- crates/philote/src/runtime.rs
- crates/philotic-web/src/explain.rs
active_seams:
- egress-policy-object
- outbound-classification
- shell-egress-fence
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

Proposed, with one implemented slice landed (the shell egress fence, below).

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

### Implemented Slice: Shell Egress Fence (2026-08-26)

The governed outbound fabric (binding-scoped `http:<binding>.request` tools with
host allowlists, DNS pinning, the secret-ref credential boundary, and a
content-free audit trail) was being bypassed by a single tool: `bash.exec`. The
L0 `exec-guard` floor blocks only unrecoverable destruction and has no network
predicate, so `curl`/`wget`/`nc`/`/dev/tcp`/interpreter-socket one-liners
egressed with no allowlist, no credential boundary, and no audit — on macOS
`Direct` mode there is also no sandbox network enforcement. This is General
HTTP / Tool Egress (§2 above) escaping the perimeter through the shell.

The fence closes the *accidental and lightly-injected* path:

- **Detector, config-free, in `exec-guard`** — `detect_network_egress` recognizes
  the raw fetch/exfil primitives and extracts the target host when it is written
  literally. It is a detector, not a policy: it reads no config, exactly like the
  L0 hardline floor.
- **Policy at the call site** — both raw-shell dispatch points
  (`tool_runner::execute_bash_tool`, `philote::runtime::run_bash_command`) reuse
  `McpEgressPolicy` (loopback + tailnet CGNAT always allowed; everything else must
  be allowlisted). A resolvable loopback/tailnet host passes; any other host, and
  any host that cannot be statically proven, is denied and the model is redirected
  to the governed binding path. Fail-closed on an unresolvable host mirrors the
  MCP stdio allowlist's empty-default-deny.
- **Config surface (transitional)** — `PHILOTIC_SHELL_EGRESS_ALLOW` (env var,
  same host grammar as the MCP egress policy) widens the allowlist. This matches
  the `PHILOTIC_SANDBOX_SOCKET` env idiom and is deliberately transitional; the
  durable home is a hotel config node unified with `mcp_egress_policy`
  (**next seam: `shell-egress-fence` → config-node unification**).

Honest scope (named per AGENTS.md §2.4, Proven vs Intended): this is a regex over
shell text under exec-guard's stated threat model — an honest-but-wrong or
lightly-injected agent, **not** containment against a process that already
controls the guest binary. It loses to base64, a written-then-run script, or
`exec 3<>/dev/tcp`, and macOS `Direct` mode still has no kernel-level network
enforcement. What it buys: the ungoverned shell door closes for the common case
and the governed door becomes the obvious one. Deliberately out of scope for this
slice: `git`, `gh`, `ssh`/`scp`, and package managers (`brew`/`cargo`/`npm`/`pip`)
— network-capable dev/admin tooling with their own semantics.

## Why This Needs Its Own Proposal

This is related to membranes and perimeter trust, but it is not identical to either.

- [MEMBRANE_COMPONENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_COMPONENT_PROPOSAL.md) defines the outside-world communication boundary
- [HOTEL_PERIMETER_TRUST_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HOTEL_PERIMETER_TRUST_PROPOSAL.md) defines hotel identity, membership, and trust
- this proposal defines outbound egress control

If we blur these together too early, we risk either:

- turning `membrane` into a universal "network stuff" bin
- or leaving outbound traffic policy implicit because every relevant rule lives in a different doc

## Current Reality

Today the repo has no unified outbound egress control plane.

Current likely shape:

- membranes make transport-native outbound calls
- tool/model/provider code can make direct HTTP calls where needed
- there is no first-class egress policy object
- there is no canonical place to audit "what external requests may leave this hotel"

That is acceptable for current implementation velocity, but not a stable long-term security posture.

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

## First Slice Recommendation

The first coherent implementation slice should:

1. Define the canonical egress policy object and finding schema.
2. Inventory current direct outbound HTTP call sites by component class.
3. Classify which current egress paths are:
   - perimeter-controlled already
   - temporary direct exceptions
   - violations of the intended future model
4. Pick one non-model outbound HTTP path and route it through the perimeter boundary.
5. Keep model/provider egress as an explicit documented exception until a later decision.

## Open Questions

- Should the first implementation live in `membrane`, a dedicated egress service, or hotel-owned request mediation?
- What is the minimum useful audit payload for outbound requests?
- Which outbound classes should support approval-gated release versus strict deterministic allow/deny?
- When should model/provider egress stop being an exception?
- How does this intersect with future perimeter health / membrane supervision checks?

## Links

- [docs/architecture/MEMBRANE_COMPONENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_COMPONENT_PROPOSAL.md)
- [docs/architecture/HOTEL_PERIMETER_TRUST_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HOTEL_PERIMETER_TRUST_PROPOSAL.md)
- [docs/architecture/TOOL_MANAGEMENT_PLANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_MANAGEMENT_PLANE_PROPOSAL.md)
- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
