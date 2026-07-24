#!/usr/bin/env bash
# Install the OPT-IN weekly launchd LaunchAgent that runs
# scripts/chaos-smoke.sh (Substrate Hardening Slice S4) against ONE
# designated hotel on this macOS account.
#
# WHY launchd (not a CronJob replicated through the mesh): CronJob
# definitions replicate mesh-wide unconditionally (see M4's
# memory.hygiene lane in MEMORY_TRANSPARENCY_PROPOSAL.md) — a chaos drill
# is exactly the kind of job that must NOT silently start firing on every
# mesh-connected hotel just because one operator opted in on one machine.
# A per-host launchd LaunchAgent is scoped to the machine it's installed
# on, which is the simplest honest mechanism for "one designated hotel."
#
# OPT-IN, twice over:
#   1. This installer is never run automatically — an operator runs it
#      explicitly on the one machine hosting the designated chaos target.
#   2. chaos-smoke.sh itself re-checks PHILOTIC_CHAOS_SMOKE_DISABLE as the
#      very first thing on every fire, so the kill switch works even after
#      the plist is installed and forgotten about.
#
# Behavior:
#   - macOS only. Non-Darwin hosts print a note and exit 0.
#   - RunAtLoad is FALSE: installing does NOT trigger an immediate chaos run.
#     The first run happens at the next scheduled fire (Sundays 03:00 local,
#     see StartCalendarInterval below), or a manual `launchctl kickstart`.
#   - Idempotent: re-running rewrites the plist and re-bootstraps cleanly.
#   - Round-robins guest-kill / config-corrupt automatically (no scenario
#     arg passed) — mesh-peer-drop (a stub) is never scheduled.
#
# Configuration: export PHILOTIC_CHAOS_* env vars (see chaos-smoke.sh's
# header) BEFORE running this installer to bake them into the plist's
# EnvironmentVariables — in particular PHILOTIC_CHAOS_HOTEL,
# PHILOTIC_CHAOS_PROFILE, and PHILOTIC_CHAOS_GUEST_ID, which almost
# certainly need to be set to match the real designated hotel/guest rather
# than left at their generic dev-default placeholders.
#
# Usage: scripts/install-chaos-smoke-schedule.sh
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "install-chaos-smoke-schedule: non-macOS host ($(uname -s)) — nothing to do."
    exit 0
fi

LABEL="com.philotic.chaos-smoke"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO}/scripts/chaos-smoke.sh"
PLIST="${HOME}/Library/LaunchAgents/${LABEL}.plist"
LOG="${HOME}/.philotic/chaos-smoke.launchd.log"

if [[ ! -x "${SCRIPT}" ]]; then
    echo "install-chaos-smoke-schedule: ${SCRIPT} not found or not executable." >&2
    exit 1
fi

PHIL_RESOLVED="$(command -v phil || echo "/opt/homebrew/bin/phil")"
JOB_PATH="$(dirname "${PHIL_RESOLVED}"):/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin:/opt/homebrew/bin"

mkdir -p "$(dirname "${LOG}")"
mkdir -p "$(dirname "${PLIST}")"

# Bake the operator's current PHILOTIC_CHAOS_* env into the plist so the
# scheduled run targets the same hotel/guest/profile the operator validated
# by hand — chaos-smoke.sh's own env-var defaults are dev placeholders, not
# safe unattended production values.
cat >"${PLIST}" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>${LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>${SCRIPT}</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key>
		<string>${JOB_PATH}</string>
		<key>PHILOTIC_CHAOS_SMOKE_DISABLE</key>
		<string>${PHILOTIC_CHAOS_SMOKE_DISABLE:-0}</string>
		<key>PHILOTIC_CHAOS_HOTEL</key>
		<string>${PHILOTIC_CHAOS_HOTEL:-default}</string>
		<key>PHILOTIC_CHAOS_PROFILE</key>
		<string>${PHILOTIC_CHAOS_PROFILE:-}</string>
		<key>PHILOTIC_CHAOS_GUEST_ID</key>
		<string>${PHILOTIC_CHAOS_GUEST_ID:-tool-runner-01}</string>
		<key>PHILOTIC_CHAOS_CONFIG_KEY</key>
		<string>${PHILOTIC_CHAOS_CONFIG_KEY:-chaos_smoke.canary_value}</string>
		<key>PHILOTIC_CHAOS_BUDGET_SECS</key>
		<string>${PHILOTIC_CHAOS_BUDGET_SECS:-120}</string>
		<key>PHILOTIC_CHAOS_HEAL_QUEUE_MAX</key>
		<string>${PHILOTIC_CHAOS_HEAL_QUEUE_MAX:-3}</string>
		<key>GRAPH_HOST</key>
		<string>${GRAPH_HOST:-http://127.0.0.1:8900}</string>
	</dict>
	<key>WorkingDirectory</key>
	<string>${REPO}</string>
	<key>StartCalendarInterval</key>
	<dict>
		<key>Weekday</key>
		<integer>0</integer>
		<key>Hour</key>
		<integer>3</integer>
		<key>Minute</key>
		<integer>0</integer>
	</dict>
	<key>RunAtLoad</key>
	<false/>
	<key>StandardOutPath</key>
	<string>${LOG}</string>
	<key>StandardErrorPath</key>
	<string>${LOG}</string>
</dict>
</plist>
PLIST_EOF

# Reject a malformed plist now, not at the next scheduled run.
if ! plutil -lint "${PLIST}" >/dev/null; then
    echo "install-chaos-smoke-schedule: WARNING — plutil rejected ${PLIST}" >&2
    exit 1
fi

DOMAIN="gui/$(id -u)"

# Re-bootstrap cleanly (idempotent). `bootout` may report "not found" the
# first time — that is fine.
launchctl bootout "${DOMAIN}/${LABEL}" 2>/dev/null || true
launchctl bootstrap "${DOMAIN}" "${PLIST}"

echo "install-chaos-smoke-schedule: installed ${LABEL} (Sundays 03:00 local, RunAtLoad=false)."
echo "  plist:  ${PLIST}"
echo "  log:    ${LOG}"
echo "  hotel:  ${PHILOTIC_CHAOS_HOTEL:-default}   guest: ${PHILOTIC_CHAOS_GUEST_ID:-tool-runner-01}"
echo "  verify: launchctl print ${DOMAIN}/${LABEL}"
echo "  disable at any time: launchctl bootout ${DOMAIN}/${LABEL}"
echo "    (or set PHILOTIC_CHAOS_SMOKE_DISABLE=1 in the plist and re-run this installer)"
