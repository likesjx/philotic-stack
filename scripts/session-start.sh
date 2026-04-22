#!/usr/bin/env bash
set -euo pipefail

if ! curl -fsS http://127.0.0.1:8900/api/status >/dev/null 2>&1; then
    echo "Graph server not reachable; skipping board registration."
    exit 0
fi

SESSION_ID="${PHILOTIC_SESSION_ID:-${CODEX_SESSION_ID:-session:codex-$(date +%Y%m%d%H%M%S)-$$}}"
AGENT="${PHILOTIC_SESSION_AGENT:-codex}"
AGENT_MODEL="${PHILOTIC_SESSION_MODEL:-gpt-5.4}"
SEAM_ID="${PHILOTIC_SESSION_SEAM:-seam:session-start-bootstrap-slice}"
PROPOSAL_ID="${PHILOTIC_SESSION_PROPOSAL:-dev-engine-optimization}"
PHASE="${PHILOTIC_SESSION_PHASE:-started}"
TASK_ID="${PHILOTIC_SESSION_TASK:-}"

PAYLOAD="$(SESSION_ID="${SESSION_ID}" \
AGENT="${AGENT}" \
AGENT_MODEL="${AGENT_MODEL}" \
SEAM_ID="${SEAM_ID}" \
PROPOSAL_ID="${PROPOSAL_ID}" \
PHASE="${PHASE}" \
TASK_ID="${TASK_ID}" \
python3 - <<'PY'
import json
import os

body = {
    "session_id": os.environ["SESSION_ID"],
    "agent": os.environ["AGENT"],
    "agent_model": os.environ["AGENT_MODEL"],
    "seam_id": os.environ["SEAM_ID"],
    "proposal_id": os.environ["PROPOSAL_ID"],
    "phase": os.environ["PHASE"],
}
task_id = os.environ.get("TASK_ID", "").strip()
if task_id:
    body["task_id"] = task_id
print(json.dumps(body))
PY
)"

RESPONSE="$(curl -fsS -X POST http://127.0.0.1:8900/api/session/start \
    -H "Content-Type: application/json" \
    -d "${PAYLOAD}")"

echo "${RESPONSE}" | jq .
echo "Graph board session started: ${SESSION_ID} (${SEAM_ID})"
