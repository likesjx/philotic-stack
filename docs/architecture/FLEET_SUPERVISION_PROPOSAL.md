---
title: Fleet Supervision — Nothing Owns "What Should Be Running Here"
doc_type: proposal
domain: operator-control-plane
status: proposed
disposition: proposed
last_updated: 2026-08-05
verification_level: field-evidence
tags:
- supervision
- self-healing
- autopoiesis
- launchd
- systemd
- observability
- heal-circuit
related_docs:
- AUTOPOIESIS_PROPOSAL.md
- SUBSTRATE_HARDENING_PROPOSAL.md
- ARCHITECTURE.md
task_refs:
- docs/task.md
---

# Fleet Supervision — Nothing Owns "What Should Be Running Here"

> Detection was never the problem. Both outages below were detected thousands of
> times. They were **action** failures.

## Premise

Two unrelated incidents, found 2026-07-26/27 while investigating why Jane stopped
answering:

1. **Muninn dead on mbp-jane for ~3 days** (2026-07-23 17:50 → 2026-07-26 22:15Z).
   Every agent on the host ran memory-blind the entire time.
2. **`agent-jane` silently deaf for ~31 hours** (2026-07-25 15:17 → 2026-07-26 22:16Z).
   Operator messages — including "Are you there?" — were accepted, turned into
   `session_turn` rows, and never delivered to any process.

Neither was noticed by the system. Both were noticed by a human wondering why it
was quiet. The self-heal circuit filed **919** entries about the first one and
repaired nothing.

The common root is that **no component owns the question: "what is supposed to be
running on this host, and is it actually serving?"**

## Evidence

### 1. `escalated` is written as `resolved`

`heal_queue` on mbp-jane, all-time: **1680** entries.

| Outcome | Count |
|---|---|
| `escalated` | 1426 (85%) |
| `noop` | 181 |
| `restart_failed` | 24 |
| `abandoned` (exceeded 259200s without dispatch) | 19 |
| `work_item_filed` | 9 |
| `restart_skipped_budget_exhausted` | 8 |
| **`restarted`** | **4** |
| `restart_skipped_not_a_guest` | 3 |

Four restarts. Ever. `service_probe_failed:muninn` alone is **919** entries at a
4.99-minute cadence over three days, and **918** of them were written
`status=resolved` / `outcome=escalated`. The circuit reports success for handing
off a fault it never fixed. A dedup work item (`97ffa94d`) exists but neither
suppresses re-filing nor maintains a single aging open record.

### 2. Escalation terminates inside the failure

All 918 escalations targeted `role:agent-jane:orchestrator` — which was *itself*
the deaf agent for 31 of those hours. The alerting path routed **through the
broken component**, with no out-of-band fallback. This circular dependency is the
reason a 3-day outage produced zero operator awareness.

### 3. No repair action exists for service probes

`service_probe_failed:*` can only escalate — even for services the hotel knows how
to start. Separately, `heal_dispatcher` refuses to act when an entry is keyed by
session id rather than guest id (`refusing restart_guest on a session id (not a
materialized guest)`), so the zombie-turn class can never self-repair either.

### 4. Liveness is asserted, serving is not

Three distinct instances of "supervised/registered but not serving":

