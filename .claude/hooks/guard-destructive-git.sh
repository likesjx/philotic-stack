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

# Collapse newlines/continuations so multi-line commands are matched too.
n=$(printf '%s' "$cmd" | tr '\n\\' '  ' | tr -s ' ')

# Only inspect commands that actually invoke git push / git merge, including
# after a pipe, semicolon, && or ||.
if printf '%s' "$n" | grep -qE '(^|[;&|]\s*)git\s+(-C\s+\S+\s+)?push\b'; then

    if printf '%s' "$n" | grep -qE '\-\-force(\b|=)|\s-f(\s|$)|\-\-force\-with\-lease'; then
        block "force-push"
    fi

    # Explicit refspec targeting main/master, e.g.
    #   git push origin main | git push origin HEAD:main | git push -u origin master
    if printf '%s' "$n" | grep -qE 'push\b.*\b(main|master)\b'; then
        block "push targeting main/master"
    fi

    # Bare `git push` while the checkout is on main/master.
    branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
    if [ "$branch" = "main" ] || [ "$branch" = "master" ]; then
        block "bare 'git push' while on branch '$branch'"
    fi
fi

# Merging anything into main from a working checkout. Releases go through the
# sync/develop-into-main PR path, not a local merge.
if printf '%s' "$n" | grep -qE '(^|[;&|]\s*)git\s+(-C\s+\S+\s+)?merge\b'; then
    branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
    if [ "$branch" = "main" ] || [ "$branch" = "master" ]; then
        block "git merge while on '$branch'"
    fi
fi

exit 0
