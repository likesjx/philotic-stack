#!/usr/bin/env bash
# Install the launchd LaunchAgent that re-bootstraps a hotel that got UNLOADED
# (booted out) on this macOS user account.
#
# WHY: macOS hotels (com.philotic.aiua.<hotel>) get externally UNLOADED when a
# parallel deploy runs its stop step (launchctl bootout) but never completes a
# start — KeepAlive can NOT respawn an unloaded job, so the hotel stays down
# until a manual `launchctl bootstrap`. This schedules scripts/aiua-watchdog.sh
# to re-bootstrap the hotel once it has been absent from launchctl longer than a
# grace period (PHILOTIC_WATCHDOG_GRACE, default 300s). The grace guard means it
# never fights an in-progress deploy's stop->install->start (which finishes in
# <2min).
#
# Behavior:
#   - macOS only. Non-Darwin hosts (vps-jane) print a note and exit 0 —
#     Linux hotels run under systemd/journald and have no launchd to watch.
#   - RunAtLoad is TRUE: the watchdog is cheap (a launchctl-print probe) and
#     needs its grace-timer state primed on install.
#   - Idempotent: re-running rewrites the plist and re-bootstraps cleanly.
#
# Usage: scripts/install-aiua-watchdog.sh [hotel]
#   hotel defaults to $PHILOTIC_HOTEL, else the sole installed
#   com.philotic.aiua.<hotel> LaunchAgent on this account.
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "install-aiua-watchdog: non-macOS host ($(uname -s)) — no launchd to watch; nothing to do."
    exit 0
fi

# When streamed over ssh (`bash -s`, deploy path) there is no source file on
# disk, so BASH_SOURCE may be unset under `set -u` — guard it. In that mode the
# watchdog script is pre-staged at DEST_SCRIPT (scp'd by the deploy) instead.
REPO="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." 2>/dev/null && pwd || true)"
SRC_SCRIPT="${REPO:+${REPO}/scripts/aiua-watchdog.sh}"

# Resolve the target hotel: explicit arg > $PHILOTIC_HOTEL > sole installed
# com.philotic.aiua.<hotel> LaunchAgent (the plist the watchdog re-bootstraps).
# The glob's literal dot after "aiua" matches com.philotic.aiua.<hotel>.plist
# but not the hyphenated com.philotic.aiua-watchdog.<hotel>.plist.
HOTEL="${1:-${PHILOTIC_HOTEL:-}}"
if [[ -z "${HOTEL}" ]]; then
    mapfile -t _hotels < <(
        ls "${HOME}/Library/LaunchAgents/com.philotic.aiua."*.plist 2>/dev/null \
        | sed -e 's#.*/com\.philotic\.aiua\.##' -e 's#\.plist$##' || true
    )
    if [[ ${#_hotels[@]} -eq 1 ]]; then
        HOTEL="${_hotels[0]}"
    elif [[ ${#_hotels[@]} -eq 0 ]]; then
        echo "install-aiua-watchdog: no hotel given and no com.philotic.aiua.<hotel>.plist found." >&2
        echo "  usage: scripts/install-aiua-watchdog.sh <hotel>" >&2
        exit 1
    else
        echo "install-aiua-watchdog: no hotel given and multiple hotels installed: ${_hotels[*]}" >&2
        echo "  usage: scripts/install-aiua-watchdog.sh <hotel>" >&2
        exit 1
    fi
fi

LABEL="com.philotic.aiua-watchdog.${HOTEL}"
DEST_SCRIPT="${HOME}/.philotic/aiua-watchdog.sh"
PLIST="${HOME}/Library/LaunchAgents/${LABEL}.plist"
LOG="${HOME}/.philotic/aiua-watchdog.${HOTEL}.launchd.log"
INTERVAL_SECONDS=120

mkdir -p "$(dirname "${DEST_SCRIPT}")"
mkdir -p "$(dirname "${PLIST}")"

# Copy the watchdog to a stable, checkout-independent path so the launchd job
# never depends on a worktree that a parallel session might prune. When the
# deploy path streams this installer over ssh, the repo copy is absent but the
# script was already scp'd to DEST_SCRIPT — accept that as-is.
if [[ -n "${SRC_SCRIPT}" && -f "${SRC_SCRIPT}" ]]; then
    cp "${SRC_SCRIPT}" "${DEST_SCRIPT}"
    chmod +x "${DEST_SCRIPT}"
elif [[ -f "${DEST_SCRIPT}" ]]; then
    chmod +x "${DEST_SCRIPT}"   # pre-staged (e.g. scp'd by scripts/push-homebrew-remote.sh)
else
    echo "install-aiua-watchdog: watchdog script not found (${SRC_SCRIPT:-<none>} or ${DEST_SCRIPT})." >&2
    exit 1
fi

cat >"${PLIST}" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>${LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>/bin/bash</string>
		<string>${DEST_SCRIPT}</string>
		<string>${HOTEL}</string>
	</array>
	<key>StartInterval</key>
	<integer>${INTERVAL_SECONDS}</integer>
	<key>RunAtLoad</key>
	<true/>
	<key>StandardOutPath</key>
	<string>${LOG}</string>
	<key>StandardErrorPath</key>
	<string>${LOG}</string>
</dict>
</plist>
PLIST_EOF

# Reject a malformed plist now, not at the next scheduled run.
if ! plutil -lint "${PLIST}" >/dev/null; then
    echo "install-aiua-watchdog: WARNING — plutil rejected ${PLIST}" >&2
    exit 1
fi

DOMAIN="gui/$(id -u)"

# Re-bootstrap cleanly (idempotent). `bootout` may report "not found" the first
# time — that is fine.
launchctl bootout "${DOMAIN}/${LABEL}" 2>/dev/null || true
launchctl bootstrap "${DOMAIN}" "${PLIST}"

echo "install-aiua-watchdog: installed ${LABEL} (every ${INTERVAL_SECONDS}s, RunAtLoad=true)."
echo "  hotel:  ${HOTEL}"
echo "  script: ${DEST_SCRIPT}"
echo "  plist:  ${PLIST}"
echo "  log:    ${LOG}"
echo "  verify: launchctl print ${DOMAIN}/${LABEL}"
