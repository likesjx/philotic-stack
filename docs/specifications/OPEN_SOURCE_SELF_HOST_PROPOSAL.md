---
domain: distribution
status: proposed
disposition: proposed
last_updated: 2026-08-04
---

# Open-Source Self-Host Proposal

**Status**: Proposed
**Domain**: distribution
**Date**: 2026-08-04
**Goal**: Someone who is not the author can clone this repository, run a single
hotel on their own machine, and debug it themselves when it breaks.

---

## Thesis

The gap between where the stack is now and a self-hostable product is **not
features**. The architecture is sound — the transport discipline, the
hotel/guest materialization model, and the storage-trait abstraction all hold up.

The gap is that **the system does not report its own failures**, and therefore
currently requires the author to notice them.

That is survivable for a fleet of three machines with one operator who wrote
every line. It is fatal for a stranger. A stranger has no memory index, no
tcpdump instinct for this particular mesh, and no prior on which subsystem lies.
When their hotel goes quiet, they will not spend 31 hours finding out why — they
will delete the repository.

---

## Evidence

Near-every significant incident recorded in this project shares one property:
the system was broken and **nothing said so**.

| Incident | How it presented |
|---|---|
| Full disk from unpruned deploy backups | aiua alive, log frozen, **zero philote guests**, no error |
| Deploy path with no release build | **no-ops and prints success** |
| `aiua-watchdog` after a bare `bootout` | respawns the **old** binary every 120s |
| `phil` after local deploy | SIGKILL, exit 137, no output — while `codesign -v` reports valid |
| Claude harness skills | **0 of 32 discoverable**, silently, for months |
| Muninn replication log | **+10 GB/day** unnoticed for weeks |
| Muninn "outage" reported by an agent | a **stale heal-queue row**; the daemon was up throughout |
| Memgraph unit bound to the wrong data dir | LifeGraph destroyed on **every restart** |
| Jane philote session pinned to an inert incarnation | deaf **31 hours**; wedge threshold unreachable, **no detector fired** |
| Philote watchdog anchored to its own tick | 600s ceiling **never held**; correct clocks sat as dead code |
| LifeGraph batch rejecting `observations` as strings | whole batches dropped for **months** |
| Muninn observers | accept writes that replicate nowhere, **no error** |
| Beacon plan steps | **model-self-certified** — reported success that never happened |
| vps life-graph-runner | ran an **unmerged build** |
| Mesh peer port drift | two-sided drift is **permanent**; `mesh_events` is not a heartbeat ledger |
| `egress-http` guest missing from every hotel | emits **black-holed**; turns leaked to the 300s zombie reaper, 30/30 on one node, catalog frozen for a week |

Sixteen distinct incidents, one shared root cause class. This is not
carelessness — it is the predictable consequence of a well-factored system that
never had to explain itself to anyone but its author.

The most recent one (2026-08-05) also exposes a defect class that matters
disproportionately for self-hosting: **additions to the guest seed list never
backfill already-provisioned hotels.** A first install gets the full seed list; a
hotel provisioned before a guest was added never receives it, and the absence is
silent. For a project where users install once and then pull updates, that is a
permanent divergence between what the code says ships and what a running node
actually has.

---

## What is already sound (do not rebuild)

Establishing this matters, because the work below should not disturb it.

- **Transport discipline.** Audited 2026-08-04: **zero** direct connections to
  another host from any Philotic process. Cross-host traffic is mesh only (UDP
  beacon, inbound TCP for blob/exec). Remote targets in live config are *node
  ids*, not hosts or IPs.
- **Muninn is already optional.** `load_muninn_config` returning `Err` logs
  "continuing without memory" and falls back to `NullMemoryEngine`. Nothing in
  the startup path hard-depends on it. This is exactly the property a self-hoster
  needs and it already holds.
- **`phil init` exists.** There is a real first-run path that generates an
  identity keypair and a `mesh-config.json`.
- **The heal queue is real, not decorative.** 1,405 resolved rows on one node,
  with pattern classification and per-class justification in code.
- **The verification ladder exists as a concept** — test-green / smoke-green /
  watched-live-green — and is used honestly in the project's own notes.

---

## Slices

Ordered by whether they block a stranger, not by effort.

### S1 — Licensing and OSS table stakes  *(blocker, trivial)*

`README.md` declares **MIT** but there is **no `LICENSE` file** in the
repository. Without one, the grant is ambiguous and GitHub will not detect it —
a careful adopter or their employer will not touch it.

- Add `LICENSE` (MIT, matching the README claim).
- Add `CONTRIBUTING.md` and `SECURITY.md`.
- Confirm CI can run for outside contributors. Fork PRs requiring maintainer
  approval is a known friction point — it silently meant zero CI on an upstream
  contribution until merge time.

**Acceptance**: GitHub displays a detected license; a fork PR runs CI.

### S2 — Zero-to-running on a clean machine  *(highest-value unknown)*

**Nobody has ever run this from zero on a machine that is not the author's.**
Every install to date has been an upgrade over existing state. Until that path
is walked, every other slice is speculation about which step fails.

