#!/usr/bin/env bash
# mempalace_reflex_hook.sh
# 
# This hook captures the agent's turn transcript and reflexively posts it to the intel-graph Memory Broker.
# It is designed to be wired into IDE/Agent Stop or PreCompact hooks.

set -e

# Default configuration
GRAPH_API_URL="http://127.0.0.1:8900/api/mempalace/turn"
AGENT_ID=${PHILOTIC_AGENT_ID:-"gemini-architect"}
SESSION_ID=${PHILOTIC_SESSION_ID:-"unknown-session"}
TRANSCRIPT_CONTENT=""

# If transcript file is passed as an argument (or from stdin)
if [ -f "$1" ]; then
    TRANSCRIPT_CONTENT=$(cat "$1")
elif [ ! -t 0 ]; then
    TRANSCRIPT_CONTENT=$(cat)
else
    # Fallback to recent working turn trace if defined in the env
    if [ -f "$PHILOTIC_WORK_TRACE" ]; then
        TRANSCRIPT_CONTENT=$(cat "$PHILOTIC_WORK_TRACE")
    fi
fi

if [ -z "$TRANSCRIPT_CONTENT" ]; then
    echo "NO_CONTENT: Skipping reflex memory post."
    exit 0
fi

PAYLOAD=$(cat <<EOF
{
  "agent_id": "$AGENT_ID",
  "session_id": "$SESSION_ID",
  "turn_transcript": $(jq -Rs . <<< "$TRANSCRIPT_CONTENT")
}
EOF
)

# Fire and forget to the intel-graph
response=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$GRAPH_API_URL" \
     -H "Content-Type: application/json" \
     -d "$PAYLOAD" || true)

if [ "$response" == "200" ]; then
    echo "Reflex memory sent successfully ($response)"
else
    echo "Warning: Reflex memory post failed ($response)"
fi
