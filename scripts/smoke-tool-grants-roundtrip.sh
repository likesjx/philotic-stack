#!/usr/bin/env bash
# Smoke test: data-driven tool grants (proposal:data-driven-tool-grants-skilldag).
#
# Proves the slice-1 acceptance criterion against a real, running hotel:
# disabling a tool at runtime removes it from a composed session snapshot with
# NO rebuild and NO deploy, and the disable survives a hotel restart.
#
# Every assertion reads `__session_snapshot__:<session_id>` back over real IPC,
# so this exercises the actual composition path (registry -> resolved grants ->
# policy strip -> bindings), not a unit-test stand-in.
#
# Steps:
#   1. boot an ephemeral hotel   -> the registry is seeded from the built-ins
#   2. bind a tool into a session -> it appears in the composed snapshot
#   3. `phil tools disable`       -> it disappears (no rebuild, no restart)
#   4. restart the hotel          -> it STAYS disabled (the seeder must not revert)
#   5. `phil tools enable`        -> it comes back
#
# Usage: bash scripts/smoke-tool-grants-roundtrip.sh
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
HOTEL_NAME="tool-grants-smoke-$$"
export PHILOTIC_AGENT_ID="agent-jane-01"
export PHILOTIC_NODE_ID="${HOTEL_NAME}-aiua-01"
export PHILOTIC_TARGET_NODE="${HOTEL_NAME}-aiua-01"
export PHILOTIC_FINAL_REPLY_TO="${HOTEL_NAME}-aiua-01"
SOCKET_PATH="/tmp/philotic-${HOTEL_NAME}.sock"
SESSION_ID="smoke:tool-grants:agent-jane-01"
TOOL="life.observe.batch"
SNAPSHOT_KEY="__session_snapshot__:${SESSION_ID}"

AIUA_BIN="${ROOT_DIR}/target/debug/aiua"
PHILOTE_BIN="${ROOT_DIR}/target/debug/philote"
PHIL_BIN="${ROOT_DIR}/target/debug/philotic-web"
DRIVER_BIN="${ROOT_DIR}/target/debug/examples/tool_grant_smoke_driver"
DB_PATH="${TMP_DIR}/aiua_context.db"

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "Tool-grants smoke FAILED. aiua log:"
    [[ -f "${TMP_DIR}/aiua.log" ]] && tail -40 "${TMP_DIR}/aiua.log"
    echo "agent log:"
    [[ -f "${TMP_DIR}/agent.log" ]] && tail -20 "${TMP_DIR}/agent.log"
  fi
  [[ -n "${AGENT_PID:-}" ]] && kill "${AGENT_PID}" >/dev/null 2>&1
  [[ -n "${ANSIBLE_PID:-}" ]] && kill "${ANSIBLE_PID}" >/dev/null 2>&1
  wait "${AGENT_PID:-}" >/dev/null 2>&1
  wait "${ANSIBLE_PID:-}" >/dev/null 2>&1
  rm -f "${SOCKET_PATH}"
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

start_hotel() {
  (
    cd "${TMP_DIR}"
    PHILOTIC_SMOKE_MODE=1 "${AIUA_BIN}" --hotel "${HOTEL_NAME}" >>"${TMP_DIR}/aiua.log" 2>&1
  ) &
  ANSIBLE_PID=$!
  for _ in {1..60}; do
    [[ -S "${SOCKET_PATH}" ]] && break
    sleep 0.2
  done
  if [[ ! -S "${SOCKET_PATH}" ]]; then
    echo "aiua socket did not appear"
    exit 1
  fi
}

# `kill`/`wait` on an already-exiting process returns non-zero, which would trip
# `set -e` and abort the smoke mid-restart — hence the explicit `|| true`.
stop_hotel() {
  if [[ -n "${ANSIBLE_PID:-}" ]]; then
    kill "${ANSIBLE_PID}" >/dev/null 2>&1 || true
    wait "${ANSIBLE_PID}" >/dev/null 2>&1 || true
  fi
  ANSIBLE_PID=""
  rm -f "${SOCKET_PATH}"
  # A hotel refuses to boot while its record still carries a live `active_pid`.
  # A SIGTERM'd instance does not always clear it, so clear it here — the same
  # step an operator takes when restarting a supervised hotel.
  if [[ -f "${DB_PATH}" ]]; then
    sqlite3 "${DB_PATH}" \
      "update graph_nodes set data_json = json_set(data_json, '\$.active_pid', json('null')) where kind='hotel';" \
      >/dev/null 2>&1 || true
  fi
}

stop_agent() {
  if [[ -n "${AGENT_PID:-}" ]]; then
    kill "${AGENT_PID}" >/dev/null 2>&1 || true
    wait "${AGENT_PID}" >/dev/null 2>&1 || true
  fi
  AGENT_PID=""
}

# Reads the composed session snapshot over IPC and reports whether the tool is
# in the session's effective toolset.
snapshot_has_tool() {
  local snapshot
  snapshot="$(PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
    "${PHIL_BIN}" config get "${SNAPSHOT_KEY}" --hotel "${HOTEL_NAME}")"
  echo "${snapshot}" >"${TMP_DIR}/last_snapshot.json"
  python3 - "$TOOL" <<'PY'
