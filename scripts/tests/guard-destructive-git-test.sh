#!/usr/bin/env bash
# Exercises .claude/hooks/guard-destructive-git.sh. Lives outside the repo so
# the invoking command line does not itself contain the trigger strings.
HOOK="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/.claude/hooks/guard-destructive-git.sh}"
pass=0; fail=0

check() { # name expected_exit json
    local name="$1" want="$2" json="$3"
    printf '%s' "$json" | "$HOOK" >/dev/null 2>&1
    local got=$?
    if [ "$got" = "$want" ]; then
        printf '  ok    %s\n' "$name"; pass=$((pass+1))
    else
        printf '  FAIL  %s (want exit %s, got %s)\n' "$name" "$want" "$got"; fail=$((fail+1))
    fi
}

P='{"tool_input":{"command":'

echo "must BLOCK (exit 2):"
check "force-push"                  2 "${P}\"git push --force origin codex/foo\"}}"
check "push -f"                     2 "${P}\"git push -f origin codex/foo\"}}"
check "push origin main"            2 "${P}\"git push origin main\"}}"
check "push HEAD:main"              2 "${P}\"git push origin HEAD:main\"}}"
check "force-with-lease"            2 "${P}\"git push --force-with-lease\"}}"
check "chained force-push"          2 "${P}\"cargo test && git push --force\"}}"
check "push -u origin master"       2 "${P}\"git push -u origin master\"}}"

echo "must ALLOW (exit 0):"
check "REGRESSION: push && gh pr whose body says rm -f" 0 \
    "${P}\"git push -q origin codex/x && gh pr create --body 'use rm -f before cp'\"}}"
check "normal feature push"         0 "${P}\"git push -u origin codex/develop-green-restore\"}}"
check "branch codex/main-thing"     0 "${P}\"git push origin codex/main-thing\"}}"
check "git status"                  0 "${P}\"git status --porcelain\"}}"
check "rm -f with no git push"      0 "${P}\"rm -f /tmp/x && cp a b\"}}"
check "ripgrep for main"            0 "${P}\"rg main crates/aiua/src/main.rs\"}}"
check "malformed json fails open"   0 'not json at all'

# --- destructive working-tree ops aimed at the MAIN CHECKOUT -----------------
#
# The hazard these encode: a session whose worktree is removed mid-run falls
# back to the main checkout SILENTLY, and the next `git reset --hard` destroys
# the operator's uncommitted files (2026-07-30, recurred 08-02). The same verbs
# are routine INSIDE a worktree, so scope — not the verb — is what decides.
MAIN=/Users/jaredlikes/code/philotic-stack
WT="$MAIN/.claude/worktrees"

echo "must BLOCK — destructive op targeting the main checkout (exit 2):"
check "cd main; reset --hard"       2 "${P}\"cd $MAIN\\ngit reset --hard origin/develop\"}}"
check "cd main; clean -fdx"         2 "${P}\"cd $MAIN && git clean -fdx\"}}"
check "cd main; checkout -- ."      2 "${P}\"cd $MAIN && git checkout -- .\"}}"
check "cd main; checkout ."         2 "${P}\"cd $MAIN && git checkout .\"}}"
check "cd main; restore ."          2 "${P}\"cd $MAIN && git restore .\"}}"
check "cd main; bare stash"         2 "${P}\"cd $MAIN && git stash\"}}"
check "cd main; stash pop"          2 "${P}\"cd $MAIN && git stash pop\"}}"
# -C must not be an escape hatch — it is the idiom CLAUDE.md recommends.
check "git -C main reset --hard"    2 "${P}\"git -C $MAIN reset --hard\"}}"
check "git -C main clean -fd"       2 "${P}\"git -C $MAIN clean -fd\"}}"
check "cd worktree then cd main"    2 "${P}\"cd $WT/foo\\ncd $MAIN\\ngit reset --hard\"}}"

echo "must ALLOW — same verbs scoped to a worktree (exit 0):"
check "-C worktree reset --hard"    0 "${P}\"git -C $WT/lifegraph-deploy-verify reset --hard origin/develop\"}}"
check "cd worktree; reset --hard"   0 "${P}\"cd $WT/lifegraph-deploy-verify && git reset --hard origin/develop\"}}"
check "cd worktree; clean -fdx"     0 "${P}\"cd $WT/lifegraph-deploy-verify && git clean -fdx\"}}"
check "cd main; branch switch"      0 "${P}\"cd $MAIN && git checkout develop\"}}"
check "cd main; status"             0 "${P}\"cd $MAIN && git status --short\"}}"
check "cd main; stash list"         0 "${P}\"cd $MAIN && git stash list\"}}"
check "cd main; restore --staged"   0 "${P}\"cd $MAIN && git restore --staged .\"}}"
check "cd main; log"                0 "${P}\"cd $MAIN && git log --oneline -1\"}}"
check "prose mentioning reset"      0 "${P}\"gh pr create --body 'we had to git reset --hard once'\"}}"

# --- the actual fallback shape -----------------------------------------------
#
# THE case this guard exists for, and the only one the harness's own
# worktree-isolation guard cannot cover. That guard refuses `cd <main> && git …`
# and `git -C <main> …` from a worktree-isolated session — but when the session's
# worktree has been REMOVED mid-run there is no worktree left to isolate to. The
# shell silently falls back to the main checkout and the command carries no `cd`
# and no `-C` at all: it is a bare `git reset --hard` whose cwd just happens to
# be the operator's checkout. Only cwd distinguishes it from a legitimate one.
check_in_dir() { # name expected_exit dir json
    local name="$1" want="$2" dir="$3" json="$4"
    local got
    printf '%s' "$json" | (cd "$dir" 2>/dev/null && "$HOOK") >/dev/null 2>&1
    got=$?
    if [ "$got" = "$want" ]; then
        printf '  ok    %s\n' "$name"; pass=$((pass+1))
    else
        printf '  FAIL  %s (want exit %s, got %s)\n' "$name" "$want" "$got"; fail=$((fail+1))
    fi
}

echo "fallback shape — bare command, no cd, no -C, cwd decides:"
check_in_dir "cwd=main: bare reset --hard"  2 "$MAIN" "${P}\"git reset --hard origin/develop\"}}"
check_in_dir "cwd=main: bare clean -fdx"    2 "$MAIN" "${P}\"git clean -fdx\"}}"
check_in_dir "cwd=main: bare stash"         2 "$MAIN" "${P}\"git stash\"}}"
check_in_dir "cwd=main: status is fine"     0 "$MAIN" "${P}\"git status --short\"}}"
check_in_dir "cwd=worktree: reset --hard"   0 "$WT/lifegraph-deploy-verify" \
    "${P}\"git reset --hard origin/develop\"}}"

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
