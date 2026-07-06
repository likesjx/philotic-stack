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

# Keepalives + timeouts on every ssh/scp: a mid-session WiFi flap on the target
# hung this script for 5 hours on a bare `ssh -n ... test -f` (2026-07-03).
SSH_OPTS=(-o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=4)

# Probe a remote path for existence. Distinguishes "file absent" (test exits 1)
# from ssh transport failure (exit 255 etc.) — a transport failure retries once,
# then aborts the whole script loudly instead of silently misclassifying the
# binary as new or hanging forever.
remote_file_exists() {
  local path="$1" attempt rc
  for attempt in 1 2; do
    if ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "test -f '${path}'"; then
      return 0
    fi
    rc=$?
    if [[ ${rc} -eq 1 ]]; then
      return 1  # connection fine, file genuinely absent
    fi
    echo "⚠ ssh probe of '${path}' on ${REMOTE} failed (exit ${rc}), attempt ${attempt}/2" >&2
    sleep 3
  done
  echo "❌ Aborting: ${REMOTE} unreachable while probing '${path}'." >&2
  exit 1
}

REMOTE_HOME="$(ssh "${SSH_OPTS[@]}" "${REMOTE}" 'echo $HOME')"
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
  ACTUAL_HOST="$(ssh "${SSH_OPTS[@]}" "${REMOTE}" "scutil --get LocalHostName 2>/dev/null || hostname -s" 2>/dev/null)"
  if [[ "${ACTUAL_HOST}" != "${EXPECTED_HOSTNAME}" ]]; then
    echo "❌ Aborting: remote hostname is '${ACTUAL_HOST}', expected '${EXPECTED_HOSTNAME}'."
    exit 1
  fi
fi

echo "▶ Building release runtime binaries (local)..."
cargo build --release --bins \
  -p aiua \
  -p philote \
  -p membrane-telegram \
  -p membrane-discord \
  -p model-router \
  -p tool-runner \
  -p graph-datasource \
  -p table-datasource \
  -p router-listener \
  -p heal-dispatcher \
  -p philotic-web

echo "▶ Preparing remote staging directory on ${REMOTE}..."
ssh "${SSH_OPTS[@]}" "${REMOTE}" "mkdir -p ${STAGE_DIR}"
ssh "${SSH_OPTS[@]}" "${REMOTE}" "mkdir -p ${REMOTE_GRAPH_DIR}"

AIUA_CELLAR="$(ssh "${SSH_OPTS[@]}" "${REMOTE}" "ls -d /opt/homebrew/Cellar/aiua/*/bin 2>/dev/null | head -1")"
PHIL_CELLAR="$(ssh "${SSH_OPTS[@]}" "${REMOTE}" "ls -d /opt/homebrew/Cellar/philotic-web/*/bin 2>/dev/null | head -1")"

if [[ -z "${AIUA_CELLAR}" ]]; then
  echo "❌ Could not locate remote aiua Cellar bin directory."
  exit 1
fi

echo "▶ Stopping hotel '${HOTEL_NAME}' on ${REMOTE}..."
ssh "${SSH_OPTS[@]}" "${REMOTE}" "uid=\$(id -u); launchctl bootout gui/\${uid}/com.philotic.aiua.${HOTEL_NAME} 2>/dev/null || true; pkill -f '[a]iua --hotel ${HOTEL_NAME}' 2>/dev/null || pkill -f '[a]iua-webrtc-debug --hotel ${HOTEL_NAME}' 2>/dev/null || true; sleep 2"

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

# Homebrew leaves Cellar bin dirs (and installed binaries) read-only; without
# this, copying a NEW binary in fails with Permission denied (bit the fleet on
# 2026-07-02). Per-binary chmods below re-lock existing files after install.
echo "▶ Unlocking Cellar bin dirs for writes..."
ssh "${SSH_OPTS[@]}" "${REMOTE}" "chmod -R u+w '${AIUA_CELLAR}' 2>/dev/null || true"
if [[ -n "${PHIL_CELLAR}" ]]; then
  ssh "${SSH_OPTS[@]}" "${REMOTE}" "chmod -R u+w '${PHIL_CELLAR}' 2>/dev/null || true"
