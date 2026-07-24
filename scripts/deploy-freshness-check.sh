#!/usr/bin/env bash
# Deploy freshness guard — sourced by push-homebrew-remote.sh and
# deploy-mac-jane.sh before anything is built or installed.
#
# Why this exists (2026-07-14 incident): a fleet push built from a worktree
# whose HEAD predated origin/develop silently REVERTED two already-merged,
# already-deployed fixes (PR #266 membrane seat filter, PR #272 DEF-051
# instrumentation) on mbp-jane. The next watchdog eviction fanned out through
# every Telegram bot again. A deploy is a statement about the integration
# edge, so the build tree must CONTAIN origin/develop — not merely compile.
#
# Contract:
#   assert_tree_fresh <repo_root> [nonfatal]
#     - fetches origin/develop (warns and compares against the last-known ref
#       when offline)
#     - HEAD missing commits from origin/develop  -> hard abort (exit 1),
#       unless PHILOTIC_DEPLOY_ALLOW_STALE=1 or "nonfatal" is passed (both
#       downgrade to a loud warning)
#     - dirty working tree                        -> warning only (surgical
#       one-off patches are a legitimate, operator-owned deploy mode)
#   warn_stale_artifacts <repo_root> <probe_binary>
#     - warns when the probe binary's mtime predates the HEAD commit time
#       (the tree moved but nothing was rebuilt) — warn-only, because file
#       mtimes are heuristics, not provenance

assert_tree_fresh() {
  local root="$1"
  local mode="${2:-fatal}"

  if ! git -C "${root}" rev-parse --git-dir >/dev/null 2>&1; then
    echo "⚠ freshness guard: ${root} is not a git checkout — cannot verify; deploying blind." >&2
    return 0
  fi

  if ! git -C "${root}" fetch -q origin develop 2>/dev/null; then
    echo "⚠ freshness guard: could not fetch origin/develop (offline?) — comparing against the last-known ref." >&2
  fi

  if ! git -C "${root}" rev-parse --verify -q origin/develop >/dev/null; then
    echo "⚠ freshness guard: no origin/develop ref available — cannot verify; deploying blind." >&2
    return 0
  fi

  local dirty
  dirty="$(git -C "${root}" status --porcelain | wc -l | tr -d ' ')"
  if [[ "${dirty}" != "0" ]]; then
    echo "⚠ freshness guard: working tree has ${dirty} uncommitted change(s) — these ship with this deploy." >&2
  fi

  if git -C "${root}" merge-base --is-ancestor origin/develop HEAD; then
    return 0
  fi

  local behind head_desc
  behind="$(git -C "${root}" rev-list --count HEAD..origin/develop)"
  head_desc="$(git -C "${root}" log -1 --format='%h %s' HEAD)"
  {
    echo "❌ freshness guard: this tree is missing ${behind} commit(s) that are already on origin/develop."
    echo "   HEAD: ${head_desc}"
    echo "   Deploying from here would silently REVERT merged fixes on the target hotel"
    echo "   (exactly how PR #266/#272 were reverted on mbp-jane, 2026-07-14)."
    echo "   Missing merges (up to 5):"
    git -C "${root}" log --merges --format='     %h %s' HEAD..origin/develop | head -5
    echo "   Fix: git fetch origin && git reset --hard origin/develop   (or rebase your branch onto it)"
    echo "   Override (you own the consequences): PHILOTIC_DEPLOY_ALLOW_STALE=1"
  } >&2

  if [[ "${PHILOTIC_DEPLOY_ALLOW_STALE:-0}" == "1" || "${mode}" == "nonfatal" ]]; then
    echo "⚠ freshness guard: stale tree OVERRIDDEN — proceeding anyway." >&2
    return 0
  fi
  exit 1
}

warn_stale_artifacts() {
  local root="$1"
  local probe="$2"

  [[ -f "${probe}" ]] || return 0
  git -C "${root}" rev-parse --git-dir >/dev/null 2>&1 || return 0

  local head_time bin_time
  head_time="$(git -C "${root}" log -1 --format='%ct' 2>/dev/null || echo 0)"
  bin_time="$(stat -f '%m' "${probe}" 2>/dev/null || stat -c '%Y' "${probe}" 2>/dev/null || echo 0)"

  if [[ "${bin_time}" -lt "${head_time}" ]]; then
    echo "⚠ freshness guard: $(basename "${probe}") was built BEFORE the current HEAD commit —" >&2
    echo "   the tree moved but nothing was rebuilt. Run the release build before deploying." >&2
  fi
}
