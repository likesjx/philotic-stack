#!/usr/bin/env bash
# PreToolUse(mcp__intel-graph__.*) guard for the unattended graph-task runner.
#
# Loaded only through .claude/runner/settings.json, i.e. only in sessions that
# scripts/claude_graph_runner.py launches. The runner itself owns every ledger
# write (claim, receipt, test run, decision, session close). The model gets:
#
#   - read-only graph tools, and
#   - session_activity, but only against the runner's own session id.
#
# The settings deny list already blocks the write tools; this hook is the
# second rail, and the only one that can check the session id argument.
#
# Contract: exit 2 blocks and shows stderr to Claude, exit 0 allows.
# Unlike guard-destructive-git.sh this guard FAILS CLOSED when it runs inside a
# runner session: nobody is watching an unattended run, so an unreadable input
# is treated as a write attempt. Outside a runner session it is a no-op.
set -uo pipefail

[ "${CLAUDE_GRAPH_RUNNER:-}" = "1" ] || exit 0

input=$(cat)

verdict=$(printf '%s' "$input" | IG_SESSION_ID="${IG_SESSION_ID:-}" python3 -c '
import json, os, sys
READ_ONLY = {
    "graph_status", "graph_digest", "graph_proposals", "graph_node", "graph_query",
    "graph_search", "graph_snippet", "graph_skeleton", "graph_context_for", "graph_impact",
    "graph_semantic_search", "graph_agent_dashboard",
}
try:
    d = json.load(sys.stdin)
except Exception:
    print("block:unreadable hook input"); sys.exit()
name = str(d.get("tool_name", ""))
tool = name.split("__", 2)[-1] if name.startswith("mcp__intel-graph__") else ""
args = d.get("tool_input") or {}
if tool in READ_ONLY:
    print("allow")
elif tool == "session_activity":
    want = os.environ.get("IG_SESSION_ID", "")
    if want and args.get("session_id") == want:
        print("allow")
    else:
        print("block:session_activity may only report on " + (want or "the runner session"))
else:
    print(f"block:{tool or name} is a ledger write; the runner records results, not the session")
' 2>/dev/null) || verdict="block:guard could not evaluate the call"

case "$verdict" in
    allow) exit 0 ;;
    *)
        echo "BLOCKED by .claude/hooks/guard-graph-writes.sh: ${verdict#block:}" >&2
        echo "" >&2
        echo "Runner sessions read the graph and report progress on their own session." >&2
        echo "Put results in your final message; the runner writes the receipt." >&2
        exit 2
        ;;
esac
