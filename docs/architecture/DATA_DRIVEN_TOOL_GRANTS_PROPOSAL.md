---
title: Data-Driven Tool Grants (SkillDAG)
doc_type: proposal
domain: tooling-execution
status: accepted for current slice
last_updated: 2026-07-24
tags:
- tooling
- grants
- skilldag
- autopoiesis
---

# proposal:data-driven-tool-grants-skilldag

Filed 2026-07-14 (handoff PR #282, architecture item). Operator-flagged
principle from that session: **"we should not have any tool hard coded."**
Motivating case: the inability that night to disable one tool
(`life.observe.batch`) without a code change and deploy.

## Goal

Grants become graph/config data — seeded once, editable at runtime with
**no deploy**. The hardcoded lists demote to a first-boot seed + fallback.

## Problem

Tool grants were compiled into the binaries across five surfaces, and the
effective grant is their **union**. That union is why removing a tool from any
one list did nothing: the others re-added it on the next projection.

1. `skill_implied_tools` in philote `catalog.rs`
2. `tools_for_allowed_class` in aiua `ipc.rs` (delegating to the shared
   `tools_for_tool_class` map in `ansible-mesh-core`)
3. seeded `implied_tools` in aiua `main.rs`
4. runner `supported_tools` in aiua `main.rs`
5. `tool_catalog()` class tags in philote

Underneath all five sat a quieter defect: `upsert_abstract_skill` is a blind
whole-record overwrite and the skill seeder runs on **every** boot from two
call sites. Editing `implied_tools` in the DB therefore worked live and was
silently reverted on the next restart. The grants looked data-driven and were
not — any registry built on top of that store would have reset on boot.

## SkillDAG decision

Keep the **authoritative, hot-path grants in the LOCAL hotel context
graph** (fast, always-available, per-hotel). Do NOT put runtime tool
resolution behind the remote LifeGraph (Memgraph on vps-jane), or every
agent bricks when it is down — that session's failure mode. The LifeGraph
is only an **optional reasoning/design layer**: the agent proposes changes
against it, and those changes **compile down** to the local toolset
(compiler pattern; autopoiesis fit).

## Slices

1. **Grant registry in the context graph** — *implemented, smoke-green*.
2. **Runner routing as data** — *implemented, smoke-green*.
3. **Governance/audit** — *implemented, smoke-green*.
4. **Later** — SkillDAG reflection in the LifeGraph. Still deferred; see
   [Slice 4](#slice-4--why-it-stays-deferred).

## Slice 1 — what shipped

**Two layers, deliberately not conflated** (AGENTS.md §2.1, §5.3.2):

- **Grant layer** — what a class or skill is allowed to project.
- **Policy layer** — `disabled_tools`, applied *last*, after every grant
  source has had its say. Never seeded from code.

A deny list alone would have demoted nothing — it would be a kill switch
bolted on top of five live authorities, and the proposal's own goal is that
the hardcoded lists become seed plus fallback. Both layers ship together.

### Record

`ToolGrantRegistryRecord` — node kind `tool_grant_registry`, key
`tool_grant_registry:default`, in the **local** hotel context graph.
Holds `class_grants` (per-class tools with provenance) and `disabled_tools`.

`GrantSource` (`seed` | `runtime`) marks who owns a grant's current value.
Runtime edits are sticky — the boot seeder preserves them. Seed-owned entries
still refresh from the compiled-in catalog, so newly shipped built-in tools
reach existing installs on upgrade.

### Resolution order

1. aiua composes session bindings and resolves grants **once per snapshot**
   against the local graph, publishing `resolved_class_grants`,
   `resolved_skill_grants`, and `disabled_tools` onto the bindings.
2. philote prefers those resolved grants and falls back to its compiled-in
   tables only for skills/classes the hotel did not resolve.
3. The disabled-tool policy is applied at the single choke point where tools
   reach the model (`project_tools_for_turn`), so a new branch in the
   projection cannot accidentally reintroduce a disabled tool.

Absent vs. empty is a real distinction: a class **absent** from the registry
falls back to the built-in table; a class **present but empty** grants
nothing. Falling back on empty would re-grant exactly what an operator just
removed.

An unreadable registry logs a warning and falls back to the built-in seed
rather than failing closed — a corrupt record must not strip every agent's
tools — but that means runtime grants are not honored until it is repaired.

### Operator surface

`phil tools show | disable | enable | set-class | set-skill`, operating
directly on the hotel context DB. Every write stamps `runtime`.

Deliberately **not** an agent-facing tool. Who may widen their own reach and
under what audit is a governance question, carved out as slice 3 — and adding
the tool now would mean granting it through the very tables this slice is
demoting.

## Slice 2 — runner routing as data

A remote runner's `supported_tools` and the model's class grant were two
independently maintained copies of one tool set. Drifting them apart is not
cosmetic: a tool granted to the model with no matching runner route produces a
turn that **hangs until the watchdog evicts it** — the failure the PR #277
reconcile was written to patch after the fact.

`ToolRunnerGrant` binds a runner role to the tool class that defines what it
serves (`life-graph-runner` → `life_graph`), and the runner's route list is
derived from that class grant at seed time. One authority, so the two lists
cannot drift. `phil tools set-runner` re-points a runner at a different class.

## Slice 3 — governance/audit

Making grants runtime-editable removed the deploy from the loop — and with it
the git history that used to answer *who changed what, and when*.
`ToolGrantAuditRecord` (node kind `tool_grant_audit`) puts that answer back
without putting the deploy back.

- Append-only, written **before** the mutation: a change that cannot be
  audited does not land (fail closed), mirroring `SkillRegistrationAuditRecord`.
- Records action, target, before → after, and actor (the invoking OS user, or
  `PHILOTIC_GRANT_ACTOR`).
- Ordering is assigned by the store, not the caller. Second-resolution
  timestamps tie constantly — a scripted operator run produces several changes
  in one second — so a monotonic `sequence` decides replay order.
- `phil tools audit` reads the trail.

**The agent-facing grant surface stays out.** Letting a model widen its own
reach is the one genuinely dangerous move in this proposal. The design answer
is already written down in the SkillDAG decision: the agent *proposes* against
the LifeGraph and the change **compiles down** into this registry. That is
slice 4, and it is gated on an operator approval step — not on handing agents a
mutation tool. Shipping the audit trail first is what makes such a step
reviewable when it arrives.

## Slice 4 — why it stays deferred

Not a scope cut for convenience; it does not have a green path today:

- It depends on the remote LifeGraph (Memgraph on vps-jane), which the SkillDAG
  decision deliberately keeps *off* the hot path. Slice 4 is the reasoning
  layer, so it needs that dependency to be healthy — and the LifeGraph has
  outstanding durability work of its own.
- Its value is the compile-down + approval loop, which needs the operator
  approval UX to exist. Slices 1–3 are the substrate that loop compiles into;
  they are now in place and proven, which is the honest prerequisite.

Building it now would mean inventing an approval surface with no operator in
the loop to validate it against.

### Verification

`smoke-green`. `just smoke-tool-grants` boots an ephemeral hotel with a real
philote guest and asserts every step against session snapshots read back over
real IPC:

1. the registry seeds from the built-ins at boot
2. a bound tool appears in the composed snapshot
3. `phil tools disable` removes it — **no rebuild, no restart**
4. a hotel restart does **not** revert the disable
5. `phil tools enable` restores it
6. the runner is bound to its class grant (slice 2)
7. both changes appear in the audit trail with an actor (slice 3)

Plus 1206 unit/reconciliation tests across `ansible-mesh-core`, `aiua`, and
`philote`, covering restart survival, seed refresh for untouched skills, the
projection path, dispatch rejection, and audit ordering under tied timestamps.

**Fleet rollout is still unproven** — this ran against ephemeral hotels, not
mac-jane/mbp-jane/vps-jane. No installed binary was replaced (AGENTS.md
§5.3.1). That is a deploy step, not a correctness gap.

## Transitional debt (named, per AGENTS.md §2.3)

- **`tools_for_skill`** — philote's on-demand *owned*-tools table is still
  compiled in. The implied-tools lane, the class lane, and the runner lane are
  data; this lane is not. `disabled_tools` covers it for revocation, but not
  for granting. Next seam.
- **Catalog class tags** — `tool_catalog()` tags remain a third grant source.
  They are now skipped for any class the registry resolved explicitly, which
  is lossless today (the catalog's `life_graph`-tagged tools are exactly the
  seeded ones), but the tag and the registry are still two places describing
  one thing.
- **Runner route refresh** — a `set-runner` or class change reaches the
  runner's route list when the hotel next seeds toolset profiles (boot), not
  instantly. The policy layer still revokes immediately.
- **Seed drift** — once a skill's grants are runtime-owned, newly shipped
  built-in tools no longer reach it automatically. That is the deliberate
  price of runtime authority. `phil tools show` marks each grant `seed` or
  `runtime` so the drift is at least visible; a diff against the current
  built-ins is not built.
- **Session refresh** — a disable reaches a session when its bindings are next
  recomposed, not instantly for an in-flight one. Dispatch is gated on the same
  bindings, so a stale session is consistent with itself rather than half-shut.

## Precedent

The PR #277 runner-routing reconcile is cited in the handoff as a
precedent for slice 2, and DEF-057's reconciling seeder
(`ToolsetProfileRecord::reconcile_seed_with_existing`, found in the
2026-07-19 role/tool deep-dive audit, fixed via codex/role-admin-hardening)
proved runtime DB changes can survive restarts. Slice 1's skill and registry
seeders follow that same shape.
