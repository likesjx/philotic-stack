#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOST="${MUNINN_REMOTE_HOST:-vps-jane}"
LOCAL_PORT="${MUNINN_TUNNEL_PORT:-18750}"
REMOTE_PORT="${MUNINN_REMOTE_PORT:-8750}"
LOCAL_BASE_URL="${MUNINN_LOCAL_BASE_URL:-http://127.0.0.1:8750/mcp}"
TUNNEL_BASE_URL="http://127.0.0.1:${LOCAL_PORT}/mcp"
TUNNEL_PID=""

cleanup_tunnel() {
  if [[ -n "${TUNNEL_PID}" ]]; then
    kill "${TUNNEL_PID}" >/dev/null 2>&1 || true
    wait "${TUNNEL_PID}" >/dev/null 2>&1 || true
    TUNNEL_PID=""
  fi
}

usage() {
  cat <<EOF
Usage: $0 [local-health|remote-bindings|tunnel-smoke|smoke|config-env]

Environment:
  MUNINN_REMOTE_HOST   SSH host alias for remote Muninn (default: vps-jane)
  MUNINN_TUNNEL_PORT   local forwarded port (default: 18750)
  MUNINN_REMOTE_PORT   remote Muninn MCP port (default: 8750)

Modes:
  local-health      Check local native Muninn MCP via scripts/muninn_mcp.py
  remote-bindings   Verify remote Muninn listens on loopback, not public interfaces
  tunnel-smoke      Open SSH tunnel to remote loopback MCP and run MCP health
  smoke             Run local-health, remote-bindings, and tunnel-smoke
  config-env        Print the MUNINN_MCP_URL value for a tunneled stdio proxy
EOF
}

local_health() {
  python3 "${ROOT_DIR}/scripts/muninn_mcp.py" --base-url "${LOCAL_BASE_URL}" health >/dev/null
  printf 'PASS local Muninn MCP health: %s\n' "${LOCAL_BASE_URL}"
}

remote_bindings() {
  ssh "${HOST}" "bash -lc 'set -euo pipefail
    muninn status >/dev/null
    if ss -ltnH | awk \"{print \\\$4}\" | grep -Eq \"(^|:)0\\.0\\.0\\.0:${REMOTE_PORT}$|^\\*:${REMOTE_PORT}$|^\\[::\\]:${REMOTE_PORT}$\"; then
      echo \"FAIL remote Muninn MCP is publicly bound on ${REMOTE_PORT}\" >&2
      exit 2
    fi
    ss -ltnp | grep -E \":(${REMOTE_PORT}|8475|8476)\" || true
  '"
  printf 'PASS %s Muninn MCP is not publicly bound on port %s\n' "${HOST}" "${REMOTE_PORT}"
}

wait_for_tunnel() {
  local base_url="$1"
  for _ in $(seq 1 20); do
    if python3 "${ROOT_DIR}/scripts/muninn_mcp.py" --base-url "${base_url}" health >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done
  return 1
}

tunnel_smoke() {
  if lsof -nP -iTCP:"${LOCAL_PORT}" -sTCP:LISTEN >/dev/null 2>&1; then
    echo "FAIL local tunnel port ${LOCAL_PORT} is already in use" >&2
    exit 3
  fi

  ssh -o ExitOnForwardFailure=yes -N -L "${LOCAL_PORT}:127.0.0.1:${REMOTE_PORT}" "${HOST}" &
  TUNNEL_PID=$!
  trap cleanup_tunnel EXIT

  if ! wait_for_tunnel "${TUNNEL_BASE_URL}"; then
    echo "FAIL tunneled Muninn MCP health did not become ready at ${TUNNEL_BASE_URL}" >&2
    exit 4
  fi

  python3 "${ROOT_DIR}/scripts/muninn_mcp.py" --base-url "${TUNNEL_BASE_URL}" health >/dev/null
  printf 'PASS tunneled Muninn MCP health: %s -> %s:127.0.0.1:%s\n' \
    "${TUNNEL_BASE_URL}" "${HOST}" "${REMOTE_PORT}"
  cleanup_tunnel
  trap - EXIT
}

mode="${1:-smoke}"

case "${mode}" in
  local-health)
    local_health
    ;;
  remote-bindings)
    remote_bindings
    ;;
  tunnel-smoke)
    tunnel_smoke
    ;;
  smoke)
    local_health
    remote_bindings
    tunnel_smoke
    ;;
  config-env)
    printf 'MUNINN_MCP_URL=%s muninn mcp\n' "${TUNNEL_BASE_URL}"
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage >&2
    exit 64
    ;;
esac