- Walk `clone → build → phil init → phil start → one working agent turn` on a
  clean user account or VM, recording every point that requires knowledge not in
  the repository.
- Explicitly determine what is **required** versus **optional** for a single
  node: model-provider key (required?), Muninn (optional — confirmed), Memgraph
  and the LifeGraph (optional?), Telegram (optional?).
- The output is a defect list, not a document.

**Acceptance**: a written transcript of the walk, and one filed defect per
manual intervention that the repository did not tell the operator to make.

### S3 — Make failure loud  *(the defining work)*

Convert the evidence table into assertions. Each is an invariant the system
should refuse to violate silently.

- A hotel with **zero materialized guests** must not report healthy.
- A deploy must **verify the binary it installed actually executes**, and fail
  loudly when it did not build or did not replace.
- Watchdog budgets must be checked against a clock the watchdog **does not
  control** (the tick-local clock defect).
- Heal-queue rows must **resolve on recovery**, so a self-healed blip cannot be
  read later as a live outage. *(One instance shipped in PR #381; the pattern
  needs a sweep.)*
- Any dependency probe that flips to unavailable and back must leave **no
  residue** that a future reader can mistake for current state.
- A guest that the code says should exist and does **not** exist on a running
  hotel must be reported, not black-holed. Emits to a missing target must fail
  fast rather than leaking a turn to the zombie reaper 315 seconds later.
- **Seed-list additions must reconcile against already-provisioned hotels**, or
  the divergence must be surfaced. An upgrade path that silently withholds new
  guests from existing installs is the self-host equivalent of a failed
  migration.

**Acceptance**: for each invariant, a test that fails when the invariant is
removed. Not merely a test that passes.

### S4 — Give the heal loop actuation  *(detection without action is half a loop)*

The heal-dispatcher supports exactly three actions — `escalate`,
`refresh_memory_config`, `restart_guest` — all pure IPC, **no process spawn**.
It can detect far more than it can repair. Confirmed 2026-08-04: nothing in the
system can start Muninn, and the admin agent's `bash.exec` requires per-call
operator approval, so it cannot self-heal unattended either.

- Add scoped, auditable, rate-limited actions for restarting supervised
  dependencies, with the outcome written back to the queue.
- Prefer narrow named actions over granting an agent a shell.

**Note**: this grants an autonomous daemon the ability to spawn processes. That
is a new capability class and a deliberate blast-radius decision, not an
incidental one.

**Acceptance**: a dependency killed by hand is restored by the dispatcher, with
the heal-queue row resolved and an audit trail.

### S5 — De-personalize the defaults  *(smaller than it looks)*

Measured, not assumed: only **3** occurrences of `/Users/jaredlikes` in shipped
`.rs` code, and the `bjork`/`jane` identifiers are overwhelmingly test fixtures
and doc-comment examples. This is cosmetic, not architectural.

The real leak is **`mesh-config.example.json`**, which ships a persona described
as *"Jared's right-hand agent"* as the default agent a new user receives.

- Neutral default persona and agent identity in the example config.
- Remove author paths from user-facing placeholders.
- Leave test fixtures alone; they are not user-facing.

**Acceptance**: a fresh `phil init` produces no reference to the author.

### S6 — Dependency contract

A self-hoster needs to know what they must install, what degrades, and what
simply switches off. Muninn already degrades correctly; the others are unproven.

- Document required versus optional dependencies with the observed behaviour
  when each is absent.
- Ensure each optional dependency **degrades loudly and specifically** rather
  than producing a confusing downstream failure.

**Acceptance**: each optional dependency removed in turn, single node still
starts, and the log names what is missing and what is consequently disabled.

### S7 — Documentation that matches reality

The architecture docs describe intent. Several incidents above happened in the
gap between intent and behaviour.

- Reconcile `README.md` and `docs/architecture/` against verified behaviour.
- Fold the S2 transcript into a genuine quickstart.

**Acceptance**: every command in the quickstart executed verbatim on a clean
machine, in order, without edits.

---

## Sequencing

S1 first — it is an afternoon and it is a hard legal blocker.

S2 next, and before committing to S3–S7 in detail. S2 is the only slice that
produces *evidence about a stranger's experience* rather than inference from the
author's. It will almost certainly reorder everything after it.

S3 is the largest and most valuable body of work, but it should be aimed by S2's
findings rather than by this document's guesses.

---

## Open questions

- **Single-node or nothing?** Is a one-node hotel a first-class supported
  configuration, or is the mesh the point? This determines whether S2 is the
  product or a demo of it.
- **Which failures are worth an invariant?** S3 lists five. Fifteen incidents
  suggests more. The selection criterion should be "a stranger cannot diagnose
  this" rather than "this bit me."
- **Does S4's process-spawn capability belong in the dispatcher at all**, or in
  a separate supervised component with a narrower trust boundary?
- **What is the support posture?** Open source implies inbound issues. The
  verification ladder is a good answer to "does it work," but there is no
  current answer to "who responds."
