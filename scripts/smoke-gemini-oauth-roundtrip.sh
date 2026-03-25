#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

# Uses the configured default hotel — OAuth tokens must already be stored via:
#   aiua auth google start ...
# GraphDB is the source of truth; this smoke never calls aiua load.
HOTEL_NAME="${PHILOTIC_SMOKE_HOTEL:-default}"
LOG_FILE="$(mktemp -t philotic-gemini-oauth-smoke.XXXXXX.log)"
trap 'rm -f "${LOG_FILE}"' EXIT

echo "Building aiua startup-smoke binary..."
cargo build -p aiua >/dev/null

echo "Running startup-driven Gemini OAuth smoke against hotel '${HOTEL_NAME}'..."
if ! target/debug/aiua \
  --hotel "${HOTEL_NAME}" \
  --test gemini-oauth-roundtrip \
  --test-text "oauth-guest-ok" \
  >"${LOG_FILE}" 2>&1; then
  echo "Smoke test failed. aiua log:"
  cat "${LOG_FILE}"
  exit 1
fi

echo "Gemini OAuth model-controller smoke round-trip succeeded."
