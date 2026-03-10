#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
SOURCE_HOTEL="local"
REMOTE_HOTEL="aria-architect-hotel"
SOURCE_SOCKET="/tmp/philotic-${SOURCE_HOTEL}.sock"
REMOTE_SOCKET="/tmp/philotic-${REMOTE_HOTEL}.sock"
EXPECTED_REPLY="${PHILOTIC_SMOKE_EXPECTED_REPLY:-remote model smoke ok}"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "Remote-model smoke failed. source ansible log:"
    [[ -f "${TMP_DIR}/source-ansible.log" ]] && cat "${TMP_DIR}/source-ansible.log"
    echo "Remote-model smoke failed. remote ansible log:"
    [[ -f "${TMP_DIR}/remote-ansible.log" ]] && cat "${TMP_DIR}/remote-ansible.log"
  fi
  [[ -n "${SOURCE_ANSIBLE_PID:-}" ]] && kill "${SOURCE_ANSIBLE_PID}" >/dev/null 2>&1
  [[ -n "${REMOTE_ANSIBLE_PID:-}" ]] && kill "${REMOTE_ANSIBLE_PID}" >/dev/null 2>&1
  wait "${SOURCE_ANSIBLE_PID:-}" >/dev/null 2>&1
  wait "${REMOTE_ANSIBLE_PID:-}" >/dev/null 2>&1
  rm -f "${SOURCE_SOCKET}" "${REMOTE_SOCKET}"
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

echo "Building remote-model smoke binaries..."
cargo build \
  -p ansible --bin ansible \
  -p agent-core --bin agent-core \
  -p model-router --bin model-controller-gemini \
  -p model-router --bin model-controller-elevenlabs \
  -p tool-runner --bin tool-runner \
  -p hegemon --bin hegemon \
  -p philotic-client --example remote_model_smoke_driver >/dev/null

ln -s "${ROOT_DIR}/target" "${TMP_DIR}/target"

echo "Starting remote hotel [${REMOTE_HOTEL}] with guest supervision..."
(
  cd "${TMP_DIR}"
  RUST_LOG=info \
  PHILOTIC_ENABLE_RUST_DISPATCHER=1 \
  PHILOTIC_ENABLE_RUST_TASK_LIFECYCLE=1 \
  PHILOTIC_ENABLE_GUEST_SUPERVISOR=1 \
  PHILOTIC_MODEL_ROUTER_STUB_RESPONSE="${EXPECTED_REPLY}" \
  "${ROOT_DIR}/target/debug/ansible" --hotel "${REMOTE_HOTEL}" >"${TMP_DIR}/remote-ansible.log" 2>&1
) &
REMOTE_ANSIBLE_PID=$!

for _ in {1..80}; do
  if [[ -S "${REMOTE_SOCKET}" ]]; then
    break
  fi
  sleep 0.25
done

if [[ ! -S "${REMOTE_SOCKET}" ]]; then
  echo "remote hotel socket did not appear"
  exit 1
fi

echo "Starting source hotel [${SOURCE_HOTEL}]..."
(
  cd "${TMP_DIR}"
  RUST_LOG=info \
  PHILOTIC_ENABLE_RUST_DISPATCHER=1 \
  PHILOTIC_ENABLE_RUST_TASK_LIFECYCLE=1 \
  "${ROOT_DIR}/target/debug/ansible" --hotel "${SOURCE_HOTEL}" >"${TMP_DIR}/source-ansible.log" 2>&1
) &
SOURCE_ANSIBLE_PID=$!

for _ in {1..80}; do
  if [[ -S "${SOURCE_SOCKET}" ]]; then
    break
  fi
  sleep 0.25
done

if [[ ! -S "${SOURCE_SOCKET}" ]]; then
  echo "source hotel socket did not appear"
  exit 1
fi

sleep 2

echo "Disabling live local model on source hotel so remote placement can actually happen..."
SOURCE_DB="${TMP_DIR}/ansible_context.db"
SOURCE_MODEL_GUEST="${SOURCE_HOTEL}:model-controller-gemini"
SOURCE_MODEL_PID=""

for _ in {1..40}; do
  SOURCE_MODEL_PID="$(sqlite3 "${SOURCE_DB}" "SELECT active_pid FROM materialized_guests WHERE hotel_name = '${SOURCE_HOTEL}' AND guest_id = '${SOURCE_MODEL_GUEST}' LIMIT 1;" || true)"
  if [[ -n "${SOURCE_MODEL_PID}" ]]; then
    break
  fi
  sleep 0.25
done

sqlite3 "${SOURCE_DB}" "UPDATE materialized_guests SET is_active = 0, active_pid = NULL WHERE hotel_name = '${SOURCE_HOTEL}' AND guest_id = '${SOURCE_MODEL_GUEST}';" >/dev/null

if [[ -n "${SOURCE_MODEL_PID}" ]]; then
  kill "${SOURCE_MODEL_PID}" >/dev/null 2>&1 || true
fi

sleep 2

echo "Driving remote-model round-trip..."
PHILOTIC_HOTEL_SOCKET="${SOURCE_SOCKET}" \
PHILOTIC_REMOTE_HOTEL_ID="${REMOTE_HOTEL}" \
PHILOTIC_REMOTE_NODE_ID="${REMOTE_HOTEL}-ansible-01" \
PHILOTIC_REMOTE_MODEL_INCID="${REMOTE_HOTEL}:model-controller-gemini" \
PHILOTIC_SMOKE_EXPECTED_REPLY="${EXPECTED_REPLY}" \
  cargo run -q -p philotic-client --example remote_model_smoke_driver

echo "Remote-model smoke round-trip succeeded."
