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

current_branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")

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
