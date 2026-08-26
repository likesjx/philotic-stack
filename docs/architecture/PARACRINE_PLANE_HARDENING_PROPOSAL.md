---
title: Paracrine Plane Hardening — Reliable Specialist Delegation End-to-End
doc_type: proposal
domain: agent-loop
status: implemented
disposition: implemented
last_updated: 2026-08-25
tags:
  - paracrine
  - delegation
  - whisper
  - role-incarnation
  - materialization
  - hardening
proposal_id: paracrine-plane-hardening
implements: []
implemented_by:
  - PR #443 (DEF-085)
  - PR #444 (DEF-086)
  - codex/paracrine-hardening-s2 (S2-S6)
active_seams:
  - paracrine-response-return-route
  - single-flight-materialization
  - park-ttl
  - dormancy-vs-operator-intent
---

# Paracrine Plane Hardening

**Why now:** the 2026-08-25 live audit (triggered by "Beacon got stuck again") found the
paracrine delegation plane structurally unable to work — every failure silent, every
symptom a 660s deaf turn. Whisper volume is tiny precisely *because* whispers have been
dying since the role plane shipped; the fleet's specialists were unreachable.

## Failure architecture found (live evidence, vps 2026-08-25)

| # | Failure | Evidence | Status |
|---|---|---|---|
| 1 | Hotel returned **success for undeliverable whispers** — role missing, lookup error, and a materialization failure it detected 1ms after parking | `Role-philote [vps-jane:philote-Chronos] could not be materialized` → `ok:true`; repro: bogus role → `ok:true` | **FIXED** #443 (DEF-085) |
| 2 | Philote checked only transport Ok — a refusal would have been discarded | code path; #441's class on the paracrine seam | **FIXED** #443 |
| 3 | Whisper past 660s **evicted the whole turn** with a generic apology | 17:29 incident timeline | **FIXED** #443 (deadline degrades to `SPECIALIST_TIMEOUT` tool_result; turn continues) |
| 4 | **Every hotel boot deactivated every dynamic role-philote** (`deactivate_legacy_managed_guests` matches `{hotel}:philote-*` + `{agent}:{role}` records); materializer honors `is_active=0` → one restart permanently kills delegation to that role | Chronos guest `is_active=0`; both records matched the sweep filters | **FIXED** #444 (DEF-086) |
| 5 | **Specialist lens never engaged**: role-philote fresh sessions auto-activated the agent default role (`orchestrator`), ignoring `PHILOTIC_ROLE_NAME` | `Auto-activated default role on fresh session. session_id=paracrine:Chronos role=orchestrator`; reply ran as plain Beacon-orchestrator | **FIXED** #444 |
| 6 | **Responses die at the return-route gate**: `EmitTask rejected unresolved response return route action="paracrine_response"` — the specialist's answer is dropped hotel-side, nothing tells the waiting caller. Same gate ate cross-hotel `datasource_response` returns for mac (15:14, 16:24) | vps file log | **FIXED** S2 (this slice) |
| 7 | **Duplicate materialization**: supervisor tick + on-demand `ensure_guest_active` spawned two `philote-Chronos` in the same second → 2 inbox subscribers → duplicate turns → LWW apartment checkpoint clobber (`Dropped stale active turn on checkpoint restore`) discarded the in-flight model response | 2 pids @ 18:20:12; delivery "to 2 local subscribers" | **MITIGATED** S3 (subscriber dedupe) |
| 8 | **Stale parks fire on materialization**: the 17:29 parked whisper was still parked at 18:20 and flushed alongside the new one; parks can outlive the caller by days | 3 task deliveries for 1 whisper | **FIXED** S4 (park TTL 720s, expired parks close their ledger rows) |
| 9 | **Dormancy is indistinguishable from operator deactivation**: role-TTL expiry (guest_manager supervisor) writes the same `is_active=0` bit; `ensure_guest_active` refuses both | code path | **FIXED** S5 (ttl_dormant marker; wake-on-demand) |

