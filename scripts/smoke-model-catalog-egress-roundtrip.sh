#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
HOTEL_NAME="catalog-egress-smoke-$$"
NODE_NAME="${HOTEL_NAME}-aiua-01"
SOCKET_PATH="/tmp/philotic-${HOTEL_NAME}.sock"
URL_FILE="${TMP_DIR}/catalog-url"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "Model catalog governed-egress smoke failed. aiua log:"
    [[ -f "${TMP_DIR}/aiua.log" ]] && sed -n '1,260p' "${TMP_DIR}/aiua.log"
    echo "Runner log:"
    [[ -f "${TMP_DIR}/runner.log" ]] && sed -n '1,260p' "${TMP_DIR}/runner.log"
    echo "Catalog stub log:"
    [[ -f "${TMP_DIR}/stub.log" ]] && sed -n '1,120p' "${TMP_DIR}/stub.log"
  fi
  [[ -n "${RUNNER_PID:-}" ]] && kill "${RUNNER_PID}" >/dev/null 2>&1
  [[ -n "${AIUA_PID:-}" ]] && kill "${AIUA_PID}" >/dev/null 2>&1
  [[ -n "${STUB_PID:-}" ]] && kill "${STUB_PID}" >/dev/null 2>&1
  wait "${RUNNER_PID:-}" >/dev/null 2>&1
  wait "${AIUA_PID:-}" >/dev/null 2>&1
  wait "${STUB_PID:-}" >/dev/null 2>&1
  rm -f "${SOCKET_PATH}"
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

echo "Building model catalog governed-egress smoke binaries..."
cargo build -p aiua -p egress-http-runner >/dev/null
cargo build -p philotic-client --example model_catalog_egress_smoke_probe >/dev/null

python3 "${ROOT_DIR}/scripts/model-catalog-smoke-stub.py" \
  --url-file "${URL_FILE}" >"${TMP_DIR}/stub.log" 2>&1 &
STUB_PID=$!
for _ in {1..50}; do
  [[ -s "${URL_FILE}" ]] && break
  sleep 0.1
done
if [[ ! -s "${URL_FILE}" ]]; then
  echo "catalog stub did not publish its URL"
  exit 1
fi
CATALOG_URL="$(<"${URL_FILE}")"

echo "Starting isolated hotel with governed model-catalog sync..."
(
  cd "${TMP_DIR}"
  PHILOTIC_SMOKE_MODE=1 \
  PHILOTIC_SMOKE_MODEL_CATALOG=1 \
  PHILOTIC_MODEL_CATALOG_URL="${CATALOG_URL}" \
  PHILOTIC_MODEL_CATALOG_EXIT_HOTEL=local \
  PHILOTIC_MODEL_CATALOG_INITIAL_DELAY_SECS=1 \
  PHILOTIC_MODEL_CATALOG_INTERVAL_SECS=2 \
  PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  "${ROOT_DIR}/target/debug/aiua" --hotel "${HOTEL_NAME}" >"${TMP_DIR}/aiua.log" 2>&1
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
"${ROOT_DIR}/target/debug/egress-http-runner" >"${TMP_DIR}/runner.log" 2>&1 &
RUNNER_PID=$!

echo "Verifying catalog state, binding authority, execution placement, and audit..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
PHILOTIC_TARGET_NODE="${NODE_NAME}" \
cargo run -q -p philotic-client --example model_catalog_egress_smoke_probe

echo "Model catalog governed-egress smoke round-trip succeeded."
