#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
HOTEL_NAME="lifegraph-flywheel-smoke-$$"
NODE_NAME="${HOTEL_NAME}-aiua-01"
SOCKET_PATH="/tmp/philotic-${HOTEL_NAME}.sock"

: "${PHILOTIC_MEMGRAPH_URI:?set PHILOTIC_MEMGRAPH_URI for the validation graph}"
: "${PHILOTIC_ONNX_SIDECAR_ADDR:?set PHILOTIC_ONNX_SIDECAR_ADDR for embed-on-write}"

cleanup() {
  local exit_code=$?
  set +e
  [[ -n "${RUNNER_PID:-}" ]] && kill "${RUNNER_PID}" >/dev/null 2>&1
  [[ -n "${AIUA_PID:-}" ]] && kill "${AIUA_PID}" >/dev/null 2>&1
  wait "${RUNNER_PID:-}" >/dev/null 2>&1
  wait "${AIUA_PID:-}" >/dev/null 2>&1
  if [[ ${exit_code} -ne 0 ]]; then
    echo "Flywheel smoke failed. aiua log:"
    [[ -f "${TMP_DIR}/aiua.log" ]] && tail -120 "${TMP_DIR}/aiua.log"
    echo "Flywheel smoke failed. runner log:"
    [[ -f "${TMP_DIR}/runner.log" ]] && tail -120 "${TMP_DIR}/runner.log"
  fi
  rm -f "${SOCKET_PATH}"
  rm -rf "${TMP_DIR}"
  exit "${exit_code}"
}
trap cleanup EXIT

echo "Building isolated LifeGraph flywheel smoke binaries..."
cargo build -q -p aiua -p data-memorygraphrag
cargo build -q -p philotic-client --example life_graph_ipc_smoke_driver

echo "Starting isolated hotel ${HOTEL_NAME}..."
(
  cd "${TMP_DIR}"
  PHILOTIC_SMOKE_MODE=1 \
  PHILOTIC_GRAPH_DB_PATH="${TMP_DIR}/context.db" \
  "${ROOT_DIR}/target/debug/aiua" --hotel "${HOTEL_NAME}" \
    >"${TMP_DIR}/aiua.log" 2>&1
) &
AIUA_PID=$!

for _ in {1..50}; do
  [[ -S "${SOCKET_PATH}" ]] && break
  sleep 0.2
done
[[ -S "${SOCKET_PATH}" ]] || {
  echo "isolated hotel socket did not appear"
  exit 1
}

echo "Starting updated LifeGraph runner..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
PHILOTIC_NODE_ID="${NODE_NAME}" \
PHILOTIC_MEMGRAPH_URI="${PHILOTIC_MEMGRAPH_URI}" \
PHILOTIC_MEMGRAPH_USER="${PHILOTIC_MEMGRAPH_USER:-}" \
PHILOTIC_MEMGRAPH_PASSWORD="${PHILOTIC_MEMGRAPH_PASSWORD:-}" \
PHILOTIC_ONNX_SIDECAR_ADDR="${PHILOTIC_ONNX_SIDECAR_ADDR}" \
  "${ROOT_DIR}/target/debug/life-graph-runner" \
    >"${TMP_DIR}/runner.log" 2>&1 &
RUNNER_PID=$!

sleep 1

echo "Driving capture, brief, review, recall, and feedback through hotel IPC..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
PHILOTIC_TARGET_NODE="${NODE_NAME}" \
PHILOTIC_REPLY_NODE="${NODE_NAME}" \
LIFE_GRAPH_SMOKE_FLYWHEEL=1 \
  cargo run -q -p philotic-client --example life_graph_ipc_smoke_driver

echo "LifeGraph flywheel isolated IPC smoke succeeded."
