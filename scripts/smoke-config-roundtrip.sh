#!/usr/bin/env bash
# Smoke test: `phil config get/set` IPC round-trip.
#
# Spins up an ephemeral, throwaway aiua hotel (same pattern as
# scripts/smoke-preapprove-roundtrip.sh) and proves the new
# `crates/philotic-web/src/config.rs` CLI surface — added for Substrate
# Hardening Slice S4's chaos-smoke.sh, which uses it to write/restore a
# sacrificial canary config key — actually round-trips over real IPC:
# unset key reads "null", a written value reads back byte-identical, restore
# works, and invalid JSON is rejected client-side before it ever reaches the
# wire.
#
# Usage: bash scripts/smoke-config-roundtrip.sh
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
HOTEL_NAME="config-smoke-$$"
SOCKET_PATH="/tmp/philotic-${HOTEL_NAME}.sock"
AIUA_BIN="${ROOT_DIR}/target/debug/aiua"
PHIL_BIN="${ROOT_DIR}/target/debug/philotic-web"
KEY="chaos_smoke.canary_value"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "Config roundtrip smoke failed. aiua log:"
    [[ -f "${TMP_DIR}/aiua.log" ]] && cat "${TMP_DIR}/aiua.log"
  fi
  [[ -n "${AIUA_PID:-}" ]] && kill "${AIUA_PID}" >/dev/null 2>&1
  wait "${AIUA_PID:-}" >/dev/null 2>&1
  rm -f "${SOCKET_PATH}"
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

echo "Building aiua + philotic-web..."
cargo build -p aiua -p philotic-web >/dev/null

echo "Starting aiua in ${TMP_DIR}..."
(
  cd "${TMP_DIR}"
  PHILOTIC_SMOKE_MODE=1 "${AIUA_BIN}" --hotel "${HOTEL_NAME}" >"${TMP_DIR}/aiua.log" 2>&1
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
echo "aiua up, socket at ${SOCKET_PATH}"

export PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}"

echo "Reading unset key (expect null)..."
GOT_UNSET="$("${PHIL_BIN}" config get "${KEY}" --hotel "${HOTEL_NAME}")"
if [[ "${GOT_UNSET}" != "null" ]]; then
  echo "FAIL: unset key returned '${GOT_UNSET}', expected null"
  exit 1
fi

echo "Writing a value..."
"${PHIL_BIN}" config set "${KEY}" '{"__chaos_smoke_bogus__":true,"ts":123}' --hotel "${HOTEL_NAME}"

echo "Reading it back..."
GOT_SET="$("${PHIL_BIN}" config get "${KEY}" --hotel "${HOTEL_NAME}")"
if [[ "${GOT_SET}" != '{"__chaos_smoke_bogus__":true,"ts":123}' ]]; then
  echo "FAIL: round-tripped value was '${GOT_SET}', expected the value just set"
  exit 1
fi

echo "Restoring a baseline value..."
"${PHIL_BIN}" config set "${KEY}" '"chaos_smoke_baseline"' --hotel "${HOTEL_NAME}"
GOT_RESTORED="$("${PHIL_BIN}" config get "${KEY}" --hotel "${HOTEL_NAME}")"
if [[ "${GOT_RESTORED}" != '"chaos_smoke_baseline"' ]]; then
  echo "FAIL: restore round-trip was '${GOT_RESTORED}', expected the baseline string"
  exit 1
fi

echo "Confirming invalid JSON is rejected client-side..."
if "${PHIL_BIN}" config set some.key 'not-json' --hotel "${HOTEL_NAME}" >/dev/null 2>&1; then
  echo "FAIL: invalid JSON was accepted"
  exit 1
fi

echo "Config roundtrip smoke succeeded."
