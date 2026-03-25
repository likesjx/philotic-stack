#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
SOURCE_HOTEL="local-remote-smoke-$$"
REMOTE_HOTEL="aria-architect-hotel-smoke-$$"
SOURCE_SOCKET="/tmp/philotic-${SOURCE_HOTEL}.sock"
REMOTE_SOCKET="/tmp/philotic-${REMOTE_HOTEL}.sock"
EXPECTED_REPLY="${PHILOTIC_SMOKE_EXPECTED_REPLY:-remote model smoke ok}"
export PHILOTIC_NODE_ID="${SOURCE_HOTEL}-aiua-01"
export PHILOTIC_TARGET_NODE="${SOURCE_HOTEL}-aiua-01"
export PHILOTIC_FINAL_REPLY_TO="${SOURCE_HOTEL}-aiua-01"
export PHILOTIC_AGENT_ID="agent-jane-01"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "Remote-model smoke failed. source aiua log:"
    [[ -f "${TMP_DIR}/source-aiua.log" ]] && cat "${TMP_DIR}/source-aiua.log"
    echo "Remote-model smoke failed. remote aiua log:"
    [[ -f "${TMP_DIR}/remote-aiua.log" ]] && cat "${TMP_DIR}/remote-aiua.log"
    echo "Remote-model smoke failed. remote model log:"
    [[ -f "${TMP_DIR}/remote-model.log" ]] && cat "${TMP_DIR}/remote-model.log"
    echo "Remote-model smoke failed. source agent log:"
    [[ -f "${TMP_DIR}/source-agent.log" ]] && cat "${TMP_DIR}/source-agent.log"
  fi
  [[ -n "${REMOTE_MODEL_PID:-}" ]] && kill "${REMOTE_MODEL_PID}" >/dev/null 2>&1
  [[ -n "${SOURCE_AGENT_PID:-}" ]]  && kill "${SOURCE_AGENT_PID}" >/dev/null 2>&1
  [[ -n "${SOURCE_ANSIBLE_PID:-}" ]] && kill "${SOURCE_ANSIBLE_PID}" >/dev/null 2>&1
  [[ -n "${REMOTE_ANSIBLE_PID:-}" ]] && kill "${REMOTE_ANSIBLE_PID}" >/dev/null 2>&1
  wait "${REMOTE_MODEL_PID:-}" >/dev/null 2>&1
  wait "${SOURCE_AGENT_PID:-}" >/dev/null 2>&1
  wait "${SOURCE_ANSIBLE_PID:-}" >/dev/null 2>&1
  wait "${REMOTE_ANSIBLE_PID:-}" >/dev/null 2>&1
  rm -f "${SOURCE_SOCKET}" "${REMOTE_SOCKET}"
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

echo "Building remote-model smoke binaries..."
cargo build \
  -p aiua --bin aiua \
  -p philote --bin philote \
  -p model-router --bin model-router \
  -p tool-runner --bin tool-runner \
  -p philotic-client --example smoke_driver >/dev/null

# ── Remote hotel ──────────────────────────────────────────────────────────────
echo "Starting remote hotel [${REMOTE_HOTEL}]..."
(
  PHILOTIC_SMOKE_MODE=1 \
    "${ROOT_DIR}/target/debug/aiua" --hotel "${REMOTE_HOTEL}" \
    >"${TMP_DIR}/remote-aiua.log" 2>&1
) &
REMOTE_ANSIBLE_PID=$!

for _ in {1..50}; do [[ -S "${REMOTE_SOCKET}" ]] && break; sleep 0.2; done
if [[ ! -S "${REMOTE_SOCKET}" ]]; then echo "remote hotel socket did not appear"; exit 1; fi

# Start a stub model-router on the remote hotel — guest_id must match what the
# smoke driver looks for in the route assembly incarnation_id.
echo "Starting remote model-router stub..."
PHILOTIC_HOTEL_SOCKET="${REMOTE_SOCKET}" \
PHILOTIC_MODEL_ROUTER_STUB_RESPONSE="${EXPECTED_REPLY}" \
  "${ROOT_DIR}/target/debug/model-router" \
  >"${TMP_DIR}/remote-model.log" 2>&1 &
REMOTE_MODEL_PID=$!

# ── Source hotel ──────────────────────────────────────────────────────────────
echo "Starting source hotel [${SOURCE_HOTEL}]..."
(
  PHILOTIC_SMOKE_MODE=1 \
    "${ROOT_DIR}/target/debug/aiua" --hotel "${SOURCE_HOTEL}" \
    >"${TMP_DIR}/source-aiua.log" 2>&1
) &
SOURCE_ANSIBLE_PID=$!

for _ in {1..50}; do [[ -S "${SOURCE_SOCKET}" ]] && break; sleep 0.2; done
if [[ ! -S "${SOURCE_SOCKET}" ]]; then echo "source hotel socket did not appear"; exit 1; fi

# Start a philote agent on the source hotel so it can route tasks.
echo "Starting source agent..."
PHILOTIC_HOTEL_SOCKET="${SOURCE_SOCKET}" \
  "${ROOT_DIR}/target/debug/philote" \
  >"${TMP_DIR}/source-agent.log" 2>&1 &
SOURCE_AGENT_PID=$!

sleep 1

# ── Smoke driver ──────────────────────────────────────────────────────────────
# Resolve the actual guest_id model-router registered with so we can pass the
# correct incarnation to the driver.
REMOTE_MODEL_GUEST_ID="model-router-legacy-01"
REMOTE_MODEL_INCID="${REMOTE_HOTEL}-aiua-01:${REMOTE_MODEL_GUEST_ID}"

echo "Driving remote-model round-trip..."
PHILOTIC_HOTEL_SOCKET="${SOURCE_SOCKET}" \
PHILOTIC_REMOTE_HOTEL_ID="${REMOTE_HOTEL}" \
PHILOTIC_REMOTE_NODE_ID="${REMOTE_HOTEL}-aiua-01" \
PHILOTIC_REMOTE_MODEL_INCID="${REMOTE_MODEL_INCID}" \
PHILOTIC_REMOTE_HOTEL_SOCKET="${REMOTE_SOCKET}" \
PHILOTIC_SMOKE_EXPECTED_REPLY="${EXPECTED_REPLY}" \
PHILOTIC_SMOKE_SESSION_ID="smoke:remote-model:agent-jane-01" \
PHILOTIC_SMOKE_TURN_ID="remote-model-turn-1" \
PHILOTIC_SMOKE_CHAT_ID="remote-model-chat-1" \
  cargo run -q -p philotic-client --example remote_model_smoke_driver

echo "Remote-model smoke round-trip succeeded."
