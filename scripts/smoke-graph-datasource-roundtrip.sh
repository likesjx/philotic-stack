#!/usr/bin/env bash
# Smoke test for graph-datasource via aiua startup test.
# (Successor to the retired graph-runner smoke — codex/graph-runner-retire.)
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
PROFILE_HOME="$(mktemp -d /tmp/gs-home-XXXXXX)"
PROFILE_NAME="gs"
HOTEL_NAME="default"
CONFIG_PATH="${TMP_DIR}/config.json"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "Graph-datasource smoke FAILED."
    echo "=== load log ==="
    [[ -f "${TMP_DIR}/load.log" ]] && cat "${TMP_DIR}/load.log"
    echo "=== aiua log ==="
    [[ -f "${TMP_DIR}/aiua.log" ]] && cat "${TMP_DIR}/aiua.log"
  fi
  rm -rf "${TMP_DIR}" "${PROFILE_HOME}"
  exit ${exit_code}
}
trap cleanup EXIT

cat >"${CONFIG_PATH}" <<'EOF'
{
  "context_graph": {
    "default_model": "gemini-flash-latest"
  },
  "hotels": {
    "default": {}
  }
}
EOF

echo "Loading temp hotel config..."
# graph-datasource is home-hotel-gated (default vps-jane); make the smoke
# hotel the home hotel so its guest record is seeded and materialized.
HOME="${PROFILE_HOME}" PHILOTIC_PROFILE="${PROFILE_NAME}" \
  PHILOTIC_GRAPH_DATASOURCE_HOME_HOTEL="${HOTEL_NAME}" \
  "${ROOT_DIR}/target/debug/aiua" load --file "${CONFIG_PATH}" --hotel "${HOTEL_NAME}" \
  >"${TMP_DIR}/load.log" 2>&1

echo "Running aiua graph startup test..."
HOME="${PROFILE_HOME}" PHILOTIC_PROFILE="${PROFILE_NAME}" \
  PHILOTIC_GRAPH_DATASOURCE_HOME_HOTEL="${HOTEL_NAME}" \
  "${ROOT_DIR}/target/debug/aiua" --hotel "${HOTEL_NAME}" --test graph-roundtrip \
  >"${TMP_DIR}/aiua.log" 2>&1

cat "${TMP_DIR}/aiua.log"
echo "Graph-datasource smoke round-trip succeeded."
