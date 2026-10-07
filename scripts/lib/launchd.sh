# shellcheck shell=bash
# Shared launchd helpers for macOS hotels (mac-jane, mbp-jane).
#
# Sourced by scripts/push-homebrew-remote.sh and scripts/install-release-mac.sh
# so the stop/restart dance lives in exactly one place. History behind it:
#   - KeepAlive=true respawns a bare `pkill`ed aiua, so stop = `bootout`.
#   - Hand-starting a launchd-managed hotel orphans it from supervision and
#     races launchd's copy over the IPC socket, so start = kickstart -k when
#     loaded, else bootstrap the plist (RunAtLoad starts it). Never nohup.
#
# Inputs (set by the caller before calling any function):
#   LAUNCHD_TARGET  ssh destination, or "local"/empty to run on this machine
#   SSH_OPTS        ssh option array (a keepalive default is provided)

if [[ -z "${SSH_OPTS+x}" ]]; then
  SSH_OPTS=(-o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=4)
fi

# Run one shell command string on the target (locally via bash -c, or over ssh).
launchd_exec() {
  local cmd="$1"
  if [[ -z "${LAUNCHD_TARGET:-}" || "${LAUNCHD_TARGET}" == "local" ]]; then
    bash -c "${cmd}" </dev/null
  else
    ssh -n "${SSH_OPTS[@]}" "${LAUNCHD_TARGET}" "${cmd}"
  fi
}

# Find the LaunchAgent label managing <hotel>, whether loaded or only installed
# as a plist. Labels are com.philotic.aiua.<hotel> or the profile-prefixed
# com.philotic.aiua.<profile>.<hotel> written by `phil service install`, so we
# match by pattern, never a hardcoded label. Prints the label, or nothing when
# the hotel is not launchd-managed (hand-start mode).
launchd_detect_label() {
  local hotel="$1" label
  # Prefer a currently-loaded service (launchctl list column 3 is the label).
  label="$(launchd_exec \
    "launchctl list 2>/dev/null | awk '{print \$3}' | grep '^com\\.philotic\\.aiua\\.' || true" \
    | grep -E "(^|\.)${hotel}\$" | head -n 1 || true)"
  if [[ -n "${label}" ]]; then
    printf '%s\n' "${label}"
    return 0
  fi
  # Fall back to an installed-but-unloaded LaunchAgent plist.
  launchd_exec \
    "ls \$HOME/Library/LaunchAgents/com.philotic.aiua.*.plist 2>/dev/null || true" \
    | sed -e 's#.*/##' -e 's#\.plist$##' \
    | grep -E "(^|\.)${hotel}\$" | head -n 1 || true
}

# Is <label> currently loaded in the target's gui domain?
launchd_loaded() {
  local label="$1"
  launchd_exec "launchctl print gui/\$(id -u)/${label} >/dev/null 2>&1"
}

# Stop <hotel>: bootout <label> (keeps it stopped despite KeepAlive), then
# pkill any hand-started copy. Never fails.
launchd_stop_hotel() {
  local label="$1" hotel="$2"
  launchd_exec "uid=\$(id -u); launchctl bootout gui/\${uid}/${label} 2>/dev/null || true; pkill -f '[a]iua --hotel ${hotel}' 2>/dev/null || pkill -f '[a]iua-webrtc-debug --hotel ${hotel}' 2>/dev/null || true; sleep 2"
}

# Start <label> under launchd: kickstart -k when loaded, otherwise bootstrap
# the plist (the stop step booted it out; RunAtLoad starts it).
launchd_start() {
  local label="$1"
  if launchd_loaded "${label}"; then
    launchd_exec "launchctl kickstart -k gui/\$(id -u)/${label}"
  else
    launchd_exec "launchctl bootstrap gui/\$(id -u) \$HOME/Library/LaunchAgents/${label}.plist"
  fi
}

# Clear hotels.active_pid for <hotel> in ~/.philotic/<profile>/context.db.
# aiua refuses to boot when the row points at a PID that still exists (or got
# reused), and a launchd respawn can race the old row. Returns sqlite's status.
hotel_clear_active_pid() {
  local profile="$1" hotel="$2"
  launchd_exec "sqlite3 \$HOME/.philotic/${profile}/context.db \"UPDATE hotels SET active_pid = NULL WHERE hotel_name = '${hotel}';\""
}

# Profile for a macOS hotel name (PHILOTIC_REMOTE_PROFILE overrides).
hotel_profile_for() {
  local hotel="$1"
  if [[ -n "${PHILOTIC_REMOTE_PROFILE:-}" ]]; then
    printf '%s\n' "${PHILOTIC_REMOTE_PROFILE}"
  elif [[ "${hotel}" == "mbp-jane" || "${hotel}" == "mac-jane" ]]; then
    printf 'jane\n'
  elif [[ "${hotel}" == "local-telegram" || "${hotel}" == "bjork" ]]; then
    printf 'bjork\n'
  else
    printf '%s\n' "${hotel}"
  fi
}