Diagnostic trap recorded: hotel-core `aiua::` logs go to `PHILOTIC_LOG_DIR` files (vps:
`/opt/philotic/data/logs/`), **not** journald — journald carries only guest-side targets.

## Remaining slices

### S2 — Response-path integrity — IMPLEMENTED
Remote-targeted response-like tasks now FORWARD to their home hotel (local
subscriber inference is meaningless for them — rejecting locally is what ate
mac's cross-hotel `datasource_response` returns, DEF-087); local unresolved
routes still reject. Both philote response emitters (delegate.merge and the
turn-completion auto-emit) now bind the hotel's refusal: warn + heal events
`paracrine_response_rejected` / `reply_emit_rejected` instead of silence.

#### Original statement
A rejected `paracrine_response` must not vanish: either re-resolve the route (the caller
is findable — `reply_to_guest_id` names it; the session graph knows the session's serving
guest) or bounce a failure back to the *specialist's* hotel-side thread AND close the
caller's whisper (the caller's philote can then fail the pending tool_result immediately
instead of waiting out the deadline). Apply the same treatment to cross-hotel
`datasource_response` rejections (mac life.* returns are dying at this gate today).

### S3 — Subscriber hygiene — IMPLEMENTED (dedupe half)
`add_subscription` now REPLACES an older subscription for the same guest_id
(newest connection wins) — double-delivery is impossible regardless of what
raced. Spawn-path single-flight was already lock-guarded within one
GuestManager; if duplicate spawns recur, audit for a second manager instance.

#### Original statement
Per-guest single-flight across supervisor and on-demand spawn paths (one spawn, others
await). Inbox registry: dedupe subscribers by `guest_id` (newest wins) so a raced double
spawn cannot double-deliver; the loser should exit on lease conflict.

### S4 — Park TTL — IMPLEMENTED
`ParkedInboundTask.parked_at` + `PARKED_TASK_TTL_SECS = 720`; expired parks
are dropped at flush with their ledger rows closed (`PARKED_TASK_EXPIRED`).

#### Original statement
A parked task older than `PARACRINE_WHISPER_WAIT_SECS` is dead on arrival — the caller
has already been timed out. Drop it at flush time (and close its ledger row with
`QUEUED_MESSAGE_EXPIRED`-style attribution) instead of delivering a stale prompt to a
freshly-woken specialist.

### S5 — Dormancy vs operator intent — IMPLEMENTED (default policy: whispers wake dormant roles)
The role-TTL sweep now writes a `guest_dormancy:{guest_id}` config marker;
`ensure_guest_active` wakes marked guests (reactivate + clear marker + spawn)
and still refuses operator deactivations (no marker). Operator can veto by
plain-deactivating a role guest.

#### Original statement
Role-TTL dormancy must remain wakeable: give TTL expiry a distinct state (e.g.
`deactivation_reason: ttl_dormant` vs `operator`) and let `ensure_guest_active` (and the
whisper path) wake `ttl_dormant` guests while continuing to honor operator deactivation.
Decision needed from operator: default wake policy for whispers.

### S6 — Verification — IMPLEMENTED
`just smoke-paracrine <socket> [role]` (scripts/smoke-paracrine.py): bogus
role → SPECIALIST_UNAVAILABLE refusal; real role → accepted.

#### Original statement
Add a `just smoke-paracrine` (whisper to a live role, a dormant role, a bogus role —
assert sub-second refusals, lens-correct replies, single spawn). Watched-live gate: a
real Beacon → Chronos scheduling request completing round-trip with the Chronos lens.

## Verification state
- #443 + #444: test-green (aiua sweep + refusal tests, philote 508), deployed mac-jane +
  vps 2026-08-25, refusal + happy-path materialization proven live on vps.
- vps restart survival of role-philote activation: **watch gate for #444** (next restart).
