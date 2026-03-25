#!/usr/bin/env bash
# smoke-agent-graph-runner.sh — live-hotel integration smoke for agent-graph-runner.
#
# Requires a running hotel (aiua). Starts agent-graph-runner against the hotel
# socket, waits for it to register, then verifies clean IPC connect/disconnect
# behavior and confirms the runner process exits on SIGTERM without a crash.
#
# Usage:
#   ./scripts/smoke-agent-graph-runner.sh [hotel-socket]
#
# Default socket: /tmp/philotic-aiua.sock (override via PHILOTIC_IPC_SOCKET or $1).
#
# For full tool-dispatch smoke, use the cargo integration test instead:
#   cargo test -p agent-graph-runner --test smoke
#
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOCKET="${1:-${PHILOTIC_IPC_SOCKET:-/tmp/philotic-aiua.sock}}"
AGENT_ID="smoke-graph-agent-$$"
TMP_DIR="$(mktemp -d)"
DB_PATH="${TMP_DIR}/agent-graph-${AGENT_ID}.db"
RUNNER_LOG="${TMP_DIR}/runner.log"

cleanup() {
  local exit_code=$?
  set +e
  if [[ -n "${RUNNER_PID:-}" ]]; then
    kill "${RUNNER_PID}" 2>/dev/null || true
    wait "${RUNNER_PID}" 2>/dev/null || true
  fi
  if [[ ${exit_code} -ne 0 ]]; then
    echo ""
    echo "FAILED — runner log:"
    [[ -f "${RUNNER_LOG}" ]] && cat "${RUNNER_LOG}"
  fi
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

echo "=== agent-graph-runner live smoke ==="
echo "Hotel socket : ${SOCKET}"
echo "Agent ID     : ${AGENT_ID}"
echo "DB path      : ${DB_PATH}"
echo ""

# ── Preflight ─────────────────────────────────────────────────────────────────

if [[ ! -S "${SOCKET}" ]]; then
  echo "ERROR: hotel socket '${SOCKET}' not found."
  echo "Start aiua first: just start-aiua"
  exit 1
fi

# ── Build ─────────────────────────────────────────────────────────────────────

echo "Building agent-graph-runner..."
cargo build -p agent-graph-runner --quiet
BINARY="${ROOT_DIR}/target/debug/agent-graph-runner"

# ── Start runner ──────────────────────────────────────────────────────────────

echo "Starting agent-graph-runner..."
PHILOTIC_AGENT_ID="${AGENT_ID}" \
PHILOTIC_IPC_SOCKET="${SOCKET}" \
PHILOTIC_AGENT_GRAPH_DB="${DB_PATH}" \
  "${BINARY}" >"${RUNNER_LOG}" 2>&1 &
RUNNER_PID=$!

# ── Wait for connection log line ──────────────────────────────────────────────

echo "Waiting for IPC connection (up to 10s)..."
for i in $(seq 1 20); do
  if grep -q "Connected to hotel IPC" "${RUNNER_LOG}" 2>/dev/null; then
    echo "  [${i}] Connected."
    break
  fi
  if ! kill -0 "${RUNNER_PID}" 2>/dev/null; then
    echo "ERROR: runner exited prematurely."
    exit 1
  fi
  sleep 0.5
done

if ! grep -q "Connected to hotel IPC" "${RUNNER_LOG}" 2>/dev/null; then
  echo "ERROR: runner did not connect within 10s."
  exit 1
fi

# ── Verify DB was created ─────────────────────────────────────────────────────

if [[ ! -f "${DB_PATH}" ]]; then
  echo "ERROR: agent graph DB was not created at ${DB_PATH}."
  exit 1
fi
echo "Agent graph DB created: OK"

# ── Graceful shutdown ─────────────────────────────────────────────────────────

echo "Sending SIGTERM..."
kill "${RUNNER_PID}"
wait "${RUNNER_PID}" 2>/dev/null || true
RUNNER_PID=""

echo ""
echo "=== agent-graph-runner smoke PASSED ==="
echo ""
echo "For full tool-dispatch coverage (no hotel required):"
echo "  cargo test -p agent-graph-runner --test smoke"
