#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
HOTEL_NAME="jane-cog-$$"
export PHILOTIC_AGENT_ID="agent-jane-01"
NODE_NAME="${HOTEL_NAME}-aiua-01"
export PHILOTIC_NODE_ID="${NODE_NAME}"
export PHILOTIC_TARGET_NODE="${NODE_NAME}"
export PHILOTIC_FINAL_REPLY_TO="${NODE_NAME}"
export PHILOTIC_SMOKE_TURN_ID="smoke-turn-1"
export PHILOTIC_SMOKE_SESSION_ID="smoke:cognitive-reentry:agent-jane-01"
export PHILOTIC_SMOKE_CHAT_ID="smoke-cognitive-reentry-chat"
export PHILOTIC_SMOKE_USER_CONTENT="use echo hello structured tool"
export PHILOTIC_SMOKE_EXPECTED_REPLY="Tool echo says: hello structured tool"
export PHILOTIC_MODEL_ROUTER_STUB_RESPONSE='smoke-turn-1=json:{"tool_call":{"tool_name":"echo","arguments":{"text":"hello structured tool"}},"active_plan":{"goal":"echo hello structured tool","status":"in_progress","steps":[{"id":1,"description":"call echo","tool_name":"echo","status":"in_progress"},{"id":2,"description":"respond to user","status":"pending"}]}};smoke-turn-1:2=json:{"display_text":"Tool echo says: hello structured tool","spoken_text":"Tool echo says: hello structured tool","active_plan":{"goal":"echo hello structured tool","status":"completed","steps":[{"id":1,"description":"call echo","tool_name":"echo","status":"done"},{"id":2,"description":"respond to user","status":"done"}]},"require_prompt_substrings":["[Tool call history]","Call 1: echo({\"text\":\"hello structured tool\"})","[Active plan]","Goal: echo hello structured tool","[in_progress] 1. call echo"]}'
SOCKET_PATH="/tmp/philotic-${HOTEL_NAME}.sock"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "Cognitive reentry smoke failed. aiua log:"
    [[ -f "${TMP_DIR}/aiua.log" ]] && cat "${TMP_DIR}/aiua.log"
    echo "Cognitive reentry smoke failed. agent log:"
    [[ -f "${TMP_DIR}/agent.log" ]] && cat "${TMP_DIR}/agent.log"
    echo "Cognitive reentry smoke failed. model log:"
    [[ -f "${TMP_DIR}/model.log" ]] && cat "${TMP_DIR}/model.log"
    echo "Cognitive reentry smoke failed. tool log:"
    [[ -f "${TMP_DIR}/tool.log" ]] && cat "${TMP_DIR}/tool.log"
  fi
  [[ -n "${TOOL_PID:-}" ]] && kill "${TOOL_PID}" >/dev/null 2>&1
  [[ -n "${MODEL_PID:-}" ]] && kill "${MODEL_PID}" >/dev/null 2>&1
  [[ -n "${AGENT_PID:-}" ]] && kill "${AGENT_PID}" >/dev/null 2>&1
  [[ -n "${ANSIBLE_PID:-}" ]] && kill "${ANSIBLE_PID}" >/dev/null 2>&1
  wait "${TOOL_PID:-}" >/dev/null 2>&1
  wait "${MODEL_PID:-}" >/dev/null 2>&1
  wait "${AGENT_PID:-}" >/dev/null 2>&1
  wait "${ANSIBLE_PID:-}" >/dev/null 2>&1
  rm -f "${SOCKET_PATH}"
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

echo "Building cognitive-reentry smoke binaries..."
cargo build -p aiua -p philote -p model-router -p tool-runner -p philotic-client --example smoke_driver >/dev/null

echo "Starting aiua in ${TMP_DIR}..."
(
  cd "${TMP_DIR}"
  PHILOTIC_SMOKE_MODE=1 cargo run -q --manifest-path "${ROOT_DIR}/crates/aiua/Cargo.toml" --bin aiua -- --hotel "${HOTEL_NAME}" >"${TMP_DIR}/aiua.log" 2>&1
) &
ANSIBLE_PID=$!

for _ in {1..50}; do
  if [[ -S "${SOCKET_PATH}" ]]; then
    break
  fi
  sleep 0.2
done

if [[ ! -S "${SOCKET_PATH}" ]]; then
  echo "aiua socket did not appear"
  exit 1
fi

echo "Starting philote..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  cargo run -q --manifest-path "${ROOT_DIR}/crates/philote/Cargo.toml" --bin philote >"${TMP_DIR}/agent.log" 2>&1 &
AGENT_PID=$!

echo "Starting model-router..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  cargo run -q --manifest-path "${ROOT_DIR}/crates/model-router/Cargo.toml" --bin model-router >"${TMP_DIR}/model.log" 2>&1 &
MODEL_PID=$!

echo "Starting tool-runner..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  cargo run -q --manifest-path "${ROOT_DIR}/crates/tool-runner/Cargo.toml" --bin tool-runner >"${TMP_DIR}/tool.log" 2>&1 &
TOOL_PID=$!

sleep 1

echo "Driving cognitive reentry round-trip..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  cargo run -q -p philotic-client --example smoke_driver

echo "Cognitive reentry smoke round-trip succeeded."