import json, sys
tool = sys.argv[1]
with open(__import__("os").environ["SNAPSHOT_FILE"]) as fh:
    raw = fh.read().strip()
if not raw or raw == "null":
    print("NOSESSION")
    sys.exit(0)
snap = json.loads(raw)
bindings = snap.get("bindings") or {}
toolset = bindings.get("effective_toolset") or []
disabled = bindings.get("disabled_tools") or []
print(("PRESENT" if tool in toolset else "ABSENT") + ":" + ("DENIED" if tool in disabled else "ALLOWED"))
PY
}

export SNAPSHOT_FILE="${TMP_DIR}/last_snapshot.json"

expect_state() {
  local want="$1" label="$2" got
  got="$(snapshot_has_tool)"
  if [[ "${got}" != "${want}" ]]; then
    echo "FAIL (${label}): expected '${want}', got '${got}'"
    echo "snapshot bindings were:"
    python3 -c "import json,os;d=json.load(open(os.environ['SNAPSHOT_FILE']));print(json.dumps(d.get('bindings',{}),indent=2)[:2000])" 2>/dev/null
    exit 1
  fi
  echo "  ok (${label}): ${got}"
}

echo "Building tool-grants smoke binaries..."
# Two invocations on purpose: passing --example alongside -p restricts the
# target selection for every package, so the philote/aiua binaries never build.
cargo build -p aiua -p philote -p philotic-web >/dev/null
cargo build -p philotic-client --example tool_grant_smoke_driver >/dev/null

echo "1. Starting ephemeral hotel..."
start_hotel

echo "   Registry seeded at boot:"
"${PHIL_BIN}" tools show --db "${DB_PATH}" | sed 's/^/     /' | head -8

echo "   Starting philote..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" "${PHILOTE_BIN}" >"${TMP_DIR}/agent.log" 2>&1 &
AGENT_PID=$!
sleep 1

echo "2. Binding ${TOOL} into a session..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  PHILOTIC_SMOKE_SESSION_ID="${SESSION_ID}" \
  PHILOTIC_SMOKE_TOOL="${TOOL}" \
  "${DRIVER_BIN}" >/dev/null
expect_state "PRESENT:ALLOWED" "granted before disable"

echo "3. Disabling ${TOOL} at runtime (no rebuild, no restart)..."
"${PHIL_BIN}" tools disable "${TOOL}" --db "${DB_PATH}" | sed 's/^/     /'
expect_state "ABSENT:DENIED" "disabled live"

echo "4. Restarting the hotel (the boot seeder must not revert the disable)..."
stop_agent
stop_hotel
start_hotel
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" "${PHILOTE_BIN}" >>"${TMP_DIR}/agent.log" 2>&1 &
AGENT_PID=$!
sleep 1
expect_state "ABSENT:DENIED" "still disabled after restart"

echo "5. Re-enabling ${TOOL}..."
"${PHIL_BIN}" tools enable "${TOOL}" --db "${DB_PATH}" | sed 's/^/     /'
expect_state "PRESENT:ALLOWED" "re-enabled live"

# ── Slice 2: runner routes follow the class grant ────────────────────────────
echo "6. Runner routes are bound to a class grant (slice 2)..."
"${PHIL_BIN}" tools show --db "${DB_PATH}" | grep -A2 "Remote runner routes" | sed 's/^/     /'
# Matched in two pieces on purpose: the rendered line separates them with an
# em-dash, and `.` does not reliably span a multi-byte character here.
RUNNER_LINE="$("${PHIL_BIN}" tools show --db "${DB_PATH}" | grep 'life-graph-runner' || true)"
if [[ -z "${RUNNER_LINE}" ]] || ! grep -q "class 'life_graph'" <<<"${RUNNER_LINE}"; then
  echo "FAIL: life-graph-runner was not seeded bound to the life_graph class"
  echo "  got: ${RUNNER_LINE:-<no runner line>}"
  exit 1
fi
if ! grep -q "life.observe.batch" <<<"${RUNNER_LINE}"; then
  echo "FAIL: the runner's served tools did not come from the class grant"
  exit 1
fi
echo "  ok: runner bound to its class grant"

# ── Slice 3: every grant change is audited ───────────────────────────────────
echo "7. Grant changes are audited (slice 3)..."
AUDIT="$("${PHIL_BIN}" tools audit --db "${DB_PATH}")"
echo "${AUDIT}" | sed 's/^/     /'
for expected in "disable ${TOOL}" "enable ${TOOL}"; do
  if ! echo "${AUDIT}" | grep -q "${expected}"; then
    echo "FAIL: audit trail is missing an entry for '${expected}'"
    exit 1
  fi
done
if ! echo "${AUDIT}" | grep -q "by phil tools"; then
  echo "FAIL: audit entries do not record who made the change"
  exit 1
fi
echo "  ok: disable and enable both recorded with an actor"

echo "Tool-grants roundtrip smoke succeeded."
