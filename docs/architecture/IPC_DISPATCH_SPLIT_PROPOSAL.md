---
title: IPC Dispatch Split — Break service/ipc.rs Into Request-Family Modules
doc_type: proposal
domain: runtime-sessions
status: proposed
last_updated: 2026-10-05
tags:
- aiua
- ipc
- refactor
- maintainability
- hot-file
related_docs:
- CODEBASE_HEALTH.md
- MESH_DELIVERY_GUARANTEES_PROPOSAL.md
- PERIMETER_ENFORCEMENT_PROPOSAL.md
proposal_id: ipc-dispatch-split
implements:
- aiua
implemented_by: []
active_seams:
- ipc-module-skeleton
- ipc-pure-fn-extraction
- ipc-family-extraction
- ipc-park-path-unification
---

# IPC Dispatch Split — Break `service/ipc.rs` Into Request-Family Modules

Origin: Philotic Stack Atlas (2026-09-30), next seam #3. Facts re-checked
against `origin/develop` @ `f170cb88` on 2026-10-05.

## Problem

`crates/aiua/src/service/ipc.rs` is **36,564 lines**.

- **Production code (lines 1–19,089):**
  - a single `impl IpcServer` block of ~15,000 lines (1,482–16,503);
  - inside it, `process_request` (5,086–12,422), a 27-parameter associated fn
    whose one `match req` (5,115–12,421) has **173 arms**, exhaustive with no
    wildcard.
- **Tests (19,090–36,564):** 194 test functions.
- **Churn:** 57 commits have touched the file since 2026-08-01, 8 of them since
  09-20. It is on the `CLAUDE.md` hot-file list and causes most merge
  collisions.
- **Precedent:** `CODEBASE_HEALTH.md:60-71,125` already proposed "split by
  concern" when the file was 4,911 lines. Five extractions in July 2026
  (`operator_surface`, `golgi`, `lease_handlers`, `role_materialization`,
  `session_activity`) proved the pattern and kept tests green
  (role_materialization: 231/231 before and after).

**Hidden debt found while planning.**

- The Atlas cited "duplicated `park_and_materialize`" and a "cron
  double-delivery race". Both were fixed on 2026-07-04 (`5a042bab`,
  `a3e094b7`), and `docs/DEFECTS.md:465` is stale.
- There are still three other park paths that drifted:

| # | Where | Problem |
|---|---|---|
| P-a | `ParacrineEmit` arm, `ipc.rs` ~9,560–9,735 | Inline cross-hotel park, seed and materialize. Its seeded env sets only `PHILOTIC_AGENT_ID`, `PHILOTIC_ROLE_NAME`, `PHILOTIC_HOTEL_SOCKET` and `PHILOTIC_NODE_ID`. It **omits `PHILOTIC_GUEST_ID`, `PHILOTIC_ROLE_INBOX` and `PHILOTIC_HOTEL_NAME`**, which `role_materialization.rs:136-142` says are required: without them a philote "could materialize into a role but never hand back out". This is a likely live bug. |
| P-b | `EmitTask` `AgentRouteResolution::Park` branch, 6,942–6,968 | A fourth inline park plus `ensure_guest_active`. |
| P-c | `CronTicker::fire`, `cron_ticker.rs:560-568` | Ignores the `bool` returned by `deliver_inbound_task`. The event is already claimed, so the mesh consumer can't rescue a task with no subscriber. |
| P-d | `ipc.rs:12,871–12,939` vs `cron_ticker.rs:728–797` | Duplicated cron sync envelope builders (also see DEF-184 in `MESH_DELIVERY_GUARANTEES_PROPOSAL.md` L6). |

## Goal

1. `ipc.rs` becomes a module directory. `mod.rs` holds types, `IpcServer`,
   `run`, `handle_client` and a dispatch `match` of one- to five-line arms,
   about 2,000 lines.
2. Each request family lives in its own file of at most ~1,500 lines, with its
   tests beside it.
