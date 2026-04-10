---
title: "Philotic Architecture Status"
doc_type: status
domain: runtime-sessions
status: active
last_updated: 2026-03-27
tags:
  - source-of-truth
  - current-state
  - active-seam
  - transitional
related_docs:
  - README.md
  - ARCHITECTURE.md
  - SESSION_LOOP_PROPOSAL.md
  - TELEGRAM_POLL_LEASE_PROPOSAL.md
  - DESKTOP_MEMBRANE_PROPOSAL.md
  - OPERATOR_MEMBRANE_PLUGIN_BOUNDARY_PROPOSAL.md
  - RUNTIME_AUTHORITY_LEASES_PROPOSAL.md
  - MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md
  - DOC_TAGGING_FRONTMATTER_PROPOSAL.md
task_refs:
  - docs/task.md
tracks_domains:
  - runtime-sessions
  - membrane-transport
  - mesh-placement
  - memory-context
  - tooling-execution
  - operator-control-plane
  - deployment-distribution
  - migration-parity
---

# Philotic Architecture Status

> **Status:** Living Snapshot | **Last Updated:** 2026-03-27

This document is the single source of truth for Philotic's current architecture status.

Use it to answer three questions fast:

1. What is implemented and considered current repo truth?
2. What is intentionally transitional?
3. What is actively being worked right now?

This is not the place for full design arguments. For those, follow the linked proposal docs.

## How To Read This

- `Implemented` means there is code and test evidence in the repo today.
- `Transitional` means the shape is real enough to rely on for the current slice, but it is not presented as final architecture.
- `Active` means the seam is currently hot based on [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md), active proposals, and the observed worktree on 2026-03-12.
- when convenience docs disagree on concrete transport details, current code and [docs/README.md](/Users/jaredlikes/code/philotic-stack/docs/README.md) win over stale crate-level prose.

## Current Architecture Summary

Philotic currently operates as a hotel-centered runtime:

- `aiua` is the runtime authority for hotel orchestration, guest materialization, context-graph persistence, IPC handling, and inter-hotel coordination.
- `membrane`, `philote`, `model-router`, and `tool-runner` are separate guest-facing binaries with explicit runtime boundaries.
- local guest-to-hotel IPC currently runs over Unix domain sockets, driven by `PHILOTIC_HOTEL_SOCKET`; default paths include `/tmp/philotic-aiua.sock` for generic local clients and hotel-specific socket paths such as `/tmp/philotic-<hotel>.sock` when `aiua` materializes a named hotel.
- canonical session state now lives in the context graph, while apartment-style checkpoints remain derived recovery projections rather than a competing source of truth.
- Telegram ingress is session-aware and guarded by hotel-owned poll-lease authority, with explicit delegated remote polling available as a transitional exception.
- local and remote execution routing both exist, but several placement, delegation, and admin/control-plane seams are still under active development.

## Implemented Foundations

### Runtime and authority

- one hotel daemon per machine is the current runtime model
- the context graph is the canonical durable owner for hotel and session state
- guest materialization and supervision are hotel-owned responsibilities
- guest binaries are resolved through the current binary-resolution contract rather than hardcoded `target/debug` assumptions

Primary references:
- [ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md)
- [GUEST_BINARY_RESOLUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/GUEST_BINARY_RESOLUTION_PROPOSAL.md)

### Sessions and approvals

- generalized session records, participants, turns, and events are modeled in the graph layer
- transport identities in `membrane` bind to stable `session_id` values
- session timeline/progress events persist through the IPC plane
- approval policy, preapproval, and session status/bindings are included in session snapshots
- approval interrupts and slash-command steering are implemented in the current agent loop

Primary references:
- [SESSION_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SESSION_LOOP_PROPOSAL.md)
- [AGENT_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_LOOP_PROPOSAL.md)
- [APPROVAL_UX_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/APPROVAL_UX_PROPOSAL.md)

### Membrane and Telegram

- Telegram text, photo, and voice ingress normalize into structured envelopes
- slash commands are short-circuited before the normal model path
- Telegram poll leases are hotel-owned, renewed, fenced, explicitly released on graceful shutdown, and can be delegated to named remote hotels as a transitional contract
- poll authority is anchored to the agent's home hotel rather than the current routed role

Primary references:
- [TELEGRAM_INTEGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_INTEGRATION_PROPOSAL.md)
- [TELEGRAM_POLL_LEASE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_POLL_LEASE_PROPOSAL.md)
- [SLASH_COMMANDS_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SLASH_COMMANDS_PROPOSAL.md)

