#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
HOTEL_NAME="cognitive-smoke-$$"
export PHILOTIC_AGENT_ID="agent-jane-01"
export PHILOTIC_NODE_ID="${HOTEL_NAME}-aiua-01"
export PHILOTIC_TARGET_NODE="${HOTEL_NAME}-aiua-01"
export PHILOTIC_FINAL_REPLY_TO="${HOTEL_NAME}-aiua-01"
LOG_FILE="${TMP_DIR}/aiua.log"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "Cognitive smoke failed. aiua log:"
    [[ -f "${LOG_FILE}" ]] && cat "${LOG_FILE}"
  fi
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

echo "Building aiua startup-smoke binary..."
cargo build -p aiua >/dev/null

echo "Running startup-driven cognitive smoke..."
if [[ -f "${ROOT_DIR}/mesh-config.json" ]]; then
  "${ROOT_DIR}/target/debug/aiua" \
    --hotel "${HOTEL_NAME}" \
    --load-config "${ROOT_DIR}/mesh-config.json" \
    --test cognitive-roundtrip \
    --test-text "${PHILOTIC_SMOKE_USER_CONTENT:-startup cognitive smoke ok}" \
    >"${LOG_FILE}" 2>&1
else
  "${ROOT_DIR}/target/debug/aiua" \
    --hotel "${HOTEL_NAME}" \
    --test cognitive-roundtrip \
    --test-text "${PHILOTIC_SMOKE_USER_CONTENT:-startup cognitive smoke ok}" \
    >"${LOG_FILE}" 2>&1
fi

echo "Cognitive smoke round-trip succeeded."
