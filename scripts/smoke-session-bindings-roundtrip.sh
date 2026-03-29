#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
HOTEL_NAME="jane-smoke-sess-bind-$$"
export PHILOTIC_AGENT_ID="agent-jane-01"
export PHILOTIC_NODE_ID="${HOTEL_NAME}-aiua-01"
export PHILOTIC_TARGET_NODE="${HOTEL_NAME}-aiua-01"
export PHILOTIC_FINAL_REPLY_TO="${HOTEL_NAME}-aiua-01"
SOCKET_PATH="/tmp/philotic-${HOTEL_NAME}.sock"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "Session-bindings smoke failed. aiua log:"
    [[ -f "${TMP_DIR}/aiua.log" ]] && cat "${TMP_DIR}/aiua.log"
    echo "Session-bindings smoke failed. agent log:"
    [[ -f "${TMP_DIR}/agent.log" ]] && cat "${TMP_DIR}/agent.log"
  fi
  [[ -n "${AGENT_PID:-}" ]] && kill "${AGENT_PID}" >/dev/null 2>&1
  [[ -n "${ANSIBLE_PID:-}" ]] && kill "${ANSIBLE_PID}" >/dev/null 2>&1
  wait "${AGENT_PID:-}" >/dev/null 2>&1
  wait "${ANSIBLE_PID:-}" >/dev/null 2>&1
  rm -f "${SOCKET_PATH}"
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

echo "Building session-bindings smoke binaries..."
cargo build -p aiua -p philote -p philotic-client --example session_bindings_smoke_driver >/dev/null
AIUA_BIN="${ROOT_DIR}/target/debug/aiua"
PHILOTE_BIN="${ROOT_DIR}/target/debug/philote"

echo "Starting aiua in ${TMP_DIR}..."
(
  cd "${TMP_DIR}"
  PHILOTIC_SMOKE_MODE=1 "${AIUA_BIN}" --hotel "${HOTEL_NAME}" >"${TMP_DIR}/aiua.log" 2>&1
) &
ANSIBLE_PID=$!

for _ in {1..50}; do
  if [[ -S "${SOCKET_PATH}" ]]; then
    break
  fi
  sleep 0.2
done

if [[ ! -S "${SOCKET_PATH}" ]]; then
  echo "aiua socket did not appear"
  exit 1
fi

echo "Starting philote..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  "${PHILOTE_BIN}" >"${TMP_DIR}/agent.log" 2>&1 &
AGENT_PID=$!

sleep 1

echo "Driving session-bindings round-trip..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  cargo run -q -p philotic-client --example session_bindings_smoke_driver

echo "Session-bindings smoke round-trip succeeded."
