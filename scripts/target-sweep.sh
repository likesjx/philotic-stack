#!/usr/bin/env bash
# =============================================================================
# target-sweep.sh — reclaim cargo target/ from LIVE but IDLE worktrees
# =============================================================================
#
# WHY THIS EXISTS
#   scripts/worktree-gc.sh removes whole worktrees, but only ones whose work is
#   already on origin/develop. In practice that reclaims nothing: a dry run with
#   19 worktrees preserved 19 of them (unmerged / dirty / detached) and freed
#   -0.05 GB. The worktrees are all legitimately alive.
#
#   The disk is still being eaten, because each of those live worktrees owns an
#   ~11 GB `target/`. So sweep the ARTIFACTS, not the worktrees: `target/` is
#   gitignored, regenerable build output. Deleting it costs a rebuild and loses
#   no work whatsoever.
#
#   Worktrees deliberately do NOT share one target/ dir — that makes cargo report
#   "Finished" while leaving another branch's binary in place. See
#   docs/guides/CARGO_BUILD_ARTIFACT_BUDGET.md.
#
# SAFETY INVARIANTS
#   1. ONLY ever removes `<worktree>/target`. Never source, never a worktree,
#      never a branch. Nothing tracked by git is touched.
#   2. NEVER sweeps a worktree whose target/ was built within --idle-days
#      (default 14) — that is someone's warm cache.
#   3. NEVER sweeps a worktree with a build in it: every live cargo/rustc process
#      is resolved to its working directory and that worktree is skipped, as is
#      any target/ whose cargo lock is held. Deliberately per-worktree, not a
#      global "is cargo running" refusal -- with ~19 parallel agents something is
#      almost always building, and a global guard would never let the sweep run.
#   4. NEVER sweeps the main checkout unless --include-main is passed explicitly
#      (it is the deploy checkout; push-homebrew-remote.sh reads its
#      target/release).
#   5. Dry run by default. --apply is required to delete anything.
# =============================================================================

set -euo pipefail

IDLE_DAYS=14
APPLY=0
INCLUDE_MAIN=0

usage() {
    sed -n '2,34p' "$0"
    cat <<'EOF'

Usage:
  scripts/target-sweep.sh [--apply] [--idle-days N] [--include-main]

  --apply          actually delete (default: dry run, deletes nothing)
  --idle-days N    only sweep target/ dirs not built in N days (default 14)
  --include-main   also consider the main checkout (default: never)
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --apply) APPLY=1; shift ;;
        --idle-days) IDLE_DAYS="${2:?--idle-days needs a value}"; shift 2 ;;
        --include-main) INCLUDE_MAIN=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown arg: $1" >&2; usage; exit 1 ;;
    esac
done

MAIN_CHECKOUT="$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")"

# Invariant 3: identify which worktrees have a LIVE build, and skip exactly those.
#
# A global "refuse if any cargo is running" check is wrong on this machine: with
# ~19 parallel agents some cargo is almost always alive somewhere, so a global
# guard means the sweep can never run. (It also trips on a hung `cargo test -p
# aiua`, which is a known macOS-keychain hang here.) Resolve each build process
# to its working directory instead, and only protect the worktree it is in.
BUSY_DIRS=""
# pgrep exits 1 when nothing matches, and `set -o pipefail` propagates that, so
# without `|| true` this assignment aborts the whole script -- silently, and only
# when NO build is running, i.e. exactly when the sweep should proceed.
build_pids="$( { pgrep -x cargo; pgrep -x rustc; } 2>/dev/null | sort -u | tr '\n' ' ' || true)"
if [ -n "${build_pids// /}" ]; then
    for pid in $build_pids; do
        # A pid can exit between pgrep and lsof; tolerate that rather than
        # aborting the sweep (a failing $(...) in an assignment trips set -e).
        cwd="$(lsof -a -p "$pid" -d cwd -Fn 2>/dev/null | sed -n 's/^n//p' | head -1 || true)"
        if [ -n "$cwd" ]; then
            BUSY_DIRS="$BUSY_DIRS
$cwd"
        fi
    done
fi

