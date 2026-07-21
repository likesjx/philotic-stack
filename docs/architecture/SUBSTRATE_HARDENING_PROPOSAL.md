---
title: Substrate Hardening — The Ground Under Autonomy
doc_type: proposal
domain: operator-control-plane
status: active
disposition: proposed
last_updated: 2026-07-20
verification_level: test-green
tags:
- substrate
- supervision
- self-healing
- verification
- chaos-testing
- autopoiesis-prerequisite
related_docs:
- AUTOPOIESIS_PROPOSAL.md
- MEMORY_TRANSPARENCY_PROPOSAL.md
- ARCH_RULES.md
task_refs:
- docs/task.md
---

# Substrate Hardening — The Ground Under Autonomy

> Autonomy compounds whatever it sits on. Granting more autonomy on a fragile
> substrate makes the system less reliable, not more.

## Goal

The autopoiesis epic ([AUTOPOIESIS_PROPOSAL.md](AUTOPOIESIS_PROPOSAL.md)) has
shipped its first five slices — the loops are built. But three facts from
2026-07-10 show what they're standing on:

- **mbp-jane was down**, and it is nohup-launched, not supervised. The hotel
  hosting most of the fleet's agents has no KeepAlive.
- **The heal-dispatcher itself has a recurring-restart instability on
  mbp-jane** — the healer needs healing and nothing heals it.
- **Nearly every proposal in the intel graph reads `verified: none`** — the
  system's ledger of its own work cannot answer "what is green right now?"
  without a human remembering.

This proposal is the explicit prerequisite gate for autopoiesis A6
(`scheduled-slice-executor`) and A10 (`fleet.canary_deploy`): no lane promotes
past ConfirmFirst until S1–S3 are live.

## Core Recommendation

Four slices, in dependency order. Each one converts a currently-implicit
operational assumption ("someone will notice") into an enforced invariant.

## Slices

| Slice | Content | Size | Verify |
|---|---|---|---|
| S1 `supervision-invariant` | Every hotel under a real supervisor, no exceptions: mbp-jane moved from nohup to launchd with KeepAlive (seam:mbp-jane-launchd-hardening); plists standardized across mac-jane/mbp-jane (env vars, log paths); vps-jane systemd confirmed equivalent. Includes the two known supervision debts: newsyslog drop-in for unbounded `aiua.log` growth on both Macs, and the router-listener startup crash (missing `router_listener.config` key) fixed so a clean boot is actually clean. | M | watched-live (kill -9 each hotel daemon; supervisor restores it; logs rotate) |
| S2 `heal-the-healer` | The heal circuit must be able to heal its own dispatcher. Doctor already reads the dispatcher heartbeat (#214); close the loop: stale heartbeat → automatic dispatcher restart with its own restart budget → budget exhaustion escalates to operator (throttled path from the F1–F10 hardening). Root-cause the mbp-jane recurring-restart instability as part of this slice — the watchdog is not a substitute for the diagnosis. Failure mode contract: the self-heal system may be degraded, but never *silently absent*. | M | smoke-green (induced dispatcher hang → auto-restart → escalation on repeated hang) |
| S3 `verification-as-data` | Wire `graph_record_test_run` into `just test` / CI so every slice stamps machine-readable pass/fail/coverage onto its proposal (`just test-and-record` exists; make it the default path, not the optional one). Add a doctor/status view answering "what is green right now?" from recorded runs, not prose. This is the evidence stream the autopoiesis A9 trust ledger and A10 canary judgments consume. | S–M | test-green + digest shows non-`verified:none` on active proposals |
| S4 `chaos-smokes` | Failure paths rot unless exercised. A scheduled (cron) chaos smoke on a designated hotel: kill a guest, drop a mesh peer, corrupt a low-stakes config key — assert the heal queue classifies and resolves each within budget, and file a proposal (A3 pattern) when it doesn't. Frequency low (weekly), scope bounded, kill switch standard. | M | **First slice landed** (`codex/substrate-s4-chaos-smokes`) — script + shellcheck + `--dry-run` only; the first real watched run is an operator drill, not this slice's own verification |

Dependency: S1 → S2 (a dispatcher watchdog under nohup is building on sand);
S3 independent; S4 after S1–S2 (chaos against an unsupervised fleet is just
outage generation).

## Standing Rules

1. **Supervision invariant:** a hotel process not under launchd/systemd
   KeepAlive is a defect, not a deployment style. Doctor flags it.
2. **Never silently absent:** every self-management component (dispatcher,
   doctor, watchers) has a heartbeat someone else reads, and heartbeat
   staleness always produces either a restart or an escalation.
3. **Green is a query:** "is X verified?" is answered from recorded test/smoke
   runs in the graph, never from memory or prose.

## Disposition

`proposed` — authored 2026-07-11 from the autopoiesis roadmap assessment.
S1 is the single highest-leverage item and should be the first slice claimed.

### Slice status

- **S1** `supervision-invariant` — not started.
- **S2** `heal-the-healer` — landed 2026-07-11 (`codex/substrate-s2-heal-the-healer`):
  `GuestManager::check_heal_dispatcher_heartbeat` watchdog closes the loop
  doctor's `heal.dispatcher-staleness` check only displayed (stale heartbeat
  on a PID-alive dispatcher → heal-restart through the shared `RespawnBudget`
  / existing throttled escalation, never a parallel budget); `phil doctor`
  gained `supervision.not-supervised` (the small S1 leftover). Root-cause
  pass on the mbp-jane "recurring-restart instability": it was a
  **misdiagnosis at the guest level** — mbp-jane's `aiua.err.log` shows
  ~9.6k `Error: Hotel 'mbp-jane' is already running with PID <N>` collisions
  from a still-unidentified duplicate launcher (no crontab, no matching
  `launchctl` job — consistent with S1 finding mbp-jane unsupervised), not
  the dispatcher crash-looping independently; every collision attempt
  re-logs the full guest roster (including heal-dispatcher) before the PID
  guard bails, which reads as dispatcher churn from outside. Tracked as
  DEF-046 (open) — needs S1 (launchd supervision) plus identifying what
  issues the duplicate launches. Verified: test-green (targeted unit +
  integration tests); no watched-live-green this slice (no live hotel to
  induce a real hang against).
