#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT_PATH="${ROOT_DIR}/tmp/voice-samples/hotel-elevenlabs-sample.mp3"
TEXT="Hello from Philotic. This is a hotel-routed voice sample."
TARGET_ROLE="model.elevenlabs"
MODEL_ID="eleven_multilingual_v2"
OUTPUT_FORMAT="mp3_44100_128"
VOICE_ID=""
PLAY_SAMPLE=0

usage() {
  cat <<'EOF'
Usage:
  scripts/generate-hotel-voice-sample.sh [options]

Options:
  --text TEXT            Text to synthesize.
  --output PATH          Output audio file path.
  --voice-id ID          Override the default voice id from the context graph.
  --target-role ROLE     Model-controller role to target (default: model.elevenlabs).
  --model ID             ElevenLabs model id (default: eleven_multilingual_v2).
  --format FORMAT        Output format (default: mp3_44100_128).
  --play                 Play the generated file after writing it.
  --help                 Show this help text.

Requirements:
  - A Philotic hotel must already be running and reachable via PHILOTIC_HOTEL_SOCKET
    or the default /tmp/philotic-aiua.sock.
  - The model-controller guest must be materialized for the target role.
  - ElevenLabs audio passthrough must be enabled for that guest
    (PHILOTIC_MODEL_CONTROLLER_INLINE_AUDIO=1).
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --text)
      TEXT="${2:-}"
      shift 2
      ;;
    --output)
      OUTPUT_PATH="${2:-}"
      shift 2
      ;;
    --voice-id)
      VOICE_ID="${2:-}"
      shift 2
      ;;
    --target-role)
      TARGET_ROLE="${2:-}"
      shift 2
      ;;
    --model)
      MODEL_ID="${2:-}"
      shift 2
      ;;
    --format)
      OUTPUT_FORMAT="${2:-}"
      shift 2
      ;;
    --play)
      PLAY_SAMPLE=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
done

SOCKET_PATH="${PHILOTIC_HOTEL_SOCKET:-/tmp/philotic-aiua.sock}"
if [[ ! -S "${SOCKET_PATH}" ]]; then
  echo "Hotel socket not found at ${SOCKET_PATH}" >&2
  echo "Start aiua first, or set PHILOTIC_HOTEL_SOCKET to the correct socket." >&2
  exit 1
fi

echo "Building voice sample driver..."
cargo build -p philotic-client --example voice_sample_driver >/dev/null

mkdir -p "$(dirname "${OUTPUT_PATH}")"

echo "Requesting voice sample through hotel role ${TARGET_ROLE}..."
PHILOTIC_HOTEL_SOCKET="${SOCKET_PATH}" \
PHILOTIC_VOICE_SAMPLE_TEXT="${TEXT}" \
PHILOTIC_VOICE_SAMPLE_OUTPUT="${OUTPUT_PATH}" \
PHILOTIC_VOICE_SAMPLE_TARGET_ROLE="${TARGET_ROLE}" \
PHILOTIC_VOICE_SAMPLE_MODEL="${MODEL_ID}" \
PHILOTIC_VOICE_SAMPLE_FORMAT="${OUTPUT_FORMAT}" \
PHILOTIC_VOICE_SAMPLE_VOICE_ID="${VOICE_ID}" \
  cargo run -q -p philotic-client --example voice_sample_driver

if [[ "${PLAY_SAMPLE}" -eq 1 ]]; then
  if command -v afplay >/dev/null 2>&1; then
    afplay "${OUTPUT_PATH}"
  elif command -v ffplay >/dev/null 2>&1; then
    ffplay -nodisp -autoexit "${OUTPUT_PATH}"
  elif command -v mpv >/dev/null 2>&1; then
    mpv --no-video "${OUTPUT_PATH}"
  else
    echo "No supported audio player found for --play (tried afplay, ffplay, mpv)." >&2
    exit 1
  fi
fi
