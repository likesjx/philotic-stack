#!/usr/bin/env bash
# smoke-desktop-membrane.sh — Desktop membrane management surface smoke
#
# Starts an ephemeral aiua hotel and philotic-web serve against it, then
# exercises the core API surface:
#
#   - lease acquisition confirmed in serve log
#   - GET /api/status   — version, hotel, daemon fields
#   - GET /api/guests   — returns JSON array
#   - GET /api/agents   — returns JSON array
#   - GET /api/mesh/targets — returns JSON array
#   - 401 rejection for unauthenticated requests
#   - 401 rejection for wrong-token requests
#   - /api/apartments/:id blocked (no 200 for unknown agent)
#   - SIGTERM triggers clean shutdown
#
# Tier: smoke-green (no external creds; self-contained hotel).
# Note: on a local desktop session this will briefly open a browser tab — that
#       is expected behaviour from philotic-web serve and is ignored by the smoke.
#
# Environment overrides:
#   SMOKE_SERVE_PORT   Port for philotic-web serve (default: 7799)

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
HOTEL_NAME="desktop-smoke-$$"
SOCKET_PATH="/tmp/philotic-${HOTEL_NAME}.sock"
SERVE_PORT="${SMOKE_SERVE_PORT:-7799}"
SERVE_ADDR="127.0.0.1:${SERVE_PORT}"
AIUA_PID=""
SERVE_PID=""

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "--- Desktop membrane smoke FAILED ---" >&2
    echo "aiua log:" >&2
    [[ -f "${TMP_DIR}/aiua.log" ]] && cat "${TMP_DIR}/aiua.log" >&2
    echo "serve log:" >&2
    [[ -f "${TMP_DIR}/serve.log" ]] && cat "${TMP_DIR}/serve.log" >&2
  fi
  [[ -n "${SERVE_PID}" ]] && kill "${SERVE_PID}" >/dev/null 2>&1 || true
  [[ -n "${AIUA_PID}" ]]  && kill "${AIUA_PID}"  >/dev/null 2>&1 || true
  wait "${SERVE_PID:-}" >/dev/null 2>&1 || true
  wait "${AIUA_PID:-}"  >/dev/null 2>&1 || true
  rm -f "${SOCKET_PATH}"
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

# ── Build ─────────────────────────────────────────────────────────────────────

echo "Building binaries..."
cargo build -p aiua -p philotic-web 2>&1 | tail -5

# ── Minimal smoke config ──────────────────────────────────────────────────────

# philotic-web reads the first key under "hotels" to form the lease scope.
cat > "${TMP_DIR}/mesh-config.json" <<CONFIGEOF
{
  "hotels": {
    "${HOTEL_NAME}": {}
  }
}
CONFIGEOF

# ── Start aiua ────────────────────────────────────────────────────────────────

echo "Starting aiua hotel '${HOTEL_NAME}'..."
(
  cd "${TMP_DIR}"
  PHILOTIC_SMOKE_MODE=1 \
    "${ROOT_DIR}/target/debug/aiua" --hotel "${HOTEL_NAME}" \
    >"${TMP_DIR}/aiua.log" 2>&1
) &
AIUA_PID=$!

for i in $(seq 1 50); do
  if [[ -S "${SOCKET_PATH}" ]]; then break; fi
  if ! kill -0 "${AIUA_PID}" 2>/dev/null; then
    echo "aiua died during startup" >&2
    cat "${TMP_DIR}/aiua.log" >&2
    exit 1
  fi
  if [[ ${i} -eq 50 ]]; then
    echo "aiua socket did not appear at ${SOCKET_PATH}" >&2
    cat "${TMP_DIR}/aiua.log" >&2
    exit 1
  fi
  sleep 0.2
done
echo "aiua socket ready."

# ── Start philotic-web serve ──────────────────────────────────────────────────

echo "Starting philotic-web serve on port ${SERVE_PORT}..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
  "${ROOT_DIR}/target/debug/philotic-web" serve \
    --port "${SERVE_PORT}" \
    --config "${TMP_DIR}/mesh-config.json" \
    >"${TMP_DIR}/serve.log" 2>&1 &
SERVE_PID=$!