- **S3** `verification-as-data` — landed (PR #248,
  `codex/substrate-s3-verification-data`, merged to develop).
- **S4** `chaos-smokes` — **First slice landed** (`codex/substrate-s4-chaos-smokes`):
  `scripts/chaos-smoke.sh` runs ONE bounded scenario per invocation against a
  designated hotel (`just chaos-smoke [scenario] [--dry-run]`). Two scenarios
  are real: `guest-kill` (SIGKILL a designated low-stakes guest —
  `PHILOTIC_CHAOS_GUEST_ID`, denylist-checked against philote/membrane/
  heal-dispatcher patterns; asserts a new live `active_pid` in
  `materialized_guests` within a budget window, read read-only via `sqlite3`,
  plus no heal-queue item left open for that guest) and `config-corrupt`
  (writes a bogus value to the dedicated sacrificial key
  `chaos_smoke.canary_value` via a new `phil config get/set` IPC surface —
  `crates/philotic-web/src/config.rs`, the same connect-and-call shape as
  `heal.rs`/`mesh.rs` — asserts `phil doctor` stays healthy, then restores the
  key). `mesh-peer-drop` is a named stub (TODO in the scenario function
  itself) — never auto-selected by the weekly round-robin, never files or
  records anything, since a stub must not fabricate evidence. Safety rails:
  `PHILOTIC_CHAOS_SMOKE_DISABLE=1` checked first; refuses to run if `phil
  doctor` is unhealthy or the heal queue exceeds `PHILOTIC_CHAOS_HEAL_QUEUE_MAX`
  open items; guest/config-key targets are denylist/namespace-checked in code,
  not just documented. On failure, files an intel-graph decision via
  `POST :8900/api/decide` (the same A3 pattern
  `crates/heal-dispatcher/src/main.rs`'s `push_intel_graph_record` uses — there
  is no dedicated proposal-create REST route) targeting
  `doc:substrate-hardening-proposal`; on success, records a `TestRun` via
  `POST :8900/api/test-run` against the same target, mirroring
  `scripts/test-and-record.sh`. Scheduling is an opt-in, per-host weekly
  launchd LaunchAgent (`just chaos-smoke-schedule` →
  `scripts/install-chaos-smoke-schedule.sh`, `RunAtLoad=false`, Sundays
  03:00 local) rather than a mesh-replicated `CronJob` — a chaos drill must
  not silently start firing on every mesh-connected hotel the moment one
  operator opts in on one machine, which is exactly the M4
  `memory.hygiene`-lane hazard the fire-time re-check pattern exists for; the
  kill-switch re-check inside `chaos-smoke.sh` itself gives the same
  protection without needing mesh-replication semantics at all.
  **Bug caught and fixed during self-review**: `phil heal list`
  (`crates/philotic-web/src/heal.rs`) hardcoded hotel `"aiua"` regardless of
  `--hotel`. With `chaos-smoke.sh`'s default `PHILOTIC_CHAOS_HOTEL=default`
  (matching `phil`'s own CLI default), the heal-queue pre-flight rail and the
  post-respawn "no stuck heal item" assertion would connect to a socket that
  doesn't exist, get a plain IPC error string back, and — because that error
  text matched neither "no open heal work items" nor the item-line regex —
  silently count as **0 open items, i.e. "clean."** Both the safety rail and
  the core PASS assertion would have quietly lied on any hotel not literally
  named `aiua`, which is exactly the "never silently absent" failure Standing
  Rule 2 forbids. Fixed two ways in the same commit: (1) `phil heal
  list`/`close` gained `--hotel` (default `"default"`, mirroring `phil
  config`) so the read actually targets the right socket; (2)
  `heal_list_or_fail()` in `chaos-smoke.sh` now treats *any* nonzero/unreadable
  `phil heal list` as an explicit `"unreadable"` sentinel that both the
  pre-flight gate and the post-kill assertion hard-refuse/fail on, instead of
  defaulting a read failure to zero — so a future regression in the same
  shape fails loud instead of failing clean. Covered by two new unit-test
  cases and confirmed against a real (ephemeral) hotel, not just a fixture.
  **Reality gap surfaced during this slice**: this proposal's own S1 row
  above still reads "not started," contradicting the assumption this slice
  was scoped under (S1–S3 live) — some S1-adjacent hardening has landed
  piecemeal (launchd watchdog, log rotation, DEF-046 root-cause), but nothing
  in git history closes S1 as a slice. Not resolved by this slice; named here
  so it isn't silently carried forward as settled. Verified: shellcheck-clean
  (style/info-only residue, no warnings/errors) across all four new scripts;
  `bash -n` syntax-clean; `cargo check`/`test`/`fmt --check -p philotic-web`
  green (183 existing tests unaffected); 26/26 bash unit tests in
  `scripts/tests/chaos-smoke-unit-test.sh` cover `json_field`,
  `guest_id_denied`, `config_key_denied`, and heal-queue open-item counting
  including the unreadable-queue regression above; **smoke-green** for the
  new `phil config get/set` IPC surface (`just smoke-config` —
  `scripts/smoke-config-roundtrip.sh` — full round-trip against a real
  ephemeral aiua hotel: unset→null, set→read-back byte-identical, restore,
  invalid JSON rejected client-side) and for `heal_open_for_guest` against
  that same real hotel post-fix; `--dry-run` exercised against both a
  nonexistent profile (clean refusal) and a real local profile's read-only
  `phil doctor` output (no mutation). The `/api/decide` and `/api/test-run`
  body shapes were POSTed for real against the live local intel-graph server
  (`~/.philotic/bin/graph-intelligence`, the shared `just intel-graph-ensure`
  instance) and both returned HTTP 200 — then immediately deleted (node +
  edge rows, verified gone via `phil graph green` reverting from a fabricated
  `1/1 · green` back to `none`) since they were verification-only POSTs, not
  a real chaos run, and leaving them would have fabricated green evidence for
  this exact proposal. **Transient, confirmed not a standing gap**: during
  that same check, `GET /api/health` — the endpoint `graph_reachable()` here
  and `scripts/test-and-record.sh` both gate filing/recording on — timed out
  repeatedly against the live server while it was mid-`graph_scan` on
  startup (13+s full-workspace scan), causing `chaos-smoke.sh`'s own
  `file_failure`/`record_success` to report "not reachable" and skip POSTing
  even though `/api/decide`/`/api/test-run` worked fine when hit directly.
  Re-checked once the server was idle: `/api/health` returned 200 in
  <3s — this was startup-scan contention on that one request, not a broken
  endpoint, so `graph_reachable()`'s existing `/api/health` check (matching
  `scripts/test-and-record.sh`'s established pattern) is left as-is.
  **Not yet run**: the actual `guest-kill`/`config-corrupt` chaos
  scenarios (SIGKILL / config-write) have only been dry-run and unit-tested,
  never executed for real — that is explicitly the operator's first watched
  run, not this slice's, per the task constraint against running chaos
  against any live hotel unsupervised.

**S3 `verification-as-data`** — implemented 2026-07-11 (codex/substrate-s3-verification-data).
`just test` now runs `scripts/test-and-record.sh`, which POSTs pass/fail/duration to
`/api/test-run` whenever the graph server at :8900 is reachable (one-line notice, not a
failure, when it isn't). `target_id` resolves via `$GRAPH_TEST_TARGET` → branch-linked
proposal (`codex/<slug>` looked up against `/api/worktrees` `linked_proposal`) →
`workspace:test-baseline` fallback. `phil graph green` reads active proposals' latest
recorded `TestRun`/`TestedBy` evidence directly from the graph DB (no server dependency)
and answers "what is green right now?" with pass/total counts and run age — live-verified
against the real graph.db: `Substrate Hardening — The Ground Under Autonomy` now shows
`1198/1198 · 41s · green`, while unrecorded active proposals correctly show `none`.
Reality gap: no proposal's slug matches this branch's `codex/substrate-s3-verification-data`
naming (it's a sub-slice slug, not the parent proposal's doc slug), so the branch→proposal
auto-link never fires for slice branches — `$GRAPH_TEST_TARGET` or the baseline fallback is
the honest default path for slice work, not silent auto-linking.
