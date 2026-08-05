#!/usr/bin/env bash
# PreToolUse(Bash) guard — the machinery behind three rules that previously
# existed only as prose in AGENTS.md and CLAUDE.md:
#
#   never push to main/master   never force-push   never merge a feature
#                                                  branch into main
#
# Prose is advisory. An agent that misreads its instructions, or a session
# whose context has been summarized past the rule, will do it anyway. This
# hook makes the rule mechanical.
#
# Contract: exit 2 blocks the tool call and shows stderr to Claude. Exit 0
# allows. Any parsing failure allows — a guard that breaks the session when it
# cannot read its own input is worse than the risk it mitigates.
#
# IMPORTANT: matching is per-SEGMENT, not over the whole command string. The
# first version scanned the entire command whenever `git push` appeared
# anywhere in it, so
#
#     git push origin foo && gh pr create --body "...use \`rm -f\`..."
#
# was blocked as a "force-push" because the PR body contained ` -f `. A real
# false positive, hit within a day of shipping. Splitting on shell separators
# means a segment's flags are only ever attributed to that segment's own
# command.
set -uo pipefail

input=$(cat)

cmd=$(printf '%s' "$input" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    print(d.get("tool_input", {}).get("command", ""))
except Exception:
    pass
' 2>/dev/null) || exit 0

[ -z "$cmd" ] && exit 0

block() {
    echo "BLOCKED by .claude/hooks/guard-destructive-git.sh: $1" >&2
    echo "" >&2
    echo "This repo's branch model: codex/<slug> -> PR -> develop. main is only" >&2
    echo "advanced by merging develop when the edge is ready to ship. If you" >&2
    echo "genuinely need this, the operator must run it by hand." >&2
    exit 2
}

block_main_checkout() {
    echo "BLOCKED by .claude/hooks/guard-destructive-git.sh: $1 in the MAIN CHECKOUT" >&2
    echo "" >&2
    echo "  $MAIN_REPO" >&2
    echo "" >&2
    echo "That checkout holds the operator's own uncommitted work, and agents are" >&2
    echo "supposed to be in a worktree. A session whose worktree was removed falls" >&2
    echo "back here SILENTLY — that is how five uncommitted operator files were" >&2
    echo "destroyed by a 'git reset --hard' (2026-07-30, and again 08-02)." >&2
    echo "" >&2
    echo "Check 'pwd' first. If you meant a worktree, use 'git -C <worktree> ...'." >&2
    echo "Only the operator runs this by hand in the main checkout." >&2
    exit 2
}

# The one checkout that must never be destructively rewritten by an agent.
MAIN_REPO="/Users/jaredlikes/code/philotic-stack"
main_real=$(cd "$MAIN_REPO" 2>/dev/null && pwd -P) || main_real="$MAIN_REPO"

# Best-effort absolute+symlink-resolved path. Falls back to the literal string
# so an unresolvable path can never silently compare equal to the main checkout.
resolve_dir() {
    local d="$1"
    case "$d" in
        '~') d="$HOME" ;;
        '~/'*) d="$HOME/${d#\~/}" ;;
    esac
    (cd "$d" 2>/dev/null && pwd -P) || printf '%s' "$d"
}

# Does this segment DESTROY uncommitted working-tree state?
#
# Deliberately narrow. A plain `git checkout <branch>` is a branch switch that
# git itself refuses when it would clobber changes, so it is NOT listed —
# blocking it would make the guard obstructive and teach people to override it.
# Only pathspec-restoring and tree-wiping forms qualify.
is_destructive_worktree_op() {
    local s="$1"
    # reset --hard
    printf '%s' "$s" | grep -qE '\breset\b.*(^| )--hard(\b|$)' && return 0
    # clean -f / -fd / -fdx / --force
    printf '%s' "$s" | grep -qE '\bclean\b.*((^| )-[a-zA-Z]*f|(^| )--force(\b|$))' && return 0
    # checkout -- <path>   /   checkout .
    printf '%s' "$s" | grep -qE '\bcheckout\b.*(^| )--( |$)' && return 0
    printf '%s' "$s" | grep -qE '\bcheckout +\.( |$)' && return 0
    # restore <path> (default target IS the worktree; --staged alone is index-only)
    if printf '%s' "$s" | grep -qE '\brestore\b'; then
        printf '%s' "$s" | grep -qE '(^| )--staged( |$)' || return 0
    fi
    # stash — the stack is SHARED across every worktree, so this both hides
    # operator work and can pop someone else's entry (see CLAUDE.md). `list`
    # and `show` only read, so they stay allowed.
    if printf '%s' "$s" | grep -qE '\bstash\b'; then
        printf '%s' "$s" | grep -qE '\bstash +(list|show)\b' || return 0
    fi
    return 1
}

