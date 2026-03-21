#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOCKET_PATH="/tmp/philotic-default.sock"
TARGET_NODE="default-aiua-01"
TARGET_GUEST_ID="${PHILOTIC_SMOKE_TARGET_GUEST_ID:-agent-jane-01}"

cleanup() {
  local exit_code=$?
  if [[ -n "${AIUA_PID:-}" ]]; then
    kill "${AIUA_PID}" >/dev/null 2>&1 || true
    wait "${AIUA_PID}" >/dev/null 2>&1 || true
  fi
  exit "${exit_code}"
}
trap cleanup EXIT

wait_for_socket() {
  local attempts=0
  while [[ ! -S "${SOCKET_PATH}" ]]; do
    attempts=$((attempts + 1))
    if [[ ${attempts} -gt 50 ]]; then
      echo "Timed out waiting for ${SOCKET_PATH}" >&2
      return 1
    fi
    sleep 0.2
  done
}

run_case() {
  local label="$1"
  local content="$2"
  local expected_event="$3"
  local expected_log="$4"
  local forbidden_log="$5"
  local session_id="smoke:auto-recall:${label}:agent-jane-01"
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  local log_file="${tmp_dir}/aiua.log"

  rm -f "${SOCKET_PATH}"
  RUST_LOG=info PHILOTIC_DEBUG_MODEL_REQUESTS=1 \
    "${ROOT_DIR}/target/debug/aiua" --hotel default >"${log_file}" 2>&1 &
  AIUA_PID=$!
  wait_for_socket
  sleep 2

  PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  PHILOTIC_TARGET_NODE="${TARGET_NODE}" \
  PHILOTIC_FINAL_REPLY_TO="${TARGET_NODE}" \
  PHILOTIC_SMOKE_TARGET_GUEST_ID="${TARGET_GUEST_ID}" \
  PHILOTIC_SMOKE_SESSION_ID="${session_id}" \
  PHILOTIC_SMOKE_TURN_ID="${label}-turn-1" \
  PHILOTIC_SMOKE_CHAT_ID="${label}-chat-1" \
  PHILOTIC_SMOKE_USER_CONTENT="${content}" \
  PHILOTIC_EXPECT_TURN_EVENT="${expected_event}" \
    "${ROOT_DIR}/target/debug/examples/recall_smoke_driver"

  sleep 1
  kill "${AIUA_PID}" >/dev/null 2>&1 || true
  wait "${AIUA_PID}" >/dev/null 2>&1 || true
  unset AIUA_PID

  if ! rg -q "${expected_log}" "${log_file}"; then
    echo "Smoke ${label} failed: missing expected log pattern ${expected_log}" >&2
    cat "${log_file}" >&2
    return 1
  fi
  if [[ -n "${forbidden_log}" ]] && rg -q "${forbidden_log}" "${log_file}"; then
    echo "Smoke ${label} failed: found forbidden log pattern ${forbidden_log}" >&2
    cat "${log_file}" >&2
    return 1
  fi

  echo "auto-recall smoke ${label} ok"
}

echo "Building aiua and recall smoke driver..."
cargo build -p aiua >/dev/null
cargo build -p philotic-client --example recall_smoke_driver >/dev/null

echo "Loading default hotel config into aiua_context.db..."
"${ROOT_DIR}/target/debug/aiua" load --file "${ROOT_DIR}/mesh-config.json" --hotel default >/dev/null

run_case \
  "meaningful" \
  "continue the memory architecture work from yesterday and use recall_seed_text" \
  "memory_auto_recall_completed" \
  '"projection_kind": "recalled_memory"' \
  ""

run_case \
  "trivial" \
  "thanks" \
  "memory_auto_recall_skipped" \
  '"recalled_memory": \[\]' \
  '"projection_kind": "recalled_memory"'

echo "auto recall smoke succeeded."