### Routing and execution

- the hotel advertises local capability availability and can route to remote execution advertisements when local implementations are unavailable
- inter-hotel execution transport is now distinct from raw UDP beacon payload bodies
- reply routing remains session-owned through the membrane boundary

Primary references:
- [INTER_HOTEL_ROUTING_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/INTER_HOTEL_ROUTING_PROPOSAL.md)
- [NATIVE_OVERLAY_VPN_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/NATIVE_OVERLAY_VPN_PROPOSAL.md)
- [MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md)

### Tooling and model execution

- abstract tool catalog seeding exists in the context graph
- tool assembly uses catalog-backed metadata and approval annotations
- local workspace tooling exists through `tool-runner`, although broader routed error-envelope and management-plane work remains incomplete
- `model-router` is the shared model execution boundary for current providers

Primary references:
- [TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md)
- [TASK_RUNNER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TASK_RUNNER_PROPOSAL.md)
- [MODEL_CONTROLLER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MODEL_CONTROLLER_PROPOSAL.md)

### Deployment and memory protocol

- the first VPS deployment boundary is defined with Red Hat Ansible as outer control plane and Philotic hotel runtime as inner authority
- Muninn bootstrap and required-memory-session discipline are part of the repo's active workflow contract

Primary references:
- [RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md)
- [MUNINN_MEMORY_PROTOCOL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MUNINN_MEMORY_PROTOCOL_PROPOSAL.md)
- [AGENT_WORKFLOW_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_WORKFLOW_PROPOSAL.md)

## Transitional Architecture

These are real current choices, but they are explicitly not the final story:

- Tailscale/MagicDNS remains the named transitional scaffold for deployed inter-hotel reachability
- model/provider egress is still an explicit exception rather than routed through a perimeter egress plane
- build-on-host VPS deployment is still transitional until artifact distribution hardens
- role incarnation design direction is adopted, and the first graph/routing substrate now exists; current design direction now favors context-shift role activation with shared self/memory by default, while concurrent role materialization remains conditional
- some architecture-facing crate READMEs still carry older `Ansible`, port-oriented, or socket-path wording and should be treated as convenience narratives until they are reconciled with current code

## Proposed Architecture Directions

These are accepted proposals not yet in implementation:

| Proposal | Core idea | Status |
| --- | --- | --- |
| Agent-centric resource model | Agents declare and request resources; hotel acts as broker; demand-derived materialization replaces static guest config; agent graph is a mesh-synced tool-runner resource; router-listener generates RL training traces | [AGENT_RESOURCE_MODEL_PROPOSAL.md](AGENT_RESOURCE_MODEL_PROPOSAL.md) — proposed |
| Graph layer unification | Introduce `GraphDomain` as the unified middle layer; all domain operations expressed in terms of `GraphAdapter` primitives; one update point for entity types across all graph stores | [GRAPH_LAYER_UNIFICATION_PROPOSAL.md](GRAPH_LAYER_UNIFICATION_PROPOSAL.md) — proposed |
| Architectural rules and roadmap | Extract standing constraints from proposals into ARCH_RULES.md; maintain dependency-ordered seam roadmap in ROADMAP.md; check rules at slice close-out | [ARCH_RULES_AND_ROADMAP_PROPOSAL.md](ARCH_RULES_AND_ROADMAP_PROPOSAL.md) — proposed |

## Active Work Right Now

These are the most clearly active seams as of 2026-03-13:

