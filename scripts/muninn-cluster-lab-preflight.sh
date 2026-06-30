#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REMOTE_HOSTS="${MUNINN_CLUSTER_REMOTE_HOSTS:-mbp-jane vps-jane}"
RUN_REMOTE="${RUN_REMOTE:-0}"

usage() {
  cat <<EOF
Usage: $0 [local|remote|all|isolation-probe|cluster-auth-probe]

This is a real-vault non-mutating preflight for Muninn cluster lab work. The
local disposable probes write only to /tmp and remove their test data on exit.
They do not enable cluster mode on the operator's continuity vaults and do not
write test memories.

Environment:
  RUN_REMOTE=1                         Include SSH checks in all mode
  MUNINN_CLUSTER_REMOTE_HOSTS="..."    Space-separated SSH hosts (default: mbp-jane vps-jane)
  MUNINN_CLUSTER_TEST_ROOT=/tmp/...     Disposable data root for local probes

Modes:
  local     Verify local CLI cluster support and local MCP health
  isolation-probe
            Start one disposable local Muninn daemon on alternate ports
  cluster-auth-probe
            Verify disposable cluster enablement reaches the admin-auth gate
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
  local_disposable_probe "isolation"
}

local_health() {
  python3 "${ROOT_DIR}/scripts/muninn_mcp.py" --timeout 10 health >/dev/null
  pass "local Muninn MCP health"
}

probe_data_root() {
  printf '%s\n' "${MUNINN_CLUSTER_TEST_ROOT:-/tmp/philotic-muninn-isolation-probe}"
}

probe_cleanup() {
  local data_dir="$1"
  local rest_addr="$2"
  local ui_addr="$3"
  local mcp_addr="$4"

  MUNINNDB_DATA="${data_dir}" \
    MUNINNDB_ADMIN_URL="http://${rest_addr}" \
    MUNINNDB_UI_URL="http://${ui_addr}" \
    MUNINNDB_MCP_URL="http://${mcp_addr}/mcp" \
    muninn stop >/dev/null 2>&1 || true
  rm -rf "${data_dir}"
}

wait_probe_mcp() {
  local mcp_url="$1"
  local log_file="$2"

  for _ in $(seq 1 30); do
    if python3 "${ROOT_DIR}/scripts/muninn_mcp.py" --base-url "${mcp_url}" --timeout 2 health >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done

  echo "FAIL disposable Muninn MCP did not become healthy at ${mcp_url}" >&2
  if [[ -s "${log_file}" ]]; then
    echo "---- disposable muninn start log ----" >&2
    tail -n 80 "${log_file}" >&2
  fi
  return 1
}

local_disposable_probe() {
  local probe="${1:-isolation}"
  local data_dir rest_addr ui_addr mcp_addr mbp_addr grpc_addr cluster_bind log_file
  data_dir="$(probe_data_root)"
  rest_addr="${MUNINN_CLUSTER_PROBE_REST_ADDR:-127.0.0.1:18475}"
  ui_addr="${MUNINN_CLUSTER_PROBE_UI_ADDR:-127.0.0.1:18476}"
  mcp_addr="${MUNINN_CLUSTER_PROBE_MCP_ADDR:-127.0.0.1:18751}"
  mbp_addr="${MUNINN_CLUSTER_PROBE_MBP_ADDR:-127.0.0.1:18474}"
  grpc_addr="${MUNINN_CLUSTER_PROBE_GRPC_ADDR:-127.0.0.1:18477}"
  cluster_bind="${MUNINN_CLUSTER_PROBE_BIND_ADDR:-127.0.0.1:19001}"
  log_file="${data_dir}/muninn-start.log"

  rm -rf "${data_dir}"
  mkdir -p "${data_dir}"
  trap 'probe_cleanup "'"${data_dir}"'" "'"${rest_addr}"'" "'"${ui_addr}"'" "'"${mcp_addr}"'"' RETURN

  MUNINNDB_DATA="${data_dir}" \
    MUNINNDB_ADMIN_URL="http://${rest_addr}" \
    MUNINNDB_UI_URL="http://${ui_addr}" \
    MUNINNDB_MCP_URL="http://${mcp_addr}/mcp" \
    muninn stop >/dev/null 2>&1 || true

  MUNINNDB_DATA="${data_dir}" muninn start \
    --rest-addr "${rest_addr}" \
    --ui-addr "${ui_addr}" \
    --mcp-addr "${mcp_addr}" \
    --mbp-addr "${mbp_addr}" \
    --grpc-addr "${grpc_addr}" \
    >"${log_file}" 2>&1

  wait_probe_mcp "http://${mcp_addr}/mcp" "${log_file}"
  pass "disposable Muninn daemon starts on alternate REST/UI/MCP/MBP/gRPC ports"

  if [[ "${probe}" == "cluster-auth" ]]; then
    local output
    set +e
    output="$(muninn cluster enable \
      --addr "http://${rest_addr}" \
      --role primary \
      --bind-addr "${cluster_bind}" \
      --secret philotic-cluster-smoke \
      --yes 2>&1)"
    local status=$?
    set -e

    if [[ ${status} -eq 0 ]]; then
      pass "disposable cluster enablement accepted on isolated data"
    elif grep -Eq "HTTP 401|AUTH_FAILED|admin session required" <<<"${output}"; then
      pass "disposable cluster enablement reaches admin-auth gate; authenticated cluster ceremony still required"
    else
      echo "FAIL unexpected disposable cluster enablement result" >&2
      printf '%s\n' "${output}" >&2
      return 1
    fi
  fi

  trap - RETURN
  probe_cleanup "${data_dir}" "${rest_addr}" "${ui_addr}" "${mcp_addr}"
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
    local_disposable_probe "cluster-auth"
    local_health
    ;;
  isolation-probe)
    local_disposable_probe "isolation"
    ;;
  cluster-auth-probe)
    require_cluster_cli
    local_disposable_probe "cluster-auth"
    ;;
  remote)
    remote_health
    ;;
  all)
    require_cluster_cli
    local_disposable_probe "cluster-auth"
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
