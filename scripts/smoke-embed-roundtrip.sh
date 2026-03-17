#!/usr/bin/env bash
# Smoke test for the ONNX embedding sidecar.
#
# Starts model-controller-onnx in --sidecar-only mode (no hotel IPC required),
# fires a POST /api/embeddings request, and validates the response contains a
# non-empty embedding vector.
#
# Environment overrides:
#   PHILOTIC_ONNX_EMBED_REPO   HuggingFace repo id (default: onnx-community/embeddinggemma-300m-ONNX)
#   SMOKE_SIDECAR_PORT         Port to bind (default: 11436, avoids 11435 used by prod)
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
SIDECAR_PORT="${SMOKE_SIDECAR_PORT:-11436}"
SIDECAR_ADDR="127.0.0.1:${SIDECAR_PORT}"
EMBED_REPO="${PHILOTIC_ONNX_EMBED_REPO:-onnx-community/embeddinggemma-300m-ONNX}"
SIDECAR_PID=""

cleanup() {
  local exit_code=$?
  set +e
  if [[ ${exit_code} -ne 0 ]]; then
    echo "--- Smoke test FAILED. Sidecar log:" >&2
    [[ -f "${TMP_DIR}/sidecar.log" ]] && cat "${TMP_DIR}/sidecar.log" >&2
  fi
  [[ -n "${SIDECAR_PID}" ]] && kill "${SIDECAR_PID}" >/dev/null 2>&1
  wait "${SIDECAR_PID:-}" >/dev/null 2>&1 || true
  rm -rf "${TMP_DIR}"
  exit ${exit_code}
}
trap cleanup EXIT

echo "Building model-controller-onnx..."
cargo build -p model-router --bin model-controller-onnx 2>&1 | tail -5

echo "Starting sidecar on ${SIDECAR_ADDR} (repo: ${EMBED_REPO})..."
PHILOTIC_ONNX_SIDECAR_ONLY=1 \
PHILOTIC_ONNX_SIDECAR_ADDR="${SIDECAR_ADDR}" \
PHILOTIC_ONNX_EMBED_REPO="${EMBED_REPO}" \
  "${ROOT_DIR}/target/debug/model-controller-onnx" \
    --sidecar-only \
    --sidecar-addr "${SIDECAR_ADDR}" \
    --embed-repo "${EMBED_REPO}" \
    >"${TMP_DIR}/sidecar.log" 2>&1 &
SIDECAR_PID=$!

echo "Waiting for sidecar to accept connections..."
for i in $(seq 1 60); do
  if curl -sf "http://${SIDECAR_ADDR}/api/embeddings" \
       -H "Content-Type: application/json" \
       -d '{"prompt":"warmup"}' \
       -o /dev/null 2>/dev/null; then
    break
  fi
  # Also accept a 4xx/5xx — the port is up, endpoint is live
  http_code=$(curl -s -o /dev/null -w "%{http_code}" \
    -X POST "http://${SIDECAR_ADDR}/api/embeddings" \
    -H "Content-Type: application/json" \
    -d '{"prompt":"warmup"}' 2>/dev/null || echo "000")
  if [[ "${http_code}" != "000" ]]; then
    break
  fi
  if [[ ${i} -eq 60 ]]; then
    echo "Sidecar did not become ready after 60s; log follows:" >&2
    cat "${TMP_DIR}/sidecar.log" >&2
    exit 1
  fi
  sleep 1
done

echo "Sending embedding request..."
RESPONSE=$(curl -sf \
  -X POST "http://${SIDECAR_ADDR}/api/embeddings" \
  -H "Content-Type: application/json" \
  -d "{\"prompt\": \"the hotel materializes its guests\"}")

echo "Response: ${RESPONSE}"

# Validate: response must have an "embedding" array with at least one element.
DIM=$(echo "${RESPONSE}" | python3 -c "
import json, sys
r = json.load(sys.stdin)
assert 'embedding' in r, 'missing embedding key'
assert isinstance(r['embedding'], list), 'embedding is not a list'
assert len(r['embedding']) > 0, 'embedding is empty'
print(len(r['embedding']))
")

MODEL_GEN=$(echo "${RESPONSE}" | python3 -c "
import json, sys
r = json.load(sys.stdin)
print(r.get('model_gen', '(none)'))
")

echo "Embedding dim: ${DIM}  model_gen: ${MODEL_GEN}"
echo "Smoke test PASSED."
