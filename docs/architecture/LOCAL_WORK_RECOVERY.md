---
title: Synthetic Local Work Recovery Candidate
doc_type: reference
domain: workflow-docs
status: active
last_updated: 2026-10-17
tags: [local-work, recovery, synthetic, fencing]
related_docs:
  - RUNTIME_AUTHORITY_LEASES_PROPOSAL.md
  - ARCHITECTURE_STATUS.md
task_refs:
  - docs/task.md
---

# Synthetic local work recovery candidate

## Work contract

Owner: `codex/local-work-recovery`, one builder; parent dot is delivery lead.
Base: `origin/develop` at `60ca892d2db85573b953dbddf4f0db7e6803d50f`.
Outcome: one synthetic local task can be claimed, checkpointed, reconciled and
verified after a process interruption. This is a local candidate, not a scheduler.
One bounded independent review follows the candidate tests.

No daemon, new IPC endpoint, external effect adapter, credentials, service changes,
memory reads/migrations, CI dispatch, publication, merge or deployment belongs here.
The occupied checkout and other worktrees are preserved. Use only a disposable
graph database; do not point this harness at the running intelligence graph.

## Contract and ownership

`graph_intelligence::local_work::LocalWork` uses existing graph `nodes`, `edges`,
transactions and `Task`/`Slice`/`RoleCharter`/`Session`/`TestRun` kinds. All writes
are namespaced `synthetic:`. The opt-in `synthetic_work_effects` table is the
simulated effect itself, not a second task state owner. No default schema change
or running-server route is added.

A Task owns its durable epoch, session identity and expiry. Every work mutation
and effect checks that complete fence inside the transaction. Competing SQLite
writers either commit one claim or fail closed (including SQLITE_BUSY conflicts).
Expiry permits a strictly higher epoch; restart never resets it. The caller must
supply a trusted, nondecreasing logical clock. This mirrors the existing
`LeaseEnvelope` owner/scope/epoch/expiry semantics but does not reuse its hotel and
guest wire fields or change the in-memory runtime lease registry.

A RoleCharter fixture contains a synthetic authenticated-authorizer reference,
exact action/resource/destination tuples, validity, revocation and a delegation
flag. Delegation can only narrow parent scope and validity. Every ancestor is
checked, with cycle/depth refusal. Fixture administration increments revision;
prepare records the entire authority revision chain, and dispatch/effect recheck
it transactionally. A task lease never grants access. This is trusted local test
data, not real authentication, credentials or a security boundary against someone
with direct database/file access.

Prepare persists event identity, operation ID and phase before dispatch; dispatch
persists intent before the effect transaction. One operation per task and unique
ledger keys make duplicate delivery idempotent. Resume reconciles an exact ledger
receipt; missing receipt after dispatch becomes `Unknown` and cannot be retried.
Dispatch records its lease epoch; a takeover cannot execute an old dispatch even
if it tries to bypass reconciliation.
The dispatching store also holds a process-local execution permit: reopening a
checkpoint with an unexpired original fence still permits only reconciliation.
Only known pre-dispatch failures may increment a retry attempt. No arbitrary
external exactly-once guarantee is claimed.

`reserve` prepays active-work milliseconds before a bounded quantum; waits reserve
zero. Reservations survive interruption and are not refunded, so this is a
conservative accounting ceiling, not a measured CPU timer. The simulated effect
costs one additional millisecond, charged atomically with its receipt. Quota is an
explicit synthetic boolean input; no provider is queried. Quota/budget/retry
exhaustion commits a stop checkpoint. Stopped tasks allow factual reconciliation
and ownership takeover, but cannot prepare, dispatch, execute or verify. No automatic
unstop policy is included.

Evidence is an immutable TestRun linked to its Task. Revision, environment, command,
exit status and result reference are required; revision/environment must equal
the current candidate. A candidate update clears `tested`. These values are
trusted harness inputs: use an exact commit or source-content digest, a concrete
environment identity and a content-addressed retained result log. The module does
not execute arbitrary commands, attest hardware, hash files or authenticate test
results. `implemented`, `tested`, `merged`, `deployed`, and `live_verified` remain
separate facts; this API never advances the last three.

## Deterministic verification

```sh
cargo test --offline -p graph-intelligence --test local_work_recovery
cargo test --offline -p graph-intelligence --lib
```

The integration harness uses temporary databases and two connections racing a
claim. A child test process exits with code 73 after durable dispatch (with and
without effect receipt). The parent reopens the database and takes over at a
deterministic lease expiry. No production process is interrupted. Tests also
cover stale writes, duplicate effects, stale evidence, retries, persisted budget
and quota stops, scope/validity/revocation/delegation, and changed authority between
prepare, dispatch and effect.

## Next executable work

Candidate verification (2026-10-17): 17 library tests and 9 integration tests pass;
`cargo clippy --offline -p graph-intelligence --lib --test local_work_recovery -- -D warnings`
passes. One independent read-only review found a takeover bypass of reconciliation;
dispatch-epoch binding and process-local permits fix it, with direct-bypass and
same-fence-reopen regressions passing. No second reviewer or live validation was used.

Review this local candidate and its retained test receipt. A later separately
authorized slice could wrap the protocol in a trusted clock/quota/evidence adapter;
that work must first specify real effect reconciliation and authorization ownership.
There is intentionally no production scheduling or rollout step in this slice.
