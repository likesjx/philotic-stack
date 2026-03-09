#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG_PATH="${ROOT_DIR}/mesh-config.json"
DEFAULT_OUTPUT="${ROOT_DIR}/tmp/voice-samples/elevenlabs-sample.mp3"
DEFAULT_TEXT="Hello from Philotic. This is a test voice sample."
DEFAULT_MODEL="eleven_multilingual_v2"
DEFAULT_FORMAT="mp3_44100_128"

usage() {
  cat <<'EOF'
Usage:
  scripts/generate-elevenlabs-sample.sh [options]

Options:
  --text TEXT            Text to synthesize.
  --text-file PATH       Read synthesis text from a file.
  --output PATH          Output audio file path.
  --voice-id ID          Override the voice ID from mesh-config.json.
  --model ID             Override the ElevenLabs model ID.
  --format FORMAT        Output format (default: mp3_44100_128).
  --config PATH          Alternate mesh-config.json path.
  --play                 Play the generated file after writing it.
  --help                 Show this help text.

The script reads:
  context_graph.elevenlabs_api_key
  context_graph.elevenlabs_voice_id
from mesh-config.json by default.
EOF
}

TEXT=""
TEXT_FILE=""
OUTPUT_PATH="${DEFAULT_OUTPUT}"
VOICE_ID=""
MODEL_ID="${DEFAULT_MODEL}"
OUTPUT_FORMAT="${DEFAULT_FORMAT}"
PLAY_SAMPLE=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --text)
      TEXT="${2:-}"
      shift 2
      ;;
    --text-file)
      TEXT_FILE="${2:-}"
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
    --model)
      MODEL_ID="${2:-}"
      shift 2
      ;;
    --format)
      OUTPUT_FORMAT="${2:-}"
      shift 2
      ;;
    --config)
      CONFIG_PATH="${2:-}"
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

if [[ -n "${TEXT_FILE}" ]]; then
  if [[ ! -f "${TEXT_FILE}" ]]; then
    echo "Text file not found: ${TEXT_FILE}" >&2
    exit 1
  fi
  TEXT="$(<"${TEXT_FILE}")"
fi

if [[ -z "${TEXT}" ]]; then
  TEXT="${DEFAULT_TEXT}"
fi

if [[ ! -f "${CONFIG_PATH}" ]]; then
  echo "Config file not found: ${CONFIG_PATH}" >&2
  exit 1
fi

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required." >&2
  exit 1
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required." >&2
  exit 1
fi

read_config() {
  local key="$1"
  python3 - "$CONFIG_PATH" "$key" <<'PY'
import json
import sys

config_path = sys.argv[1]
key = sys.argv[2]

with open(config_path, "r", encoding="utf-8") as fh:
    data = json.load(fh)

context = data.get("context_graph", data)
value = context.get(key, "")

if isinstance(value, (dict, list)):
    print(json.dumps(value))
elif value is None:
    print("")
else:
    print(str(value))
PY
}

API_KEY="$(read_config "elevenlabs_api_key")"
if [[ -z "${VOICE_ID}" ]]; then
  VOICE_ID="$(read_config "elevenlabs_voice_id")"
fi

if [[ -z "${API_KEY}" ]]; then
  echo "elevenlabs_api_key is missing in ${CONFIG_PATH}" >&2
  exit 1
fi

if [[ -z "${VOICE_ID}" ]]; then
  echo "elevenlabs_voice_id is missing in ${CONFIG_PATH} and no --voice-id override was provided." >&2
  exit 1
fi

mkdir -p "$(dirname "${OUTPUT_PATH}")"
TMP_BODY="$(mktemp)"
TMP_ERR="$(mktemp)"
trap 'rm -f "${TMP_BODY}" "${TMP_ERR}"' EXIT

python3 - "${TEXT}" "${MODEL_ID}" >"${TMP_BODY}" <<'PY'
import json
import sys

text = sys.argv[1]
model_id = sys.argv[2]

payload = {
    "text": text,
    "model_id": model_id,
}

print(json.dumps(payload))
PY

HTTP_STATUS="$(
  curl -sS \
    -o "${OUTPUT_PATH}" \
    -w "%{http_code}" \
    -X POST \
    "https://api.elevenlabs.io/v1/text-to-speech/${VOICE_ID}?output_format=${OUTPUT_FORMAT}" \
    -H "Content-Type: application/json" \
    -H "xi-api-key: ${API_KEY}" \
    --data @"${TMP_BODY}" \
    2>"${TMP_ERR}"
)"

if [[ "${HTTP_STATUS}" != 2* ]]; then
  echo "ElevenLabs request failed with HTTP ${HTTP_STATUS}." >&2
  if [[ -s "${TMP_ERR}" ]]; then
    cat "${TMP_ERR}" >&2
  fi
  if [[ -f "${OUTPUT_PATH}" ]]; then
    echo "Response body:" >&2
    cat "${OUTPUT_PATH}" >&2 || true
    rm -f "${OUTPUT_PATH}"
  fi
  exit 1
fi

echo "Wrote sample to ${OUTPUT_PATH}"

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
