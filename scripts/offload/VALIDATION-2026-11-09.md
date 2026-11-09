# Fallback validation, 2026-11-09

This is partial ordinary source CI, not full CI or native package qualification.

## Source and execution identity

- philotic-stack: b4c44b86292ffa07f72f27f4560a77f531f46816,
  selected source manifest e08539cf37270c10e197b29908cffdaae4ad8934d5696b4a126d5c2b98737af2.
- jaredlikes-com: e408c470065cdb307cadd518a5703723e2a9e135,
  selected source manifest f8a049f514009823b1bb5940913930d1f4be24bdf2097f5da628e1bd1a4e2b4e.
- Executed runner SHA256:
  ff7d1b92bdde935f0f50387fda12925bc96310e7ddb7b9bd36a54e3376af8f16.
- Linux Node 24.22.1 / Python 3.12.14. Snapshot identities remained unchanged.
- Source came from already available local refs; no claim of latest remote state.

## Actual results

- Native egress inventory passed.
- Native Python: 36 passed (3 egress, 14 release manifest, 19 worktree safety).
- Native Node: 130 passed, 10 failed, zero skipped. Real loopback HTTP integration
  passed. Six acceptance-handoff Unix-socket fixtures failed listen with EPERM;
  four lifegraph-owner fixtures failed with EINVAL. EPERM paths were 104 bytes;
  EINVAL paths were 113 bytes. Excessive Unix-socket path length plausibly
  explains EINVAL, not EPERM, but neither cause was verified by a retry.
  No denied socket operation was retried
  or moved to another path.
- Issuer Python: 135 passed, 4 errors, 2 skipped. Runtime UID1000 conflicts with
  trusted-parent UID0/1001 assumptions. Skipped cases require root/real Compose.
- Issuer Node: 21 passed, 1 failed. CLI statically imports mongodb, unavailable in
  the dependency-free source snapshot. No dependencies were installed.
- Linux compile was not run: read-only doctor reports missing cargo and rustc.
- Both selected application profiles therefore failed. No full-CI pass.

The executed runner incorrectly recorded zero skips for a FAILED unittest summary.
Raw logs show two. The local runner now parses FAILED summaries correctly with a
regression test; raw run evidence has not been rewritten. The local unit suite
after that correction is 81 passing tests per draft, including exporter tests.
A later export selection adds vendor/neo4rs for compile completeness; the source
snapshot above did not include it and was not used for compilation.

## Boundaries and next prerequisites

The first workspace mkdir failed in the default read-only sandbox. The same
operation was subsequently requested through the standard narrow access-review
route and approved, as were extraction and ordinary test execution. No reviewed
denial or alternate path was used to bypass that restriction.

No Docker socket, ps, blocked GitHub receipt-capture job, production service,
workflow, billing or CI-permission change was attempted. No application tests
were retried after their access errors.

Full fallback parity still needs an approved disposable Linux execution profile:
permitted Unix sockets and correct fixture UID, a prepared Rust1.94.0 and locked
dependency cache, issuer Node dependencies, and separately authorized isolated
real-service fixtures. Native macOS-only gates remain separate. Resolve access
denials through permission review or operator action, never by alternate routing.
Do not treat changing fixture assumptions or skipping failures as qualification.

## Later implementation verification

The final dependency-free local suite passes 92 tests per draft. Current-source
export reproducibility, approved-but-blocked npm setup, corrected Rust prerequisites
and the complete coverage ledger are in [COVERAGE.md](COVERAGE.md). No historical application
run or original Linux evidence was replaced by this later implementation work.
