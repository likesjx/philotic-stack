#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
HOTEL_NAME="jane-smoke-deny-$$"
export PHILOTIC_NODE_ID="${HOTEL_NAME}-aiua-01"
export PHILOTIC_TARGET_NODE="${HOTEL_NAME}-aiua-01"
export PHILOTIC_FINAL_REPLY_TO="${HOTEL_NAME}-aiua-01"
export PHILOTIC_MODEL_ROUTER_STUB_RESPONSE="approval-turn-1=APPROVAL_REQUIRED: deploy the thing;approval-turn-2=Denied: deploy the thing"
SOCKET_PATH="/tmp/philotic-${HOTEL_NAME}.sock"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "Deny smoke failed. aiua log:"
    [[ -f "${TMP_DIR}/aiua.log" ]] && cat "${TMP_DIR}/aiua.log"
    echo "Deny smoke failed. agent log:"
    [[ -f "${TMP_DIR}/agent.log" ]] && cat "${TMP_DIR}/agent.log"
    echo "Deny smoke failed. model log:"
    [[ -f "${TMP_DIR}/model.log" ]] && cat "${TMP_DIR}/model.log"
  fi
  [[ -n "${MODEL_PID:-}" ]] && kill "${MODEL_PID}" >/dev/null 2>&1
  [[ -n "${AGENT_PID:-}" ]] && kill "${AGENT_PID}" >/dev/null 2>&1
  [[ -n "${ANSIBLE_PID:-}" ]] && kill "${ANSIBLE_PID}" >/dev/null 2>&1
  wait "${MODEL_PID:-}" >/dev/null 2>&1
  wait "${AGENT_PID:-}" >/dev/null 2>&1
  wait "${ANSIBLE_PID:-}" >/dev/null 2>&1
  rm -f "${SOCKET_PATH}"
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

echo "Building deny-smoke binaries..."
cargo build -p aiua -p philote -p model-router -p philotic-client --example approval_smoke_driver >/dev/null
AIUA_BIN="${ROOT_DIR}/target/debug/aiua"
PHILOTE_BIN="${ROOT_DIR}/target/debug/philote"
MODEL_ROUTER_BIN="${ROOT_DIR}/target/debug/model-router"

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

echo "Starting model-router..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  "${MODEL_ROUTER_BIN}" >"${TMP_DIR}/model.log" 2>&1 &
MODEL_PID=$!

sleep 1

echo "Driving deny round-trip..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
PHILOTIC_SMOKE_APPROVAL_COMMAND="/deny" \
PHILOTIC_SMOKE_EXPECTED_FINAL="Denied: deploy the thing" \
  cargo run -q -p philotic-client --example approval_smoke_driver

echo "Deny smoke round-trip succeeded."
