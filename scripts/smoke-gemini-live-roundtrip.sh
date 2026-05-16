#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

HOTEL_NAME="${PHILOTIC_SMOKE_HOTEL:-gemini-live-smoke-$$}"
LOG_FILE="$(mktemp -t philotic-gemini-live-smoke.XXXXXX.log)"
trap 'rm -f "${LOG_FILE}"' EXIT

echo "Building aiua startup-smoke binary..."
cargo build -p aiua -p model-router >/dev/null

echo "Running startup-driven Gemini Live smoke against hotel '${HOTEL_NAME}'..."
if [[ -f "${ROOT_DIR}/mesh-config.json" ]]; then
  "${ROOT_DIR}/target/debug/aiua" load \
    --file "${ROOT_DIR}/mesh-config.json" \
    --hotel "${HOTEL_NAME}" \
    >>"${LOG_FILE}" 2>&1
fi
if ! target/debug/aiua \
  --hotel "${HOTEL_NAME}" \
  --test gemini-live-roundtrip \
  --test-text "gemini live startup ok" \
  >"${LOG_FILE}" 2>&1; then
  echo "Smoke test failed. aiua log:"
  cat "${LOG_FILE}"
  exit 1
fi

echo "Gemini Live complete-turn smoke round-trip succeeded."
