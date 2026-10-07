---
title: Release Runbook
doc_type: process
domain: deployment-distribution
status: active
last_updated: 2026-10-07
related_docs:
- ../architecture/RELEASE_TRAIN_PROPOSAL.md
- WORKFLOW.md
---

# Release Runbook

> Status: active. The operator must sign off on the freeze rules before the
> first release (`proposal:release-train` slice R0). This runbook replaces
> `docs/legacy/release-process.md`, which describes ZeroClaw and is historical.

A release is a tag. `.github/workflows/release.yml` turns the tag into a GitHub
Release that holds two tarballs, one for linux-x86_64 (vps-jane) and one for
darwin-arm64 (mac-jane and mbp-jane). Release assets do not expire. Hotels
install a release by tag, verify it and can roll it back.

## Branches and tags

| Tag | Example | Tag it on | Release type |
|---|---|---|---|
| Release candidate | `v0.2.0-rc.1` | `develop` | pre-release |
| Alpha or beta | `v0.2.0-alpha.1`, `v0.2.0-beta.1` | `develop` | pre-release |
| Stable | `v0.2.0` | `main` | release |

- **Tags need three numeric parts.** `v0.2` and `v0.2.0-rc1` do not match the
  workflow's tag patterns, so the release workflow never runs for them. That is
  why the hand-made `v0.1.0-alpha`, `v0.1.0-beta` and `v0.1.0-rc1` tags never
  produced a release. Pre-release tags need a dot: `-rc.1`, not `-rc1`.
- The `validate-branch` job fails the run if a stable tag is not on `main`, or a
  pre-release tag is not on `develop`.
- Only the operator (`likesjx`) pushes release tags. Agents prepare the release
  and write the commands, but they do not push tags.

## What "green" means (freeze rule)

A SHA can be released only when **both** of these workflows succeeded on that
exact SHA:

1. `PR Check` (`pr-check.yml`): rustfmt, the Linux `cargo check` over the
   release package set, the clippy ratchet and the macOS `cargo test` job.
2. `Build Linux (develop)` (`build-linux.yml`): the full linux-x86_64 release
   build and its required-binary check.

A green PR run does not count, and neither does a green run on a parent commit.
Look the SHA up directly:

```bash
SHA=$(git rev-parse origin/develop)
gh api "repos/likesjx/philotic-stack/commits/${SHA}/check-runs" \
  -q '.check_runs[] | "\(.name)\t\(.conclusion)"'
```

From the moment the RC tag is pushed until the stable tag ships, nothing merges
to `develop` except fixes for that release. A fix gets a new RC tag (`rc.2`, and
so on).

## Package manifest

`release/packages.toml` is the only list of deployable binaries. Read it with
`scripts/release-packages.sh`:

```bash
scripts/release-packages.sh cargo-flags --platform linux   # -p aiua -p philote ...
scripts/release-packages.sh bins --platform darwin         # one binary name per line
scripts/release-packages.sh required-bins                  # bins a build must produce
```

`build-linux.yml`, `pr-check.yml` (the Linux check) and `release.yml` all read
it. If you add a guest binary, add it there. The ansible `philotic_binaries` list
is checked against it by `scripts/tests/test_release_manifest.py`.

## Precedents

- **#349 (2026-07-24): sync merge with `-s ours`.** `main` had drifted, so
  develop's history was merged into main with `git merge -s ours` and develop's
  content kept. Use it only when `main` holds commits that `develop` lacks.
- **#415 (2026-08-06): squash straight to `main`.** Licensing and self-host
  docs were squash-merged into `main` so that GitHub's license detection
  (which reads the default branch) saw them. It was a one-off. It is not a
  release path, and it left `main` holding a commit `develop` did not.

The normal path is the one below: a PR from `develop` into `main`, merged with a
merge commit.

## Releasing v0.2.0

