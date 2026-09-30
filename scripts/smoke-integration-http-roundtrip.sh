#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
HOTEL_NAME="integration-smoke-$$"
NODE_NAME="${HOTEL_NAME}-aiua-01"
SOCKET_PATH="/tmp/philotic-${HOTEL_NAME}.sock"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "Integration HTTP smoke failed. aiua log:"
    [[ -f "${TMP_DIR}/aiua.log" ]] && sed -n '1,240p' "${TMP_DIR}/aiua.log"
    echo "Integration HTTP smoke failed. runner log:"
    [[ -f "${TMP_DIR}/runner.log" ]] && sed -n '1,240p' "${TMP_DIR}/runner.log"
  fi
  [[ -n "${RUNNER_PID:-}" ]] && kill "${RUNNER_PID}" >/dev/null 2>&1
  [[ -n "${AIUA_PID:-}" ]] && kill "${AIUA_PID}" >/dev/null 2>&1
  wait "${RUNNER_PID:-}" >/dev/null 2>&1
  wait "${AIUA_PID:-}" >/dev/null 2>&1
  rm -f "${SOCKET_PATH}"
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

echo "Building governed HTTP integration smoke binaries..."
cargo build -p aiua -p egress-http-runner >/dev/null
cargo build -p philotic-client --example integration_http_smoke_driver >/dev/null

AIUA_BIN="${ROOT_DIR}/target/debug/aiua"
RUNNER_BIN="${ROOT_DIR}/target/debug/egress-http-runner"

echo "Starting isolated hotel..."
(
  cd "${TMP_DIR}"
  PHILOTIC_SMOKE_MODE=1 \
  PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  "${AIUA_BIN}" --hotel "${HOTEL_NAME}" >"${TMP_DIR}/aiua.log" 2>&1
) &
AIUA_PID=$!

for _ in {1..50}; do
  [[ -S "${SOCKET_PATH}" ]] && break
  sleep 0.2
done
if [[ ! -S "${SOCKET_PATH}" ]]; then
  echo "aiua socket did not appear"
  exit 1
fi

echo "Starting bounded HTTP runner..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
PHILOTIC_NODE_ID="${NODE_NAME}" \
PHILOTIC_GUEST_ID="${HOTEL_NAME}:egress-http" \
"${RUNNER_BIN}" >"${TMP_DIR}/runner.log" 2>&1 &
RUNNER_PID=$!
sleep 0.5

echo "Driving binding, credential, execution, audit, and revoke round-trip..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
PHILOTIC_TARGET_NODE="${NODE_NAME}" \
cargo run -q -p philotic-client --example integration_http_smoke_driver

echo "Governed HTTP integration smoke round-trip succeeded."