3. Every step is a **verbatim move**: no behavior change and an equal test
   count, each step in its own PR.
4. Behavior fixes (P-a…P-d) land as **separate** small PRs after the code
   they touch has moved.

### Non-goals

- Changing the `IpcRequest` wire enum or splitting it per family in
  `philotic-client`. That is a later option.
- Making `handle_client` process requests concurrently.
- Renaming handlers or redesigning `process_request` parameters, beyond the
  optional additive context structs in S0.

## Target layout

```
crates/aiua/src/service/ipc/
  mod.rs               ~2,000  types, IpcServer, run, handle_client, dispatch match
  tests.rs             fixtures only (path crate::service::ipc::tests::* preserved)
  delivery.rs          ~1,000  add/remove_subscriptions, deliver_inbound_task, repark, rescue
  agent_context.rs     ~1,050  placement/reflex/agent-graph helpers (256–1,310)
  tool_assembly.rs     ~1,200  compose_tool_assembly, select_component_route … (16,505–17,705)
  session_snapshot.rs    ~750  compose_session_snapshot, mesh registry snapshot
  emit.rs              ~1,350  EmitTask, DelegateToPeer, DelegateToExternalPeer, ParacrineEmit
  tasks.rs               ~450  Create/Update/Complete/FailTask, QueryStatus/Timeline, user tasks
  desktop_membrane.rs  ~1,480  desktop membrane + operator views (1,581–2,876)
  placement.rs           ~370  handoff, role/transport homes, ConfigureRole, incarnations
  remote_handoff.rs      ~690  remote handoff / materialize / continuity (15,347–16,033)
  subagents.rs           ~430  Spawn…AcceptSubagentLease, AbortSubagentSpawn, ExecuteWorkflow
  agent_bundle.rs        ~780  bundles, user profile, AgentMigrateToHotel
  skills.rs              ~900  skill registry, toolset profiles
  procedures.rs          ~850  procedure registry, patches, trials
  surfaces.rs            ~150  A2UI surfaces (S1a)
  routing_policy.rs      ~650  graph instances, rules, reflex, routing policy
  components.rs          ~650  resources, components inventory/restart/remove
  cron.rs                ~300  cron arms + access control (then dedupe vs cron_ticker)
  hotel_status.rs        ~480  hotel status, memory report, cortex read, best place, logs
  mcp_endpoint.rs        ~900  routes, endpoints, token grants, owner check
  mcp_upstream.rs        ~700  upstream registry, catalog, credentials
  integrations.rs        ~800  integration bindings, credentials, audit, OIDC exchange
  heal_autonomy.rs     ~1,060  heal intake, work items, autonomy, model route
  config_vault.rs        ~450  Get/SetConfig, GetSecret, RotateSecret, AddVaultEntry
  mesh_invite.rs         ~350  mesh identity, keys, invites
  memory.rs              ~450  memory-token heal, Muninn probe, delta digest (pre-dispatch)
  media_setup.rs         ~470  training, ASR, vision (pre-dispatch)
```

The lease arms join the existing `service/lease_handlers.rs`.

## Rules for every extraction PR

1. **Verbatim body move.** The arm stays in `mod.rs` as a one-line delegate,
   `Self::handle_x(...)` with the same arguments. Review with
   `git diff --color-moved=dimmed-zebra`. Only visibility bumps are allowed
   beyond the move.
2. **Pattern.** The new file has `use super::*` (or explicit imports) and
   `impl IpcServer { pub(super) async fn handle_x(...) }`. Child modules of
   `ipc` see its private items, so few `pub(super)` bumps are needed.
3. **Identity.** Handlers take `current_identity: Option<&GuestIdentity>` and
   never re-derive identity from payloads.
4. **Response coupling.** If a handler returns `ComponentRegistered`,
   `RoleHomeSet` or `TransportHomeSet`, the post-processing in `handle_client`
   (3,583–3,638) still sees the same variant. Add a test asserting the dirty
   flag or broadcast when such a family moves.
