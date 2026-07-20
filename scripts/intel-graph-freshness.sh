#!/usr/bin/env bash
# Refresh intel-graph scan freshness and harness drift state.
#
# - If the graph server is up on :8900, scan through its API so there is a
#   single writer; otherwise scan directly into the DB with `phil graph scan`.
# - Then verify every managed harness so drift is continuously observed
#   instead of only when a Claude Code SessionStart hook happens to fire.
#
# Safe to run unattended: scan is idempotent, verify only records observations.
set -uo pipefail

REPO="${PHILOTIC_REPO:-/Users/jaredlikes/code/philotic-stack}"
GRAPH_URL="${PHILOTIC_GRAPH_URL:-http://127.0.0.1:8900}"
cd "$REPO"

echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] intel-graph freshness run"

if curl -s -m 3 -o /dev/null "$GRAPH_URL/api/status"; then
    if curl -s -m 900 -X POST "$GRAPH_URL/api/scan" \
        ${PHILOTIC_GRAPH_TOKEN:+-H "Authorization: Bearer $PHILOTIC_GRAPH_TOKEN"} \
        -o /dev/null; then
        echo "scan: refreshed via server API"
    else
        echo "scan: server API scan failed" >&2
    fi
else
    if phil graph scan >/dev/null 2>&1; then
        echo "scan: refreshed directly (server not running)"
    else
        echo "scan: direct scan failed (is phil on PATH?)" >&2
    fi
fi

# Verify all managed harnesses; tolerate individual failures so one broken
# harness doesn't hide drift state for the rest.
phil graph harness list 2>/dev/null | awk 'NR>2 {print $1}' | sed 's/^harness://' | while read -r harness; do
    [ -n "$harness" ] || continue
    phil graph harness verify "$harness" || true
done

phil graph harness drift || true