fi

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

  scp -q "${SSH_OPTS[@]}" "${bin_path}" "${REMOTE}:${STAGE_DIR}/${bin}"
  ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "chmod +x '${STAGE_DIR}/${bin}'"

  if ! remote_file_exists "${AIUA_CELLAR}/${bin}"; then
    # New binary not yet in Cellar — install it and create the symlink
    ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "cp '${STAGE_DIR}/${bin}' '${AIUA_CELLAR}/${bin}'"
    ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "chmod +x '${AIUA_CELLAR}/${bin}' && xattr -d com.apple.quarantine '${AIUA_CELLAR}/${bin}' 2>/dev/null || true && codesign -s - --force '${AIUA_CELLAR}/${bin}' >/dev/null 2>&1 || true"
    ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "chmod 555 '${AIUA_CELLAR}/${bin}'"
    ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "ln -sf '${AIUA_CELLAR}/${bin}' '/opt/homebrew/bin/${bin}'"
    echo "  + ${bin} (new)"
    continue
  fi

  ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "chmod u+w '${AIUA_CELLAR}/${bin}' 2>/dev/null || true"
  ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "cp '${STAGE_DIR}/${bin}' '${AIUA_CELLAR}/${bin}'"
  ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "chmod +x '${AIUA_CELLAR}/${bin}' && xattr -d com.apple.quarantine '${AIUA_CELLAR}/${bin}' 2>/dev/null || true && codesign -s - --force '${AIUA_CELLAR}/${bin}' >/dev/null 2>&1 || true"
  ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "ln -sf '${AIUA_CELLAR}/${bin}' '/opt/homebrew/bin/${bin}'"
  ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "chmod u-w '${AIUA_CELLAR}/${bin}' 2>/dev/null || true"
  echo "  ✓ ${bin}"
done

if [[ -n "${PHIL_CELLAR}" && -f "${ROOT_DIR}/target/release/philotic-web" ]]; then
  echo "▶ Installing phil / philotic-web..."
  scp -q "${SSH_OPTS[@]}" "${ROOT_DIR}/target/release/philotic-web" "${REMOTE}:${STAGE_DIR}/philotic-web"
  ssh "${SSH_OPTS[@]}" "${REMOTE}" "chmod +x '${STAGE_DIR}/philotic-web'"
  ssh "${SSH_OPTS[@]}" "${REMOTE}" "chmod u+w '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil' 2>/dev/null || true"
  ssh "${SSH_OPTS[@]}" "${REMOTE}" "cp '${STAGE_DIR}/philotic-web' '${PHIL_CELLAR}/philotic-web'"
  ssh "${SSH_OPTS[@]}" "${REMOTE}" "cp '${STAGE_DIR}/philotic-web' '${PHIL_CELLAR}/phil'"
  ssh "${SSH_OPTS[@]}" "${REMOTE}" "chmod +x '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil'"
  ssh "${SSH_OPTS[@]}" "${REMOTE}" "xattr -d com.apple.quarantine '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil' 2>/dev/null || true"
  ssh "${SSH_OPTS[@]}" "${REMOTE}" "codesign -s - --force '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil' >/dev/null 2>&1 || true"
  ssh "${SSH_OPTS[@]}" "${REMOTE}" "chmod u-w '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil' 2>/dev/null || true"
  echo "  ✓ phil / philotic-web"
fi

echo "▶ Applying mesh-config on ${REMOTE}..."
ssh "${SSH_OPTS[@]}" "${REMOTE}" "env PHILOTIC_PROFILE='${REMOTE_PROFILE}' PHILOTIC_GRAPH_DATABASE_DIR='${REMOTE_GRAPH_DIR}' PHILOTIC_LIFE_GRAPH_RUNNER_HOME_NODE='${LIFE_GRAPH_RUNNER_HOME_NODE}' PHILOTIC_REMOTE_LIFE_GRAPH_RUNNER_NODE='${REMOTE_LIFE_GRAPH_RUNNER_NODE}' /opt/homebrew/bin/aiua load --file ~/mesh-config.json --hotel ${HOTEL_NAME}"

echo "▶ Starting hotel '${HOTEL_NAME}' on ${REMOTE} with Rust cutover flags..."
ssh "${SSH_OPTS[@]}" "${REMOTE}" "ulimit -n 65536; nohup env PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin PHILOTIC_PROFILE='${REMOTE_PROFILE}' PHILOTIC_GRAPH_DATABASE_DIR='${REMOTE_GRAPH_DIR}' PHILOTIC_LIFE_GRAPH_RUNNER_HOME_NODE='${LIFE_GRAPH_RUNNER_HOME_NODE}' PHILOTIC_REMOTE_LIFE_GRAPH_RUNNER_NODE='${REMOTE_LIFE_GRAPH_RUNNER_NODE}' PHILOTIC_ENABLE_RUST_AUTH=1 PHILOTIC_ENABLE_RUST_DISPATCHER=1 PHILOTIC_ENABLE_RUST_TASK_LIFECYCLE=1 /opt/homebrew/bin/aiua --hotel ${HOTEL_NAME} >> ~/.philotic/${REMOTE_PROFILE}/aiua.log 2>&1 & echo \$! > ~/.philotic/${REMOTE_PROFILE}/aiua.pid && echo 'aiua started pid '\$(cat ~/.philotic/${REMOTE_PROFILE}/aiua.pid)"

echo "✅ ${REMOTE}:${HOTEL_NAME} updated and restarted."
