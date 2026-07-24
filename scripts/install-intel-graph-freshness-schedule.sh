#!/usr/bin/env bash
# Install the launchd LaunchAgent that refreshes intel-graph scan freshness and
# harness drift state every 6 hours on this macOS user account.
#
# WHAT IT SCHEDULES
#   scripts/intel-graph-freshness.sh — rescans the project graph (via the
#   running server when available, directly otherwise) and verifies every
#   managed harness so drift is observed continuously, not just when a Claude
#   Code session happens to start.
#
# Behavior:
#   - macOS only. Non-Darwin hosts print a note and exit 0.
#   - RunAtLoad is FALSE: installing does NOT trigger an immediate run.
#   - Idempotent: re-running rewrites the plist and re-bootstraps cleanly.
#
# Usage: scripts/install-intel-graph-freshness-schedule.sh
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "install-intel-graph-freshness-schedule: non-macOS host ($(uname -s)) — nothing to do."
    exit 0
fi

LABEL="com.philotic.intel-graph-freshness"
REPO="/Users/jaredlikes/code/philotic-stack"
SCRIPT="${REPO}/scripts/intel-graph-freshness.sh"
PLIST="${HOME}/Library/LaunchAgents/${LABEL}.plist"
LOG="${HOME}/.philotic/intel-graph-freshness.launchd.log"
INTERVAL_SECONDS=21600   # every 6 hours

if [[ ! -x "${SCRIPT}" ]]; then
    echo "install-intel-graph-freshness-schedule: ${SCRIPT} not found or not executable." >&2
    echo "  (The PR must be merged and present in the main checkout first.)" >&2
    exit 1
fi

PHIL_BIN="$(command -v phil || true)"
if [[ -z "${PHIL_BIN}" ]]; then
    echo "install-intel-graph-freshness-schedule: phil not on PATH" >&2
    exit 1
fi
PHIL_DIR="$(dirname "${PHIL_BIN}")"
JOB_PATH="${PHIL_DIR}:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin:/opt/homebrew/bin"

mkdir -p "$(dirname "${LOG}")"
mkdir -p "$(dirname "${PLIST}")"

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
        <string>${SCRIPT}</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>${JOB_PATH}</string>
        <key>PHILOTIC_REPO</key>
        <string>${REPO}</string>
    </dict>
    <key>StartInterval</key>
    <integer>${INTERVAL_SECONDS}</integer>
    <key>RunAtLoad</key>
    <false/>
    <key>StandardOutPath</key>
    <string>${LOG}</string>
    <key>StandardErrorPath</key>
    <string>${LOG}</string>
</dict>
</plist>
PLIST_EOF

launchctl bootout "gui/$(id -u)/${LABEL}" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "${PLIST}"
echo "Installed ${LABEL} (every $((INTERVAL_SECONDS / 3600))h). Log: ${LOG}"
echo "Kick off a run now with: launchctl kickstart gui/$(id -u)/${LABEL}"
