#!/usr/bin/env bash
# smoke-auto-recall.sh — Tier 2 auto-recall smoke
#
# Requires a running hotel with a configured agent and Muninn.
# Set PHILOTIC_HOTEL_SOCKET to the running hotel's UDS socket path.
# The hotel must already be provisioned (aiua load was run once; GraphDB is source of truth).
#
# Usage:
#   PHILOTIC_HOTEL_SOCKET=/tmp/philotic-default.sock \
#   PHILOTIC_TARGET_NODE=default-aiua-01 \
#   PHILOTIC_SMOKE_TARGET_GUEST_ID=agent-jane-01 \
#   just smoke-auto-recall
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOCKET_PATH="${PHILOTIC_HOTEL_SOCKET:-/tmp/philotic-default.sock}"
TARGET_NODE="${PHILOTIC_TARGET_NODE:-default-aiua-01}"
TARGET_GUEST_ID="${PHILOTIC_SMOKE_TARGET_GUEST_ID:-}"

if [[ ! -S "${SOCKET_PATH}" ]]; then
  echo "ERROR: Hotel socket not found at ${SOCKET_PATH}" >&2
  echo "  Start the hotel first: just uat  (or set PHILOTIC_HOTEL_SOCKET)" >&2
  exit 1
fi

run_case() {
  local label="$1"
  local content="$2"
  local expected_event="$3"
  local expected_log="$4"
  local forbidden_log="$5"
  local session_id="smoke:auto-recall:${label}:$(date +%s)"

  echo "Running auto-recall case: ${label}..."
  (
    unset PHILOTIC_SMOKE_TARGET_GUEST_ID
    [[ -n "${TARGET_GUEST_ID}" ]] && export PHILOTIC_SMOKE_TARGET_GUEST_ID="${TARGET_GUEST_ID}"
    PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
    PHILOTIC_TARGET_NODE="${TARGET_NODE}" \
    PHILOTIC_FINAL_REPLY_TO="${TARGET_NODE}" \
    PHILOTIC_SMOKE_SESSION_ID="${session_id}" \
    PHILOTIC_SMOKE_TURN_ID="${label}-turn-1" \
    PHILOTIC_SMOKE_CHAT_ID="${label}-chat-1" \
    PHILOTIC_SMOKE_USER_CONTENT="${content}" \
    PHILOTIC_EXPECT_TURN_EVENT="${expected_event}" \
      "${ROOT_DIR}/target/debug/examples/recall_smoke_driver"
  )

  echo "auto-recall smoke ${label} ok"
}

echo "Building recall smoke driver..."
cargo build -p aiua -p philote --bin philote >/dev/null
cargo build -p philotic-client --example recall_smoke_driver >/dev/null

echo "Connecting to hotel at ${SOCKET_PATH} (node: ${TARGET_NODE})"

run_case \
  "meaningful" \
  "continue the memory architecture work from yesterday and use recall_seed_text" \
  "memory_auto_recall_completed" \
  "" \
  ""

run_case \
  "trivial" \
  "thanks" \
  "memory_auto_recall_skipped" \
  "" \
  ""

echo "auto recall smoke succeeded."
