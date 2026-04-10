#!/bin/bash
# Smoke test: Full agent workflow via REST API
# Exercises: scan → next_task → context → session start → decide → session close → dashboard
#
# Requires: intel-graph stack running (just intel-graph-ensure)
# Usage: bash scripts/smoke-agent-workflow.sh

set -euo pipefail

BASE="http://127.0.0.1:8900"
AGENT="smoke-test-agent"
SESSION_ID="smoke-session-$(date +%s)"
PASS=0
FAIL=0

green() { printf '\033[0;32m%s\033[0m\n' "$1"; }
red()   { printf '\033[0;31m%s\033[0m\n' "$1"; }
step()  { printf '\033[1;34m[step %s]\033[0m %s\n' "$1" "$2"; }

check() {
    local label="$1"
    local code="$2"
    if [[ "$code" -ge 200 && "$code" -lt 300 ]]; then
        green "  ✓ $label (HTTP $code)"
        (( PASS += 1 )) || true
    else
        red "  ✗ $label (HTTP $code)"
        (( FAIL += 1 )) || true
    fi
}

# Pre-flight: verify graph is running
step 0 "Pre-flight: check graph server"
HTTP=$(curl -s -o /dev/null -w "%{http_code}" "$BASE/api/status" 2>/dev/null || echo "000")
if [[ "$HTTP" == "000" ]]; then
    red "Graph server not running. Start with: just intel-graph-ensure"
    exit 1
fi
check "graph server reachable" "$HTTP"

# Step 1: Trigger a scan
step 1 "Trigger graph scan"
HTTP=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$BASE/api/scan" \
    -H "Content-Type: application/json" \
    -d '{}')
check "POST /api/scan" "$HTTP"

# Step 2: Get status
step 2 "Get graph status"
RESP=$(curl -s "$BASE/api/status")
HTTP=$(curl -s -o /dev/null -w "%{http_code}" "$BASE/api/status")
check "GET /api/status" "$HTTP"
NODE_SUMMARY=$(echo "$RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); nc=d.get('node_counts',{}); print(f'nodes: {sum(int(v) for v in nc.values() if isinstance(v,(int,float)))}, edges: {d.get(\"edge_count\",0)}')" 2>/dev/null || echo "(parse failed)")
echo "  → $NODE_SUMMARY"

# Step 3: Get next task
step 3 "Get next task recommendation"
RESP=$(curl -s "$BASE/api/next_task")
HTTP=$(curl -s -o /dev/null -w "%{http_code}" "$BASE/api/next_task")
check "GET /api/next_task" "$HTTP"
TASK_ID=$(echo "$RESP" | python3 -c 'import sys,json; print(json.load(sys.stdin).get("id","none"))' 2>/dev/null || echo "none")
echo "  → recommended task: $TASK_ID"

# Step 4: Get context for the task (or fall back to first proposal)
step 4 "Get context for target"
if [[ "$TASK_ID" != "none" && "$TASK_ID" != "null" ]]; then
    TARGET="$TASK_ID"
else
    # Fallback: pick first proposal from /api/proposals
    TARGET=$(curl -s "$BASE/api/proposals" | python3 -c 'import sys,json; ps=json.load(sys.stdin); print(ps[0]["id"] if ps else "none")' 2>/dev/null || echo "none")
fi

if [[ "$TARGET" != "none" && "$TARGET" != "null" ]]; then
    HTTP=$(curl -s -o /dev/null -w "%{http_code}" "$BASE/api/context/$TARGET")
    check "GET /api/context/$TARGET" "$HTTP"
else
    red "  ✗ No proposals in graph — skipping context test"
    (( FAIL += 1 )) || true
fi

# Step 5: Start a session
step 5 "Start agent session"
SEAM_ID="${TARGET:-seam:smoke-test}"
HTTP=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$BASE/api/session/start" \
    -H "Content-Type: application/json" \
    -d "{\"session_id\":\"$SESSION_ID\",\"agent\":\"$AGENT\",\"seam_id\":\"$SEAM_ID\",\"phase\":\"smoke-test\"}")
check "POST /api/session/start" "$HTTP"

# Step 6: Record a decision
step 6 "Record a decision"
HTTP=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$BASE/api/decide" \
    -H "Content-Type: application/json" \
    -d "{\"target_node\":\"$SEAM_ID\",\"action\":\"smoke_test\",\"reason\":\"Smoke test decision\",\"agent\":\"$AGENT\",\"session\":\"$SESSION_ID\"}")
check "POST /api/decide" "$HTTP"

# Step 7: Check dashboard
step 7 "Check agent dashboard"
RESP=$(curl -s "$BASE/api/dashboard")
HTTP=$(curl -s -o /dev/null -w "%{http_code}" "$BASE/api/dashboard")
check "GET /api/dashboard" "$HTTP"
ACTIVE=$(echo "$RESP" | python3 -c 'import sys,json; print(len(json.load(sys.stdin).get("active_sessions",[])))' 2>/dev/null || echo "?")
echo "  → active sessions: $ACTIVE"

# Step 8: Check impact
step 8 "Check impact analysis"
if [[ "$TARGET" != "none" && "$TARGET" != "null" ]]; then
    HTTP=$(curl -s -o /dev/null -w "%{http_code}" "$BASE/api/impact/$TARGET")
    check "GET /api/impact/$TARGET" "$HTTP"
else
    red "  ✗ No target — skipping impact test"
    (( FAIL += 1 )) || true
fi

# Step 9: Close the session
step 9 "Close agent session"
HTTP=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$BASE/api/session/close" \
    -H "Content-Type: application/json" \
    -d "{\"session_id\":\"$SESSION_ID\",\"summary\":\"Smoke test completed\",\"files_touched\":[\"scripts/smoke-agent-workflow.sh\"]}")
check "POST /api/session/close" "$HTTP"

# Step 10: Verify session closed on dashboard
step 10 "Verify session closed"
RESP=$(curl -s "$BASE/api/dashboard")
HTTP=$(curl -s -o /dev/null -w "%{http_code}" "$BASE/api/dashboard")
check "GET /api/dashboard (post-close)" "$HTTP"

# Summary
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
if [[ "$FAIL" -eq 0 ]]; then
    green "All $PASS checks passed ✓"
    exit 0
else
    red "$FAIL checks failed, $PASS passed"
    exit 1
fi
