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

SMOKE_PROFILE="cog-smoke-$$"
SMOKE_PROFILE_DIR="${HOME}/.philotic/${SMOKE_PROFILE}"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "Cognitive smoke failed. aiua log:"
    [[ -f "${LOG_FILE}" ]] && cat "${LOG_FILE}"
  fi
  rm -rf "${TMP_DIR}"
  rm -rf "${SMOKE_PROFILE_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

echo "Building aiua startup-smoke binary..."
cargo build -p aiua >/dev/null

echo "Running startup-driven cognitive smoke..."

# Use an isolated PHILOTIC_PROFILE so the smoke DB doesn't collide with the
# production hotel (which may be running under PHILOTIC_PROFILE=jane).
# The profile dir (~/.philotic/<SMOKE_PROFILE>/) is cleaned up on exit.
export PHILOTIC_PROFILE="${SMOKE_PROFILE}"
mkdir -p "${SMOKE_PROFILE_DIR}"

# Seed hotel config if mesh-config.json is available.
# The cognitive-roundtrip test needs a pre-provisioned hotel (agent + model guests).
CONFIG_FILE="${ROOT_DIR}/mesh-config.json"
if [[ -f "${CONFIG_FILE}" ]]; then
  "${ROOT_DIR}/target/debug/aiua" load \
    --file "${CONFIG_FILE}" --hotel "default" >>"${LOG_FILE}" 2>&1
  COGNITIVE_HOTEL="default"
else
  COGNITIVE_HOTEL="${HOTEL_NAME}"
fi

"${ROOT_DIR}/target/debug/aiua" \
  --hotel "${COGNITIVE_HOTEL}" \
  --test cognitive-roundtrip \
  --test-text "${PHILOTIC_SMOKE_USER_CONTENT:-startup cognitive smoke ok}" \
  >>"${LOG_FILE}" 2>&1

echo "Cognitive smoke round-trip succeeded."