- `agent-jane:orchestrator` passed the hotel's `is_registered()` check and received
  deliveries into a void. Proof: the base-agent fallback in
  `crates/aiua/src/service/role_materialization.rs` (log string *"delivering to its
  live base agent registration"*) **is present in the deployed binary and fired
  zero times**, and no `is not registered for session` warning was ever emitted.
  So `resolve_agent_route` took the `Deliver(Some(active_guest_id))` branch. Every
  downstream safety net — base-agent normalization, provenance-hint preference,
  parking — sits *below* that check and is unreachable when the registration row is
  live but the channel is dead.
- `com.philotic.web.mbp-jane`: last exit 1, listening on no port.
- `com.philotic.aiua.mbp-jane`: last exit -9.

### 5. Supervision sets have drifted with nothing to compare against

| Host | Supervised units |
|---|---|
| mac-jane (launchd) | aiua, aiua-watchdog, intel-graph, intel-graph-freshness, logrotate, worktree-gc, diskspacewatch, onnx-sidecar, muninn.mcp |
| mbp-jane (launchd) | aiua, aiua-watchdog, intel-graph, intel-graph-freshness, logrotate, gemini400watch, web |
| vps-jane (systemd) | muninn, philotic-hotel, philotic-memgraph, philotic-onnx-sidecar, philotic-web |

mbp-jane lacks `worktree-gc`, `diskspacewatch`, `onnx-sidecar`; mac-jane lacks
`gemini400watch`, `web`; **neither Mac supervised the muninn daemon at all** until
2026-07-27. The drift is invisible because nothing declares the intended set.

### 6. The fleet's memory service was kept alive by accident

mac-jane's muninn never died only because `muninn mcp` stdio proxies spawned by
Claude desktop/CLI clients resurrect the daemon on demand. Verified directly: with
no launchd job loaded, `SIGKILL` of the daemon produced a fresh `ppid=1` instance
within ~6 seconds. mbp-jane has no Claude client, so nothing resurrected it there.

The most important shared service in the fleet was surviving on which desktop apps
happened to be open.

## Slices

**S1 — Declared process manifest.** Each host declares its expected supervised set
in the context graph. A reconciler reports drift (absent / present-but-undeclared /
mismatched command) and feeds `host-health`. This is the piece that makes the
divergence table above impossible to reproduce silently.

**S2 — Serving, not running.** Liveness must assert the service *answers* (TCP +
protocol probe), and dispatch must assert delivery is *ACKed*. Never trust a
registration row. Covers evidence 4.

*Recovery half shipped (PR #382); detection half still open.* When the zombie
watchdog reaps a turn, a session pinned to a role incarnation that let two or
more consecutive turns die is now re-pointed to its base agent and files
`role_incarnation_not_serving`. That bounds the damage; it does not prevent the
wedge.

The 2026-07-28 recurrence — agent-jane deaf a second time, ~17h, same pin, that
philote again inert at 0.54s CPU over 17h — surfaced the two mechanisms the
detection half has to close:

1. **The re-pin needs no operator command.** `deliver_live_guest_task`
   (`ipc.rs:4161`) calls `update_session_active_incarnation` for *any* delivery
   carrying `activate_session_id`. A single whisper, parked-task flush, or role
   dispatch to a role incarnation therefore re-points the whole conversational
   session at it — no `/role`, no `/sfw`, nothing in the log that reads like a
   decision. Its liveness test is "has an inbox subscription under this role",
   which the inert philote satisfies: it subscribed and then never consumed.
   *Having a subscription is not the same as draining it* — the same conflation
   as evidence 4, one layer down.
2. **No detector could fire.** `SUBSCRIBER_BACKLOG_WEDGE_THRESHOLD` is 32
   undrained frames. A turn that dies undispatched contributes roughly one
   frame, so the ~11 turns lost in the recurrence never came close. The
   threshold is calibrated for a chatty streaming guest and is effectively
   unreachable for a conversational agent — it would take 32 lost turns to trip.
   `subscriber_wedged` exists in the deployed binary and has fired **zero**
   times fleet-wide.

So the wedge is entered silently by a routine delivery, and every existing
detector is either downstream of the bad assumption or calibrated out of range.
A correct S2 must (a) refuse to re-pin a session onto an incarnation that has
not demonstrably consumed a task, and (b) make the backlog gauge sensitive to
*undelivered turns*, not just frame count.

**S3 — Repair actions for service probes.** `service_probe_failed:<svc>` gains a
bounded restart action for known host services — backoff, budget, and only then
escalate. Covers evidence 3.

**S4 — Escalation truth.** ✅ **Implemented.** `escalated` is no longer written as
`resolved`. Covers evidence 1.

- `HEAL_STATUS_ESCALATED` + `terminal_status_for_outcome()` in
  `ansible-mesh-core::heal_queue`: only outcomes that actually repaired
  something may claim `resolved`. Both `escalate` and `escalated` (the
  dispatcher has emitted each into that column over time) map to the new
  status.
- Terminal for dispatch — `pending_errors` selects `status = 'pending'`, so an
  escalated row is never re-picked and the 5-minute cadence cannot return under
  a new name. The retention vacuum reaps `escalated` alongside `resolved` and
  `abandoned` so a recurring fault cannot grow the table without bound.
- New `phil doctor` check **`heal.escalated-unrepaired`** surfaces the worst
  `(pattern_tag, guest_id)` pair still escalating: Warning past 1h, Critical
  past 6h (≥3 escalations). The muninn outage would have gone Critical on day
  one instead of hiding behind 918 rows marked `resolved`.

Deliberately **not** collapsed at push time: the A3 recurrence tracker counts
rows, so one row per detection must keep landing or it never reaches its
work-item filing threshold. Aggregation happens in the doctor check instead. A
regression test pins this coupling.

**S5 — Out-of-band operator path.** Escalation must never depend solely on an agent
that may be the failing component. Folds in the previously-filed
`operator-chat-bound-heal-push` (Telegram push without a live session).
Covers evidence 2.

**S6 — Silence as signal.** ✅ **Implemented (session half).**

`detect_session_silence` (`aiua/src/service/session_activity.rs`) flags an active
session that is **receiving turns and answering none**: two or more consecutive
failures, no success among them, oldest failure at least an hour old. It files a
`session_silent` heal entry at high severity and rides the zombie sweep's
cadence — the two are the same question one level apart, one asking "did this
turn die?" and the other "has this conversation died?".

The distinction that makes it usable is idle vs. silent. An idle conversation
has no recent turns and must never page anyone; a silent outage has turns
arriving and none succeeding. That is exactly what agent-jane looked like for
31h and then 17h while every existing detector stayed quiet.

Validated against the real incident rather than a synthetic case — a regression
test replays the actual epochs from
`session:telegram:7898847424:agent-jane`: last success 15:16:14Z, first failure
15:17:00Z, four dead turns by 15:52. It fires at **16:17:00Z, one hour in**,
against the **31 hours** the outage really ran. It stays silent at the 30-minute
mark, on a lone failure, and on a week-idle conversation.

Thresholds are deliberately forgiving: one failure is a blip (the 2026-07-28
Telegram/openrouter outage produced exactly one), and an hour is long enough
that a slow model or a provider retry ladder cannot manufacture a false
positive.

*Still open:* the service half — alerting when a supervised service reports zero
successful probes over N cycles. The muninn outage was already detected 919
times, so that half is about action, not detection (S3).

## Already applied (2026-07-27)

A `com.muninn.daemon` launchd job (`KeepAlive`, `ThrottleInterval 5`, absolute
`/opt/homebrew/bin/muninn`) is installed on **both** Macs, mirroring vps-jane's
systemd `Restart=always` / `RestartSec=5`.

Watched verification: `SIGKILL` → launchd respawn on both hosts (mac-jane
72573→74346, mbp-jane 69942→70039), exactly one daemon instance each, ports
8475/8750 serving afterward. mbp-jane muninn probe failures since: **1** (during
the swap window itself) versus **919** before.

This covers process *exit* only. A wedged-but-listening daemon still requires
S2 + S3.

### Install gotcha

Pebble takes an exclusive data-dir lock. If a hand-started or mcp-spawned daemon
holds it, the launchd instance dies with `open pebble: resource temporarily
unavailable` (exit 1) and retries forever without ever winning. Correct order:

```sh
launchctl bootout gui/$(id -u)/com.muninn.daemon   # if loaded
pkill -9 -f "muninn --daemon"                      # release the lock
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.muninn.daemon.plist
```

Bootstrap immediately — on a host with Claude clients an mcp proxy will respawn the
daemon within ~6 seconds and take the lock back.

## Note on where proposals live

This proposal exists as a repo document on purpose. Proposal nodes created only via
the graph MCP (`graph_create_node` with a bare `proposal:` id) **do not survive a
rescan** — two nodes and their recorded decisions filed on 2026-07-26 were gone by
the next freshness run. Every durable proposal in the graph is `doc:`-prefixed and
derived from a committed file. See DEF-072.
