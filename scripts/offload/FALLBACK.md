# Ordinary fallback CI

This is the executable source-test route. It is separate from `run.py`'s
original build/qualification stages and `plan.py`'s non-executing mapping.
It does not dispatch GitHub jobs, use Docker, install tools, deploy, publish
checks, or run the budget-blocked fresh-authority receipt-capture job.

## Use

Use reviewed source and an authorized disposable environment. The runner is
not a security sandbox. Node 22+ and Python 3.11+ are the base requirements.
Git is needed by the temporary-repository safety suite. Tests use synthetic
data, local fixture files and loopback HTTP servers, never production endpoints.

Export an already available local commit without changing the dirty worktree:

```sh
python3 -B scripts/offload/export_source.py \
  --repo /path/to/repository --ref refs/remotes/origin/develop \
  --project likesjx/philotic-stack --destination /new/path/native-source
```

For the website, use project `likesjx/jaredlikes-com` and the desired locally
available main commit/ref. The exporter prints the exact commit and the
identity-manifest digest. Retain these separately when transferring the snapshot.
No fetch occurs. Uncommitted source is deliberately excluded; this result cannot
be used to claim that uncommitted work passed. The snapshots are selected source,
not full clones or independently authenticated GitHub attestations.

Transfer the snapshot and this runner using the environment's authorized file
transfer. Verify the bundle and manifest digests before execution. Keep evidence
outside the snapshot, in a new directory whose parent already exists.

```sh
python3 -B scripts/offload/fallback.py --profile native-core \
  --source-root /path/native-source --expect-sha EXACT_COMMIT --doctor

python3 -B scripts/offload/fallback.py --profile native-core \
  --source-root /path/native-source --expect-sha EXACT_COMMIT \
  --evidence /path/evidence/new-run

python3 -B scripts/offload/fallback.py --profile issuer-contracts \
  --source-root /path/issuer-source --expect-sha EXACT_COMMIT \
  --evidence /path/evidence/new-issuer-run
```

Equivalent local recipes are `just fallback-doctor PROFILE SOURCE SHA` and
`just fallback-ci PROFILE SOURCE SHA EVIDENCE`.

## Coverage

- native-portable-verification: explicitly limited egress inventory and Python
  contracts/worktree safety checks. It does not run or waive Node/socket gates.
  It is the dependency-free subset verified on the current native commit on Mac;
  it is not a substitute for native-core or Linux integration.
- `native-core`: production egress inventory and negative tests, release
  manifest contracts, disposable worktree retirement safety, and the full
  selected personal-MCP Node test suite. This includes real loopback HTTP
  gateway/broker interactions with synthetic fixtures.
- `issuer-contracts`: Python issuer/activation contracts and Node admission/offline
  CLI checks. CLI import needs a prepared mongodb dependency. Engine and Docker
  interactions are mocked.
- `native-linux-build`: native-core first, then the Linux release-package
  compile selection from `release/packages.toml`, with Rust 1.94.0, locked
  dependencies and offline Cargo. This profile is implemented but not qualified.
  It needs a separately approved, prepared toolchain/dependency-cache environment.
  The isolated HOME does not reuse personal Rustup/Cargo state.

Not covered: full macOS workspace Clippy/tests, real Mongo/Memgraph/container
composition, browser tests, website dependency/TypeScript/Next builds, artifact
publication, native package qualification, or required GitHub checks. A green
selected profile is never a full-CI, deployment, or permission decision.

## Evidence and failure behavior

`report.json` records exact source/manifest, runner and profile hashes, stage
arguments, log hashes, test counters and explicit uncovered scope. Reports also
record the running Python version and resolved executable paths/SHA256 identities.
Binary identity is not a complete shared-library or operating-system attestation.
`full_ci_passed` always remains false. Exit 0 means only the selected profile
passed; 1 is a test/source failure; 78 is incomplete or unavailable.

Every stage verifies unchanged snapshot content afterward. Missing tools,
missing source, zero discovered tests, skips, TODO/cancelled tests, expected
failures, inconsistent summaries and dependency failures are not green. Reported
failures or unexpected successes cannot be overridden by an exit status of zero. Logs and an atomic final report survive ordinary failures. The child
environment excludes inherited credentials, uses isolated HOME/tmp, disables
automatic installs and uses only local file Git remotes for synthetic tests.

Signals are deferred across spawn. Timeouts stop owned process groups including
surviving descendants. Unresolved cleanup retains process/log evidence and stops
the profile; do not retry or launch more work until an operator resolves it.
SIGKILL, machine loss and power loss can still leave a running report or resources.
There is no durable restart/recovery service.

Immediate OS-delivered cancellation was not qualified on the selected Mac:
the regression hit an access restriction and was replaced by a fully mocked
handler/child test. Timeout cleanup and a TERM-ignoring descendant are covered
by local subprocess tests. Never retry denied operations through another route.

## Validation

```sh
python3 -B -m unittest discover -s scripts/offload -p 'test_*.py' -v
```

Synthetic runner/library tests do not count as application integration passes.
Actual Linux run outcomes, source SHAs and remaining blockers belong in the
separate run evidence; do not rewrite a failed result or reuse stale evidence.

See [PROVISIONING.md](PROVISIONING.md) for the exact next setup decision,
current-source status and non-executing twelve-package dependency planner.

See [COVERAGE.md](COVERAGE.md) for the consolidated current evidence and exact
remaining environment blockers. No report from an older runner is rewritten.
