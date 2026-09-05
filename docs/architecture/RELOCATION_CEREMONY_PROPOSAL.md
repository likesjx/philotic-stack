---
title: Relocation Ceremony — Materialization Anywhere, At Any Time, By Philote Initiation
doc_type: proposal
domain: mesh-placement
status: proposed
disposition: proposed
last_updated: 2026-09-05
verification_level: none
tags:
- placement
- relocation
- materialization
- membrane-transport
- secrets
- continuity
- active-seam
related_docs:
- MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md
- INTER_HOTEL_ROUTING_PROPOSAL.md
- MESH_VISIBILITY_AND_STATE_PLACEMENT_PROPOSAL.md
- AGENT_INCARNATION_PROPOSAL.md
- AGENT_RESOURCE_MODEL_PROPOSAL.md
- FLEET_SUPERVISION_PROPOSAL.md
- COMPONENT_TEMPLATE_SCHEMA_PROPOSAL.md
- GUEST_BINARY_RESOLUTION_PROPOSAL.md
- TELEGRAM_POLL_LEASE_PROPOSAL.md
- RUNTIME_AUTHORITY_LEASES_PROPOSAL.md
- HOTEL_PERIMETER_TRUST_PROPOSAL.md
- MESH_PKI_HOTEL_IDENTITY_PROPOSAL.md
- KEY_VAULT_PROPOSAL.md
- BLOB_EXECUTION_PERIMETER_HARDENING_PROPOSAL.md
- ARCH_RULES.md
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md
proposal_id: relocation-ceremony
implements: []
implemented_by: []
active_seams:
- relocation-ceremony
- remote-materialization-ceremony
- membrane-transport-home
- graph-truth-over-seed
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
- ARCH_RULES.md
---

# Relocation Ceremony — Materialization Anywhere, At Any Time, By Philote Initiation

## Goal

Make it possible for a philote to say, from inside a conversation, "move me, my
membrane, and my controllers to `vps-jane`" and have the platform do it with no
operator hands on a shell, no redeploy, and no gap in service — and make the
reverse move, or a move to any other admitted hotel, equally routine.

The operator's founding tenet for the Philotic Web is **materialization anywhere,
at any time**. That covers every component class: membrane, agent (role
incarnation), model controller, tool runner, datasource. Until this proposal the
tenet was never written down; the closest statement is Multi-Hotel Component
Distribution's "make it routine to place agents on any hotel." This proposal
names the tenet, records how far the runtime is from it, and defines the
ceremony that closes the gap.

## Disposition

`proposed` (2026-09-05). No slice implemented. The first concrete relocation
this ceremony must carry is moving every orchestrator incarnation (Bjork, Coach,
Mac on `mac-jane`; Astrid, Ariel, Jane, Aria on `mbp-jane`) to `vps-jane`, with
Mac-bound specialists (Architect and anything holding `bash.exec`, desktop, ONNX
voice, iMessage) staying pinned to their laptop and reached by whisper.

Track execution in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
→ `New Project: Agent-Initiated Relocation`.

## Current Reality (audited 2026-09-05)

What a philote can already do:

- `role.set_home` pins a role's `home_node`; a later handoff to that role emits a
  `session.handoff` mesh event carrying the `RoleIncarnationRecord`, the
  `ToolsetProfileRecord`, and a `HandoffBundle`; the receiving hotel upserts both
  records and materializes the role (`role_materialization.rs`, `ipc.rs`).
  Proven live in both directions between `mac-jane` and `mbp-jane`.
- `transport.set_home` writes a `membrane_transport_home` record and the
  Telegram poll lease enforces it fail-closed. Proven live for Beacon on
  `vps-jane` (2026-05-29).
- Heartbeats advertise `NodeHealthSnapshot` (disk, memory, load) and
  `hotel.best_place_to_run` ranks hotels by role/tool match, reachability,
  health, and locality.

What stops the tenet from being true today — every one of these is a hard gap,
not a polish item:

