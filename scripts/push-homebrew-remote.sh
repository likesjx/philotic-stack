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
if [[ -n "${PHILOTIC_REMOTE_PROFILE:-}" ]]; then
  REMOTE_PROFILE="${PHILOTIC_REMOTE_PROFILE}"
elif [[ "${HOTEL_NAME}" == "mbp-jane" || "${HOTEL_NAME}" == "mac-jane" ]]; then
  REMOTE_PROFILE="jane"
elif [[ "${HOTEL_NAME}" == "local-telegram" || "${HOTEL_NAME}" == "bjork" ]]; then
  REMOTE_PROFILE="bjork"
else
  REMOTE_PROFILE="${HOTEL_NAME}"
fi
STAGE_DIR="${PHILOTIC_REMOTE_STAGE_DIR:-${REMOTE_HOME}/philotic-stage/bin}"
REMOTE_GRAPH_DIR="${REMOTE_HOME}/.philotic/${REMOTE_PROFILE}/graphs"
LIFE_GRAPH_RUNNER_HOME_NODE="${PHILOTIC_LIFE_GRAPH_RUNNER_HOME_NODE:-vps-jane-aiua-01}"
REMOTE_LIFE_GRAPH_RUNNER_NODE="${PHILOTIC_REMOTE_LIFE_GRAPH_RUNNER_NODE:-${LIFE_GRAPH_RUNNER_HOME_NODE}}"

cd "${ROOT_DIR}"

if [[ -n "${EXPECTED_HOSTNAME}" ]]; then
  ACTUAL_HOST="$(ssh "${REMOTE}" "scutil --get LocalHostName 2>/dev/null || hostname -s" 2>/dev/null)"
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
  -p table-datasource \
  -p router-listener \
  -p heal-dispatcher \
  -p philotic-web

echo "▶ Preparing remote staging directory on ${REMOTE}..."
ssh "${REMOTE}" "mkdir -p ${STAGE_DIR}"
ssh "${REMOTE}" "mkdir -p ${REMOTE_GRAPH_DIR}"

AIUA_CELLAR="$(ssh "${REMOTE}" "ls -d /opt/homebrew/Cellar/aiua/*/bin 2>/dev/null | head -1")"
PHIL_CELLAR="$(ssh "${REMOTE}" "ls -d /opt/homebrew/Cellar/philotic-web/*/bin 2>/dev/null | head -1")"

if [[ -z "${AIUA_CELLAR}" ]]; then
  echo "❌ Could not locate remote aiua Cellar bin directory."
  exit 1
fi

echo "▶ Stopping hotel '${HOTEL_NAME}' on ${REMOTE}..."
ssh "${REMOTE}" "uid=\$(id -u); launchctl bootout gui/\${uid}/com.philotic.aiua.${HOTEL_NAME} 2>/dev/null || true; pkill -f '[a]iua --hotel ${HOTEL_NAME}' 2>/dev/null || pkill -f '[a]iua-webrtc-debug --hotel ${HOTEL_NAME}' 2>/dev/null || true; sleep 2"

echo "▶ Signing and verifying local binaries before push..."
UNSIGNED=()
while IFS= read -r bin_path; do
  bin="$(basename "${bin_path}")"
  if [[ "${bin}" == "philotic-web" || "${bin}" == "phil" || "${bin}" == "graph-intelligence" ]]; then
    continue
  fi
  codesign -s - --force "${bin_path}" 2>/dev/null || true
  sig_info=$(codesign -dv "${bin_path}" 2>&1 || true)
  if ! grep -q "adhoc" <<<"${sig_info}"; then
    UNSIGNED+=("${bin}")
  fi
done < <(find "${ROOT_DIR}/target/release" -maxdepth 1 -type f -perm -111 -print | sort)