5. **No locks across `.await`.** About 46 lock sites are clean today. Add
   `#![deny(clippy::await_holding_lock)]` to `aiua` in S0 so a move can't
   regress this.
6. **Tests move with the family**, as `role_materialization` did. Shared
   fixtures stay in `ipc/tests.rs` because other modules import them:
   - fixtures: `TestGraphAdapter`, `MockMaterializationRequester`,
     `test_socket_path`, `ipc_env_guard`, `VaultKeyEnv`, `expect_config_data`,
     `make_hotel_graph`;
   - importers: `role_materialization.rs`, `lease_handlers.rs`,
     `session_activity.rs`, `main.rs`.
   - Keep `test_dispatcher_channel` (line 241) reachable for `cron_ticker`.
7. **The PR body states** `cargo test -p aiua` before and after (equal count),
   plus `just check`.
8. **Before opening:** run `just workstream-overlap <slug>`. Land a family only
   when no open branch touches it. Merge the same day to keep the rebase
   window short. As of 10-05, open PRs touching `ipc.rs` are #381
   (`codex/muninn-heal-queue-recovery`) and #354
   (`codex/data-driven-tool-grants`), both stale since late July: ask their
   owners to rebase or close.

## Slices

### S0 — Skeleton (S)

- `git mv service/ipc.rs service/ipc/mod.rs`.
- Move `#[cfg(test)] mod tests` to `ipc/tests.rs` as `mod tests;`.
- Delete the orphaned doc comment at 12,423–12,433, which describes
  FileHealWorkItem but sits above `handle_query_model_route`.
- Add `#![deny(clippy::await_holding_lock)]` to the aiua crate root.
- Optional and additive: `struct RequestCtx<'a>` holding the shared `Arc`
  handles and `&GraphDomain`, plus `struct ConnCtx<'a>` for the `&mut`
  per-connection state. Build both at the top of `process_request`; only new
  handler signatures use them.
- **Done when:** external paths compile unchanged (`main.rs`, `cron_ticker.rs`,
  `mesh_runtime.rs`, `golgi.rs`, `webrtc_signaling.rs` and the four sibling
  modules) and the test count is equal.

### S1 — Pure free functions (S, about 4 PRs)

These have no `IpcServer` state, so the move risk is lowest.

- `tool_assembly.rs` ← 16,505–17,705
- `skills.rs`, `procedures.rs`, `surfaces.rs` ← the free functions at
  17,803–19,071 (the arms follow in S3)
- `cron.rs` ← cron ACL functions 17,706–17,802
- `agent_context.rs` ← 256–1,310

**Tests:** about 27 direct calls into private functions need `pub(super)` or
must move with the code.

### S2 — Small leaf families (S, about 6 PRs)

`surfaces`, `cron` (arms only), `hotel_status`, `media_setup` and `memory`
(both pre-dispatch, from `handle_client` 3,282–3,550), and `components`.

For the pre-dispatch families, keep the early-return interception in
`handle_client`. Only the bodies move. The "UNREACHABLE" stub arms in the main
match (8,448–8,487) stay.

### S3 — Self-contained mid-size families (M, about 14 PRs)

`heal_autonomy`, `routing_policy`, `integrations`, `mcp_endpoint`,
`mcp_upstream`, `config_vault`, `mesh_invite`, `session_snapshot`,
`agent_bundle`, `remote_handoff`, `skills` (arms), `procedures` (arms),
`subagents`, and the lease arms (into `lease_handlers.rs`).

**Coordinate with `PERIMETER_ENFORCEMENT_PROPOSAL.md`:** its IPC-identity
slices edit `config_vault`, `mcp_endpoint` (`mcp_owner_identity_ok`) and
Register. Do those families' moves **before** that work starts, or after it
merges.

### S4 — Operator-surface coupled families (M, 2 PRs)

