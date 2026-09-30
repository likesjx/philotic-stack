#!/usr/bin/env bash
# =============================================================================
# worktree-gc.sh — SAFE, continually-schedulable git worktree garbage collector
# =============================================================================
#
# WHY THIS EXISTS
#   ~15 parallel codex sessions each leave a 5-10GB cargo `target/` inside a
#   sibling worktree. Merged/dead worktrees then accumulate until the disk
#   fills (the Air hit 0 bytes). This prunes ONLY worktrees whose work is
#   already safe on origin/develop, reclaiming their `target/` dirs.
#
# SAFETY INVARIANTS (non-negotiable — a bug here destroys active work)
#   1. NEVER touch the main checkout (/Users/jaredlikes/code/philotic-stack).
#   2. NEVER remove a worktree with uncommitted work. "Uncommitted" counts
#      EVERY `git status --porcelain` line EXCEPT untracked `target/` dirs
#      (those are exactly the build artifacts we want to reclaim). A dirty
#      worktree is always PRESERVED.
#   3. NEVER remove a worktree whose work is not on origin/develop. Merged
#      means EITHER the tip is an ancestor of origin/develop (merge-commit /
#      fast-forward PRs) OR the tip is EXACTLY the head commit (headRefOid) of
#      a squash-merged PR per the GitHub API — squash merges rewrite history,
#      so ancestry alone preserved every squashed branch forever. A commit
#      added after the squash merge makes the tip differ -> preserved. If `gh`
#      is unavailable the fallback is skipped (fail safe: preserve). Ancestry
#      is verified against a FRESH `git fetch origin`.
#   4. NEVER remove an EXCLUDE-listed branch (held-epic bookmarks, operator
#      pins via PHILOTIC_WTGC_KEEP).
#   5. NEVER remove a detached-HEAD worktree (no branch to reason about).
#   6. Branch deletion uses the merge-safe `git branch -d` (self-refuses
#      anything not merged). NEVER `-D`. A refused `-d` is logged and skipped;
#      the disk is already reclaimed by the worktree removal.
#   7. NEVER `cargo clean` a preserved worktree, never touch remote branches.
#   8. FAIL SAFE: a bare run is a DRY RUN. Deletion requires `--apply` or
#      PHILOTIC_WTGC_APPLY=1.
#   9. GRACE PERIOD: NEVER reap an otherwise-removable (merged+clean+non-
#      excluded) worktree whose most-recent git activity is within
#      PHILOTIC_WTGC_GRACE_HOURS (default 6). A brand-new `git worktree add`
#      is clean and sits at the develop tip, so it counts as merged+clean the
#      instant it exists — without this, the scheduled job could delete a fresh
#      worktree out from under the session that just created it but hasn't
#      committed yet. "Most-recent activity" is the NEWEST mtime among the
#      worktree's `.git` pointer file and the HEAD file in its resolved gitdir;
#      both are (re)written by `git worktree add` and by any commit/checkout,
#      but NOT by cargo builds — so an idle-but-recently-created worktree is
#      protected while a long-idle merged one is still reaped.
#
# USAGE
#   scripts/worktree-gc.sh [--dry-run | --apply]
#     --dry-run   (default) report only; delete nothing
#     --apply     actually remove merged+clean+non-excluded worktrees
#   env PHILOTIC_WTGC_APPLY=1   same as --apply
#   env PHILOTIC_WTGC_KEEP      extra branch names to preserve
#                               (space- or comma-separated)
#   env PHILOTIC_WTGC_GRACE_HOURS   grace window in hours (default 6); newer
#                               otherwise-removable worktrees are preserved
#
# LOG: every run appends to ~/.philotic/worktree-gc.log
# =============================================================================

set -euo pipefail

# --- configuration -----------------------------------------------------------

# The one checkout that must never be removed, regardless of state.
MAIN_REPO="/Users/jaredlikes/code/philotic-stack"

# Branches that must never be reaped even when merged+clean.
#   - codex/model-catalog-sync : held-epic bookmark (won't compile on develop)
EXCLUDE_BRANCHES=("codex/model-catalog-sync")

LOG_FILE="${HOME}/.philotic/worktree-gc.log"
DATA_VOLUME="/System/Volumes/Data"

# Grace window (hours). An otherwise-removable worktree whose most-recent git
# activity is newer than this is PRESERVED so a freshly-created, not-yet-
# committed worktree isn't reaped before its session starts working.
GRACE_HOURS="${PHILOTIC_WTGC_GRACE_HOURS:-6}"

# --- argument / mode parsing --------------------------------------------------

