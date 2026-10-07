# Pending workflow patches

Changes to `.github/workflows/` that a cloud session could not push: the
Claude GitHub App used by cloud sessions lacks the `workflows` permission, so
GitHub rejects any push that touches workflow files.

Apply from a checkout with normal credentials (a Mac session or the operator):

```bash
git apply --check docs/process/workflow-patches/<file>.patch
git apply docs/process/workflow-patches/<file>.patch
git add .github/workflows && git commit -m "ci: apply <file>"
git rm docs/process/workflow-patches/<file>.patch   # once applied
```

| Patch | What | Proposal |
|---|---|---|
| `0001-pr-check-docs-lint.patch` | Non-blocking `fmt`-job step: script unit tests + `docs-metadata-check.py --warn-only` (status vocabularies) | WATCH_LIVE_BURNDOWN W0 |
| `0002-workflows-read-package-manifest.patch` | `build-linux.yml` + `pr-check.yml` take their package list from `release/packages.toml` (generated list verified identical to today's 16 packages / 22 required bins) | RELEASE_TRAIN R3 |
| `0003-release-yml-fleet-matrix.patch` | `release.yml` rewrite: linux-x86_64 (ubuntu-24.04) + darwin-arm64 (macos-14), libopus, desktop dist staging, `PHILOTIC_BUILD_SHA`, tarball + `SHA256SUMS` + `manifest.json`, build provenance attestation, GitHub Release, `philotic-web` tap formula | RELEASE_TRAIN R3 |

Apply in order 0001 → 0002 → 0003 (verified to apply cleanly in sequence on
top of this branch). 0003 is a `git format-patch` mail; `git am` works too.
After applying, rehearse with a `v0.2.0-rc.1` tag on develop (docs/process/RELEASE.md).

Alternative: grant the GitHub App the `workflows` permission, after which a
cloud session can push these directly.