| Seam | Current truth | Next pressure |
| --- | --- | --- |
| Session leases and ownership semantics | session durability, approval state, and timeline projection exist; explicit active-work ownership semantics are still incomplete in the task board | define and implement canonical active ownership without creating a second authority shadow |
| Runtime authority leases | a shared `LeaseEnvelope` and central runtime lease registry/provider now exist, Telegram poll lease has been migrated onto that abstraction, and the first explicit boundary contract now separates lease authority from materialization, supervision, routing, and vault access | move the next runtime seam onto the shared provider path and prove the contract on a non-Telegram path |
| Desktop membrane boundary | `philotic-web serve` now acquires and renews a dedicated desktop membrane lease, serves the embedded desktop through a same-origin `HttpOnly` session cookie without JS credential injection, uses same-session cookie auth for both API and WebSocket access, routes the first membrane reads (`status`, `guests`, `agents`) through explicit hotel-owned IPC view models, exposes a first typed `/api/mesh/targets` inventory with `source_hotel`, `target_hotel`, reachability, and freshness attribution from the hotel-owned registry, adds `/api/mesh/targets/:target_node_id/status` with `local-canonical` local status plus a real remote management query attempt that falls back to `remote-heartbeat-observed` when the target does not answer, exposes `/api/mesh/targets/:target_node_id/guests` with canonical local reads plus a first real cross-hotel management query attempt backed by a daemon-owned generic `management.operator_surface_query` worker and typed `OperatorSurfaceQueryHandoff` envelope with explicit `remote-query-failed` fallback when the remote path cannot complete, exposes `/api/mesh/targets/:target_node_id/agents` as a bounded redacted target-agent inventory surface with canonical local reads and the same routed remote query/failure semantics, exposes `POST /api/mesh/targets/:target_node_id/agents/:agent_id/chat` as the thin desktop operator-chat adapter over the canonical conversation path, now returning `202 Accepted` and streaming in-flight `operator_chat:turn_event`, `operator_chat:partial_reply`, `operator_chat:reply`, and `operator_chat:error` updates over `/ws` while the routed turn is in flight; the synchronous operator-chat helper now preserves `partial_reply` frames as non-terminal observations instead of mistaking them for the final answer, the lower conversation/model path can now carry optional `partial_replies` through `model_result.result.partial_replies` so `philote` emits real `partial_reply` frames before the final reply, and the Telegram membrane now edits the active draft message on `partial_reply` / final text completion instead of treating progressive delivery as a decorative comment; targeted test proof shows a routed operator chat turn can leave the local hotel, traverse a remote-hotel bridge, and return to the local reply inbox over the same conversation semantics; apartment inspection remains explicitly denied on the default membrane surface; bearer auth remains only as a transitional remote/debug path; the first reusable `operator.targets.*` IPC contract is now landed for targets/status/guests/agents, the first routed operator-chat contract is landed, the shared target payload structs are operator-owned with desktop names retained only as compatibility aliases, `philotic-web` uses that seam as an adapter under the current desktop routes, and only the current target-oriented desktop routes are still accepted as transitional adapters | provider-native incremental generation, watched-live remote-hotel proof beyond the current test bridge, and backing-authority swaps behind router resolution |
| Governed workflow skills | first `abstract_skill` graph scaffolding now exists for handoff/governance, and the architecture now distinguishes same-agent role handoff from peer delegation and external cognitive peer handoff; same-identity handoff is now being refined toward context-shift semantics by default, and the existing `HandoffBundle` wire path now carries a first compatibility-first richer packet; `philote` has been split into persona + worker binaries; the graph now also carries first real `WorkflowSkillRecord` seeds for `handoff.to_role` and `role.create_or_update`, so role creation/update finally has a dedicated governed workflow home while `role.authoring` narrows back toward cognitive role-lens assembly instead of pretending the mutation sequence itself is a skill; role-authoring skill metadata and the `role.create_or_update` workflow contract now compile from repo-local markdown frontmatter embedded into the binary instead of living only as hand-maintained duplicate seed strings; the prompt-facing tool surface now prefers `role.create_or_update` while `role.configure` remains a compatibility alias and low-level hotel execution path; the role seam now has a distinct hotel-side workflow execution plane via `ExecuteWorkflow { workflow_name: \"role.create_or_update\" }`, even though the workflow still resolves internally through the existing role mutation machinery | a generic workflow execution substrate beyond the role seam is still pending, along with skill lifecycle validation layers, field sourcing map, meta-skill contract, and `skill.register` + `subagent.spawn` tool contracts now defined in `SKILL_LIFECYCLE_PROPOSAL.md`; real `SpawnSubagent` execution and `philote` worker integration are in progress |
| Telegram membrane authority | poll-lease acquire, renew, expiry, home-hotel checks, graceful release, dual-poller smoke coverage, and explicit delegated remote polling are implemented | canonical mesh-visible poll authority is still deferred |
| External membranes and edge trust | membrane is documented as the outside-world boundary, and `A2A` / `Nostr` are now proposed as membrane transports with explicit trust, sentinel, and perimeter-defense contracts rather than mesh replacements | define the first normalized external transport envelope, external principal trust records, and membrane sentinel finding schema before implementing one narrow transport |
| Mesh-visible state placement | current local authorities mostly live in hotel runtime state, SQLite, or file-backed records; shared criteria for what becomes mesh-visible are now being defined explicitly | classify current state families and stop solving each cross-hotel visibility seam with a bespoke projection ritual |
| Role incarnation model | `RoleIncarnationRecord`, `TurnLoopConfig`, `ConfigureRole` IPC action, session `active_incarnation_id`, inbound agent-task routing to the active incarnation, orchestrator fallback for missing active guests, a first parked-delivery/on-demand materialization path for configured inbound roles, basic `HandoffToRole` / `HandoffBack` IPC, `/role <name>` + `/back` + `/roles` operator surfaces, first `abstract_skill` graph scaffolding, a compatibility-first typed `role_activation` object through hotel snapshot -> `philote` session state -> context projection, a first richer same-identity handoff packet through the existing `HandoffBundle` path, and a compatibility-first `SubagentDelegation` / `SpawnSubagent` wire contract with explicit not-yet-implemented hotel rejection now exist; the hotel now also tracks `RoleReadinessState`, eagerly materializes newly configured role workers as separate `philote` OS processes, marks them `Routable` only after `Register + SubscribeInbox { role:<agent_id>:<role_name> }`, and returns `HandoffPending` until that role route is genuinely live so handoff stops confusing configured records with materialized workers; same-self `handoff.to_role` / `handoff.back` now project as workflow reflexes rather than generic approval scalpels, and successful same-self handoffs now feed a dedicated hotel-side evidence receptor that accumulates agent-graph habit metadata (`success_count`, `habit_state`) for remembered role-shift reflexes; `philote` now treats those habits the same way a nervous system should: single-success role reflexes remain candidate posture cues, reinforced reflexes (or explicitly rewarded ones) can auto-project for matching work like implementation-heavy turns, and rejected linked reflex policy can still suppress them through the same immune path used for other agent-learned posture; that remembered same-self handoff path is now explicitly manifest- and toolset-informed too, with `handoff.to_role` seeded as a workflow skill whose field sourcing points at the target role manifest/toolset lens, successful role-handoff habit writes preserving target role metadata (`toolset_profile`, allowed skills, identity addendum, manifest excerpt), and same-identity handoff bundles carrying that target-role lens into the worker handoff context so roles act more like scoped cognitive manifests than bare names; remembered role handoff matching now prefers declared receptor markers derived from that role lens (`manifest_markers`, `skill_markers`, `toolset_markers`) and only falls back to legacy `trigger_class` residue for older reflex records; the current role seam now also treats explicit non-admin same-self role authoring as specific enough to skip the generic approval interrupt, and role workers now carry their actual guest incarnation through handoff activation plus canonical snapshot merge so materialized role posture stops silently collapsing back to orchestrator in local session state | formalize workflow-owned handoff/delegation assembly rules, prove the post-handoff canonical-route path live again after the activation/local-state fix, implement real `SpawnSubagent` execution and result routing, and only then decide where concurrent role materialization is genuinely warranted |
| Tool execution envelope | catalog-backed tools and approval policy exist | extend structured error behavior across more routed components instead of falling back to ad hoc strings |
| Perimeter egress control | proposal exists and the lack of a unified egress plane is explicitly called out | define the first policy object and classify current egress exceptions |
| Deployment hardening | VPS boundary and peer rendering contract are defined | remove plaintext secret assumptions and prove real VPS smokes |