APPLY=0
[[ "${PHILOTIC_WTGC_APPLY:-0}" == "1" ]] && APPLY=1
for arg in "$@"; do
    case "${arg}" in
        --apply)   APPLY=1 ;;
        --dry-run) APPLY=0 ;;
        -h|--help)
            sed -n '2,45p' "$0"
            exit 0
            ;;
        *)
            echo "worktree-gc: unknown argument: ${arg}" >&2
            exit 2
            ;;
    esac
done

# Fold operator-pinned branches (PHILOTIC_WTGC_KEEP, space/comma separated).
if [[ -n "${PHILOTIC_WTGC_KEEP:-}" ]]; then
    _keep_normalized="${PHILOTIC_WTGC_KEEP//,/ }"
    for _b in ${_keep_normalized}; do
        [[ -n "${_b}" ]] && EXCLUDE_BRANCHES+=("${_b}")
    done
fi

# --- helpers ------------------------------------------------------------------

mkdir -p "$(dirname "${LOG_FILE}")"

RUN_TS="$(date '+%Y-%m-%dT%H:%M:%S%z')"

# Emit to stdout AND append to the persistent log.
log() {
    printf '%s\n' "$*"
    printf '[%s] %s\n' "${RUN_TS}" "$*" >>"${LOG_FILE}"
}

# Available 1K-blocks on the data volume (portable df parse).
disk_free_kb() {
    df -k "${DATA_VOLUME}" | awk 'NR==2 {print $4}'
}

is_excluded() {
    local branch="$1"
    local ex
    for ex in "${EXCLUDE_BRANCHES[@]}"; do
        [[ "${branch}" == "${ex}" ]] && return 0
    done
    return 1
}

# --- squash-merge detection ---------------------------------------------------
# PRs into develop are often SQUASH-merged, so a merged branch tip is never an
# ancestor of origin/develop and the ancestry check alone preserves everything
# forever (observed: 27 worktrees / ~75GB of target/ accumulated while every
# 2h apply run reclaimed 0.00 GB). Fallback: ask GitHub for merged PRs and
# treat a branch as merged IFF its CURRENT tip commit is exactly the head
# commit a merged PR was squashed from (headRefOid). Any commit added to the
# branch after the merge makes the tip differ from headRefOid -> preserved.
# FAIL SAFE: if gh is missing or the API call fails, the list stays empty and
# every non-ancestor branch is preserved, exactly as before this fallback.
# Newline-separated "branch<SP>oid" lines (macOS bash 3.2: no assoc arrays).
SQUASH_MERGED_LINES=""
load_squash_merged_lines() {
    if ! command -v gh >/dev/null 2>&1; then
        log "WARNING: gh unavailable — squash-merge detection disabled (ancestry check only)"
        return 0
    fi
    local rows
    if rows="$(cd "${MAIN_REPO}" && gh pr list --state merged --limit 300 \
        --json headRefName,headRefOid,baseRefName \
        --jq '.[] | select(.baseRefName == "develop" or .baseRefName == "main") | "\(.headRefName) \(.headRefOid)"' 2>>"${LOG_FILE}")"; then
        SQUASH_MERGED_LINES="${rows}"
        log "loaded $(printf '%s\n' "${rows}" | grep -c .) merged-PR head refs for squash-merge detection"
    else
        log "WARNING: gh pr list failed — squash-merge detection disabled (ancestry check only)"
    fi
}

# True iff this branch's tip is exactly the head commit of a merged PR.
is_squash_merged() {
    local branch="$1" head="$2"
    [[ -n "${SQUASH_MERGED_LINES}" ]] || return 1
    printf '%s\n' "${SQUASH_MERGED_LINES}" | grep -qxF "${branch} ${head}"
}

# has_uncommitted <worktree>: true if any porcelain line is NOT an untracked
# target/ dir. Anchored on `^\?\? ` so modified/staged tracked files (which
# start with ` M`, `M `, `A `, etc.) are ALWAYS counted, and a directory that
# merely ends in "target/" but is not literally the target dir (e.g.
# "mytarget/") would still be counted because the segment must be a full path
# component preceded by `/` or the start.
has_uncommitted() {
    local wt="$1"
    local leftover
    leftover="$(git -C "${wt}" status --porcelain 2>/dev/null \
        | grep -vE '^\?\? (.*/)?target/?$' || true)"
    [[ -n "${leftover}" ]]
}

