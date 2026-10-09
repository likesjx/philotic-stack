#!/usr/bin/env bash
# =============================================================================
# setup-build-cache.sh — share compiler work across worktrees, safely
# =============================================================================
#
# WHY THIS EXISTS
#   ~20 parallel worktrees each cold-build 605 dependencies into their own
#   `target/`. That is both slow and, at ~11 GB a piece, the reason this machine
#   keeps filling its disk.
#
#   The obvious fix — point every worktree at ONE shared `target/` via
#   CARGO_TARGET_DIR — is UNSAFE for this repo, and not in a subtle way. Cargo
#   fingerprints a compilation unit by package name/version/profile/features,
#   NOT by workspace path. Two worktrees of philotic-stack therefore produce
#   colliding units. Measured directly (see the doc):
#
#       worktree A: cargo build  -> target/debug/app prints "I am worktree A"
#       worktree B: cargo build  -> target/debug/app prints "I am worktree B"
#       worktree A: cargo build  -> "Finished" (no rebuild!)
#                                   target/debug/app STILL prints "worktree B"
#
#   Cargo reports success while leaving the *other* branch's binary in place.
#   scripts/ has 98 `${ROOT_DIR}/target/{debug,release}/...` call sites, so every
#   smoke script would then verify the wrong build and report green. That is the
#   exact failure class $runtime-rollout-watch exists to catch.
#
#   So: keep per-worktree `target/` dirs (correctness), and share the *compiler
#   cache* instead. sccache is keyed on preprocessed input, so the 605 deps are
#   compiled once for the whole machine and every later worktree gets cache hits
#   — with no shared lock, so worktrees still build in parallel.
#
# WHAT THIS CHANGES (machine setup, not repo truth — hence a script, not
# .cargo/config.toml, which is checked in and must stay portable for
# self-hosters)
#   1. installs sccache via Homebrew if missing
#   2. sets `build.rustc-wrapper` in ~/.cargo/config.toml
#   3. writes an sccache config with an explicit, BOUNDED cache size
#
# Idempotent. Re-run freely. `--status` inspects without changing anything.
# `--disable` reverts step 2 (the only step that changes build behaviour).
# =============================================================================

set -euo pipefail

CACHE_SIZE="${PHILOTIC_SCCACHE_SIZE:-30G}"
CARGO_CONFIG="${CARGO_HOME:-$HOME/.cargo}/config.toml"
SCCACHE_CONF_DIR="$HOME/Library/Application Support/Mozilla.sccache"
SCCACHE_CONF="$SCCACHE_CONF_DIR/config"

info() { printf '  %s\n' "$*"; }
head2() { printf '\n== %s ==\n' "$*"; }

have_sccache() { command -v sccache >/dev/null 2>&1; }

wrapper_enabled() {
    [ -f "$CARGO_CONFIG" ] && grep -qE '^[[:space:]]*rustc-wrapper[[:space:]]*=' "$CARGO_CONFIG"
}

cmd_status() {
    head2 "build cache status"
    if have_sccache; then
        info "sccache:        $(command -v sccache) ($(sccache --version 2>/dev/null | head -1))"
    else
        info "sccache:        NOT INSTALLED"
    fi
    if wrapper_enabled; then
        info "rustc-wrapper:  enabled in $CARGO_CONFIG"
    else
        info "rustc-wrapper:  not set (builds are NOT cached)"
    fi
    [ -f "$SCCACHE_CONF" ] && info "sccache config: $SCCACHE_CONF" || info "sccache config: (none)"
    if have_sccache; then
        head2 "sccache stats"
        sccache --show-stats 2>/dev/null | grep -iE 'cache hits|cache misses|cache size|max cache size|hit rate' || true
    fi
    head2 "per-worktree target dirs"
    local root
    root="$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")"
    git -C "$root" worktree list --porcelain 2>/dev/null \
        | awk '/^worktree /{print $2}' \
        | while read -r wt; do
            [ -d "$wt/target" ] || continue
            printf '  %-6s %s\n' "$(du -sh "$wt/target" 2>/dev/null | cut -f1)" "$wt/target"
        done
    info ""
    info "reclaim idle ones:  scripts/target-sweep.sh          (live worktrees, keeps the worktree)"
    info "reclaim merged ones: scripts/worktree-gc.sh          (removes the whole worktree)"
}

cmd_disable() {
    if ! wrapper_enabled; then
        info "rustc-wrapper was not set — nothing to do."
        return 0
    fi
    # Drop only the rustc-wrapper line; leave the rest of the user's config alone.
    local tmp
    tmp="$(mktemp)"
    grep -vE '^[[:space:]]*rustc-wrapper[[:space:]]*=' "$CARGO_CONFIG" > "$tmp"
    mv "$tmp" "$CARGO_CONFIG"
    info "removed rustc-wrapper from $CARGO_CONFIG (sccache left installed)"
}

cmd_setup() {
    head2 "1/3 sccache"
    if have_sccache; then
        info "already installed: $(sccache --version 2>/dev/null | head -1)"
    else
        command -v brew >/dev/null 2>&1 || {
            echo "error: Homebrew not found; install sccache manually, then re-run" >&2
            exit 1
        }
        info "installing via Homebrew..."
        brew install sccache
    fi

    head2 "2/3 bounded cache (${CACHE_SIZE})"
    mkdir -p "$SCCACHE_CONF_DIR"
    # An unbounded compiler cache just relocates the disk problem, so the size
    # cap is not optional. sccache evicts LRU once it is reached.
    cat > "$SCCACHE_CONF" <<EOF
# Written by scripts/setup-build-cache.sh — safe to edit.
[cache.disk]
size = $(numfmt --from=iec "${CACHE_SIZE}" 2>/dev/null || echo 32212254720)
EOF
    info "wrote $SCCACHE_CONF"

    head2 "3/3 enable for cargo"
    if wrapper_enabled; then
        info "rustc-wrapper already set in $CARGO_CONFIG"
    else
        mkdir -p "$(dirname "$CARGO_CONFIG")"
        if [ -s "$CARGO_CONFIG" ] && grep -qE '^\[build\]' "$CARGO_CONFIG"; then
            # Insert under the existing [build] table rather than adding a second one.
            awk '/^\[build\]/{print; print "rustc-wrapper = \"sccache\""; next} {print}' \
                "$CARGO_CONFIG" > "$CARGO_CONFIG.tmp"
            mv "$CARGO_CONFIG.tmp" "$CARGO_CONFIG"
        else
            printf '\n[build]\nrustc-wrapper = "sccache"\n' >> "$CARGO_CONFIG"
        fi
        info "enabled sccache as rustc-wrapper in $CARGO_CONFIG"
    fi

    cat <<'EOF'

Done. The next cold build populates the cache; the one after that — in any
worktree — hits it.

  scripts/setup-build-cache.sh --status    inspect + hit rate
  scripts/setup-build-cache.sh --disable   turn caching back off
  sccache --zero-stats                     reset counters before a timing run

NOTE: worktrees still each keep their own target/. That is deliberate — see the
header of this script. sccache makes a fresh worktree build FAST, not SMALL; to
reclaim disk use scripts/target-sweep.sh (idle live worktrees) or
scripts/worktree-gc.sh (fully merged ones).
EOF
}

case "${1:---setup}" in
    --status|status)   cmd_status ;;
    --disable|disable) cmd_disable ;;
    --setup|setup)     cmd_setup ;;
    -h|--help)         sed -n '2,45p' "$0" ;;
    *) echo "usage: $0 [--setup|--status|--disable]" >&2; exit 1 ;;
esac
