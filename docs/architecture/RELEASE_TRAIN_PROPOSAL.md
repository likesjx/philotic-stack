---
title: Release Train — Sync main, Tag v0.2.0, Ship Hotels From Release Artifacts
doc_type: proposal
domain: deployment-distribution
status: proposed
last_updated: 2026-10-05
tags:
- release
- ci
- artifacts
- deployment
- rollback
- versioning
related_docs:
- RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md
- HOMEBREW_DISTRIBUTION_PROPOSAL.md
- GUEST_BINARY_RESOLUTION_PROPOSAL.md
- RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md
- IMPORTANT_INCOMPLETE_ITEMS_PROPOSAL.md
proposal_id: release-train
implements: []
implemented_by: []
active_seams:
- release-main-sync
- release-version-stamp
- release-ci-artifacts
- release-install-linux
- release-install-macos
- release-rollback
- release-rollout-proof
---

# Release Train — Sync main, Tag v0.2.0, Ship Hotels From Release Artifacts

Origin: Philotic Stack Atlas assessment (2026-09-30), next seam #4. Facts below
were re-checked against `origin/develop` @ `f170cb88` on 2026-10-05.

## Problem

- **No release since April.** Tags `v0.1.0-alpha` (03-21), `v0.1.0-beta`
  (03-23) and `v0.1.0-rc1` (04-11) were made by hand. None matches the
  `release.yml` tag patterns, so the release workflow has **never run**, and
  there are no GitHub Releases.
- **`main` is 416 commits behind `develop`.** It is a strict ancestor, so it
  can fast-forward. `CONTRIBUTING.md:37-40` calls `main` "stable", but nothing
  ships from it.
