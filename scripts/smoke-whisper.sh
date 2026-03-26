#!/usr/bin/env bash
# smoke-whisper.sh — offline unit smoke for WhisperBackend Slice 2.
#
# Does NOT require a running hotel or network access.
# Downloads onnx-community/whisper-small via HF Hub cache (network required on
# first run; cached thereafter), runs a mel spectrogram unit test, and confirms
# the binary starts up with --sidecar-only.
#
# Usage:
#   ./scripts/smoke-whisper.sh
#
# Environment:
#   PHILOTIC_ONNX_WHISPER_REPO  — override Whisper repo (default: onnx-community/whisper-small)
#   PHILOTIC_ONNX_PREFER_QUANTIZED — "true" (default) or "false"

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WHISPER_REPO="${PHILOTIC_ONNX_WHISPER_REPO:-onnx-community/whisper-small}"
PREFER_Q="${PHILOTIC_ONNX_PREFER_QUANTIZED:-true}"

log() { echo "[smoke-whisper] $*"; }
fail() { echo "[smoke-whisper] FAIL: $*" >&2; exit 1; }

cd "${ROOT_DIR}"

# ── Step 1: unit tests (includes mel spectrogram shape + filterbank) ──────────
log "running onnx-runner unit tests …"
cargo test -p onnx-runner --quiet 2>&1 | tail -5
log "unit tests: PASS"

# ── Step 2: build the model-controller-onnx binary ───────────────────────────
log "building model-controller-onnx …"
cargo build -p model-router --bin model-controller-onnx --quiet
BINARY="${ROOT_DIR}/target/debug/model-controller-onnx"
[[ -f "${BINARY}" ]] || fail "binary not found at ${BINARY}"
log "build: PASS"

# ── Step 3: dry-run sidecar-only startup ─────────────────────────────────────
# We use --sidecar-only to skip hotel IPC registration and just verify the
# provider loads + the HTTP server binds.  We kill it after a short delay.
SIDECAR_ADDR="127.0.0.1:11437"
log "starting sidecar-only (${SIDECAR_ADDR}) with whisper-repo=${WHISPER_REPO} …"
log "(first run will download models from HF Hub — may take several minutes)"

"${BINARY}" \
  --sidecar-only \
  --sidecar-addr "${SIDECAR_ADDR}" \
  --whisper-repo "${WHISPER_REPO}" \
  --prefer-quantized "${PREFER_Q}" \
  &
SIDECAR_PID=$!

cleanup() {
  kill "${SIDECAR_PID}" 2>/dev/null || true
  wait "${SIDECAR_PID}" 2>/dev/null || true
}
trap cleanup EXIT

# Wait up to 120 s for the sidecar to respond (model download can be slow).
log "waiting for sidecar to become ready …"
for i in $(seq 1 120); do
  if curl -sf "http://${SIDECAR_ADDR}/api/health" >/dev/null 2>&1; then
    log "sidecar health check: PASS (${i}s)"
    break
  fi
  sleep 1
  if ! kill -0 "${SIDECAR_PID}" 2>/dev/null; then
    fail "sidecar process exited before becoming ready"
  fi
  if [[ "${i}" -eq 120 ]]; then
    fail "sidecar did not become ready within 120 s"
  fi
done

# ── Step 4: probe the sidecar /api/transcribe endpoint ───────────────────────
# We POST a minimal 16 kHz mono WAV (1 s of silence) and expect a 200 with
# an empty or near-empty transcript — not an error.
log "generating 1 s silence WAV for transcription probe …"
TMP_WAV="$(mktemp /tmp/smoke-whisper-XXXXXX.wav)"

python3 - <<PYEOF
import wave, struct, math
with wave.open("${TMP_WAV}", "wb") as wf:
    wf.setnchannels(1)
    wf.setsampwidth(2)
    wf.setframerate(16000)
    silence = struct.pack("<" + "h" * 16000, *([0] * 16000))
    wf.writeframes(silence)
print("WAV written")
PYEOF

log "POSTing silence WAV to /api/transcribe …"
RESPONSE=$(curl -sf \
  -X POST "http://${SIDECAR_ADDR}/api/transcribe" \
  -H "Content-Type: audio/wav" \
  --data-binary "@${TMP_WAV}" \
  2>&1) || fail "POST /api/transcribe failed: ${RESPONSE}"

log "transcribe response: ${RESPONSE}"
log "transcribe endpoint: PASS"

rm -f "${TMP_WAV}"

log "smoke-whisper: ALL CHECKS PASSED"
