#!/usr/bin/env bash
# smoke-mlx-controller.sh — MLX model controller smoke test
#
# Level 1 (no hotel required):
#   - Start mlx_lm.server with a tiny model in attached mode
#   - Verify /v1/models and a chat completion via curl
#   - Run model-controller-mlx --discover to verify fleet startup + model discovery
#
# Level 2 (hotel roundtrip, optional):
#   - If mesh-config.json exists, start aiua and run a text roundtrip via model.mlx role
#
# Usage:
#   ./scripts/smoke-mlx-controller.sh
#   SMOKE_MODEL=mlx-community/Qwen2.5-0.5B-Instruct-4bit ./scripts/smoke-mlx-controller.sh
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
SMOKE_PORT="${SMOKE_MLX_PORT:-11499}"
SMOKE_MODEL="${SMOKE_MODEL:-mlx-community/Qwen2.5-0.5B-Instruct-4bit}"
SERVER_PID=""

pass() { echo "  PASS  $*"; }
fail() { echo "  FAIL  $*"; exit 1; }
step() { echo; echo "==> $*"; }

cleanup() {
  local exit_code=$?
  set +e
  [[ -n "${SERVER_PID}" ]] && kill "${SERVER_PID}" 2>/dev/null && wait "${SERVER_PID}" 2>/dev/null
  if [[ ${exit_code} -ne 0 ]]; then
    echo
    echo "Smoke failed. mlx_lm.server log:"
    [[ -f "${TMP_DIR}/mlx_server.log" ]] && tail -30 "${TMP_DIR}/mlx_server.log"
  fi
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

# ── Prerequisites ─────────────────────────────────────────────────────────────

step "Checking prerequisites"
# For attached-only smoke, the controller binary has no Python dependency.
# mlx_lm must be available for starting the server, but not for the controller itself.
python3 -c "import mlx_lm" 2>/dev/null \
  || fail "mlx_lm not installed — needed to start smoke server. Run: pip3 install mlx-lm"
pass "mlx_lm importable (smoke server only — controller itself has no Python dependency)"

# ── Build ─────────────────────────────────────────────────────────────────────

step "Building model-controller-mlx"
cargo build -p model-router --bin model-controller-mlx -q
BINARY="${ROOT_DIR}/target/debug/model-controller-mlx"
pass "binary built: ${BINARY}"

# ── Start mlx_lm.server ───────────────────────────────────────────────────────

step "Starting mlx_lm.server (model=${SMOKE_MODEL}, port=${SMOKE_PORT})"
echo "  (first run will download the model — this may take a few minutes)"
python3 -m mlx_lm.server \
  --model "${SMOKE_MODEL}" \
  --port "${SMOKE_PORT}" \
  >"${TMP_DIR}/mlx_server.log" 2>&1 &
SERVER_PID=$!

# Poll until ready (up to 5 minutes for model download + load)
echo -n "  Waiting for server"
for i in $(seq 1 150); do
  sleep 2
  if curl -sf "http://localhost:${SMOKE_PORT}/v1/models" >/dev/null 2>&1; then
    echo " ready (${i}x2s)"
    break
  fi
  echo -n "."
  if [[ $i -eq 150 ]]; then
    echo
    fail "mlx_lm.server did not start within 5 minutes"
  fi
done
pass "mlx_lm.server ready on port ${SMOKE_PORT}"

# ── Level 1: direct HTTP checks ───────────────────────────────────────────────

step "Level 1: /v1/models discovery"
MODELS_RESP=$(curl -sf "http://localhost:${SMOKE_PORT}/v1/models")
echo "  ${MODELS_RESP}" | python3 -m json.tool --no-ensure-ascii 2>/dev/null || echo "  ${MODELS_RESP}"
LOADED_MODEL=$(echo "${MODELS_RESP}" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['data'][0]['id'])" 2>/dev/null || echo "unknown")
pass "/v1/models returned model: ${LOADED_MODEL}"

step "Level 1: chat completion via curl"
CHAT_RESP=$(curl -sf "http://localhost:${SMOKE_PORT}/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -d "{\"model\":\"${LOADED_MODEL}\",\"messages\":[{\"role\":\"user\",\"content\":\"Reply with exactly: pong\"}],\"max_tokens\":10}")
CONTENT=$(echo "${CHAT_RESP}" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['choices'][0]['message']['content'])" 2>/dev/null || echo "")
[[ -n "${CONTENT}" ]] || fail "chat completion returned empty content"
pass "chat completion: \"${CONTENT}\""

# ── Level 1: --discover mode ──────────────────────────────────────────────────

step "Level 1: model-controller-mlx --discover"
cat >"${TMP_DIR}/fleet.json" <<JSON
{
  "health_check_interval_secs": 300,
  "models": [
    {
      "class": "text",
      "repo_id": "${SMOKE_MODEL}",
      "mode": "attached",
      "host": "127.0.0.1",
      "port": ${SMOKE_PORT},
      "priority": 100
    }
  ]
}
JSON

DISCOVER_OUT=$(RUST_LOG=info "${BINARY}" \
  --config "${TMP_DIR}/fleet.json" \
  --discover 2>"${TMP_DIR}/discover.log")

echo "  Fleet summary:"
echo "${DISCOVER_OUT}" | sed 's/^/    /'

# Verify the loaded model was discovered.
echo "${DISCOVER_OUT}" | grep -q "loaded=" || fail "--discover output missing loaded= field"
echo "${DISCOVER_OUT}" | grep -q "health=Healthy" || fail "instance not Healthy after startup"
pass "--discover: fleet startup and model discovery OK"

# Log any warnings from discovery
if grep -qi "differs from configured" "${TMP_DIR}/discover.log" 2>/dev/null; then
  echo "  NOTE: loaded model differs from configured repo_id (expected for some attached servers)"
fi

# ── Level 2: hotel roundtrip (optional) ──────────────────────────────────────

if [[ -f "${ROOT_DIR}/mesh-config.json" ]]; then
  step "Level 2: hotel roundtrip (mesh-config.json found)"
  HOTEL_NAME="mlx-smoke-$$"
  export PHILOTIC_NODE_ID="${HOTEL_NAME}-aiua-01"
  export PHILOTIC_TARGET_NODE="${HOTEL_NAME}-aiua-01"
  export PHILOTIC_MLX_CONFIG="${TMP_DIR}/fleet.json"

  cargo build -p aiua -q
  "${ROOT_DIR}/target/debug/aiua" \
    --hotel "${HOTEL_NAME}" \
    --load-config "${ROOT_DIR}/mesh-config.json" \
    --test text-roundtrip \
    --test-text "ping from mlx smoke" \
    >"${TMP_DIR}/aiua.log" 2>&1 || {
      echo "  Hotel roundtrip log:"
      tail -20 "${TMP_DIR}/aiua.log"
      fail "hotel roundtrip failed"
    }
  pass "hotel roundtrip succeeded"
else
  step "Level 2: skipped (no mesh-config.json — hotel roundtrip not tested)"
  echo "  Copy mesh-config.example.json → mesh-config.json to enable."
fi

echo
echo "MLX controller smoke: ALL CHECKS PASSED"
