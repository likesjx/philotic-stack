#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REMOTE_HOSTS="${MUNINN_CLUSTER_REMOTE_HOSTS:-mbp-jane vps-jane}"
RUN_REMOTE="${RUN_REMOTE:-0}"

usage() {
  cat <<EOF
Usage: $0 [local|remote|all]

This is a non-mutating preflight for Muninn cluster lab work. It does not enable
cluster mode and does not write test memories.

Environment:
  RUN_REMOTE=1                         Include SSH checks in all mode
  MUNINN_CLUSTER_REMOTE_HOSTS="..."    Space-separated SSH hosts (default: mbp-jane vps-jane)

Modes:
  local     Verify local CLI cluster support and local MCP health
  remote    Verify remote CLI support, status, and loopback MCP binding
  all       Run local and remote when RUN_REMOTE=1
EOF
}

pass() {
  printf 'PASS %s\n' "$1"
}

skip() {
  printf 'SKIP %s\n' "$1"
}

require_cluster_cli() {
  muninn cluster --help | grep -q "cluster enable" || {
    echo "FAIL local muninn cluster CLI does not expose cluster enable" >&2
    exit 1
  }
  muninn cluster --help | grep -q "cluster status" || {
    echo "FAIL local muninn cluster CLI does not expose cluster status" >&2
    exit 1
  }
  pass "local Muninn CLI exposes cluster management commands"
}

local_isolation_check() {
  local start_help
  start_help="$(muninn start --help)"
  if grep -Eq -- "--data|--rest-addr|--admin-addr|--ui-addr|--mbp-addr" <<<"${start_help}"; then
    pass "local Muninn start help exposes at least one isolation-related daemon flag"
  else
    skip "same-host multi-node lab isolation not proven; muninn start help exposes no alternate data/admin/UI/MBP port flags"
  fi
}

local_health() {
  python3 "${ROOT_DIR}/scripts/muninn_mcp.py" --timeout 10 health >/dev/null
  pass "local Muninn MCP health"
}

remote_check_host() {
  local host="$1"
  ssh "${host}" "bash -lc 'set -euo pipefail
    muninn cluster --help | grep -q \"cluster enable\"
    muninn cluster --help | grep -q \"cluster status\"
    muninn status >/dev/null
    if command -v ss >/dev/null 2>&1; then
      if ss -ltnH | awk \"{print \\\$4}\" | grep -Eq \"(^|:)0\\.0\\.0\\.0:8750$|^\\*:8750$|^\\[::\\]:8750$\"; then
        echo \"FAIL ${host} Muninn MCP is publicly bound on 8750\" >&2
        exit 2
      fi
    elif command -v lsof >/dev/null 2>&1; then
      if lsof -nP -iTCP:8750 -sTCP:LISTEN | awk \"NR>1 {print \\\$9}\" | grep -Eq \"(^|\\*)[:.]8750$|0\\.0\\.0\\.0:8750$|\\[::\\]:8750$\"; then
        echo \"FAIL ${host} Muninn MCP is publicly bound on 8750\" >&2
        exit 2
      fi
    else
      echo \"FAIL ${host} cannot inspect listener bindings; ss/lsof unavailable\" >&2
      exit 3
    fi
  '"
  pass "${host} Muninn cluster CLI present, daemon healthy, MCP not public-bound"
}

remote_health() {
  for host in ${REMOTE_HOSTS}; do
    remote_check_host "${host}"
  done
}

mode="${1:-local}"
case "${mode}" in
  local)
    require_cluster_cli
    local_isolation_check
    local_health
    ;;
  remote)
    remote_health
    ;;
  all)
    require_cluster_cli
    local_isolation_check
    local_health
    if [[ "${RUN_REMOTE}" == "1" ]]; then
      remote_health
    else
      skip "remote cluster lab preflight; set RUN_REMOTE=1 to include SSH checks"
    fi
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage >&2
    exit 64
    ;;
esac