# newest_activity_epoch <worktree>: echoes the NEWEST mtime (epoch seconds)
# among the worktree's git-activity markers — its `.git` pointer file and the
# HEAD file in its resolved gitdir. Both are (re)written by `git worktree add`
# and by any commit/checkout, but NOT by cargo builds, so this tracks real git
# activity rather than build churn. Echoes 0 if no marker is stat-able.
newest_activity_epoch() {
    local wt="$1"
    local gitdir newest=0 marker m
    gitdir="$(git -C "${wt}" rev-parse --absolute-git-dir 2>/dev/null || true)"
    # .git and HEAD alone are a POOR activity signal: neither is touched by
    # editing files, compiling, or running tests, so a worktree an agent has
    # been working in for hours looks completely idle. index/logs/HEAD/ORIG_HEAD
    # move whenever anyone runs git in the worktree (status refreshes the
    # index), which is a far better proxy for "someone is here".
    for marker in \
        "${wt}/.git" \
        "${gitdir:+${gitdir}/HEAD}" \
        "${gitdir:+${gitdir}/index}" \
        "${gitdir:+${gitdir}/logs/HEAD}" \
        "${gitdir:+${gitdir}/ORIG_HEAD}"; do
        [[ -n "${marker}" && -e "${marker}" ]] || continue
        m="$(stat -f %m "${marker}" 2>/dev/null || true)"
        [[ -n "${m}" && "${m}" -gt "${newest}" ]] && newest="${m}"
    done
    printf '%s' "${newest}"
}

# worktree_in_use <worktree>: 0 if any LIVE process has its cwd inside it.
#
# This is the invariant that was missing, and it is the only one that is
# actually true by construction: a worktree somebody is standing in must not be
# deleted, regardless of how its branch looks. Without it, a worktree is
# eligible the moment its work is committed and pushed — which is exactly when
# an agent is most likely to still be working in it. It has removed an active
# worktree out from under a running session three times; the session's shell
# then silently falls back to the MAIN checkout, where the next git command
# operates on the wrong repository.
#
# `-d cwd` restricts lsof to current-working-directory descriptors, which is
# vastly cheaper than `+D` (that would walk the whole tree, target/ included).
# If lsof is unavailable we fail SAFE — treat the worktree as in use — because
# wrongly keeping a stale worktree costs disk, and wrongly deleting a live one
# costs work.
worktree_in_use() {
    local wt="$1" resolved
    resolved="$(cd "${wt}" 2>/dev/null && pwd -P)" || return 0
    command -v lsof >/dev/null 2>&1 || return 0
    lsof -a -d cwd -F n 2>/dev/null | awk -v p="${resolved}" '
        /^n/ {
            path = substr($0, 2)
            if (path == p || index(path, p "/") == 1) { found = 1; exit }
        }
        END { exit(found ? 0 : 1) }
    '
}

# --- preflight ----------------------------------------------------------------

if [[ ! -d "${MAIN_REPO}/.git" && ! -f "${MAIN_REPO}/.git" ]]; then
    echo "worktree-gc: main repo not found at ${MAIN_REPO}" >&2
    exit 1
fi

MODE_LABEL=$([[ "${APPLY}" == "1" ]] && echo "APPLY" || echo "DRY-RUN")
log "=== worktree-gc run (${MODE_LABEL}) @ ${RUN_TS} ==="

# Merged-check must be against CURRENT origin/develop.
if git -C "${MAIN_REPO}" fetch origin --quiet 2>/dev/null; then
    log "fetched origin (merged-check is against current origin/develop)"
else
    log "WARNING: git fetch origin failed — merged-check uses stale origin/develop"
fi

if ! git -C "${MAIN_REPO}" rev-parse --verify --quiet origin/develop >/dev/null; then
    log "ERROR: origin/develop is unknown — refusing to remove anything"
    exit 1
fi

load_squash_merged_lines

FREE_BEFORE_KB="$(disk_free_kb)"

# --- main loop ----------------------------------------------------------------

n_preserved=0
n_removed=0

# Parse `git worktree list --porcelain` block-by-block.
current_wt=""
current_head=""
current_branch=""