| # | Gap | Where | Consequence |
|---|---|---|---|
| G1 | `transport.set_home` and `role.set_home` write only the **local** hotel's graph. `membrane_transport_home` is not in any gossip payload. | `ipc.rs` SetTransportHome; `heartbeat.rs` `HotelStateSyncPayload` | The target hotel never learns it is now home unless someone also writes the record there. Two hotels can disagree about who owns a token. |
| G2 | No mesh request exists for "materialize guest X with template Y on hotel B." `ensure_guest_active` only wakes a dormant row already seeded on the same hotel. | `guest_manager.rs` | A membrane, controller, or runner cannot appear on a hotel that was not deployed with it. |
| G3 | Guest launch definitions (`command`, `args`, `env`) are per-hotel `materialized_guests` rows seeded from that hotel's `mesh-config.json`, some with hardcoded binary paths. Nothing replicates them. | `graph.rs`, `guest_manager.rs` | Even with G2 solved, the target has nothing to launch from. |
| G4 | Secrets never cross the mesh, and the mesh is HMAC-signed, not encrypted. A membrane on hotel B needs the bot token in B's own vault. | `authz.rs`, `egress.rs`, membrane `fetch_bot_token` | A relocated membrane fails closed on the target unless the operator pre-provisioned the secret. |
| G5 | Checkpoint, dialogue window, and apartment live only in the current hotel's SQLite via `sync_apartment`. The `HandoffBundle` is a string briefing, not a state object. | `philote/runtime.rs`, `ipc.rs` `compose_session_snapshot` | A moved philote forgets its in-flight turn state and recent dialogue. |
| G6 | `seed_orchestrator_roles` hardcodes `home_node: None` on every boot; `upsert_guest` overwrites unconditionally. | `aiua/main.rs` | The origin hotel's next restart silently un-migrates the agent. Config truth beats graph truth. **DEF-106.** |
| G7 | The old poller learns it lost authority only when its next lease renewal fails, then re-probes every 180 s. No push, no drain. | `lease_handlers.rs`, membrane lib | A "seamless" move has a multi-minute deaf window and can drop an in-flight turn. **DEF-107.** |
| G8 | Placement scoring ignores whether the candidate hotel has the controller resource (ONNX model, Ollama, MLX), the secret refs, or headroom; `max_concurrent_jobs` is enforced only on tool fallback. | `ipc.rs` `best_place_to_run_view` | A placement can "win" a hotel that cannot actually run the component. |
| G9 | Both home tools gate on `has_operational_admin_authority` only. No target readiness check, no risk tier. | `graph.rs` | Moving a membrane (custody of an external identity) is treated the same as moving a research role. |
| G10 | Two structs named `MeshCatalogSyncPayload`: the canonical catalog one (`membership.rs`) is built only in a test; the only emitter and the wire handler use the peer-directory one (`heartbeat.rs`, `beacon.rs`). | `membership.rs`, `heartbeat.rs`, `beacon.rs` | Code-confirmed; live confirmation pending. **DEF-108.** If true, a hotel that never receives a handoff for an agent never learns its toolset. |

## Core Recommendation

Treat relocation as a **recorded, resumable ceremony** owned by the requesting
hotel, executed by the target hotel, and initiated by the philote through one
tool. Reuse every plane that already exists — route contract, TCP execution
plane, leases, component templates, binary resolution, vault refs — and add
only what is missing: a request/ready pair, a continuity blob, a push
stand-down, and graph truth that survives a reboot.

### Invariants

1. **Graph truth outlives config.** `mesh-config.json` and Ansible YAML are
   fill-only seeds. A runtime placement decision recorded in the graph must
   never be reverted by a restart.
2. **One authority per singleton resource.** Telegram tokens, desktop membranes,
   and agent sessions keep exactly one acting holder (`lease-at-resource-not-agent`).
   Materialization creates a candidate; readiness makes it routeable; the lease
   makes it allowed to act.