1. **Sync `main` (R1).** Check that main is an ancestor of develop:

   ```bash
   git fetch origin
   git merge-base --is-ancestor origin/main origin/develop && echo "ok: fast-forwardable"
   ```

   Pick the release SHA: the newest develop commit that meets the freeze rule.
   Open a PR from `develop` into `main` and merge it **with a merge commit**,
   which is the operator's decision. Then confirm that `git diff origin/develop
   origin/main` is empty, apart from commits added to develop after the SHA you
   picked.

2. **Rehearse with `v0.2.0-rc.1` on develop.**

   ```bash
   git tag -a v0.2.0-rc.1 <sha> -m "v0.2.0-rc.1"
   git push origin v0.2.0-rc.1
   ```

   Wait for the `Release` workflow to finish. Then download both tarballs,
   verify them, and run `bin/aiua --version` on each platform:

   ```bash
   gh release download v0.2.0-rc.1 -R likesjx/philotic-stack -D /tmp/rc1
   cd /tmp/rc1 && shasum -a 256 -c SHA256SUMS
   gh attestation verify philotic-v0.2.0-rc.1-darwin-arm64.tar.gz -R likesjx/philotic-stack
   mkdir x && tar -xzf philotic-v0.2.0-rc.1-darwin-arm64.tar.gz -C x
   python3 scripts/release-manifest.py verify --dir x && x/bin/aiua --version
   ```

3. **Install the RC on the hotels**, vps-jane last because it holds the Cortex
   and LifeGraph. The commands are under "Install" below. Run `verify-release`
   on each hotel.

4. **Rollback drill (R6).** Tag `v0.2.0-rc.2`, install it, roll back to `rc.1`
   and time the downtime. The target is under 60 s per hotel. Record the result
   in the history table.

5. **Tag stable on `main`.** After the merge commit from step 1 is on `main`:

   ```bash
   git tag -a v0.2.0 origin/main -m "v0.2.0"
   git push origin v0.2.0
   ```

   Install, verify and record the release in the history table.

## Install

### vps-jane (Linux)

```bash
just vps-deploy-release v0.2.0
```

This resolves the asset URLs with `gh api`. The VPS then downloads the release
from GitHub into `/home/deploy/release-cache/<tag>/` and checks it with
`sha256sum -c`. Next, ansible runs with `-e philotic_release_tag=<tag>`, and the
role does four things:

- unpacks the tarball into `/opt/philotic/releases/<tag>/` (`bin/`,
  `SHA256SUMS`, `manifest.json`) and checks the inner sums;
- swaps `/opt/philotic/current` to point at that release in one atomic rename;
- points the systemd unit at `/opt/philotic/current/bin` (`PATH`,
  `PHILOTIC_BIN_DIR`, `ExecStart`) and restarts the hotel;
- keeps the newest 3 releases and never deletes the one `current` points at.

Once `/opt/philotic/current` exists, the host stays in release mode. `just
vps-config` and other runs without a tag keep the unit on `current/bin`.
`just vps-deploy-ci` still ships develop builds the old way. It passes
`philotic_bin_layout=legacy`, which copies into `/opt/philotic/bin` and points
the unit back there. Run `just vps-deploy-release <tag>` to return to a release.

### mac-jane and mbp-jane (macOS)

```bash
scripts/install-release-mac.sh local v0.2.0                     # mac-jane, this machine
scripts/install-release-mac.sh mbp-jane v0.2.0                  # mbp-jane over ssh
scripts/install-release-mac.sh mbp-jane v0.2.0 --hotel mbp-jane # explicit hotel name
```

The script does the following:

1. Downloads the darwin-arm64 tarball and checks it against the release's
   `SHA256SUMS`.
2. Removes the quarantine attribute, unpacks into
   `~/.philotic/releases/<tag>/` (new files, so new inodes) and checks the
   inner sums before signing.
3. Re-signs ad hoc with `codesign -f -s -`. It records the hashes after signing
   in `INSTALLED_SHA256SUMS`, because re-signing can change a binary's sha256.
4. Stops the hotel through launchd (bootout), swaps `~/.philotic/current`,
   clears the stale `active_pid` and starts the hotel again through launchd.
5. Keeps the newest 3 releases.

**One-time plist change.** The launchd plists still point at the
`0.1.0-alpha` Homebrew Cellar. The script does not edit plists. When the plist
does not point at `~/.philotic/current/bin`, the script prints the commands
below and skips the restart, because a restart would not change anything:

```bash
PLIST=~/Library/LaunchAgents/com.philotic.aiua.<hotel>.plist
/usr/libexec/PlistBuddy -c "Set :ProgramArguments:0 $HOME/.philotic/current/bin/aiua" "$PLIST"
/usr/libexec/PlistBuddy -c "Set :EnvironmentVariables:PHILOTIC_BIN_DIR $HOME/.philotic/current/bin" "$PLIST"
# if the plist sets PATH, put ~/.philotic/current/bin first:
/usr/libexec/PlistBuddy -c "Print :EnvironmentVariables:PATH" "$PLIST"
/usr/libexec/PlistBuddy -c "Set :EnvironmentVariables:PATH $HOME/.philotic/current/bin:/opt/homebrew/bin:/usr/bin:/bin" "$PLIST"
launchctl bootout gui/$(id -u)/com.philotic.aiua.<hotel>; launchctl bootstrap gui/$(id -u) "$PLIST"
```

Leave the Cellar copy where it is for one release as a fallback. To fall back,
point the two keys back at `/opt/homebrew/Cellar/aiua/0.1.0-alpha/bin` and
bootstrap again.

The release binaries link Homebrew's `opus` (`/opt/homebrew/opt/opus`), which
both Macs already have. The `phil` CLI on the Macs still comes from Homebrew.

## Verify

```bash
just verify-release vps-jane v0.2.0
just verify-release mac-jane v0.2.0
just verify-release mbp-jane v0.2.0
```

`scripts/verify-release.sh` downloads `manifest.json` from the release, hashes
every installed binary on the host and prints a PASS/FAIL table. It exits
non-zero when any of these is true:

- a binary's hash does not match;
- a binary is missing;
- `current` does not point at the tag;
- no hotel process is running, or the running `aiua` does not come from the
  release (`releases/<tag>/bin` or `current/bin`).

On macOS, re-signing can change a binary's hash. Such a binary reports
`RESIGNED`, which counts as a pass, only when both of these hold: the release
directory's `SHA256SUMS` (the pre-signing hashes from the tarball) matches the
manifest, and the binary still matches the post-signing hash recorded in
`INSTALLED_SHA256SUMS`.

The script also prints `aiua --version`. Until slice R2 (version stamping)
lands, this shows `0.1.0` with no build SHA. The script reports that as `build
sha: not reported` instead of failing.

A release is **watched-live-green** only after `verify-release` passes on all
three hotels **and** a Telegram round trip succeeds on each persona hotel. See
`skills/runtime-rollout-watch/SKILL.md`.

## Rollback

```bash
just rollback vps-jane            # previous release (newest that is not current)
just rollback vps-jane v0.2.0-rc.1
just rollback mac-jane            # scripts/install-release-mac.sh local --rollback
just rollback mbp-jane v0.2.0-rc.1
```

On Linux, rollback runs the role's `rollback` task tag. On macOS, it runs
`install-release-mac.sh --rollback`. Either way, it swaps `current` to the
target release and restarts the hotel. "Previous" means the most recently
installed release other than the current one, so two rollbacks in a row toggle
between the same two releases. Only installed releases (the newest 3) can be
targets. To go further back, install the old tag again.

## Homebrew tap

For stable tags only, the `update-tap` job writes `Formula/philotic-web.rb` in
`likesjx/homebrew-philotic` from the darwin-arm64 tarball. This is the formula
`README.md` documents. The job needs the `TAP_GITHUB_TOKEN` secret. Without it,
the job logs a notice and skips. The old `aiua` source formula is no longer
bumped.

## Desktop UI

`philotic-web` embeds the private `jaredlikes-desktop` UI. If the
`DESKTOP_REPO_TOKEN` secret exists, the release job copies that repo's committed
`dist/` into `crates/philotic-web/ui-dist`, the same way `build-linux` does.
Otherwise the release ships the placeholder UI. Never set
`PHILOTIC_DESKTOP_DIR` in CI: it makes `build.rs` run the broken npm build.

## History

| Date | Tag / event | SHA | Notes |
|---|---|---|---|
| 2026-03-21 | `v0.1.0-alpha` | — | Hand tag. It does not match the workflow patterns, so no release was made. |
| 2026-03-23 | `v0.1.0-beta` | — | Hand tag. No release. |
| 2026-04-11 | `v0.1.0-rc1` | — | Hand tag (`rc1` has no dot). No release. |
| 2026-07-24 | #349 sync of develop into main | `5a7cab45` | `-s ours` merge that kept develop's content. |
| 2026-08-06 | #415 squash to main | `f527fb38` | Licensing docs, needed for license detection. |
| _pending_ | develop → main sync (R1) | | Merge commit. |
| _pending_ | `v0.2.0-rc.1` | | Rehearsal. |
| _pending_ | `v0.2.0` | | First release built from artifacts. |
