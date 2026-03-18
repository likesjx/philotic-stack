#!/usr/bin/env bash
# Smoke test for graph-runner: create graph → upsert node → get node → export.
# Starts a throw-away hotel + graph-runner, drives the full round-trip via
# graph_smoke_driver, then tears down cleanly.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
HOTEL_NAME="graph-smoke-$$"
SOCKET_PATH="/tmp/philotic-${HOTEL_NAME}.sock"
DB_PATH="${TMP_DIR}/graph-runner.db"
export PHILOTIC_NODE_ID="${HOTEL_NAME}-aiua-01"
export PHILOTIC_TARGET_NODE="${HOTEL_NAME}-aiua-01"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "Graph-runner smoke FAILED."
    echo "=== aiua log ==="
    [[ -f "${TMP_DIR}/aiua.log" ]] && cat "${TMP_DIR}/aiua.log"
    echo "=== graph-runner log ==="
    [[ -f "${TMP_DIR}/graph-runner.log" ]] && cat "${TMP_DIR}/graph-runner.log"
  fi
  [[ -n "${GRAPH_PID:-}" ]] && kill "${GRAPH_PID}" 2>/dev/null || true
  [[ -n "${AIUA_PID:-}" ]]  && kill "${AIUA_PID}"  2>/dev/null || true
  wait "${GRAPH_PID:-}" 2>/dev/null || true
  wait "${AIUA_PID:-}"  2>/dev/null || true
  rm -f "${SOCKET_PATH}"
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

echo "Building graph-runner smoke binaries..."
cargo build -p aiua -p graph-runner -p philotic-client --example graph_smoke_driver 2>/dev/null

echo "Starting aiua (hotel=${HOTEL_NAME})..."
PHILOTIC_SMOKE_MODE=1 \
  cargo run -q --manifest-path "${ROOT_DIR}/crates/aiua/Cargo.toml" --bin aiua \
  -- --hotel "${HOTEL_NAME}" \
  >"${TMP_DIR}/aiua.log" 2>&1 &
AIUA_PID=$!

# Wait for socket
for _ in {1..50}; do
  [[ -S "${SOCKET_PATH}" ]] && break
  sleep 0.2
done
if [[ ! -S "${SOCKET_PATH}" ]]; then
  echo "aiua socket did not appear at ${SOCKET_PATH}"
  exit 1
fi

echo "Starting graph-runner..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
PHILOTIC_NODE_ID="${PHILOTIC_NODE_ID}" \
PHILOTIC_GRAPH_RUNNER_ID="${HOTEL_NAME}:graph-runner" \
PHILOTIC_GRAPH_DB_PATH="${DB_PATH}" \
  cargo run -q --manifest-path "${ROOT_DIR}/crates/graph-runner/Cargo.toml" --bin graph-runner \
  >"${TMP_DIR}/graph-runner.log" 2>&1 &
GRAPH_PID=$!

# Wait for graph-runner to register
for _ in {1..50}; do
  if grep -q "Graph runner ready" "${TMP_DIR}/graph-runner.log" 2>/dev/null; then
    break
  fi
  sleep 0.2
done
if ! grep -q "Graph runner ready" "${TMP_DIR}/graph-runner.log" 2>/dev/null; then
  echo "graph-runner did not log 'Graph runner ready'"
  exit 1
fi

echo "Driving graph round-trip..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  cargo run -q -p philotic-client --example graph_smoke_driver

echo "Graph-runner smoke round-trip succeeded."
