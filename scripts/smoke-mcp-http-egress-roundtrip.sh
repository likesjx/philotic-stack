#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
HOTEL_NAME="mcp-egress-smoke-$$"
NODE_NAME="${HOTEL_NAME}-aiua-01"
SOCKET_PATH="/tmp/philotic-${HOTEL_NAME}.sock"
PORT_FILE="${TMP_DIR}/mcp-port"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    for log in aiua runner mcp-client stub; do
      echo "MCP HTTP egress smoke ${log} log:"
      [[ -f "${TMP_DIR}/${log}.log" ]] && sed -n '1,240p' "${TMP_DIR}/${log}.log"
    done
  fi
  for pid in "${MCP_PID:-}" "${RUNNER_PID:-}" "${STUB_PID:-}" "${AIUA_PID:-}"; do
    [[ -n "${pid}" ]] && kill "${pid}" >/dev/null 2>&1
  done
  wait "${MCP_PID:-}" >/dev/null 2>&1
  wait "${RUNNER_PID:-}" >/dev/null 2>&1
  wait "${STUB_PID:-}" >/dev/null 2>&1
  wait "${AIUA_PID:-}" >/dev/null 2>&1
  rm -f "${SOCKET_PATH}"
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

echo "Building MCP-over-governed-egress smoke binaries..."
cargo build -p aiua -p egress-http-runner -p membrane-mcp-client >/dev/null
cargo build -p philotic-client --example mcp_upstream_smoke_driver >/dev/null

python3 "${ROOT_DIR}/scripts/mcp-http-egress-stub.py" \
  --port-file "${PORT_FILE}" >"${TMP_DIR}/stub.log" 2>&1 &
STUB_PID=$!
for _ in {1..50}; do
  [[ -s "${PORT_FILE}" ]] && break
  sleep 0.1
done
[[ -s "${PORT_FILE}" ]] || { echo "MCP stub did not publish its port"; exit 1; }
MCP_PORT="$(<"${PORT_FILE}")"

(
  cd "${TMP_DIR}"
  PHILOTIC_SMOKE_MODE=1 \
  PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  "${ROOT_DIR}/target/debug/aiua" --hotel "${HOTEL_NAME}" >"${TMP_DIR}/aiua.log" 2>&1
) &
AIUA_PID=$!
for _ in {1..50}; do
  [[ -S "${SOCKET_PATH}" ]] && break
  sleep 0.2
done
[[ -S "${SOCKET_PATH}" ]] || { echo "aiua socket did not appear"; exit 1; }

PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
PHILOTIC_NODE_ID="${NODE_NAME}" \
PHILOTIC_GUEST_ID="${HOTEL_NAME}:egress-http" \
"${ROOT_DIR}/target/debug/egress-http-runner" >"${TMP_DIR}/runner.log" 2>&1 &
RUNNER_PID=$!

PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
PHILOTIC_NODE_ID="${NODE_NAME}" \
PHILOTIC_GUEST_ID="${HOTEL_NAME}:mcp-client" \
"${ROOT_DIR}/target/debug/membrane-mcp-client" >"${TMP_DIR}/mcp-client.log" 2>&1 &
MCP_PID=$!
sleep 0.5

PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
MCP_SMOKE_NODE="${NODE_NAME}" \
MCP_SMOKE_URL="http://127.0.0.1:${MCP_PORT}/mcp" \
MCP_SMOKE_TOOL="echo" \
MCP_SMOKE_CREDENTIAL="smoke-mcp-token" \
MCP_SMOKE_EXPECT_EGRESS_AUDIT=1 \
cargo run -q -p philotic-client --example mcp_upstream_smoke_driver

echo "MCP HTTP through governed egress smoke round-trip succeeded."