3. **Transport home is distinct from role home.** A relocation may move both,
   but as two recorded steps with their own rollback.
4. **The target owns spawn and admission.** The requester sends a request; it
   never spawns remotely by force.
5. **Readiness before release.** Parked work is released only after the target
   publishes ready; authority switches only after readiness.
6. **Secrets never travel in plaintext.** Only `secret_ref` crosses the mesh
   until a sealed channel exists (R7). Missing secrets are a loud decline, not a
   silent failure on first use.
7. **Every phase is recorded and idempotent.** A crashed ceremony resumes from
   its last recorded phase or rolls back to the origin; it never leaves two
   acting holders or zero.
8. **Decline is loud.** Every infeasibility names its reason to the philote.

### The ceremony

```
philote ──hotel.relocate──▶ origin hotel (A)
  1 INTENT      A records ceremony{id, manifest, phase=intent}
                manifest = components × {agent_identity, role_incarnations,
                toolset_profiles, component templates, transport homes,
                secret_refs, resource requirements, continuity refs}
  2 FEASIBILITY A asks candidate(s) (or the named target) → offer | decline(reason)
                checks: binary resolvable, secret_refs present, resources present,
                headroom, admission policy, version compatibility
  3 STANDBY     A → B  materialize.request (point-to-point, TCP execution plane)
                B upserts records, resolves binaries, spawns guests in STANDBY
                B → A  materialize.ready | materialize.failed(reason)
  4 CONTINUITY  A exports continuity blob (session snapshot, checkpoint, dialogue
                window, apartment) → B imports → B acks
  5 SWITCH      A drains in-flight turn (finish, or park + carry)
                A writes home_node / transport home = B and gossips the record
                A pushes TransportHomeChanged → A's poller stands down now
                B's standby acquires the lease → B is acting
  6 RECONCILE   A marks its local guests dormant (not deleted); fleet
                supervision on A must never resurrect them from seed
  7 CLOSE       ceremony phase=complete, or rollback to A at any failed step
```

Every phase writes the `relocation_ceremony` graph record and emits a
session_event, so the philote can report progress in the same conversation
that asked for the move.

### Authority and risk tiers

| Component class moved | Tier | Gate |
|---|---|---|
| Role incarnation (no external custody) | low | orchestrator/admin authority, as today |
| Model controller, tool runner, datasource | medium | plus feasibility offer from target |
| Membrane (external identity custody) | high | plus operator approval through the existing approval UX; "trust for session" may cover repeat moves of the same transport |

Risk tiers follow the DEF-103 shape: graduated, subset-aware, and never an
unconditional interrupt for a move the operator has already blessed.

### What stays hotel-bound

Some resources are the hotel: a laptop's desktop, its checkout, its ONNX and
MLX models, its iMessage database. Roles that need them are **pinned** by
`home_node` and reached by whisper. The ceremony must refuse to move a role
whose toolset needs a resource the target lacks (R4) rather than moving it into
uselessness.

## Slices

