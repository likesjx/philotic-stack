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

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
