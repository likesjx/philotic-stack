#!/usr/bin/env bash
# smoke-agent-graph-roundtrip.sh — live smoke for agent-graph-runner (B1 Seams 3 & 4).
#
# Spins up a temporary hotel (smoke mode, no mesh/materialization), starts
# agent-graph-runner against it, fires agent.graph.write + agent.graph.declare +
# agent.graph.sync via the smoke driver, then verifies side effects in SQLite.
#
# Seam 3: agent.graph.write, agent.graph.declare, agent.graph.recall dispatch
# Seam 4: agent.graph.sync LWW snapshot merge
#
# Usage:
#   ./scripts/smoke-agent-graph-roundtrip.sh
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOTEL_NAME="smoke-ag-$$"
AGENT_ID="smoke-agent-$$"
NODE_ID="${HOTEL_NAME}-aiua-01"
SOCKET_PATH="/tmp/philotic-${HOTEL_NAME}.sock"
TMP_DIR="$(mktemp -d)"
DB_PATH="${TMP_DIR}/agent-graph-${AGENT_ID}.db"
AIUA_LOG="${TMP_DIR}/aiua.log"
RUNNER_LOG="${TMP_DIR}/runner.log"

cleanup() {
  local exit_code=$?
  set +e
  [[ -n "${RUNNER_PID:-}" ]] && kill "${RUNNER_PID}" 2>/dev/null || true
  [[ -n "${AIUA_PID:-}" ]]   && kill "${AIUA_PID}"   2>/dev/null || true
  wait "${RUNNER_PID:-}" 2>/dev/null || true
  wait "${AIUA_PID:-}"   2>/dev/null || true
  rm -f "${SOCKET_PATH}"
  if [[ ${exit_code} -ne 0 ]]; then
    echo ""
    echo "=== FAILED — aiua log ==="
    [[ -f "${AIUA_LOG}" ]] && tail -40 "${AIUA_LOG}"
    echo ""
    echo "=== FAILED — runner log ==="
    [[ -f "${RUNNER_LOG}" ]] && tail -40 "${RUNNER_LOG}"
  fi
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

echo "=== agent-graph-runner live smoke (Seams 3 & 4) ==="
echo "Hotel     : ${HOTEL_NAME}  (node ${NODE_ID})"
echo "Agent ID  : ${AGENT_ID}"
echo "Socket    : ${SOCKET_PATH}"
echo "DB path   : ${DB_PATH}"
echo ""

# ── Build ─────────────────────────────────────────────────────────────────────

echo "Building binaries..."
cargo build -p aiua -p agent-graph-runner \
  -p philotic-client --example agent_graph_smoke_driver \
  --quiet 2>&1
echo "Build OK"
echo ""

AIUA_BIN="${ROOT_DIR}/target/debug/aiua"
RUNNER_BIN="${ROOT_DIR}/target/debug/agent-graph-runner"
DRIVER_BIN="${ROOT_DIR}/target/debug/examples/agent_graph_smoke_driver"

# ── Start hotel (smoke mode) ───────────────────────────────────────────────────

echo "Starting hotel in smoke mode..."
(
  cd "${TMP_DIR}"
  PHILOTIC_SMOKE_MODE=1 "${AIUA_BIN}" --hotel "${HOTEL_NAME}" \
    >"${AIUA_LOG}" 2>&1
) &
AIUA_PID=$!

for _ in {1..50}; do
  [[ -S "${SOCKET_PATH}" ]] && break
  sleep 0.2
done
if [[ ! -S "${SOCKET_PATH}" ]]; then
  echo "ERROR: hotel socket did not appear within 10s"
  exit 1
fi
echo "Hotel socket ready"

# ── Start agent-graph-runner ───────────────────────────────────────────────────

echo "Starting agent-graph-runner..."
PHILOTIC_AGENT_ID="${AGENT_ID}" \
PHILOTIC_IPC_SOCKET="${SOCKET_PATH}" \
PHILOTIC_AGENT_GRAPH_DB="${DB_PATH}" \
  "${RUNNER_BIN}" >"${RUNNER_LOG}" 2>&1 &
RUNNER_PID=$!

echo "Waiting for runner IPC connection (up to 10s)..."
for i in $(seq 1 20); do
  grep -q "Connected to hotel IPC" "${RUNNER_LOG}" 2>/dev/null && { echo "  [${i}] Connected."; break; }
  kill -0 "${RUNNER_PID}" 2>/dev/null || { echo "ERROR: runner exited prematurely"; exit 1; }
  sleep 0.5
done
if ! grep -q "Connected to hotel IPC" "${RUNNER_LOG}" 2>/dev/null; then
  echo "ERROR: runner did not connect within 10s"
  exit 1
fi

# ── Dispatch tool calls via smoke driver ──────────────────────────────────────

echo ""
echo "Dispatching agent.graph.* tool calls..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
PHILOTIC_TARGET_NODE="${NODE_ID}" \
PHILOTIC_AGENT_ID="${AGENT_ID}" \
  "${DRIVER_BIN}"

# ── Wait for runner to process the tasks ─────────────────────────────────────

echo ""
echo "Waiting for runner to process tasks (2s)..."
sleep 2

# ── Verify side effects in SQLite ─────────────────────────────────────────────

echo ""
echo "Verifying SQLite side effects..."

if [[ ! -f "${DB_PATH}" ]]; then
  echo "ERROR: agent graph DB not found at ${DB_PATH}"
  exit 1
fi

# Seam 3: agent.graph.write created a tool_preference row
PREF_COUNT=$(sqlite3 "${DB_PATH}" "SELECT COUNT(*) FROM tool_preferences WHERE tool_name='smoke.tool';" 2>/dev/null || echo "0")
if [[ "${PREF_COUNT}" -lt 1 ]]; then
  echo "ERROR: expected tool_preference 'smoke.tool' — got count=${PREF_COUNT}"
  exit 1
fi
echo "  tool_preferences['smoke.tool']   : ${PREF_COUNT} row(s) — OK (Seam 3 write)"

# Seam 3: agent.graph.declare created a resource_declaration row
# resource_type is stored as a JSON-serialized string, e.g. '"router_listener"' (with quotes).
DECL_COUNT=$(sqlite3 "${DB_PATH}" "SELECT COUNT(*) FROM resource_declarations WHERE resource_type='\"router_listener\"';" 2>/dev/null || echo "0")
if [[ "${DECL_COUNT}" -lt 1 ]]; then
  echo "ERROR: expected resource_declaration 'RouterListener' — got count=${DECL_COUNT}"
  exit 1
fi
echo "  resource_declarations[RouterListener]: ${DECL_COUNT} row(s) — OK (Seam 3 declare)"

# Seam 4: agent.graph.sync inserted 'sync.tool' preference via LWW snapshot
SYNC_COUNT=$(sqlite3 "${DB_PATH}" "SELECT COUNT(*) FROM tool_preferences WHERE tool_name='sync.tool';" 2>/dev/null || echo "0")
if [[ "${SYNC_COUNT}" -lt 1 ]]; then
  echo "ERROR: expected LWW-merged preference 'sync.tool' — got count=${SYNC_COUNT}"
  exit 1
fi
SYNC_AT=$(sqlite3 "${DB_PATH}" "SELECT updated_at FROM tool_preferences WHERE tool_name='sync.tool';" 2>/dev/null || echo "0")
if [[ "${SYNC_AT}" -lt 9000000000 ]]; then
  echo "ERROR: sync.tool updated_at expected ~9999999999, got ${SYNC_AT}"
  exit 1
fi
echo "  tool_preferences['sync.tool']    : updated_at=${SYNC_AT} — OK (Seam 4 LWW sync)"

# ── Graceful shutdown ─────────────────────────────────────────────────────────

echo ""
echo "Sending SIGTERM to runner..."
kill "${RUNNER_PID}"
wait "${RUNNER_PID}" 2>/dev/null || true
RUNNER_PID=""

echo ""
echo "=== agent-graph-runner smoke PASSED (Seams 3 & 4) ==="
