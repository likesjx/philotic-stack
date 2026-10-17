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

## Normal-loop inspection candidate

The local inspection candidate receives `development.inspect_manifest` inside
`AgentRuntime::run`, after normal startup. It uses no alternate main entrypoint,
model call or shell. The explicitly opted-in guest authenticates the expected
hotel peer; the hotel adapter checks the owned incarnation and exact task scope.
The adapter is opt-in API surface, not installed by production hotel startup.

The supervisor pins at most 16 KiB of `Cargo.toml` and `README.md` snapshot bytes.
The guest computes lengths/SHA-256 hashes; the adapter validates the result against
the immutable snapshot. The authoritative receipt is in LocalWork's SQLite file,
committed with fence and authority revision checks. No second output-file commit
exists. A receipt survives lost responses; a dispatched operation without a receipt
blocks as Unknown. Only a recorded pre-dispatch failure permits bounded retry.
All authority records remain synthetic; prepaid active-work quanta are not measured
CPU/token usage. The adapter's clock advances monotonically from its supplied base.

This work does not restore ordinary conversational WorkingTurns. Existing startup
sweeping is unchanged; inspection state lives in Task rather than session apartments.
Normal configuration requests receive only exact inert fixture responses. Input
snapshots in the acceptance test are compile-time repository bytes; no live mutable
repository or symlink traversal is exercised. Fault injection and duplicate delivery
are test-only. A prior synthetic effect is not inspection evidence.

Reproduction (explicit owned-process test; not run by ordinary cargo test):

```sh
cargo build --offline -p philote --bin philote
PHILOTIC_INSPECTION_BINARY=/absolute/path/to/target/debug/philote PHILOTIC_INSPECTION_REVISION=<exact-source-commit> cargo test --offline -p aiua --bin aiua actual_normal_runtime_inspection_recovery -- --ignored --nocapture
cargo test --offline -p graph-intelligence --test local_work_recovery
```

Acceptance records the owned `/tmp/nr-*` directory, process IDs and runtime markers.
Databases and stdout/stderr remain as evidence; owned children and sockets are reaped.
The first actual-process acceptance run and all 11 ledger regressions passed.
Focused independent review corrected receipt identity validation inside both ledger
transactions; candidate changes before commit or reconciliation now refuse stale
results. Final revision/process receipts accompany local delivery. Revocation after
snapshot release prevents commit, not computation over bytes already disclosed.
This candidate is not published, merged, deployed or production-live-verified.

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

## Follow-up: opt-in disposable IPC commit boundary

Local candidate on `codex/philote-local-recovery`, based on merged foundation
`a178f8adff1fe7d9c681e20fb9a21a519eed351e`. The hotel may explicitly install
`ProtectedDisposable` on a dedicated disposable socket. Production startup does
not install it. All requests on that socket bypass generic IPC dispatch, including
broadcast setup, registration subscriptions, mesh routing and parked-task delivery.
Only Register, one exact EmitTask and one allowlisted SyncApartment key are accepted.
Even GetConfig is denied; this is not yet a Philote bootstrap-compatible fixture.

The server-owned contract names guest/principal/role, an exact operation JSON and
local destination, separate checkpoint and receipt keys, and a monotonic deadline.
It cannot be supplied or delegated over IPC. Socket kernel credentials authenticate
against LocalLaunchRegistry. A synthetic contract revocation serializes with commits.
This is fixture configuration, not a new access-grant service or production policy.

`VerifiedLocalSession::with_current_launch` holds the launch-registry mutex while a
bounded synchronous closure runs. Retirement/replacement cannot return before an
already admitted commit finishes; later commits fail. Lock order for the adapter is
contract mutex, launch registry, graph adapter. There is no async wait, external
callback, child control, or reverse acquisition inside the commit. Callers of the
primitive must not re-enter the registry; the API does not make arbitrary closures
safe. OS exit after the liveness check is distinct from registry retirement.

The synthetic effect is precisely a graph apartment receipt for the fixed operation,
not an external tool invocation. Exact duplicate delivery reads that receipt without
rewriting it; a conflicting receipt fails closed. Checkpoints cannot overwrite the
receipt key. Use only a disposable graph and one adapter owner. This boundary does
not add durable task epochs, retries, budget accounting or grants: those remain the
separate `LocalWork` foundation contracts, not inferred from launch authentication.