# Is $1 (a worktree) the cwd of, or an ancestor of the cwd of, a live build?
is_busy() {
    local wt=$1 d
    while IFS= read -r d; do
        [ -n "$d" ] || continue
        case "$d/" in "$wt"/*) return 0 ;; esac
    done <<< "$BUSY_DIRS"
    return 1
}

# Invariant 3 (per-dir): is a cargo lock under this target/ held by a live process?
#
# This is the backstop for a build whose cwd is NOT the worktree root (e.g. a
# wrapper that cd's elsewhere and passes --manifest-path), which is_busy cannot
# see. Cargo 1.94 keeps the lock at target/<profile>/.cargo-lock -- NOT at
# target/.cargo-lock -- so check every profile dir. Getting this path wrong makes
# the whole invariant silently decorative, which it was until measured.
lock_held() {
    local target=$1 lock
    for lock in "$target"/*/.cargo-lock; do
        [ -f "$lock" ] || continue
        if lock_file_held "$lock"; then
            return 0
        fi
    done
    return 1
}

lock_file_held() {
    local lock=$1
    python3 - "$lock" <<'PY' 2>/dev/null
import fcntl, sys
try:
    f = open(sys.argv[1], "a")
    fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    sys.exit(0)   # held -> "true"
except Exception:
    sys.exit(1)
sys.exit(1)       # acquired freely -> not held
PY
}

human() { du -sh "$1" 2>/dev/null | cut -f1; }
mb()    { du -sm "$1" 2>/dev/null | cut -f1; }

free_gb() { df -g "$MAIN_CHECKOUT" 2>/dev/null | awk 'NR==2{print $4}'; }

mode="DRY-RUN"; [ "$APPLY" -eq 1 ] && mode="APPLY"
echo "=== target-sweep ($mode) @ $(date +%FT%T%z) ==="
echo "idle threshold: ${IDLE_DAYS} days   main checkout: $([ "$INCLUDE_MAIN" -eq 1 ] && echo included || echo EXCLUDED)"
echo

before_gb="$(free_gb)"
total_mb=0
swept=0
kept=0

while read -r wt; do
    [ -n "$wt" ] || continue
    target="$wt/target"

    if [ "$wt" = "$MAIN_CHECKOUT" ] && [ "$INCLUDE_MAIN" -eq 0 ]; then
        if [ -d "$target" ]; then
            printf 'KEEP  (main checkout)   %-6s %s\n' "$(human "$target")" "$wt"
            kept=$((kept+1))
        fi
        continue
    fi

    [ -d "$target" ] || continue

    if is_busy "$wt"; then
        printf 'KEEP  (build running)   %-6s %s\n' "$(human "$target")" "$wt"
        kept=$((kept+1)); continue
    fi

    if lock_held "$target"; then
        printf 'KEEP  (build lock held) %-6s %s\n' "$(human "$target")" "$wt"
        kept=$((kept+1)); continue
    fi

    # Recently built? -mtime -N is "modified less than N days ago".
    if [ -n "$(find "$target" -maxdepth 0 -mtime "-${IDLE_DAYS}" 2>/dev/null)" ]; then
        printf 'KEEP  (built <%sd ago)  %-6s %s\n' "$IDLE_DAYS" "$(human "$target")" "$wt"
        kept=$((kept+1)); continue
    fi

    size_mb="$(mb "$target")"
    total_mb=$((total_mb + size_mb))
    swept=$((swept+1))

    if [ "$APPLY" -eq 1 ]; then
        printf 'SWEEP                   %-6s %s\n' "$(human "$target")" "$wt"
        rm -rf "$target"
    else
        printf 'WOULD SWEEP             %-6s %s\n' "$(human "$target")" "$wt"
    fi
done < <(git -C "$MAIN_CHECKOUT" worktree list --porcelain | awk '/^worktree /{print $2}')

echo
echo "--- summary ($mode) ---"
echo "swept:     $swept   kept: $kept"
printf 'reclaim:   %.1f GB\n' "$(echo "$total_mb" | awk '{print $1/1024}')"
if [ "$APPLY" -eq 1 ]; then
    echo "disk free: ${before_gb} GB -> $(free_gb) GB"
else
    echo "disk free: ${before_gb} GB (nothing deleted; re-run with --apply)"
fi
echo "=== end target-sweep ==="
