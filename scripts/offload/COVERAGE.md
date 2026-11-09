# Fallback CI coverage and next steps

As of November 9, 2026, **full CI has not passed**. Both ordinary Linux application
profiles failed. This report separates application results, local runner tests
and current-source reproducibility. Machine-readable counts and evidence hashes
are in [COVERAGE.json](COVERAGE.json).

## What actually ran on Linux

| Stage | Passed | Failed/errors | Skipped | Result |
| --- | ---: | ---: | ---: | --- |
| Native egress inventory | n/a | 0 | 0 | Passed |
| Native Python contracts | 36 | 0 | 0 | Passed |
| Personal-MCP Node suite | 130 | 10 | 0 | Failed |
| Issuer Python contracts | 135 | 4 errors | 2 | Failed |
| Issuer admission Node suite | 21 | 1 | 0 | Failed |
| Linux Rust compile | n/a | n/a | n/a | Not run |

The application runs used native **b4c44b8** and issuer **e408c47**, Node 24.22.1,
Python 3.12.14, and runner ff7d1b92. Exact identities are in COVERAGE.json and the
unchanged evidence archive. The issuer raw report incorrectly says zero skips;
the raw log says two. This report corrects the count without rewriting evidence.

Successful integration includes real loopback HTTP discovery and recall,
separate-connection policy revision/revocation, and a synthetic issuer OAuth
lifecycle (discovery, registration, consent, PKCE, token exchange, revocation).
Issuer mutation/CAS/reconciliation tests use doubles, not real Mongo.

## What failed or cannot run

- Six Unix handoff listeners failed EPERM. Four owner listeners failed EINVAL.
  The latter paths were 113 bytes; excessive Unix-socket path length is plausible
  but not established by a retry. The EPERM paths were 104 bytes. None was retried
  or relocated.
- Four issuer Python fixtures conflict with UID1000 versus the production
  trusted-parent UID0/1001 guard. Two root/real-Compose cases were skipped.
- One Node CLI test cannot import mongodb. Its twelve-package installation is
  already approved, but both official-registry attempts failed EAI_AGAIN with
  zero downloaded or installed packages. Verified local cache bytes remain
  untouched and are not an authorized alternate route.
- Rust/Cargo are absent. Separate toolchain/dependency approval, generated-output
  handling and the failed native-core prerequisites remain unresolved.
- Real databases, containers, browser, linked native packages, macOS gates,
  deployment and required GitHub checks are not qualified. The budget-blocked
  receipt-capture job remains out of scope.

## What is now reproducible

Both current remote heads were rechecked: native develop **732bf080** and issuer
main **e408c470**. Each already-local commit was exported twice, with identical
per-file identities and manifest digests. This did not execute application tests.

- Native: 985 files, including vendor/neo4rs; manifest
  d302369e3c762e0287670f98de9f3a91bb5b5fa0de588365fd9a3ebee8809129.
- Issuer: 331 files; manifest
  7ecc952d86fda83479fb772726f311f1af353db55f031f77a83e3a3f134820e2.
  All file hashes match the previously tested issuer snapshot. Only the manifest's
  reference field changed from the symbolic ref to the exact SHA.
- The non-executing twelve-package dependency plan also matched byte-for-byte
  across both exports, SHA256
  27566adcc427e34ab286832ef2e69c8df7388ae731174cc5a1048549655245df.

The new native source is nine commits beyond the historical Linux-tested source.
After export verification, its unaffected portable gates passed on macOS:
egress inventory plus all 36 Python contracts, zero failures or skips. Source
identity remained unchanged. This separately named profile is reusable:

    python3 -B scripts/offload/fallback.py --profile native-portable-verification --source-root EXACT_SNAPSHOT --expect-sha 732bf080197842f4cc0fbc1d82c02e7494f370db --evidence NEW_DIRECTORY

The full native-core and native-linux-build profiles still require the Node gate.
No failed socket/Node gate was retried, moved or waived. This is not Linux
integration or a full application pass. Reproduce exports with the exact commit
rather than a movable branch; see FALLBACK.md.

## Runner, cleanup and review

The dependency-free local suite passes **92 tests per draft**, independently
reviewed. TODO, cancellation, expected failure, missing/ambiguous summary and
reported failure can no longer hide behind exit zero. Reports record Python
version and executable paths/hashes without launching extra version probes.
Git is an explicit prerequisite for the disposable worktree tests.

Artifact verification and fixture lifecycle/cleanup remain synthetic interfaces.
Timeout cleanup was tested with disposable subprocesses; immediate OS-delivered
cancellation remains unqualified, with a mocked regression after its restriction.
No new live-container adapter or policy change was introduced.

Both worktrees contain uncommitted work and are retained. No worktree, source
snapshot or original evidence was removed. No commits, pushes, merges, GitHub
settings or VPS changes were made by this work.

## Exact next step

The immediate blocker is a supported authorized personal-account registry-access
route for the already-approved npm preparation. Socket/UID support and isolated
container qualification are separate later execution requirements. Current
personal-dot documentation does not verify a network exception control for this
running cloud. Work-only toggles and enterprise admin controls are not personal
instructions. Product feedback is the smallest documented escalation; no feedback
was submitted here. Do not repeat the same install approval or retry denied
operations while this remains unresolved.

[PROVISIONING.md](PROVISIONING.md) records official setup sources and the corrected
minimal dependencies: pinned ort-sys permits offline cargo check without ONNX
acquisition; successful Opus pkg-config discovery avoids CMake. Neither statement
qualifies a linked/runtime build.
