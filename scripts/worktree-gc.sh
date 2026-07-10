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
#   3. NEVER remove a worktree whose HEAD is not an ancestor of origin/develop
#      (i.e. it still holds unmerged commits). Merged is verified against a
#      FRESH `git fetch origin` so the check reflects current origin/develop.
#   4. NEVER remove an EXCLUDE-listed branch (held-epic bookmarks, operator
#      pins via PHILOTIC_WTGC_KEEP).
#   5. NEVER remove a detached-HEAD worktree (no branch to reason about).
#   6. Branch deletion uses the merge-safe `git branch -d` (self-refuses
#      anything not merged). NEVER `-D`. A refused `-d` is logged and skipped;
#      the disk is already reclaimed by the worktree removal.
#   7. NEVER `cargo clean` a preserved worktree, never touch remote branches.
#   8. FAIL SAFE: a bare run is a DRY RUN. Deletion requires `--apply` or
#      PHILOTIC_WTGC_APPLY=1.
#
# USAGE
#   scripts/worktree-gc.sh [--dry-run | --apply]
#     --dry-run   (default) report only; delete nothing
#     --apply     actually remove merged+clean+non-excluded worktrees
#   env PHILOTIC_WTGC_APPLY=1   same as --apply
#   env PHILOTIC_WTGC_KEEP      extra branch names to preserve
#                               (space- or comma-separated)
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

    # Invariant 3: unmerged commits are always preserved.
    if ! git -C "${MAIN_REPO}" merge-base --is-ancestor "${head}" origin/develop 2>/dev/null; then
        log "PRESERVE (unmerged): ${wt} (${branch})"
        n_preserved=$((n_preserved + 1))
        return
    fi

    # Invariant 4: excluded branches are always preserved.
    if is_excluded "${branch}"; then
        log "PRESERVE (excluded): ${wt} (${branch})"
        n_preserved=$((n_preserved + 1))
        return
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