| Slice | Content | Closes | Rung |
|---|---|---|---|
| **R0** Tenet and rules | This proposal; `materialization-anywhere-anytime` and `graph-truth-outlives-config-seed` in `ARCH_RULES.md`; glossary terms | — | docs |
| **R1** Graph truth outlives seed | `seed_orchestrator_roles` preserves `home_node`; guest seeding preserves placement fields like it already preserves `is_active`; `home_node` and `membrane_transport_home` join the `HotelStateSync` gossip so every hotel agrees who is home | G1, G6 | test-green, then smoke: restart origin, home survives |
| **R2** Membrane standby and push stand-down | every membrane startup path exposes standby before acting; `TransportHomeChanged` mesh event; old poller stands down on receipt; standby acquires within one lease tick | G7 | watched-live: move Beacon's token vps ↔ mac and back with no dropped message |
| **R3** Remote materialization request/ready | `materialize.request` / `materialize.ready` on the TCP execution plane carrying component template refs, `agent_identity`, role and toolset records, transport-home standby; target resolves binaries (Guest Binary Resolution), admits, spawns, reports | G2, G3 | test-green on two local hotels; smoke mac ↔ mbp |
| **R4** Feasibility and placement | offer/decline with reasons: binary resolvable, secret refs present, controller resources present, `NodeHealthSnapshot` headroom, `max_concurrent_jobs`, version compatibility; `best_place_to_run` consumes the same checks | G8, G9 (readiness half) | test-green |
| **R5** Continuity transfer | export session snapshot + checkpoint + dialogue window + apartment as an authenticated blob; import on target before switch. Requires blob-plane auth and mesh-interface bind (DEF-104 follow-on). Until then the ceremony runs in a declared degraded mode carrying only the `HandoffBundle` | G5 | smoke: in-flight parked turn survives a move |
| **R6** `hotel.relocate` and the ceremony record | the philote-facing tool; `relocation_ceremony` graph record with phases; risk tiers; drain contract; rollback bounds; resume after crash | G9 (tier half), orchestration | watched-live: Bjork's orchestrator moves `mac-jane` → `vps-jane` from a Telegram turn and answers the next message from the VPS |
| **R7** Sealed secret transfer | AEAD-sealed `secret_ref` payload over the peer-authenticated channel (X25519 material already exists for HMAC key derivation), or a TLS execution plane; until then secrets are pre-provisioned through the vault plane and R4 declines otherwise | G4 | security review + smoke |
| **R8** Catalog sync truth | verify G10 live; if confirmed, wire the real `MeshCatalogSync` payload | G10 | smoke |

Order: R0 → R1 → R2 → R3 → R4 → R6 (degraded continuity) → R5 → R7. R8 runs
whenever G10 is verified. The orchestrator migration to `vps-jane` is the
watched-live gate of R6, not a separate project.

## Prerequisites Outside This Proposal

- **Blob plane auth and mesh-interface bind** before R5 (DEF-104 fixed the bind;
  auth is still absent). Until `vps-jane` is redeployed its execution and blob
  ports remain publicly reachable.
- **Ansible peer port drift** on `jane-vps` host vars (mac-jane listed at
  24849-24851, actual 16370-16371). A redeploy re-seeds peers every boot.
- **Target capacity** is unmeasured. `vps-jane` already hosts Beacon,
  life-graph-runner, Memgraph, the Muninn Cortex, philotic-web, and the ONNX
  sidecar.
- **Beacon's Telegram token** on `vps-jane` has been 401 since May; the same
  vault pass that lands new tokens should fix it.

## Recommended Validation Ladder

1. **test-green**: two in-process hotels, full ceremony for a role-only move,
   then a membrane move with a fake transport, then a declined move (missing
   secret, missing resource, no headroom).
2. **smoke-green**: `mac-jane` ↔ `mbp-jane`, role-only move, restart origin,
   confirm home survives (R1); membrane token flip with no dropped message (R2).
3. **watched-live-green**: Bjork's orchestrator and Telegram membrane relocate
   to `vps-jane` from a single Telegram request, Architect stays pinned on
   `mac-jane`, the next operator message is answered from the VPS, and a whisper
   to Architect round-trips. Then Coach and Mac, then the `mbp-jane` agents.

## Open Questions

- Should the continuity blob include Muninn-bound memory at all, or is Muninn
  (Cortex on `vps-jane`) already the durable half of continuity, leaving the
  blob to carry only turn-local state? Recommendation: turn-local only.
- Who arbitrates when two hotels both believe they are home after a partition?
  Recommendation: the ceremony record's `switch_at` with the lease as tiebreak,
  per `lease-at-resource-not-agent`.
- Is a ceremony a Fleet Supervision desired-state change, or does Fleet
  Supervision merely read the result? Recommendation: the ceremony writes
  desired state; supervision reconciles toward it and never toward the seed.