`desktop_membrane` (1,581–2,876 plus arms 5,974–6,149) and `placement`.
`TransportHomeSet` drives a broadcast in `handle_client`, so add a test that
asserts the broadcast first, then move.

### S5 — Highest-risk core (M–L, 3 PRs)

1. `tasks.rs`.
2. `emit.rs`: the EmitTask arm (716 lines) and ParacrineEmit (393 lines).
   **Coordinate with `MESH_DELIVERY_GUARANTEES_PROPOSAL.md` L5**, which stamps
   `expires_at` in EmitTask: never run them concurrently.
3. `delivery.rs`. Bump the internals of `CountedSender` and `RoleSubscriber`
   to `pub(super)`, and keep the `#[cfg(test)]` timing constants (lines
   106–129, 224–226) visible. The spawned write-confirm watcher (3,997) moves
   with `deliver_inbound_task`.
4. `Register` and `SubscribeInbox` stay in `mod.rs`.

### S6 — Behavior fixes (S each, separate PRs after S5)

1. **P-a:** route ParacrineEmit's cross-hotel park through
   `IpcServer::park_and_materialize(…, ParkTarget::CrossHotelGuest)`
   (`role_materialization.rs:45`). The env gap is fixed as a side effect.
   - Test: the seeded `config_json` env contains `PHILOTIC_GUEST_ID`,
     `PHILOTIC_ROLE_INBOX` and `PHILOTIC_HOTEL_NAME`.
   - Live check: a paracrine whisper to a sleeping role on another hotel
     materializes it, and it hands back out (`just smoke-paracrine`).
2. **P-b:** EmitTask's `Park` branch goes through `ParkTarget` too.
3. **P-c:** `CronTicker::fire` acts on a `false` from `deliver_inbound_task`:
   park and materialize when a role record exists; otherwise push heal tag
   `cron_fire_unserved` and release the claim.
4. **P-d:** one cron sync envelope builder, or deletion of both if DEF-184
   option (a) is chosen.
5. **Doc hygiene:** close the stale `DEFECTS.md:465` tech-debt row, and
   refresh `CODEBASE_HEALTH.md` (line counts, test counts, unwrap density:
   4 in production code today).

## Ordering

```
S0 ─► S1 ─► S2 ─► S3 (family by family) ─► S4 ─► S5 ─► S6
```

The family PRs within S1–S3 can go in any order. One family per PR, and at
most two extraction PRs in flight at once, so rebases stay trivial.

## Verification

This is a refactor, so the ladder is TEST-GREEN per PR plus one binary smoke
after S5:

- `just smoke-suite`, `smoke-routed-tool`, `smoke-approval`, `smoke-subagent`,
  `smoke-paracrine` on an isolated hotel profile;
- then a normal fleet deploy, watched with `runtime-rollout-watch`.

S6 fixes each get their own smoke.

## Risks

- **Hidden private coupling.** About 27 tests call private handlers directly.
  The compiler catches all of it, so the cost is churn, not breakage.
- **Long-lived branches.** Any branch touching `ipc.rs` rebases into a moved
  file. Bodies are byte-identical, so re-applying a hunk in the new file is
  mechanical, and `git log --follow` keeps blame.
- **Scope creep.** The rule is "move, don't fix". Every fix found during a move
  goes in a follow-up PR or a DEFECTS row.

## Definition of done

- `ipc/mod.rs` is under ~2,500 lines, and no `ipc/*.rs` file exceeds ~1,500
  lines of production code.
- The aiua test count is unchanged or higher.
- P-a through P-d are fixed, with tests.
- `CODEBASE_HEALTH.md` and `DEFECTS.md` are updated.
- The `CLAUDE.md` hot-file entry changes from `ipc.rs` to `ipc/mod.rs`.

## Operator decisions needed

1. Approve the directory layout (`service/ipc/`) over flat siblings
   (`service/ipc_*.rs`).
2. Close or rebase the stale PRs #381 and #354 before S3.
