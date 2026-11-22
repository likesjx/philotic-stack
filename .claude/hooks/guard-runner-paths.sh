#!/usr/bin/env bash
# PreToolUse(Read|Grep|Glob|Bash) guard for the unattended graph-task runner.
#
# Loaded only through .claude/runner/settings.json, i.e. only in sessions that
# scripts/claude_graph_runner.py launches. It keeps every file read inside the
# runner's worktree, so the session cannot reach ~/.ssh, ~/.aws, keychains,
# token files or other checkouts, whatever the permission rules miss.
#
#   Read / Grep / Glob   the path argument (and an absolute or `..` pattern)
#                        must resolve inside the worktree.
#   Bash                 no argument may name a path outside the worktree
#                        (absolute paths, `~`, `..`), no shell expansion
#                        (`$`, backticks), and git may not use the flags that
#                        read or write outside the repo (--no-index, --output,
#                        -C, -c, --git-dir, --work-tree, --exec-path).
#
# /dev/null is the one path outside the worktree that is allowed.
#
# Contract: exit 2 blocks and shows stderr to Claude, exit 0 allows.
# Inside a runner session it FAILS CLOSED: an unreadable input or an argument
# it cannot parse is blocked. Outside a runner session it is a no-op.
set -uo pipefail

[ "${CLAUDE_GRAPH_RUNNER:-}" = "1" ] || exit 0

input=$(cat)

verdict=$(printf '%s' "$input" | python3 -c '
import json, os, shlex, sys

def out(v):
    print(v); sys.exit()

try:
    d = json.load(sys.stdin)
except Exception:
    out("block:unreadable hook input")

cwd = d.get("cwd") or os.getcwd()
root = os.path.realpath(os.environ.get("CLAUDE_PROJECT_DIR") or cwd)
ALLOWED_OUTSIDE = {"/dev/null"}

def inside(p):
    p = os.path.expanduser(str(p))
    if not os.path.isabs(p):
        p = os.path.join(cwd, p)
    real = os.path.realpath(p)
    return real in ALLOWED_OUTSIDE or real == root or real.startswith(root + os.sep)

tool = str(d.get("tool_name", ""))
args = d.get("tool_input") or {}

if tool in ("Read", "Grep", "Glob"):
    for key in ("file_path", "path", "notebook_path"):
        val = args.get(key)
        if val and not inside(val):
            out("block:" + tool + " may only read inside the worktree, not " + str(val))
    for key in ("pattern", "glob"):
        val = str(args.get(key) or "")
        if tool != "Grep" or key == "glob":
            if val.startswith(("/", "~")) or ".." in val.split("/"):
                out("block:" + tool + " " + key + " must stay inside the worktree")
    out("allow")

if tool == "Bash":
    cmd = str(args.get("command", ""))
    if "$" in cmd or "`" in cmd:
        out("block:shell expansion is not allowed in runner sessions")
    try:
        tokens = shlex.split(cmd, comments=False, posix=True)
    except ValueError:
        out("block:could not parse the command")
    GIT_BANNED = ("--no-index", "--output", "--git-dir", "--work-tree", "--exec-path",
                  "--namespace", "--super-prefix")
    seen_git = False
    for i, tok in enumerate(tokens):
        if tok == "git":
            seen_git = True
        if seen_git and (tok in ("-C", "-c") or tok.startswith(GIT_BANNED)):
            out("block:git " + tok.split("=")[0] + " can reach outside the repo")
        value = tok.split("=", 1)[1] if tok.startswith("-") and "=" in tok else tok
        if not value:
            continue
        looks_like_path = (value.startswith(("/", "~")) or ".." in value.split("/")
                           or os.path.lexists(os.path.join(cwd, value)))
        if looks_like_path and not inside(value):
            out("block:" + value + " is outside the worktree")
    out("allow")

out("allow")
' 2>/dev/null) || verdict="block:guard could not evaluate the call"

case "$verdict" in
    allow) exit 0 ;;
    *)
        echo "BLOCKED by .claude/hooks/guard-runner-paths.sh: ${verdict#block:}" >&2
        echo "" >&2
        echo "Runner sessions read and run commands inside their own worktree only." >&2
        echo "If the task needs something outside it, stop and report a blocker." >&2
        exit 2
        ;;
esac