if [[ ${#UNSIGNED[@]} -gt 0 ]]; then
  echo "❌ Binaries that could not be signed (macOS will SIGKILL these on remote):"
  for b in "${UNSIGNED[@]}"; do echo "   - ${b}"; done
  exit 1
fi
echo "  ✓ All local binaries have adhoc signatures"

echo "▶ Staging and installing runtime binaries on ${REMOTE}..."
# Collect paths into array first — SSH commands inside a while-read loop would otherwise
# consume stdin from the pipe, causing all but the first binary to be silently skipped.
BIN_PATHS=()
while IFS= read -r _bp; do BIN_PATHS+=("${_bp}"); done \
  < <(find "${ROOT_DIR}/target/release" -maxdepth 1 -type f -perm -111 -print | sort)
for bin_path in "${BIN_PATHS[@]}"; do
  bin="$(basename "${bin_path}")"
  if [[ "${bin}" == "philotic-web" || "${bin}" == "phil" || "${bin}" == "graph-intelligence" ]]; then
    continue
  fi

  scp -q "${bin_path}" "${REMOTE}:${STAGE_DIR}/${bin}"
  ssh -n "${REMOTE}" "chmod +x '${STAGE_DIR}/${bin}'"

  if ! ssh -n "${REMOTE}" "test -f '${AIUA_CELLAR}/${bin}'"; then
    # New binary not yet in Cellar — install it and create the symlink
    ssh -n "${REMOTE}" "cp '${STAGE_DIR}/${bin}' '${AIUA_CELLAR}/${bin}'"
    ssh -n "${REMOTE}" "chmod +x '${AIUA_CELLAR}/${bin}' && xattr -d com.apple.quarantine '${AIUA_CELLAR}/${bin}' 2>/dev/null || true && codesign -s - --force '${AIUA_CELLAR}/${bin}' >/dev/null 2>&1 || true"
    ssh -n "${REMOTE}" "chmod 555 '${AIUA_CELLAR}/${bin}'"
    ssh -n "${REMOTE}" "ln -sf '${AIUA_CELLAR}/${bin}' '/opt/homebrew/bin/${bin}'"
    echo "  + ${bin} (new)"
    continue
  fi

  ssh -n "${REMOTE}" "chmod u+w '${AIUA_CELLAR}/${bin}' 2>/dev/null || true"
  ssh -n "${REMOTE}" "cp '${STAGE_DIR}/${bin}' '${AIUA_CELLAR}/${bin}'"
  ssh -n "${REMOTE}" "chmod +x '${AIUA_CELLAR}/${bin}' && xattr -d com.apple.quarantine '${AIUA_CELLAR}/${bin}' 2>/dev/null || true && codesign -s - --force '${AIUA_CELLAR}/${bin}' >/dev/null 2>&1 || true"
  ssh -n "${REMOTE}" "ln -sf '${AIUA_CELLAR}/${bin}' '/opt/homebrew/bin/${bin}'"
  ssh -n "${REMOTE}" "chmod u-w '${AIUA_CELLAR}/${bin}' 2>/dev/null || true"
  echo "  ✓ ${bin}"
done

if [[ -n "${PHIL_CELLAR}" && -f "${ROOT_DIR}/target/release/philotic-web" ]]; then
  echo "▶ Installing phil / philotic-web..."
  scp -q "${ROOT_DIR}/target/release/philotic-web" "${REMOTE}:${STAGE_DIR}/philotic-web"
  ssh "${REMOTE}" "chmod +x '${STAGE_DIR}/philotic-web'"
  ssh "${REMOTE}" "chmod u+w '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil' 2>/dev/null || true"
  ssh "${REMOTE}" "cp '${STAGE_DIR}/philotic-web' '${PHIL_CELLAR}/philotic-web'"
  ssh "${REMOTE}" "cp '${STAGE_DIR}/philotic-web' '${PHIL_CELLAR}/phil'"
  ssh "${REMOTE}" "chmod +x '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil'"
  ssh "${REMOTE}" "xattr -d com.apple.quarantine '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil' 2>/dev/null || true"
  ssh "${REMOTE}" "codesign -s - --force '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil' >/dev/null 2>&1 || true"
  ssh "${REMOTE}" "chmod u-w '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil' 2>/dev/null || true"
  echo "  ✓ phil / philotic-web"
fi

echo "▶ Applying mesh-config on ${REMOTE}..."
ssh "${REMOTE}" "env PHILOTIC_PROFILE='${REMOTE_PROFILE}' PHILOTIC_GRAPH_DATABASE_DIR='${REMOTE_GRAPH_DIR}' PHILOTIC_LIFE_GRAPH_RUNNER_HOME_NODE='${LIFE_GRAPH_RUNNER_HOME_NODE}' PHILOTIC_REMOTE_LIFE_GRAPH_RUNNER_NODE='${REMOTE_LIFE_GRAPH_RUNNER_NODE}' /opt/homebrew/bin/aiua load --file ~/mesh-config.json --hotel ${HOTEL_NAME}"

echo "▶ Starting hotel '${HOTEL_NAME}' on ${REMOTE} with Rust cutover flags..."
ssh "${REMOTE}" "ulimit -n 65536; nohup env PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin PHILOTIC_PROFILE='${REMOTE_PROFILE}' PHILOTIC_GRAPH_DATABASE_DIR='${REMOTE_GRAPH_DIR}' PHILOTIC_LIFE_GRAPH_RUNNER_HOME_NODE='${LIFE_GRAPH_RUNNER_HOME_NODE}' PHILOTIC_REMOTE_LIFE_GRAPH_RUNNER_NODE='${REMOTE_LIFE_GRAPH_RUNNER_NODE}' PHILOTIC_ENABLE_RUST_AUTH=1 PHILOTIC_ENABLE_RUST_DISPATCHER=1 PHILOTIC_ENABLE_RUST_TASK_LIFECYCLE=1 /opt/homebrew/bin/aiua --hotel ${HOTEL_NAME} >> ~/.philotic/${REMOTE_PROFILE}/aiua.log 2>&1 & echo \$! > ~/.philotic/${REMOTE_PROFILE}/aiua.pid && echo 'aiua started pid '\$(cat ~/.philotic/${REMOTE_PROFILE}/aiua.pid)"

echo "✅ ${REMOTE}:${HOTEL_NAME} updated and restarted."
