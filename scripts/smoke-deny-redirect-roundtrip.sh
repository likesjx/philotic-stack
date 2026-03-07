#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
HOTEL_NAME="smk-deny-redir-$$"
SOCKET_PATH="/tmp/philotic-${HOTEL_NAME}.sock"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "Deny-redirect smoke failed. ansible log:"
    [[ -f "${TMP_DIR}/ansible.log" ]] && cat "${TMP_DIR}/ansible.log"
    echo "Deny-redirect smoke failed. agent log:"
    [[ -f "${TMP_DIR}/agent.log" ]] && cat "${TMP_DIR}/agent.log"
    echo "Deny-redirect smoke failed. model log:"
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

echo "Building deny-redirect smoke binaries..."
cargo build -p ansible -p agent-core -p model-router -p philotic-client --example approval_smoke_driver >/dev/null

echo "Starting ansible in ${TMP_DIR}..."
(
  cd "${TMP_DIR}"
  PHILOTIC_SMOKE_MODE=1 cargo run -q --manifest-path "${ROOT_DIR}/crates/ansible/Cargo.toml" -- --hotel "${HOTEL_NAME}" >"${TMP_DIR}/ansible.log" 2>&1
) &
ANSIBLE_PID=$!

for _ in {1..50}; do
  if [[ -S "${SOCKET_PATH}" ]]; then
    break
  fi
  sleep 0.2
done

if [[ ! -S "${SOCKET_PATH}" ]]; then
  echo "ansible socket did not appear"
  exit 1
fi

echo "Starting agent-core..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  cargo run -q --manifest-path "${ROOT_DIR}/crates/agent-core/Cargo.toml" >"${TMP_DIR}/agent.log" 2>&1 &
AGENT_PID=$!

echo "Starting model-router..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  cargo run -q --manifest-path "${ROOT_DIR}/crates/model-router/Cargo.toml" >"${TMP_DIR}/model.log" 2>&1 &
MODEL_PID=$!

sleep 1

echo "Driving deny-redirect round-trip..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
PHILOTIC_SMOKE_APPROVAL_COMMAND="/deny summarize the plan instead" \
PHILOTIC_SMOKE_EXPECTED_FINAL="Redirected: summarize the plan instead" \
  cargo run -q -p philotic-client --example approval_smoke_driver

echo "Deny-redirect smoke round-trip succeeded."
