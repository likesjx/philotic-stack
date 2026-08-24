#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
HOTEL_NAME="catalog-egress-smoke-$$"
NODE_NAME="${HOTEL_NAME}-aiua-01"
SOCKET_PATH="/tmp/philotic-${HOTEL_NAME}.sock"
OPENROUTER_URL_FILE="${TMP_DIR}/openrouter-catalog-url"
HUGGINGFACE_URL_FILE="${TMP_DIR}/huggingface-catalog-url"

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
  --openrouter-url-file "${OPENROUTER_URL_FILE}" \
  --huggingface-url-file "${HUGGINGFACE_URL_FILE}" \
  >"${TMP_DIR}/stub.log" 2>&1 &
STUB_PID=$!
for _ in {1..50}; do
  [[ -s "${OPENROUTER_URL_FILE}" && -s "${HUGGINGFACE_URL_FILE}" ]] && break
  sleep 0.1
done
if [[ ! -s "${OPENROUTER_URL_FILE}" || ! -s "${HUGGINGFACE_URL_FILE}" ]]; then
  echo "catalog stub did not publish both URLs"
  exit 1
fi
CATALOG_URL="$(<"${OPENROUTER_URL_FILE}")"
HUGGINGFACE_CATALOG_URL="$(<"${HUGGINGFACE_URL_FILE}")"

echo "Starting isolated hotel with governed model-catalog sync..."
(
  cd "${TMP_DIR}"
  PHILOTIC_SMOKE_MODE=1 \
  PHILOTIC_SMOKE_MODEL_CATALOG=1 \
  PHILOTIC_MODEL_CATALOG_URL="${CATALOG_URL}" \
  PHILOTIC_HUGGINGFACE_MODEL_CATALOG_URL="${HUGGINGFACE_CATALOG_URL}" \
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

echo "Verifying both catalog states, SkillDAG gate, binding authority, execution placement, and audit..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
PHILOTIC_TARGET_NODE="${NODE_NAME}" \
cargo run -q -p philotic-client --example model_catalog_egress_smoke_probe

# The sync opened a session turn when it emitted; it must be terminal now.
# A turn left `running` here is the leak that made a *successful* catalog sync
# still surface as a ~691s ZOMBIE_TURN_REPAIR "stuck turn" in production.
echo "Verifying the sync closed its session turn (no reaper leak)..."
HOTEL_DB="$(ls -1 "${TMP_DIR}"/*.db 2>/dev/null | head -1)"
if [[ -z "${HOTEL_DB}" ]]; then
  echo "could not locate the smoke hotel DB under ${TMP_DIR}"
  exit 1
fi
for _ in {1..40}; do
  RUNNING_TURNS="$(sqlite3 "${HOTEL_DB}" \
    "select count(*) from graph_nodes where kind='session_turn' \
     and json_extract(data_json,'\$.session_id')='system:model-catalog-sync' \
     and json_extract(data_json,'\$.status')='running';" 2>/dev/null || echo 0)"
  [[ "${RUNNING_TURNS}" == "0" ]] && break
  sleep 0.25
done
TOTAL_TURNS="$(sqlite3 "${HOTEL_DB}" \
  "select count(*) from graph_nodes where kind='session_turn' \
   and json_extract(data_json,'\$.session_id')='system:model-catalog-sync';" 2>/dev/null || echo 0)"
if [[ "${TOTAL_TURNS}" == "0" ]]; then
  echo "no model-catalog-sync turn was recorded — cannot assert closure"
  exit 1
fi
if [[ "${RUNNING_TURNS}" != "0" ]]; then
  echo "FAIL: ${RUNNING_TURNS} model-catalog-sync turn(s) left running — they would be reaped as ZOMBIE_TURN_REPAIR at 300s"
  sqlite3 "${HOTEL_DB}" "select data_json from graph_nodes where kind='session_turn' \
    and json_extract(data_json,'\$.session_id')='system:model-catalog-sync';"
  exit 1
fi
echo "governed egress closed its turn (${TOTAL_TURNS} turn(s), 0 left running)"

echo "Model catalog governed-egress smoke round-trip succeeded."