current_branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")

# The directory a segment will actually run in. Starts at the hook's own cwd
# (which is the session's cwd — precisely the main checkout in the silent
# fallback case) and follows any `cd` the command performs, so
# `cd <main-checkout>` followed by `git reset --hard` is judged correctly even
# though the two are separate segments.
cur_dir=$(pwd -P 2>/dev/null) || cur_dir="${PWD:-}"

# Split into segments on ; && || | and newlines, then judge each on its own.
segments=$(printf '%s' "$cmd" | python3 -c '
import re, sys
raw = sys.stdin.read()
for part in re.split(r"(?:\|\||&&|[;&|\n])", raw):
    part = part.strip()
    if part:
        print(part)
' 2>/dev/null) || exit 0

while IFS= read -r seg; do
    [ -z "$seg" ] && continue

    # Follow `cd` so later segments are judged against the directory they will
    # really run in, not the one the hook happened to start in.
    if printf '%s' "$seg" | grep -qE '^cd( |$)'; then
        newd=$(printf '%s' "$seg" | sed -E 's/^cd +//; s/[[:space:]].*$//' | tr -d "\"'")
        [ -n "$newd" ] && cur_dir=$(resolve_dir "$newd")
        continue
    fi

    # Destructive working-tree ops aimed at the MAIN CHECKOUT. Routine and
    # allowed inside a worktree; never allowed against the operator's checkout.
    if printf '%s' "$seg" | grep -qE '^(sudo +)?git( +-C +[^ ]+)? +(reset|clean|checkout|restore|stash)\b'; then
        target="$cur_dir"
        # An explicit -C wins over cwd — otherwise the guard is bypassed by the
        # very idiom the operator's own notes recommend (`prefer git -C <path>`).
        if printf '%s' "$seg" | grep -qE '^(sudo +)?git +-C +'; then
            cdir=$(printf '%s' "$seg" | sed -E 's/^(sudo +)?git +-C +([^ ]+).*$/\2/' | tr -d "\"'")
            target=$(resolve_dir "$cdir")
        fi
        if [ -n "$target" ] && [ "$target" = "$main_real" ] && is_destructive_worktree_op "$seg"; then
            block_main_checkout "destructive git op"
        fi
    fi

    # Only a segment that IS a git push is judged as one.
    if printf '%s' "$seg" | grep -qE '^(sudo +)?git( +-C +[^ ]+)? +push\b'; then
        if printf '%s' "$seg" | grep -qE '(^| )--force(\b|=)|(^| )-f( |$)|(^| )--force-with-lease'; then
            block "force-push"
        fi
        # Explicit refspec targeting main/master, e.g.
        #   git push origin main | git push origin HEAD:main | git push -u origin master
        # Whole-word so a branch like codex/main-thing is not caught.
        if printf '%s' "$seg" | grep -qE '(^| |:)(main|master)( |$)'; then
            block "push targeting main/master"
        fi
        # Bare `git push` while the checkout is on main/master.
        if [ "$current_branch" = "main" ] || [ "$current_branch" = "master" ]; then
            block "bare 'git push' while on branch '$current_branch'"
        fi
    fi

    # Merging into main from a working checkout. Releases go through the
    # sync/develop-into-main PR path, not a local merge.
    if printf '%s' "$seg" | grep -qE '^(sudo +)?git( +-C +[^ ]+)? +merge\b'; then
        if [ "$current_branch" = "main" ] || [ "$current_branch" = "master" ]; then
            block "git merge while on '$current_branch'"
        fi
    fi
done <<< "$segments"

exit 0
