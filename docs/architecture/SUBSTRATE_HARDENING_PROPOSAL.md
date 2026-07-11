---
title: Substrate Hardening — The Ground Under Autonomy
doc_type: proposal
domain: operator-control-plane
status: active
disposition: proposed
last_updated: 2026-07-11
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
| S4 `chaos-smokes` | Failure paths rot unless exercised. A scheduled (cron) chaos smoke on a designated hotel: kill a guest, drop a mesh peer, corrupt a low-stakes config key — assert the heal queue classifies and resolves each within budget, and file a proposal (A3 pattern) when it doesn't. Frequency low (weekly), scope bounded, kill switch standard. | M | watched-live (first full chaos cycle reviewed; one deliberately-broken heal path caught) |

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