# Wait for the token line in the serve log.
TOKEN=""
for i in $(seq 1 30); do
  if ! kill -0 "${SERVE_PID}" 2>/dev/null; then
    echo "philotic-web serve died during startup" >&2
    cat "${TMP_DIR}/serve.log" >&2
    exit 1
  fi
  TOKEN=$(grep -oE 'philotic-[a-f0-9]+' "${TMP_DIR}/serve.log" 2>/dev/null | head -1 || true)
  if [[ -n "${TOKEN}" ]]; then break; fi
  if [[ ${i} -eq 30 ]]; then
    echo "philotic-web serve did not emit token within 15s" >&2
    cat "${TMP_DIR}/serve.log" >&2
    exit 1
  fi
  sleep 0.5
done
echo "Serve ready. Token prefix: ${TOKEN:0:22}..."

# ── Verify lease acquisition ──────────────────────────────────────────────────

if ! grep -q "Desktop membrane lease:" "${TMP_DIR}/serve.log"; then
  echo "ERROR: desktop membrane lease line not found in serve log" >&2
  cat "${TMP_DIR}/serve.log" >&2
  exit 1
fi
echo "Lease acquisition: confirmed."

# ── API helpers ───────────────────────────────────────────────────────────────

api() {
  local method="$1" path="$2"
  shift 2
  curl -sf -X "${method}" \
    -H "Authorization: Bearer ${TOKEN}" \
    "http://${SERVE_ADDR}${path}" \
    "$@"
}

http_code() {
  local path="$1"; shift
  curl -s -o /dev/null -w "%{http_code}" "http://${SERVE_ADDR}${path}" "$@"
}

# ── GET /api/status ───────────────────────────────────────────────────────────

echo "Testing GET /api/status..."
STATUS=$(api GET /api/status)
echo "${STATUS}" | python3 -c "
import json, sys
r = json.load(sys.stdin)
assert 'version' in r, f'missing version: {r}'
assert 'hotel' in r,   f'missing hotel: {r}'
assert 'daemon' in r,  f'missing daemon: {r}'
print(f'  hotel={r[\"hotel\"]}  daemon={r[\"daemon\"]}')
"
echo "  /api/status OK"

# ── GET /api/guests ───────────────────────────────────────────────────────────

echo "Testing GET /api/guests..."
GUESTS=$(api GET /api/guests)
echo "${GUESTS}" | python3 -c "
import json, sys
r = json.load(sys.stdin)
assert isinstance(r, list), f'expected list, got: {r}'
print(f'  {len(r)} guest(s)')
"
echo "  /api/guests OK"

# ── GET /api/agents ───────────────────────────────────────────────────────────

echo "Testing GET /api/agents..."
AGENTS=$(api GET /api/agents)
echo "${AGENTS}" | python3 -c "
import json, sys
r = json.load(sys.stdin)
assert isinstance(r, list), f'expected list, got: {r}'
print(f'  {len(r)} agent(s)')
"
echo "  /api/agents OK"

# ── GET /api/mesh/targets ─────────────────────────────────────────────────────

echo "Testing GET /api/mesh/targets..."
TARGETS=$(api GET /api/mesh/targets)
echo "${TARGETS}" | python3 -c "
import json, sys
r = json.load(sys.stdin)
assert isinstance(r, list), f'expected list, got: {r}'
print(f'  {len(r)} target(s)')
"
echo "  /api/mesh/targets OK"

# ── GET /api/skills ───────────────────────────────────────────────────────────

echo "Testing GET /api/skills..."
SKILLS=$(api GET /api/skills)
echo "${SKILLS}" | python3 -c "
import json, sys
r = json.load(sys.stdin)
assert isinstance(r, list), f'expected list, got: {r}'
print(f'  {len(r)} skill(s)')
"
echo "  /api/skills OK"

# ── GET /api/agents/:id/roles and /rules (unknown agent → stable response) ───

echo "Testing GET /api/agents/smoke-agent/roles..."
ROLES_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
  -H "Authorization: Bearer ${TOKEN}" \
  "http://${SERVE_ADDR}/api/agents/smoke-agent/roles")
