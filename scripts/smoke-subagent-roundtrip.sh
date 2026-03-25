#!/usr/bin/env bash
# Smoke test: subagent spawn → lease accept → task assign → completion hook → release.
#
# Uses PHILOTIC_SMOKE_MODE (no mesh, no materializer) so the driver handles both
# sides (parent + worker) in a single process without needing a real model runner.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
HOTEL_NAME="jane-smoke-sub-$$"
export PHILOTIC_NODE_ID="${HOTEL_NAME}-aiua-01"
export PHILOTIC_TARGET_NODE="${HOTEL_NAME}-aiua-01"
export PHILOTIC_FINAL_REPLY_TO="${HOTEL_NAME}-aiua-01"
SOCKET_PATH="/tmp/philotic-${HOTEL_NAME}.sock"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "--- subagent smoke FAILED ---"
    echo "aiua log:"
    [[ -f "${TMP_DIR}/aiua.log" ]] && cat "${TMP_DIR}/aiua.log"
  fi
  [[ -n "${ANSIBLE_PID:-}" ]] && kill "${ANSIBLE_PID}" >/dev/null 2>&1
  wait "${ANSIBLE_PID:-}" >/dev/null 2>&1
  rm -f "${SOCKET_PATH}"
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

echo "Building subagent smoke binaries…"
cargo build -p aiua -p philotic-client --example subagent_smoke_driver 2>&1 | grep -v "^$"

echo "Starting aiua (smoke mode) in ${TMP_DIR}…"
(
  cd "${TMP_DIR}"
  PHILOTIC_SMOKE_MODE=1 \
    "${ROOT_DIR}/target/debug/aiua" --hotel "${HOTEL_NAME}" \
    >"${TMP_DIR}/aiua.log" 2>&1
) &
ANSIBLE_PID=$!

echo "Waiting for hotel socket…"
for _ in {1..50}; do
  [[ -S "${SOCKET_PATH}" ]] && break
  sleep 0.2
done

if [[ ! -S "${SOCKET_PATH}" ]]; then
  echo "Hotel socket did not appear. aiua log:"
  cat "${TMP_DIR}/aiua.log"
  exit 1
fi

echo "Running subagent smoke driver…"
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  "${ROOT_DIR}/target/debug/examples/subagent_smoke_driver"

echo "Subagent smoke: PASSED"
