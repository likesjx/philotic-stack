#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
  echo "usage: $0 <ssh-host> <hotel-name> [expected-hostname]"
  exit 1
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REMOTE="$1"
HOTEL_NAME="$2"
EXPECTED_HOSTNAME="${3:-}"
REMOTE_HOME="$(ssh "${REMOTE}" 'echo $HOME')"
STAGE_DIR="${PHILOTIC_REMOTE_STAGE_DIR:-${REMOTE_HOME}/philotic-stage/bin}"

cd "${ROOT_DIR}"

if [[ -n "${EXPECTED_HOSTNAME}" ]]; then
  ACTUAL_HOST="$(ssh "${REMOTE}" hostname -s 2>/dev/null)"
  if [[ "${ACTUAL_HOST}" != "${EXPECTED_HOSTNAME}" ]]; then
    echo "❌ Aborting: remote hostname is '${ACTUAL_HOST}', expected '${EXPECTED_HOSTNAME}'."
    exit 1
  fi
fi

echo "▶ Building release runtime binaries (local)..."
cargo build --release --bins \
  -p aiua \
  -p philote \
  -p membrane \
  -p membrane-telegram \
  -p model-router \
  -p tool-runner \
  -p graph-runner \
  -p graph-datasource \
  -p graph-intelligence \
  -p philotic-web

echo "▶ Preparing remote staging directory on ${REMOTE}..."
ssh "${REMOTE}" "mkdir -p ${STAGE_DIR}"

AIUA_CELLAR="$(ssh "${REMOTE}" "ls -d /opt/homebrew/Cellar/aiua/*/bin 2>/dev/null | head -1")"
PHIL_CELLAR="$(ssh "${REMOTE}" "ls -d /opt/homebrew/Cellar/philotic-web/*/bin 2>/dev/null | head -1")"

if [[ -z "${AIUA_CELLAR}" ]]; then
  echo "❌ Could not locate remote aiua Cellar bin directory."
  exit 1
fi

echo "▶ Stopping hotel '${HOTEL_NAME}' on ${REMOTE}..."
ssh "${REMOTE}" "pkill -f 'aiua --hotel ${HOTEL_NAME}' 2>/dev/null || pkill -f 'aiua-webrtc-debug --hotel ${HOTEL_NAME}' 2>/dev/null || true; sleep 2"

echo "▶ Staging and installing runtime binaries on ${REMOTE}..."
while IFS= read -r bin_path; do
  bin="$(basename "${bin_path}")"
  if [[ "${bin}" == "philotic-web" || "${bin}" == "phil" ]]; then
    continue
  fi

  scp -q "${bin_path}" "${REMOTE}:${STAGE_DIR}/${bin}"
  ssh "${REMOTE}" "chmod +x '${STAGE_DIR}/${bin}'"

  if ! ssh "${REMOTE}" "test -f '${AIUA_CELLAR}/${bin}'"; then
    echo "  – ${bin} (not in remote Cellar, skipping)"
    continue
  fi

  ssh "${REMOTE}" "chmod u+w '${AIUA_CELLAR}/${bin}' 2>/dev/null || true"
  ssh "${REMOTE}" "cp '${STAGE_DIR}/${bin}' '${AIUA_CELLAR}/${bin}'"
  ssh "${REMOTE}" "chmod +x '${AIUA_CELLAR}/${bin}' && xattr -d com.apple.quarantine '${AIUA_CELLAR}/${bin}' 2>/dev/null || true"
  ssh "${REMOTE}" "ln -sf '${AIUA_CELLAR}/${bin}' '/opt/homebrew/bin/${bin}'"
  ssh "${REMOTE}" "chmod u-w '${AIUA_CELLAR}/${bin}' 2>/dev/null || true"
  echo "  ✓ ${bin}"
done < <(find "${ROOT_DIR}/target/release" -maxdepth 1 -type f -perm -111 -print | sort)

if [[ -n "${PHIL_CELLAR}" && -f "${ROOT_DIR}/target/release/philotic-web" ]]; then
  echo "▶ Installing phil / philotic-web..."
  scp -q "${ROOT_DIR}/target/release/philotic-web" "${REMOTE}:${STAGE_DIR}/philotic-web"
  ssh "${REMOTE}" "chmod +x '${STAGE_DIR}/philotic-web'"
  ssh "${REMOTE}" "chmod u+w '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil' 2>/dev/null || true"
  ssh "${REMOTE}" "cp '${STAGE_DIR}/philotic-web' '${PHIL_CELLAR}/philotic-web'"
  ssh "${REMOTE}" "cp '${STAGE_DIR}/philotic-web' '${PHIL_CELLAR}/phil'"
  ssh "${REMOTE}" "chmod +x '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil'"
  ssh "${REMOTE}" "xattr -d com.apple.quarantine '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil' 2>/dev/null || true"
  ssh "${REMOTE}" "chmod u-w '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil' 2>/dev/null || true"
  echo "  ✓ phil / philotic-web"
fi

echo "▶ Applying mesh-config on ${REMOTE}..."
ssh "${REMOTE}" "/opt/homebrew/bin/aiua load --file ~/mesh-config.json --hotel ${HOTEL_NAME}"

echo "▶ Starting hotel '${HOTEL_NAME}' on ${REMOTE} with Rust cutover flags..."
ssh "${REMOTE}" "nohup env PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin PHILOTIC_ENABLE_RUST_AUTH=1 PHILOTIC_ENABLE_RUST_DISPATCHER=1 PHILOTIC_ENABLE_RUST_TASK_LIFECYCLE=1 /opt/homebrew/bin/aiua --hotel ${HOTEL_NAME} >> ~/.philotic/aiua.log 2>&1 & echo \$! > ~/.philotic/aiua.pid && echo 'aiua started pid '\$(cat ~/.philotic/aiua.pid)"

echo "✅ ${REMOTE}:${HOTEL_NAME} updated and restarted."