## Domain Status Matrix

| Domain | Status | Source of truth | Active work |
| --- | --- | --- | --- |
| Runtime and sessions | implemented, still evolving | [SESSION_LOOP_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SESSION_LOOP_PROPOSAL.md) and code in `aiua`, `philote`, `ansible-mesh-core` | session ownership semantics, compaction policy, bounded loop follow-through, and role context-shift semantics |
| Membrane and transport | implemented, still evolving | [TELEGRAM_INTEGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_INTEGRATION_PROPOSAL.md), [TELEGRAM_POLL_LEASE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_POLL_LEASE_PROPOSAL.md), [DESKTOP_MEMBRANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DESKTOP_MEMBRANE_PROPOSAL.md), and [MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL.md) | delegated poll authority, desktop/operator membrane hardening, broader transport surfaces, and external membrane trust/edge-defense contracts |
| Mesh and placement | partially implemented | [INTER_HOTEL_ROUTING_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/INTER_HOTEL_ROUTING_PROPOSAL.md), [MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md) | placement policy, trust boundaries, overlay evolution, and mesh-visible state classification |
| Memory and context | partially implemented | [MUNINN_MEMORY_PROTOCOL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MUNINN_MEMORY_PROTOCOL_PROPOSAL.md), [PERSONALITY_AND_CONTEXT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERSONALITY_AND_CONTEXT_PROPOSAL.md), and [PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md) | typed context projection path is now smoke-green for the current cognitive request path through `philote` and `model-router`; the first typed `role_activation` object now flows into session/context projection, and next pressure is expanding role addendum/toolset/skillset and hook-backed refresh |
| Tooling and execution | partially implemented | [TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md) and [MODEL_CONTROLLER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MODEL_CONTROLLER_PROPOSAL.md) | structured model envelope and initial `request_class` routing are now smoke-green for the current cognitive path; `philote` also carries a first staged turn routing plan for voice ingress -> cognition -> voice egress, now uses stage-aware context trimming for ingress requests, suppresses tool projection on non-cognitive stages, keeps cognitive re-entry on the same projection policy, narrows low-intent prompt/context affordance cues like skill guidance and approval posture, rejects inappropriate non-cognitive approval interrupts, redirects low-intent free-form approval asks back toward direct response, and now also suppresses approval-gated tools from generic cognitive projection unless the operator explicitly names them, so workflow scalpels do not show up as default membrane chat affordances; same-self `handoff.to_role` / `handoff.back` now sit in a separate workflow-reflex lane instead of the generic approval-gated bucket, which means they no longer require per-action approval and can be projected naturally from role-shift intent like “switch to developer” or “switch back to orchestrator” without reopening shell/config authority; it forwards stage-derived routing hints without turning model-controller into turn owner, exposes a governed `routing.policy.propose` path plus `routing.refinement` skill so routing self-improvement can be proposed without silently mutating live behavior, now stores routing-policy proposals as dedicated hotel-graph `routing_policy` records with explicit operator disposition and evaluation history instead of smuggling them through the general rule substrate, now also gives those records a later-life operator control path through list + disposition update surfaces so governance is not frozen at birth approval, and now lets hotel projection act as both immune system and reward system for linked `agent_learned` reflex layers: rejected routing-policy disposition suppresses them with visible suppression markers, while approved disposition reinforces them with a precedence boost and visible reward markers, and `philote` now feeds those markers back into live cognition-stage routing-preference ranking so the nervous system can actually bias turn-plan selection instead of just decorating bindings; the shared catalog layer has now grown a first `abstract_model` substrate too, with hotel-projected model markers flowing into session bindings and then into live stage-aware routing ranking, and now also projects first `abstract_tool` / `abstract_skill` marker catalogs into session bindings so runner assembly can consume shared tool ligands to shape model-visible tool schema/description, mark `high_agency` tools as approval-sensitive, and suppress remote-only routing for `local_only` tools without widening the hotel's key ring; it now also has a first explicit turn-routed capability taxonomy in `philote`, separating stage-local species like `voice.transcribe`, `text.generate`, and `voice.synthesize` from collapsible native-live species like `response.generate` and `voice.dialogue`, and `model-router` controller parsing/validation now recognizes those native-live species as first-class task kinds while providers still refuse them explicitly unless wired, so future Gemini Live-style paths can be added as governed stage compositions instead of magical realtime exceptions; those native-live species now influence real `TurnRoutingPlan` compilation under policy too, so eligible voice turns can collapse ingress into a native-live cognition receptor when shared model markers plus routing preferences actually express that ligand, and the chosen cognition species now threads through outbound request assembly and re-entry instead of living only in plan metadata; current external pressure is now very concrete: Gemini 3.1 Flash Live is documented as a stateful Live API session with streamed PCM audio I/O, sequential function calling, and session-resumption lifecycle rather than a plain `generateContent` call, and `model-router` now has a working first Gemini Live websocket path on the native-live provider seam instead of only a stub, with websocket setup, native text `response.generate`, live tool-call parsing, runtime interception of native-live tool calls before envelope serialization, resumption-handle capture, and a first narrow shared `media-prep` substrate that now handles both transported non-PCM audio ligand preparation for `voice.dialogue` and a shared `audio_artifact` envelope for outbound artifact interception across `model-router`, `philote`, and `membrane`; it now also keeps Gemini Live websocket sessions alive across tool execution, surfaces the pending live `functionCall.id` back to `philote`, and sends the resulting `toolResponse` back over the same live receptor instead of rebooting the turn from scratch, and a startup-driven `smoke-gemini-live` path now proves the binary-level `response.generate -> tool_call -> toolResponse -> final reply` continuity against a fake local Live websocket receptor; `philote` now also recognizes cognitive-stage audio artifacts in `model_result.artifacts` and will deliver them directly on voice turns instead of reflexively invoking a second synth pass, which makes the native-live path less ironically self-duplicating, and stage-derived provider hints now update stage controller dispatch too so an ingress `voice.transcribe` preference for ElevenLabs actually targets `model.elevenlabs` instead of asking the generic `model` controller to impersonate the wrong provider; as a narrower live-debug experiment, Gemini `voice.transcribe` fallback traffic now defaults to `gemini-3-flash-preview` instead of the generic latest alias so Bjork voice ingress can test that preview receptor directly without changing the broader cognitive default; the hotel now also has a first stable config-delta graft in `aiua import-config --file ... --hotel ...`, so long-running hotels can absorb graph config and agent identity changes without reseeding guests or turning routine startup into accidental configuration management; the remaining reality gap is now narrower and more honest: the current shared substrate covers audio ligand prep plus audio artifact envelopes rather than a fuller artifact/interceptor path, the PCM conversion path still depends on host `ffmpeg` rather than an in-process codec substrate, and live-session continuity is currently a provider-local pool inside the long-lived `model-router` process rather than a broader hotel-governed substrate; it stores agent-local routing preferences in the active agent graph through `agent.graph.read`/`agent.graph.write`, projects those preferences through hotel session bindings so `philote` can apply advisory provider/model hints while compiling live turn-routing plans, carries an explicit `effective_rights` key ring through hotel bindings so tool/component assembly paths refuse to widen visibility beyond projected rights even when a runner or route exists, now has a first shared abstract-right catalog plus controller-side validation that surfaced tools still match that key ring, and now opportunistically carries and hydrates `agent_graph_snapshot` payloads on transported agent-directed work when the source hotel knows the owning agent and is that agent's `authority_hotel`, with receiving hotels persisting typed placement markers (`marker_kind`, `marker_source`, `marker_strength`) plus inferred `placement_risk_level` and current delivery placement into session runtime provenance, projecting that posture into session bindings, using fresh persisted local placement to guide local delivery/materialization before generic fallback while stale placement hints undergo TTL-based apoptosis, fresher active-incarnation truth supersedes older local provenance immediately for weak `receptor_ingress` markers, explicit `transport_continuity` / `role_handoff` markers keep their placement claim under that same conflict, weak markers are prevented from triggering parking/materialization on their own, and posture-derived right policy classes now split remote reach by class so guarded sessions can still use remote model/component execution while denying remote tool execution and shrinking credential scope without changing the hotel's underlying key ring; that fast posture surface is now also exposed with `effective_reflexes` naming (`remote_tool_reflex`, `remote_component_reflex`, `credential_scope_reflex`) while `effective_right_policy` remains a transitional compatibility bridge underneath, and reflex governance has now advanced from bare `reflex_overrides` / `reflex_evaluations` blobs to an ordered `effective_reflex_policy` projection with distinct origin classes, where hotel-projected `reflex_policy_defaults` from bindings form `hotel_default` layers, mesh-synced agent-graph `reflex_preferences` project as `agent_learned` layers, explicit session `reflex_policy_records` stay the top override class, and approved `routing.policy.propose` calls can now write an explicit learned reflex payload back into that agent-owned layer while recording both a local reflex-evaluation trace and durable routing-policy evaluations; current design pressure is now explicitly three-layered: shared catalogs define what exists, agent graphs hold mutable overlay/posture, and the hotel projects the effective key ring and enforceable bindings; next pressure is deciding whether live-session continuity should remain provider-local or graduate into a broader governed substrate without accidentally creating a stealth second turn owner |
| Operator and control plane | proposed to early transitional | [ROLE_POSTURE_AND_ADMIN_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROLE_POSTURE_AND_ADMIN_PROPOSAL.md), [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md) | elevation, admin workflows, perimeter trust and egress |
| Deployment and distribution | implemented boundary, incomplete rollout | [RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md) | real VPS smoke, secret handling hardening, artifact distribution |
| Migration and parity | in planning | [OPENCLAW_PARITY_MIGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OPENCLAW_PARITY_MIGRATION_PROPOSAL.md) | explicit parity matrix and migration-critical seams |

## Documentation Maintenance Rule

When a slice lands:

1. Update this file if the answer to "what is implemented" or "what is active right now" changed.
2. Update the relevant proposal disposition/current-slice text.
3. Update [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md) if sequencing or work ownership changed.

## Related Entry Points

- [docs/README.md](/Users/jaredlikes/code/philotic-stack/docs/README.md)
- [README.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/README.md)
- [ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md)
- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
