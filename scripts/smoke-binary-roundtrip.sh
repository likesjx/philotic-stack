#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
HOTEL_NAME="jane-smoke-$$"
export PHILOTIC_AGENT_ID="agent-jane-01"
export PHILOTIC_NODE_ID="${HOTEL_NAME}-ansible-01"
export PHILOTIC_TARGET_NODE="${HOTEL_NAME}-ansible-01"
export PHILOTIC_FINAL_REPLY_TO="${HOTEL_NAME}-ansible-01"
SOCKET_PATH="/tmp/philotic-${HOTEL_NAME}.sock"
MODEL_REPLY="${PHILOTIC_SMOKE_EXPECTED_REPLY:-pong}"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "Smoke test failed. ansible log:"
    [[ -f "${TMP_DIR}/ansible.log" ]] && cat "${TMP_DIR}/ansible.log"
    echo "Smoke test failed. agent log:"
    [[ -f "${TMP_DIR}/agent.log" ]] && cat "${TMP_DIR}/agent.log"
    echo "Smoke test failed. model log:"
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

echo "Building smoke-test binaries..."
cargo build -p ansible -p agent-core -p philotic-client --example smoke_driver >/dev/null

echo "Starting ansible in ${TMP_DIR}..."
(
  cd "${TMP_DIR}"
  PHILOTIC_SMOKE_MODE=1 "${ROOT_DIR}/target/debug/ansible" --hotel "${HOTEL_NAME}" >"${TMP_DIR}/ansible.log" 2>&1
) &
ANSIBLE_PID=$!

for _ in {1..50}; do
  if [[ -S "${SOCKET_PATH}" ]]; then
    break
  fi
  sleep 0.2
done

if [[ ! -S "${SOCKET_PATH}" ]]; then
  echo "ansible socket did not appear; log follows:"
  cat "${TMP_DIR}/ansible.log"
  exit 1
fi

echo "Starting agent-core..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  "${ROOT_DIR}/target/debug/agent-core" >"${TMP_DIR}/agent.log" 2>&1 &
AGENT_PID=$!

sleep 1

echo "Driving smoke round-trip..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
PHILOTIC_SMOKE_EXPECTED_REPLY="${MODEL_REPLY:-pong}" \
PHILOTIC_SMOKE_USER_CONTENT="${PHILOTIC_SMOKE_USER_CONTENT:-/ping}" \
  cargo run -q -p philotic-client --example smoke_driver

echo "Smoke round-trip succeeded."