# 200 (empty roles) or 500 (agent not found) — either is a valid wired response
if [[ "${ROLES_CODE}" != "200" && "${ROLES_CODE}" != "500" ]]; then
  echo "ERROR: /api/agents/:id/roles returned unexpected ${ROLES_CODE}" >&2
  exit 1
fi
echo "  /api/agents/smoke-agent/roles → ${ROLES_CODE} OK"

echo "Testing GET /api/agents/smoke-agent/rules..."
RULES_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
  -H "Authorization: Bearer ${TOKEN}" \
  "http://${SERVE_ADDR}/api/agents/smoke-agent/rules")
if [[ "${RULES_CODE}" != "200" && "${RULES_CODE}" != "500" ]]; then
  echo "ERROR: /api/agents/:id/rules returned unexpected ${RULES_CODE}" >&2
  exit 1
fi
echo "  /api/agents/smoke-agent/rules → ${RULES_CODE} OK"

# ── GET /api/config ───────────────────────────────────────────────────────────

echo "Testing GET /api/config..."
CONFIG=$(api GET /api/config)
echo "${CONFIG}" | python3 -c "
import json, sys
r = json.load(sys.stdin)
assert isinstance(r, dict), f'expected dict, got: {r}'
print(f'  keys: {list(r.keys())}')
"
echo "  /api/config OK"

# ── GET /api/config/telegram ──────────────────────────────────────────────────

echo "Testing GET /api/config/telegram..."
TG=$(api GET /api/config/telegram)
echo "${TG}" | python3 -c "
import json, sys
r = json.load(sys.stdin)
assert 'token_configured' in r, f'missing token_configured: {r}'
print(f'  token_configured={r[\"token_configured\"]}')
"
echo "  /api/config/telegram OK"

# ── GET /api/config/gemini ────────────────────────────────────────────────────

echo "Testing GET /api/config/gemini..."
GM=$(api GET /api/config/gemini)
echo "${GM}" | python3 -c "
import json, sys
r = json.load(sys.stdin)
assert 'oauth_metadata' in r, f'missing oauth_metadata: {r}'
assert 'token_refs_configured' in r, f'missing token_refs_configured: {r}'
print(f'  refs_configured={r[\"token_refs_configured\"]}')
"
echo "  /api/config/gemini OK"

# ── Auth rejection — no token ─────────────────────────────────────────────────

echo "Testing 401 for unauthenticated request..."
CODE=$(http_code /api/status)
if [[ "${CODE}" != "401" ]]; then
  echo "ERROR: expected 401 (no token), got ${CODE}" >&2
  exit 1
fi
echo "  Unauthenticated → 401 OK"

# ── Auth rejection — wrong token ──────────────────────────────────────────────

echo "Testing 401 for wrong-token request..."
CODE=$(http_code /api/status -H "Authorization: Bearer philotic-wrongtoken000000000000000000000000000000000")
if [[ "${CODE}" != "401" ]]; then
  echo "ERROR: expected 401 (wrong token), got ${CODE}" >&2
  exit 1
fi
echo "  Wrong token → 401 OK"

# ── Apartment inspection should not return 200 ────────────────────────────────

echo "Testing /api/apartments/:id is not openly accessible..."
APT_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
  -H "Authorization: Bearer ${TOKEN}" \
  "http://${SERVE_ADDR}/api/apartments/smoke-test-agent")
if [[ "${APT_CODE}" == "200" ]]; then
  echo "ERROR: /api/apartments returned 200 for unknown agent — expected 404 or 403" >&2
  exit 1
fi
echo "  /api/apartments/smoke-test-agent → ${APT_CODE} (not 200, as expected)"

# ── Clean shutdown ────────────────────────────────────────────────────────────

echo "Sending SIGTERM to philotic-web serve (pid ${SERVE_PID})..."
kill "${SERVE_PID}"
for i in $(seq 1 20); do
  if ! kill -0 "${SERVE_PID}" 2>/dev/null; then break; fi
  if [[ ${i} -eq 20 ]]; then
    echo "ERROR: philotic-web serve did not exit after SIGTERM" >&2
    exit 1
  fi
  sleep 0.5
done
SERVE_PID=""
echo "philotic-web serve exited cleanly."

# ── Done ──────────────────────────────────────────────────────────────────────

echo ""
echo "Desktop membrane smoke PASSED."