Tests cover kernel identity mismatch, registration/scope/owner restrictions, expiry,
revocation, retired/replaced proofs, duplicate receipts, synchronous lock release and
retirement ordering, plus actual IpcServer socket dispatch bypass. Unit launch records
use a labelled synthetic process fixture; they do not claim an actual Philote child.

The boundary alone does not alter the real executable's startup sweep or restore
turns. The next follow-up adds a separate feature-gated test driver and ledger
integration, without changing default recovery policy. Implemented/tested local
boundary, merged, deployed and live-verified remain separate facts.

## Follow-up: actual executable, disposable recovery driver

The `philote` Cargo feature `disposable-recovery` plus the explicit supervisor
fixture environment select `AgentRuntime::run_disposable_recovery`. This runs the
actual `philote` binary and existing IPC client/ScriptedLoopExecutor, but **not the
default AgentRuntime::run startup/turn loop**. Default startup sweep behavior is
unchanged. The driver verifies the connected hotel's kernel PID/UID, requests a
single protected bootstrap key, and accepts only one `synthetic.receipt` script
step. It has no model, real tool, memory or network operation path.

The aiua test-only `RecoveryFixture` plugs into the protected adapter under the
same launch guard. It uses the existing `LocalWork` SQLite ledger for claims,
epochs, authority checks, operation intent, synthetic effects, retries and budget.
`Work.checkpoint` (serde-defaulted for old records) stores the compact serialized
script in the same fenced Task transaction; no apartment receipt or second
execution ledger is written by this fixture. Checkpoint writes validate the
current task fence and original authority revision. Lock order extends to the
fixture mutex and then the ledger transaction; no asynchronous wait occurs under
these locks. The test intentionally withholds the response only after all guards
are released and the effect transaction commits.

The ignored `actual_philote_interruption_reconciles_existing_ledger` test requires
an explicitly supplied built binary. It supervises only its own children using
`ProtectedLocalGuest`, attaching the registry before accepting their connection:

- interrupt after the durable effect but before result checkpoint; reopen ledger;
- claim a new epoch, reconcile success, restore script and refuse duplicate dispatch;
- refuse a third actual child after durable authority revocation;
- refuse a still-live child's result checkpoint after a separate ledger takeover;
- deny actual-child bootstrap for an unknown dispatched outcome with no receipt;
- assert durable attempts, prepaid budget quanta and exhaustion; reap children and
  remove their sockets, retaining databases/stdout/stderr as evidence.

Commands (use absolute paths for the two environment variables):

```sh
cargo build --offline -p philote --bin philote --features disposable-recovery
PHILOTIC_DISPOSABLE_BINARY=/path/to/target/debug/philote PHILOTIC_DISPOSABLE_REVISION=<commit> cargo test --offline -p aiua --bin aiua actual_philote_interruption_reconciles_existing_ledger -- --ignored --nocapture
cargo test --offline -p graph-intelligence --test local_work_recovery
```

The fixture clock is deterministic (epochs at logical times 0/10/20). Budget
values are prepaid simulation quanta, not measured child CPU time or token usage.
The hotel is a test-only in-process socket fixture using the production protected
adapter; the children are actual executables. This proves the opt-in local recovery
path, not general production turn restoration, external exactly-once effects,
production grants, a running hotel integration or live verification. No default
runtime installation, migration, daemon or grant service is added.

Hosted verification: `.github/workflows/disposable-philote-recovery.yml` runs on
path-matching PRs, checks out the exact PR head, builds the opt-in executable and
explicitly invokes the ignored actual-process test. Its read-only job uses no
secrets, production endpoints or deployment steps. The evidence artifact contains
revision, hosted environment, toolchain, binary checksum, command/result log and
synthetic ledgers. A successful run asserts one main recovery effect, no unknown
outcome effect, and owned-process/socket cleanup. Ordinary workspace CI alone does
not establish this coverage. The dedicated driver and prepaid accounting limits
above continue to apply on hosted runners.
