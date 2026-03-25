#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
HOTEL_NAME="model-smoke-$$"
export PHILOTIC_NODE_ID="${HOTEL_NAME}-aiua-01"
export PHILOTIC_TARGET_NODE="${HOTEL_NAME}-aiua-01"
export PHILOTIC_FINAL_REPLY_TO="${HOTEL_NAME}-aiua-01"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "Smoke test failed. aiua log:"
    [[ -f "${TMP_DIR}/aiua.log" ]] && cat "${TMP_DIR}/aiua.log"
  fi
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

echo "Building aiua startup-smoke binary..."
cargo build -p aiua >/dev/null

echo "Running startup-driven model-controller smoke..."
if [[ -f "${ROOT_DIR}/mesh-config.json" ]]; then
  "${ROOT_DIR}/target/debug/aiua" load \
    --file "${ROOT_DIR}/mesh-config.json" \
    --hotel "${HOTEL_NAME}" \
    >>"${TMP_DIR}/aiua.log" 2>&1
fi
"${ROOT_DIR}/target/debug/aiua" \
  --hotel "${HOTEL_NAME}" \
  --test text-roundtrip \
  --test-text "${PHILOTIC_SMOKE_USER_CONTENT:-hello model controller}" \
  >>"${TMP_DIR}/aiua.log" 2>&1

echo "Model-controller smoke round-trip succeeded."