- **Hotels run whatever was last built somewhere:**
  - **vps-jane** runs a 14-day-retention `build-linux` workflow artifact from
    develop (`just vps-deploy-ci`, `justfile:865-923`). Selecting that artifact
    picked a stale run in DEF-213 (fixed in #610).
  - **The Macs** run a dev Mac's `target/release`, copied into a mutable
    Homebrew Cellar directory pinned to `0.1.0-alpha` (`justfile:581-582`,
    `scripts/deploy-mac-jane.sh:35-37`). Builds use Homebrew rustc, so they are
    not reproducible.
- **No rollback on Linux.** The ansible role copies in place with no backup
  (`ansible/roles/philotic_hotel/tasks/main.yml:103-157`). "Rollback" means
  redeploying an older CI run, and only while it is under 14 days old.
- **No provenance.** Every crate reports `0.1.0`. `PHILOTIC_BUILD_SHA` is
  baked in only by `build-linux`, and only datasource guests log it.
  `binary_sha256` exists only in the intel-graph dev server.
- **Package lists drift:**

  | Where | Packages |
  |---|---|
  | `build-linux`, `pr-check`, `vps-push`, `local-push` | 16 |
  | `push-homebrew-remote` | 13 |
  | `release.yml` | 6 |
  | ansible `philotic_binaries` | 28 names, including the nonexistent `agent-core` and `hegemon` |

## Goal

One command produces a versioned, hashed, provenance-stamped release for the
two real platforms: linux x86_64 (vps-jane) and macOS arm64 (mac-jane,
mbp-jane). Every hotel installs that release by tag, can prove it is running
it, and can roll back to the previous tag in under a minute.

### Non-goals

- Windows, Intel macOS and Linux arm64 builds. No hotel runs them.
- Notarized macOS distribution and Homebrew bottles. Ad-hoc signing stays;
  bottles are a follow-up.
- The full artifact control plane in `RUNNER_ARTIFACT_BUILD_DISTRIBUTION`
  (builder / distributor / materializer guests). This proposal is its
  operator-driven precursor. That proposal stays `deferred`.
- The Apple app release process (TestFlight / notarization). Tracked
  separately.

## Design

```
develop (green pr-check + build-linux)
   │  RC tag v0.2.0-rc.N  → release.yml (pre-release, validates on develop)
   ▼  fast-forward
main ── tag v0.2.0 ──► release.yml
                          ├─ linux-x86_64  (ubuntu-24.04)  ┐  tarball + SHA256SUMS + manifest.json
                          └─ darwin-arm64  (macos-14)      ┘  (+ build provenance attestation)
                                │ GitHub Release (assets never expire)
          ┌─────────────────────┴──────────────────────┐
  just vps-deploy-release v0.2.0          scripts/install-release-mac.sh <host> v0.2.0
  /opt/philotic/releases/v0.2.0/bin       ~/.philotic/releases/v0.2.0/bin
  current → v0.2.0 (symlink flip)         current → v0.2.0 (symlink flip)
          └──────────────► just verify-release <host> v0.2.0 ◄──────┘
                         (live build_sha + binary hashes == manifest.json)
```

A single package manifest, `release/packages.toml`, is the only list of
deployable binaries. Every workflow, the ansible role and every push script
reads it.

## Slices

### R0 — Release runbook and freeze rules (S)

- Write `docs/process/RELEASE.md`. It covers:
  - the two precedents: #349 was a `-s ours` sync merge; #415 was a direct
    squash to `main`;
  - RC tags on develop, then stable tags on main;
  - who can tag;
  - what "green" means: `pr-check` (fmt, linux check, clippy ratchet, macOS
    test) **and** `build-linux` on the same SHA.
- Mark `docs/legacy/release-process.md` (ZeroClaw) as historical.
- Optional, operator decision: re-enable the disabled "Protect & Serve" ruleset
  on `main` so only fast-forwards or release merges land there.
- **Done when:** the runbook is merged and the operator signs off on the freeze
  rules.

### R1 — Sync main (S)

1. Confirm `git merge-base --is-ancestor origin/main origin/develop`. It was
   true on 2026-10-05; main has 0 commits that develop lacks.
2. Pick the release SHA: the newest develop merge with green `pr-check` and
   `build-linux`.
3. Open a PR `develop → main` and merge it **with a merge commit**, as #349
   did, so the release boundary is visible in history. A fast-forward also
   works if the operator prefers a linear `main`.
4. Record the SHA in `docs/process/RELEASE.md` § history.

- **Done when:** `origin/main` equals the chosen SHA, and develop and main show
  no content diff.

### R2 — Version stamp and provenance (S–M)

1. Add `[workspace.package] version = "0.2.0"` to `Cargo.toml`, and switch the
   34 members to `version.workspace = true`.
2. Remove or archive the two stray crate directories:
   - `crates/agent-graph-runner` (orphan, superseded by agent-datasource);
   - `crates/philotic-primitives-mesh`. Confirm whether anything still
     path-depends on it before deleting.
3. Generalize `philotic_client::build_sha()` (`crates/philotic-client/src/lib.rs:41-46`):
   - add a `philotic_client::build_info()` returning `{version, sha, built_at}`;
   - have every binary print it in `--version` (clap `version = build_info_str()`);
   - have every guest log it once at startup, not just datasources (`crates/datasource/src/runtime.rs:117`).
4. Add `version`, `build_sha` and `binary_sha256` to:
   - aiua hotel status (`GetHotelStatus`), plus the `hotel` record's `build_version` (`crates/aiua/src/main.rs:1033`);
   - philotic-web `/api/status` and `/health` (`serve.rs:2407-2428`, `:7266-7272`).
   Reuse `server_identity()` from `crates/graph-intelligence/src/server/mod.rs:121-136`
   by moving it into `philotic-client` or `ansible-mesh-core`.
5. `scripts/deploy-mac-jane.sh` and `push-homebrew-remote.sh` export
   `PHILOTIC_BUILD_SHA=$(git rev-parse HEAD)`, so interim Mac builds stop
   reporting `unknown`.

- **Tests:**
  - a unit test that `build_info()` returns a non-empty version;
  - a philotic-web route test asserting `/api/status` has `build_sha`;
  - an aiua status snapshot test.
- **Done when:** `aiua --version` prints `0.2.0 (<sha>)` on all three hotels
  after the next deploy.

### R3 — Release workflow that matches the fleet (M–L)

Rewrite `.github/workflows/release.yml`:

1. **Matrix:**
   - `x86_64-unknown-linux-gnu` on **ubuntu-24.04**. The `ort` prebuilt
     binaries need glibc ≥ 2.38, as `build-linux.yml:29-34` documents.
   - `aarch64-apple-darwin` on **macos-14**.
   - Drop the Windows targets: 15 crates use `std::os::unix`. Also drop
     `macos-13` (retired) and Linux arm64.
2. **Build steps:**
   - install `libopus-dev` on Linux and `opus` on macOS;
   - reuse the desktop-dist staging from `build-linux.yml:60-98`. Do **not** set
     `PHILOTIC_DESKTOP_DIR`, which triggers the broken npm build;
   - set `PHILOTIC_BUILD_SHA=${{ github.sha }}`;
   - build every package listed in `release/packages.toml`.
3. **Assets:**
   - `philotic-<tag>-<os>-<arch>.tar.gz`, containing `bin/*`, `SHA256SUMS` and
     `manifest.json` (`{version, sha, built_at, target, bins:[{name, sha256}]}`);
   - an outer `SHA256SUMS` over the tarballs;
   - `actions/attest-build-provenance` on each tarball.
4. **Tag patterns:** keep them, and document that tags need three parts
   (**`v0.2.0`, not `v0.2`**). Keep `validate-branch`: stable tags on main,
   pre-release tags on develop.
5. **Tap job:** retarget it to the formula name the README documents
   (`philotic-web`), or disable it until the tap repo is audited (it returned
   403 from the cloud session).
6. **Shared manifest:** `build-linux.yml`, `pr-check.yml`, `just vps-push`,
   `local-push`, `push-homebrew-remote.sh` and the ansible `philotic_binaries`
   all read `release/packages.toml`. A small `scripts/release-packages.sh`
   prints `-p` flags and bin names. Delete `agent-core` and `hegemon` from the
   ansible list.

- **Rehearsal:** tag `v0.2.0-rc.1` on develop. It should produce a draft
  pre-release with both tarballs. Download each, verify hashes, and run
  `bin/aiua --version` on each platform.
- **Done when:** the rc.1 pre-release exists, assets verify, and both platforms
  run `--version` successfully.

### R4 — Install from release on Linux (M)

1. Add the recipe `just vps-deploy-release <tag>`, modeled on `vps-deploy-ci`:
   - resolve the release asset URL with `gh api repos/:owner/:repo/releases/tags/<tag>`;
   - curl it on the VPS into `/home/deploy/release-cache/<tag>/`;
   - `sha256sum -c`;
   - run ansible with `philotic_release_tag=<tag>`.
2. Change the ansible role (`ansible/roles/philotic_hotel/tasks/main.yml:103-157`):
   - unpack into `/opt/philotic/releases/<tag>/bin`;
   - atomically flip the `/opt/philotic/current` symlink;
   - point `PHILOTIC_BIN_DIR` at `/opt/philotic/current/bin` in
     `templates/philotic-hotel.service.j2:21,70`;
   - keep the newest 3 releases.
3. Fix `ansible/host_vars/jane-vps.yml:52-54`: `philotic_artifacts_dir` stops
   pointing at the on-host `target/release`.
4. `vps-deploy-ci` keeps working for develop builds. It installs into
   `releases/ci-<sha>`, which gets the same rollback.

- **Done when:** vps-jane runs `v0.2.0` from `/opt/philotic/releases/v0.2.0`,
  and `just verify-release vps-jane v0.2.0` passes.

### R5 — Install from release on macOS (M)

1. Add `scripts/install-release-mac.sh <host> <tag>`. It:
   - downloads the darwin-arm64 tarball and verifies it;
   - runs `xattr -d com.apple.quarantine`;
   - unpacks into `~/.philotic/releases/<tag>/bin`, using new-inode installs as
     in `deploy-mac-jane.sh:68-90`;
   - ad-hoc re-signs with `codesign -f -s -`;
   - flips `~/.philotic/current`;
   - restarts through the existing launchd bootout/kickstart logic, extracted
     from `push-homebrew-remote.sh` into a shared function.
2. Point the launchd plists' `PHILOTIC_BIN_DIR` at `~/.philotic/current/bin`
   instead of the `0.1.0-alpha` Cellar path. Leave the Cellar copy in place
   for one release as a fallback.
3. Change the freshness check (`scripts/deploy-freshness-check.sh`, from
   DEF-052) for release installs: it now checks that the tag is at least the
   fleet's expected tag, instead of "tree contains origin/develop".

- **Done when:** mac-jane and mbp-jane both run `v0.2.0` from release assets,
  and `verify-release` passes.

### R6 — Rollback (S–M)

1. Add `just rollback <host> [<tag>]`. With no tag it flips `current` to the
   previous release, then restarts and verifies. It works on Linux (ansible
   task tag `rollback`) and on macOS (the script with `--rollback`).
2. Run the drill once per platform: deploy `v0.2.0-rc.2`, roll back to
   `rc.1`, and confirm `--version` and `verify-release`.

- **Done when:** a recorded rollback drill on vps-jane and one Mac each
  finishes in under 60 s of hotel downtime.

### R7 — Rollout proof (M)

1. Add `just verify-release <host> <tag>`. It reads `manifest.json` from the
   release and compares against the live hotel:
   - `build_sha`, using hotel status from R2;
   - every installed binary's sha256 against the manifest;
   - every running guest's logged `build_sha`.
2. Record the result in the Intel Graph with `graph_record_test_run`, kind
   `rollout`, so a release counts as SMOKE-GREEN only with a proof per hotel.
3. Add a line to the `runtime-rollout-watch` skill: a release is
   watched-live-green only after `verify-release` passes on all three hotels
   and a Telegram round trip succeeds on each persona hotel.

## Ordering and dependencies

```
R0 ─► R1 ─► (R2 ∥ R3) ─► rc.1 rehearsal ─► R4 ∥ R5 ─► R6 ─► R7 ─► tag v0.2.0 on main
```

R2 and R3 can proceed in parallel worktrees. R2 touches crates; R3 touches
CI. R4 and R5 are independent once release assets exist.

## Risks

- **Desktop UI embed.** `philotic-web/build.rs` pulls the private
  `jaredlikes-desktop` dist. The release job needs `DESKTOP_REPO_TOKEN` (not
  `TAP_GITHUB_TOKEN`), or it ships a placeholder UI. Decide before R3.
- **Homebrew rustc vs rustup.** Mac CI builds use rustup's toolchain from
  `rust-toolchain.toml`, while local Mac builds use Homebrew rustc. Accept the
  mismatch: releases come from CI only.
- **launchd plist edits** on two Macs are manual and easy to get wrong. Script
  them in R5, and keep the Cellar fallback for one release.
- **Mesh version skew during a rolling upgrade.** Upgrade vps-jane last, since
  it holds the Cortex and LifeGraph. Watch for the DEF-182 deserialize drops
  (see `MESH_DELIVERY_GUARANTEES_PROPOSAL.md`, which should land first or with
  this).

## Definition of done

`v0.2.0` exists as a GitHub Release with linux-x86_64 and darwin-arm64
tarballs, hashes and attestations. All three hotels run it from a versioned
release directory, and each reports `0.2.0 (<sha>)`. `verify-release` passes
on each, and a rollback drill is recorded per platform. `docs/process/RELEASE.md`
is the runbook for v0.3.0.

## Operator decisions needed

1. Merge commit or fast-forward for the develop → main sync?
2. Re-enable the "Protect & Serve" ruleset on `main`?
3. Release UI source: the token for `jaredlikes-desktop` in the release job, or
   ship without the desktop UI?
4. Keep the Homebrew tap job, or drop it until the tap repo is audited?