process_worktree() {
    local wt="$1" head="$2" branch="$3"

    # Invariant 1: never the main checkout.
    if [[ "${wt}" == "${MAIN_REPO}" ]]; then
        return
    fi

    # Invariant 5: never a detached-HEAD worktree.
    if [[ -z "${branch}" ]]; then
        log "PRESERVE (detached): ${wt}"
        n_preserved=$((n_preserved + 1))
        return
    fi

    # Invariant 2: dirty worktrees are always preserved.
    if has_uncommitted "${wt}"; then
        log "PRESERVE (dirty): ${wt} (${branch})"
        n_preserved=$((n_preserved + 1))
        return
    fi

    # Invariant 3: unmerged commits are always preserved. Merged means either
    # the tip is an ancestor of origin/develop (merge-commit / fast-forward
    # PRs) OR the tip is exactly the head commit of a squash-merged PR.
    if ! git -C "${MAIN_REPO}" merge-base --is-ancestor "${head}" origin/develop 2>/dev/null; then
        if is_squash_merged "${branch}" "${head}"; then
            log "merged via squash PR (tip ${head} == merged PR headRefOid): ${wt} (${branch})"
        else
            log "PRESERVE (unmerged): ${wt} (${branch})"
            n_preserved=$((n_preserved + 1))
            return
        fi
    fi

    # Invariant 4: excluded branches are always preserved.
    if is_excluded "${branch}"; then
        log "PRESERVE (excluded): ${wt} (${branch})"
        n_preserved=$((n_preserved + 1))
        return
    fi

    # Invariant 9: never reap a worktree a live process is sitting in.
    if worktree_in_use "${wt}"; then
        log "PRESERVE (in use: a live process has its cwd here): ${wt} (${branch})"
        n_preserved=$((n_preserved + 1))
        return
    fi

    # Invariant 10: grace period. This worktree is otherwise removable
    # (merged+clean+not-excluded), but a freshly-created one looks exactly like
    # this the instant it exists. Preserve it if its most-recent git activity is
    # within the grace window so a session that just created it isn't reaped
    # before its first commit.
    local activity age_h
    activity="$(newest_activity_epoch "${wt}")"
    if [[ "${activity}" -gt 0 ]]; then
        age_h=$(( ( $(date +%s) - activity ) / 3600 ))
        if [[ "${age_h}" -lt "${GRACE_HOURS}" ]]; then
            log "PRESERVE (grace: touched ${age_h}h ago): ${wt} (${branch})"
            n_preserved=$((n_preserved + 1))
            return
        fi
    fi

    # Eligible: merged AND clean AND not-excluded.
    if [[ "${APPLY}" == "1" ]]; then
        if git -C "${MAIN_REPO}" worktree remove --force "${wt}" 2>>"${LOG_FILE}"; then
            # Merge-safe branch delete; NEVER -D. A refusal is fine — disk is
            # already reclaimed; the branch just lingers.
            if git -C "${MAIN_REPO}" branch -d "${branch}" 2>>"${LOG_FILE}"; then
                log "REMOVED: ${wt} (${branch})"
            else
                log "REMOVED: ${wt} (worktree gone; branch '${branch}' kept — 'git branch -d' refused)"
            fi
            n_removed=$((n_removed + 1))
        else
            log "ERROR: failed to remove worktree ${wt} — preserved"
            n_preserved=$((n_preserved + 1))
        fi
    else
        log "WOULD REMOVE: ${wt} (${branch})"
        n_removed=$((n_removed + 1))
    fi
}

while IFS= read -r line || [[ -n "${line}" ]]; do
    case "${line}" in
        "worktree "*)
            current_wt="${line#worktree }"
            current_head=""
            current_branch=""
            ;;
        "HEAD "*)
            current_head="${line#HEAD }"
            ;;
        "branch "*)
            # e.g. "branch refs/heads/codex/foo" -> "codex/foo"
            current_branch="${line#branch refs/heads/}"
            ;;
        "detached")
            current_branch=""
            ;;
        "")
            # blank line ends a block
            if [[ -n "${current_wt}" ]]; then
                process_worktree "${current_wt}" "${current_head}" "${current_branch}"
            fi
            current_wt=""
            current_head=""
            current_branch=""
            ;;
    esac
done < <(git -C "${MAIN_REPO}" worktree list --porcelain)

# Flush a trailing block (porcelain output may not end with a blank line).
if [[ -n "${current_wt}" ]]; then
    process_worktree "${current_wt}" "${current_head}" "${current_branch}"
fi

# --- prune stale metadata -----------------------------------------------------

if [[ "${APPLY}" == "1" ]]; then
    git -C "${MAIN_REPO}" worktree prune 2>>"${LOG_FILE}" || true
fi

# --- summary ------------------------------------------------------------------

FREE_AFTER_KB="$(disk_free_kb)"
gb_of() { awk -v k="$1" 'BEGIN { printf "%.2f", k / 1024 / 1024 }'; }
reclaimed_gb="$(awk -v b="${FREE_BEFORE_KB}" -v a="${FREE_AFTER_KB}" \
    'BEGIN { printf "%.2f", (a - b) / 1024 / 1024 }')"

log "--- summary (${MODE_LABEL}) ---"
log "preserved: ${n_preserved}"
if [[ "${APPLY}" == "1" ]]; then
    log "removed:   ${n_removed}"
else
    log "would remove: ${n_removed}"
fi
log "disk free before: $(gb_of "${FREE_BEFORE_KB}") GB / after: $(gb_of "${FREE_AFTER_KB}") GB"
log "reclaimed: ${reclaimed_gb} GB"
log "=== end worktree-gc run ==="
