# Known-source replacement candidate

Status: local source candidate, not Linux-qualified, deployed or live-verified.

## Source closure

- Repository base: `60b7ac3712995a980c04dd2b3675963089a6944e`.
- `crates/aiua/src/main.rs` is exactly the reviewed migration implementation from
  `230d5aaa1ccdf9fe2b18fd14c7d66c91cff95219`. This retains atomic startup credential
  migrations and validates existing provider references before removing legacy
  values. These are replacement safety requirements, not the identified live cause.
- The helper and its vendor sources remain unchanged from the base revision.
- Historical guest/state fixture sources came from repository revision
  `e9605cce625e379c9b72417bfabfa2f6e58b488c`. The compatibility runner now requires
  both freshly built executable digests as inputs instead of assuming an old
  artifact digest. Historical source is pinned to
  `56b0cdc2ed1123aad01b22eb88fa7b43baf02bf6`; it is not claimed to identify any
  installed executable.
- Candidate changes: fixed internal storage/absence/exact-ACL/decryption reason
  classifications, unchanged generic client error, and nonce-length rejection
  instead of a worker panic. No references, raw backend errors or secret values
  are included in internal classifications. No new retrieval interface or grant.

No installed executable, production database, configuration, credential, memory
or encryption key is an input to this package.

## Qualification and limits

Published parent `c31a5074328a7ad69d202265d8e62fd754aee93d` built successfully
in [run40353107671](https://github.com/likesjx/philotic-stack/actions/runs/40353107671).
Three migration tests, 21 Rust broker tests, the existing native Linux acceptance
checks and all 11 historical state/rollback tests passed. The actual-helper case
failed before helper launch because its socket-pair tuple was incorrectly used
as a context manager. Container cleanup passed. Failure receipts were not copied
out before the workflow's errexit, so retained logs and build evidence establish
these results; no completed interoperability receipt is claimed.

Local fixture-only corrections assign both sockets before entering their context
managers, and preserve attach/container failure status while copying receipts.
Eleven fixture tests, four reporting tests and four exact-workflow-shell tests pass.
The new entrypoint regression reproduces the old tuple failure; both new shell
failure-receipt regressions fail against the old workflow and pass with the fix.
Explicit SQLite closure now covers both backup pairs and bootstrap seeding;
a WAL-mode regression checks that all fixture connections close.
The full local fixture test uses an actual fake subprocess/socketpair/SQLite
snapshot with simulated Linux guards, not real UID/security/encryption semantics.
The unchanged provisioning suite's retained result is 58 passes and one skip.
Python, shell and YAML checks pass. Focused independent review is clear for this repair.

Rust sources remain unchanged from that compiled parent. The earlier local
locked build was unavailable because cached `memchr 2.8.0` did not satisfy locked
`2.8.3`; no dependency downgrade/download was performed. The corrected fixture has
not run in Linux CI, and no publication or rerun of this repair is authorized yet.

Proposed isolated workflow: `.github/workflows/muninn-known-source.yml`, branch
`codex/muninn-known-source-candidate`, repository `likesjx/philotic-stack` only.
Read-only repository permission; no SSH, repository secrets, deployment or
workflow cascade is requested. It checks out exact candidate head plus the fixed
historical source, builds both from source and the existing helper with locked
dependencies and Rust 1.94.1, and records source/toolchain/base image/binary inputs.
The Ubuntu base image ID is resolved once and used for the build container.

Build network access retrieves ordinary public build tools/dependencies inside
the disposable container. Tests use a separate network-none container, zero host
mounts, no privilege escalation, bounded resources and a minimal environment.
No Linux execution has occurred yet. The artifact includes the newly source-built candidate executable, helper and
synthetic qualification evidence; no installed executable is included. Only future approval to publish/execute the
workflow would authorize those operations.

The test runner requires:

1. Exactly three atomic migration tests, with zero-test success refused.
2. Existing real socket/protocol/activation/worker deadline tests.
3. Actual helper creation using only a public dummy key and dummy value, allocated
   reference and persistent scope, followed by actual candidate-hotel retrieval
   as UID/GID1003 with hotel UID999/GID987. The fixed helper database path exists
   only in a fresh disposable container. Fixture snapshots are closed synthetic
   SQLite files; no live state is copied. SIGTERM/restart and invalid activation
   cases reuse the existing bootstrap fixture.
4. Historical SDK guest, synthetic schema/state preservation, migration failure,
   interruption and executable-plus-context/training-state rollback tests.

Timeout/start failure is recorded as an incomplete fixed classification before
rethrowing; exceptions are not serialized. Inputs/artifacts are retained for one
day; disposable containers are removed on success/failure. Cleanup outcome must
be checked, not inferred from a successful command alone.

Passing establishes these synthetic contracts. It does not establish all current
guest ABIs, production writer quiescence, loaded policy or effective key state,
installed-executable equivalence, perimeter integrity, or a repaired connection.
It does not finalize the existing uncertain adoption journal.

## Minimum proposed production transaction after green qualification

This is a scoped design, **not yet an executable approved cutover**. Artifact
digests and fresh production preconditions are deliberately unresolved until the
exact candidate has passed Linux qualification.

- Bind the qualified runtime and unchanged helper digests, existing service
  identity/configuration/profile, unit hooks and database paths. Retain the
  accepted proxy. No general hotel reconfiguration or new credential is proposed.
- Approve a maintenance window for `philotic-hotel.service` and its shared guests;
  leave `percival-personal-mcp.service` stopped and OAuth off. Preserve the vault
  socket's existing configuration. Check effective hooks before selecting stop
  order; do not execute unknown hooks by assumption. Stop is bounded by the
  recorded 40-second service limit, with no blind retry after uncertainty.
- Establish quiescence for every affected state writer. Process/service lists
  alone are not proof. If the actual writer set cannot be fenced, refuse cutover.
  Capture a consistent context/training database pair and required selected
  profile/state paths, preserving absence, UID999/GID987 and modes. Keep the old
  executable server-privately for rollback, without transferring or analyzing it.
  Backups may contain private data and stay in root-only server storage. Proposed
  retention is through reconciled cutover/rollback plus seven-day disposal review;
  deletion requires separate approval.
- Install only the exact qualified runtime at `/opt/philotic/bin/aiua`, preserving
  ownership/modes, then start the same service with its existing configuration.
  Check actual running artifact, socket/process identity and readiness within a
  proposed 30-second deadline. Use one separately scoped existing introspection
  getter and internal verifier comparison; output classification only. Existing
  credential/policy/verifier are reused, not regenerated or adopted again.
- If pairing and existing-guest acceptance succeed, reconcile only the pending
  verification step via a reviewed journal CAS, preserving original evidence.
  This journal finalization is not implemented here. Never rerun creation or
  adoption to hide an uncertain result; the existing recovery action restores
  configuration and is not a success-finalization command.
- On confirmed failure, restore matching executable **and** captured state after
  fencing writers; retain uncertain evidence and keep OAuth/gateway closed.
  Ambiguous operations stop for reconciliation instead of automatic retries.

Native observe-key association/validity is separate. Its retained metadata expiry
is `2026-10-26T18:42:00Z`; no renewal, replacement or validity assumption is made.
Any native-key action requires its exact recipient/scope/lifetime/revocation
decision. After verified pairing and that gate, the exact gateway/issuer/admission
activation transaction still requires approval. Acceptance must include owner
Claude consent, an agreed harmless recall fixture, unauthorized/client-isolation
rejection, revocation, restart persistence and existing guest health. Only that
actual acceptance can establish completion.
