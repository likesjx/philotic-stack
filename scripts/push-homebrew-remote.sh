#!/usr/bin/env bash
set -euo pipefail

DRY_RUN=0
if [[ "${1:-}" == "--dry-run" ]]; then
  DRY_RUN=1
  shift
fi

if [[ $# -lt 2 || $# -gt 3 ]]; then
  echo "usage: $0 [--dry-run] <ssh-host> <hotel-name> [expected-hostname]"
  echo "  --dry-run: read-only — probe the remote, print the restart plan, change nothing"
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
    ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "test -f '${path}'"
    rc=$?
    if [[ ${rc} -eq 0 ]]; then
      return 0
    fi
    if [[ ${rc} -eq 1 ]]; then
      return 1  # connection fine, file genuinely absent
    fi
    echo "⚠ ssh probe of '${path}' on ${REMOTE} failed (exit ${rc}), attempt ${attempt}/2" >&2
    sleep 3
  done
  echo "❌ Aborting: ${REMOTE} unreachable while probing '${path}'." >&2
  exit 1
}

# Find the launchd LaunchAgent label managing this hotel on the remote, whether
# currently loaded or just installed as a plist. Labels follow
# com.philotic.aiua.<hotel> (mac-jane → com.philotic.aiua.mac-jane, mbp-jane →
# com.philotic.aiua.mbp-jane) or the profile-prefixed
# com.philotic.aiua.<profile>.<hotel> written by `phil service install` — so we
# match by pattern, never a hardcoded label. Prints the label, or nothing when
# the hotel is not launchd-managed (hand-start mode).
detect_launchd_label() {
  local label
  # Prefer a currently-loaded service (launchctl list column 3 is the label).
  label="$(ssh -n "${SSH_OPTS[@]}" "${REMOTE}" \
    "launchctl list 2>/dev/null | awk '{print \$3}' | grep '^com\\.philotic\\.aiua\\.' || true" \
    | grep -E "(^|\.)${HOTEL_NAME}\$" | head -n 1 || true)"
  if [[ -n "${label}" ]]; then
    printf '%s\n' "${label}"
    return 0
  fi
  # Fall back to an installed-but-unloaded LaunchAgent plist.
  ssh -n "${SSH_OPTS[@]}" "${REMOTE}" \
    "ls \$HOME/Library/LaunchAgents/com.philotic.aiua.*.plist 2>/dev/null || true" \
    | sed -e 's#.*/##' -e 's#\.plist$##' \
    | grep -E "(^|\.)${HOTEL_NAME}\$" | head -n 1 || true
}

# Is the given launchd service currently loaded in the remote gui domain?
remote_launchd_loaded() {
  local label="$1"
  ssh -n "${SSH_OPTS[@]}" "${REMOTE}" \
    "launchctl print gui/\$(id -u)/${label} >/dev/null 2>&1"
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

# Freshness guard: refuse to build/push from a tree missing origin/develop
# commits — a stale-tree push silently reverts merged fixes on the target
# hotel (2026-07-14: PR #266/#272 reverted on mbp-jane by a #274-era push).
# Dry runs report instead of aborting; PHILOTIC_DEPLOY_ALLOW_STALE=1 overrides.
# shellcheck source=scripts/deploy-freshness-check.sh
source "${ROOT_DIR}/scripts/deploy-freshness-check.sh"
if [[ ${DRY_RUN} -eq 1 ]]; then
  assert_tree_fresh "${ROOT_DIR}" nonfatal
else
  assert_tree_fresh "${ROOT_DIR}"
fi

if [[ -n "${EXPECTED_HOSTNAME}" ]]; then
  ACTUAL_HOST="$(ssh "${SSH_OPTS[@]}" "${REMOTE}" "scutil --get LocalHostName 2>/dev/null || hostname -s" 2>/dev/null)"
  if [[ "${ACTUAL_HOST}" != "${EXPECTED_HOSTNAME}" ]]; then
    echo "❌ Aborting: remote hostname is '${ACTUAL_HOST}', expected '${EXPECTED_HOSTNAME}'."
    exit 1
  fi
fi

# Detect launchd management up front: the stop step must bootout the right
# label (KeepAlive would otherwise respawn mid-install), and the restart step
# must go back through launchd — hand-starting a launchd-managed hotel orphans
# it from supervision and forces a manual bootout/bootstrap dance on the next
# deploy (bit us 3+ times on 2026-07-06).
echo "▶ Probing launchd service for '${HOTEL_NAME}' on ${REMOTE}..."
LAUNCHD_LABEL="$(detect_launchd_label)"
if [[ -n "${LAUNCHD_LABEL}" ]]; then
  echo "  ✓ launchd-managed: ${LAUNCHD_LABEL}"
else
  echo "  – no launchd service found; will hand-start after install"
fi

if [[ ${DRY_RUN} -eq 1 ]]; then
  echo "▶ Dry run — no changes will be made."
  AIUA_CELLAR="$(ssh "${SSH_OPTS[@]}" "${REMOTE}" "ls -d /opt/homebrew/Cellar/aiua/*/bin 2>/dev/null | head -1")"
  echo "  remote cellar:      ${AIUA_CELLAR:-<not found>}"
  echo "  remote profile:     ${REMOTE_PROFILE}"
  if [[ -n "${LAUNCHD_LABEL}" ]]; then
    if remote_launchd_loaded "${LAUNCHD_LABEL}"; then
      loaded_now="loaded"
    else
      loaded_now="installed but not loaded"
    fi
    echo "  launchd service:    ${LAUNCHD_LABEL} (${loaded_now})"
    echo "  restart plan:       clear hotels.active_pid in ~/.philotic/${REMOTE_PROFILE}/context.db,"
    echo "                      then launchctl kickstart -k (or bootstrap if unloaded)"
  else
    echo "  launchd service:    none"
    echo "  restart plan:       hand-start via nohup (legacy path)"
  fi
  echo "  log rotation:       would run scripts/install-log-rotation.sh on ${REMOTE}"
  echo "  hotel watchdog:     would install scripts/aiua-watchdog.sh via install-aiua-watchdog.sh ${HOTEL_NAME} on ${REMOTE}"
  echo "✅ Dry run complete."
  exit 0
fi

echo "▶ Building release runtime binaries (local)..."
cargo build --release --bins \
  -p aiua \
  -p philote \
  -p membrane-telegram \
  -p membrane-discord \
  -p membrane-mcp-client \
  -p egress-http-runner \
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
BOOTOUT_LABEL="${LAUNCHD_LABEL:-com.philotic.aiua.${HOTEL_NAME}}"
ssh "${SSH_OPTS[@]}" "${REMOTE}" "uid=\$(id -u); launchctl bootout gui/\${uid}/${BOOTOUT_LABEL} 2>/dev/null || true; pkill -f '[a]iua --hotel ${HOTEL_NAME}' 2>/dev/null || pkill -f '[a]iua-webrtc-debug --hotel ${HOTEL_NAME}' 2>/dev/null || true; sleep 2"

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
    ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "cp '${STAGE_DIR}/${bin}' '${AIUA_CELLAR}/${bin}.new-inode' && mv -f '${AIUA_CELLAR}/${bin}.new-inode' '${AIUA_CELLAR}/${bin}'"
    ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "chmod +x '${AIUA_CELLAR}/${bin}' && xattr -d com.apple.quarantine '${AIUA_CELLAR}/${bin}' 2>/dev/null || true && codesign -s - --force '${AIUA_CELLAR}/${bin}' >/dev/null 2>&1 || true"
    ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "chmod 555 '${AIUA_CELLAR}/${bin}'"
    ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "ln -sf '${AIUA_CELLAR}/${bin}' '/opt/homebrew/bin/${bin}'"
    echo "  + ${bin} (new)"
    continue
  fi

  ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "chmod u+w '${AIUA_CELLAR}/${bin}' 2>/dev/null || true"
  ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "cp '${STAGE_DIR}/${bin}' '${AIUA_CELLAR}/${bin}.new-inode' && mv -f '${AIUA_CELLAR}/${bin}.new-inode' '${AIUA_CELLAR}/${bin}'"
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
  # New-inode install (cp to temp + mv): an in-place cp reuses the inode and the
  # kernel's cached code signature then SIGKILLs the binary at spawn
  # (OS_REASON_CODESIGNING) — `codesign -f` alone does not clear that cache.
  ssh "${SSH_OPTS[@]}" "${REMOTE}" "cp '${STAGE_DIR}/philotic-web' '${PHIL_CELLAR}/philotic-web.new-inode' && mv -f '${PHIL_CELLAR}/philotic-web.new-inode' '${PHIL_CELLAR}/philotic-web'"
  ssh "${SSH_OPTS[@]}" "${REMOTE}" "cp '${STAGE_DIR}/philotic-web' '${PHIL_CELLAR}/phil.new-inode' && mv -f '${PHIL_CELLAR}/phil.new-inode' '${PHIL_CELLAR}/phil'"
  ssh "${SSH_OPTS[@]}" "${REMOTE}" "chmod +x '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil'"
  ssh "${SSH_OPTS[@]}" "${REMOTE}" "xattr -d com.apple.quarantine '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil' 2>/dev/null || true"
  ssh "${SSH_OPTS[@]}" "${REMOTE}" "codesign -s - --force '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil' >/dev/null 2>&1 || true"
  ssh "${SSH_OPTS[@]}" "${REMOTE}" "chmod u-w '${PHIL_CELLAR}/philotic-web' '${PHIL_CELLAR}/phil' 2>/dev/null || true"
  echo "  ✓ phil / philotic-web"
fi

if [[ -n "${LAUNCHD_LABEL}" ]]; then
  echo "▶ Restarting hotel '${HOTEL_NAME}' via launchd (${LAUNCHD_LABEL})..."
  # Clear the stale active_pid row first: aiua refuses to boot when the row
  # points at a PID that still exists (or got reused), and a launchd respawn
  # can race the old row. Same profile→db derivation the rest of this script
  # uses: ~/.philotic/<profile>/context.db.
  if ! ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "sqlite3 \$HOME/.philotic/${REMOTE_PROFILE}/context.db \"UPDATE hotels SET active_pid = NULL WHERE hotel_name = '${HOTEL_NAME}';\""; then
    echo "⚠ Could not clear hotels.active_pid (continuing — aiua may refuse to start if a stale live PID matches)"
  fi
  if remote_launchd_loaded "${LAUNCHD_LABEL}"; then
    ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "launchctl kickstart -k gui/\$(id -u)/${LAUNCHD_LABEL}"
  else
    # The stop step booted the service out; bring it back under launchd
    # (RunAtLoad starts it). Never hand-start a launchd-managed hotel.
    ssh -n "${SSH_OPTS[@]}" "${REMOTE}" "launchctl bootstrap gui/\$(id -u) \$HOME/Library/LaunchAgents/${LAUNCHD_LABEL}.plist"
  fi
  echo "  ✓ ${LAUNCHD_LABEL} restarted under launchd supervision"
else
  echo "▶ No launchd service — hand-starting hotel '${HOTEL_NAME}' on ${REMOTE} with Rust cutover flags..."
  ssh "${SSH_OPTS[@]}" "${REMOTE}" "ulimit -n 65536; nohup env PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin PHILOTIC_PROFILE='${REMOTE_PROFILE}' PHILOTIC_GRAPH_DATABASE_DIR='${REMOTE_GRAPH_DIR}' PHILOTIC_LIFE_GRAPH_RUNNER_HOME_NODE='${LIFE_GRAPH_RUNNER_HOME_NODE}' PHILOTIC_REMOTE_LIFE_GRAPH_RUNNER_NODE='${REMOTE_LIFE_GRAPH_RUNNER_NODE}' PHILOTIC_ENABLE_RUST_AUTH=1 PHILOTIC_ENABLE_RUST_DISPATCHER=1 PHILOTIC_ENABLE_RUST_TASK_LIFECYCLE=1 /opt/homebrew/bin/aiua --hotel ${HOTEL_NAME} >> ~/.philotic/${REMOTE_PROFILE}/aiua.log 2>&1 & echo \$! > ~/.philotic/${REMOTE_PROFILE}/aiua.pid && echo 'aiua started pid '\$(cat ~/.philotic/${REMOTE_PROFILE}/aiua.pid)"
fi

# Loading mesh config may consult hotel-materialized services such as Muninn.
# Do it only after supervision is restored, and retry during guest startup. A
# failed config load can now fail the deploy without leaving the hotel booted
# out — the old ordering stranded mbp-jane after every dependency outage.
echo "▶ Applying mesh-config on ${REMOTE}..."
for attempt in {1..12}; do
  if ssh "${SSH_OPTS[@]}" "${REMOTE}" "env PHILOTIC_PROFILE='${REMOTE_PROFILE}' PHILOTIC_GRAPH_DATABASE_DIR='${REMOTE_GRAPH_DIR}' PHILOTIC_LIFE_GRAPH_RUNNER_HOME_NODE='${LIFE_GRAPH_RUNNER_HOME_NODE}' PHILOTIC_REMOTE_LIFE_GRAPH_RUNNER_NODE='${REMOTE_LIFE_GRAPH_RUNNER_NODE}' /opt/homebrew/bin/aiua load --file ~/mesh-config.json --hotel ${HOTEL_NAME}"; then
    break
  fi
  if [[ ${attempt} -eq 12 ]]; then
    echo "❌ Mesh-config load still failing after ${attempt} attempts; hotel remains supervised and running."
    exit 1
  fi
  echo "  waiting for hotel dependencies (${attempt}/12)..."
  sleep 5
done

echo "▶ Ensuring log rotation on ${REMOTE}..."
# Streams the installer over ssh — no repo checkout needed on the remote.
# The installer is macOS-only (Linux hotels log to journald, which
# self-rotates) and never exits non-zero over missing sudo; a transport
# failure here must not fail an otherwise-complete push.
if ! ssh "${SSH_OPTS[@]}" "${REMOTE}" 'bash -s' < "${ROOT_DIR}/scripts/install-log-rotation.sh"; then
  echo "⚠ Log-rotation install failed on ${REMOTE} (non-fatal). Run scripts/install-log-rotation.sh there manually."
fi

echo "▶ Ensuring hotel watchdog on ${REMOTE}..."
# Re-bootstraps this hotel if a future parallel deploy boots it out without a
# completed start (KeepAlive can't respawn an UNLOADED job). Unlike log rotation
# the installer needs the watchdog script too, so scp it to the stable path the
# installer reads, then stream the installer with the hotel arg. macOS-only and
# best-effort — a transport failure here must not fail an otherwise-complete push.
if ! ssh "${SSH_OPTS[@]}" "${REMOTE}" "mkdir -p '${REMOTE_HOME}/.philotic'" \
   || ! scp -q "${SSH_OPTS[@]}" "${ROOT_DIR}/scripts/aiua-watchdog.sh" "${REMOTE}:${REMOTE_HOME}/.philotic/aiua-watchdog.sh" \
   || ! ssh "${SSH_OPTS[@]}" "${REMOTE}" "bash -s ${HOTEL_NAME}" < "${ROOT_DIR}/scripts/install-aiua-watchdog.sh"; then
  echo "⚠ Watchdog install failed on ${REMOTE} (non-fatal). Run scripts/install-aiua-watchdog.sh ${HOTEL_NAME} there manually."
fi

echo "✅ ${REMOTE}:${HOTEL_NAME} updated and restarted."
