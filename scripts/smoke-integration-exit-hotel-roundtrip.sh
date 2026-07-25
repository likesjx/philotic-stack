#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE_REMOTE="${PHILOTIC_SOURCE_REMOTE:-mbp-jane}"
SOURCE_NODE="${PHILOTIC_SOURCE_NODE:-mbp-jane-aiua-01}"
SOURCE_SOCKET="${PHILOTIC_SOURCE_SOCKET:-/Users/jaredlikes/.philotic/jane/aiua-mbp-jane.sock}"
EXIT_REMOTE="${PHILOTIC_EXIT_REMOTE:-vps-jane}"
EXIT_HOTEL="${PHILOTIC_EXIT_HOTEL:-vps-jane}"
EXIT_NODE="${PHILOTIC_EXIT_NODE:-vps-jane-aiua-01}"
EXIT_PORT="${PHILOTIC_EXIT_SMOKE_PORT:-19187}"
TARGET_DIR="${CARGO_TARGET_DIR:-${ROOT_DIR}/target}"
DRIVER_LOCAL="${TARGET_DIR}/release/examples/integration_http_smoke_driver"
DRIVER_REMOTE="/tmp/integration_http_exit_smoke_driver"
STUB_REMOTE="/tmp/integration-exit-smoke-stub.py"
STUB_LOG="/tmp/integration-exit-smoke-stub.log"
STUB_PID=""

cleanup() {
  if [[ -n "${STUB_PID}" ]]; then
    ssh "${EXIT_REMOTE}" "kill '${STUB_PID}' >/dev/null 2>&1 || true"
  fi
  ssh "${SOURCE_REMOTE}" "rm -f '${DRIVER_REMOTE}'" >/dev/null 2>&1 || true
  ssh "${EXIT_REMOTE}" "rm -f '${STUB_REMOTE}' '${STUB_LOG}'" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "▶ Building two-hotel integration smoke driver..."
cargo build --release -p philotic-client --example integration_http_smoke_driver
codesign -s - --force "${DRIVER_LOCAL}" >/dev/null 2>&1

echo "▶ Staging driver on ${SOURCE_REMOTE} and loopback stub on ${EXIT_REMOTE}..."
scp -q "${DRIVER_LOCAL}" "${SOURCE_REMOTE}:${DRIVER_REMOTE}"
scp -q "${ROOT_DIR}/scripts/integration-exit-smoke-stub.py" "${EXIT_REMOTE}:${STUB_REMOTE}"
ssh "${SOURCE_REMOTE}" "chmod +x '${DRIVER_REMOTE}'"
ssh "${EXIT_REMOTE}" "chmod 700 '${STUB_REMOTE}'"

STUB_PID="$(ssh "${EXIT_REMOTE}" \
  "nohup python3 '${STUB_REMOTE}' --port '${EXIT_PORT}' --token smoke-token >'${STUB_LOG}' 2>&1 & echo \$!")"
for attempt in {1..20}; do
  if ssh "${EXIT_REMOTE}" \
    "curl -fsS -H 'Authorization: Bearer smoke-token' 'http://127.0.0.1:${EXIT_PORT}/v1/echo?probe=bounded' >/dev/null"; then
    break
  fi
  if [[ ${attempt} -eq 20 ]]; then
    echo "✗ exit-hotel HTTP stub did not become ready" >&2
    exit 1
  fi
  sleep 0.25
done

echo "▶ Proving ${SOURCE_NODE} routes required integration execution to ${EXIT_NODE}..."
ssh "${SOURCE_REMOTE}" \
  "env PHILOTIC_HOTEL_SOCKET='${SOURCE_SOCKET}' \
       PHILOTIC_TARGET_NODE='${EXIT_NODE}' \
       PHILOTIC_REPLY_NODE='${SOURCE_NODE}' \
       PHILOTIC_EXIT_HOTEL='${EXIT_HOTEL}' \
       PHILOTIC_SMOKE_BASE_URL='http://127.0.0.1:${EXIT_PORT}/v1' \
       '${DRIVER_REMOTE}'"

echo "WATCHED-LIVE-GREEN: governed HTTP exited through ${EXIT_HOTEL} and returned to ${SOURCE_NODE}"
