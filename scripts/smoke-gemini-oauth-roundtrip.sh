#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

HOTEL_NAME="gemini-oauth-smoke-$$"
LOG_FILE="$(mktemp -t philotic-gemini-oauth-smoke.XXXXXX.log)"
trap 'rm -f "${LOG_FILE}"' EXIT

echo "Building ansible startup-smoke binary..."
cargo build -p ansible >/dev/null

echo "Running startup-driven Gemini OAuth smoke..."
if ! target/debug/ansible \
  --hotel "${HOTEL_NAME}" \
  --test gemini-oauth-roundtrip \
  --test-text "oauth-guest-ok" \
  >"${LOG_FILE}" 2>&1; then
  echo "Smoke test failed. ansible log:"
  cat "${LOG_FILE}"
  exit 1
fi

echo "Gemini OAuth model-controller smoke round-trip succeeded."
