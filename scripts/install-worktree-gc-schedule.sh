#!/usr/bin/env bash
# Install the launchd LaunchAgent that runs the safe worktree garbage-collector
# every 2 hours on this macOS user account (the Air / mac-jane).
#
# WHAT IT SCHEDULES
#   scripts/worktree-gc.sh --apply  — removes ONLY merged+clean+non-excluded
#   sibling worktrees (see that script's safety invariants). Because --apply can
#   never touch dirty or unmerged worktrees, an unattended run cannot destroy
#   active work; the worst case is reclaiming a freshly-created, untouched
#   worktree (pin it via PHILOTIC_WTGC_KEEP if that matters).
#
# WHY launchd (not cron): survives logout/login, integrated logging, StartInterval.
#
# Behavior:
#   - macOS only. Non-Darwin hosts print a note and exit 0.
#   - RunAtLoad is FALSE: installing does NOT trigger an immediate --apply. The
#     first run happens on the interval (or a manual `launchctl kickstart`).
#   - Idempotent: re-running rewrites the plist and re-bootstraps cleanly.
#
# Usage: scripts/install-worktree-gc-schedule.sh
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "install-worktree-gc-schedule: non-macOS host ($(uname -s)) — nothing to do."
    exit 0
fi

LABEL="com.philotic.worktree-gc"
REPO="/Users/jaredlikes/code/philotic-stack"
SCRIPT="${REPO}/scripts/worktree-gc.sh"
PLIST="${HOME}/Library/LaunchAgents/${LABEL}.plist"
LOG="${HOME}/.philotic/worktree-gc.launchd.log"
INTERVAL_SECONDS=7200   # every 2 hours

if [[ ! -x "${SCRIPT}" ]]; then
    echo "install-worktree-gc-schedule: ${SCRIPT} not found or not executable." >&2
    echo "  (The PR must be merged and present in the main checkout first.)" >&2
    exit 1
fi

# Build a PATH that definitely contains the git the job will invoke.
GIT_BIN="$(command -v git || true)"
[[ -n "${GIT_BIN}" ]] || { echo "install-worktree-gc-schedule: git not on PATH" >&2; exit 1; }
GIT_DIR="$(dirname "${GIT_BIN}")"
JOB_PATH="${GIT_DIR}:/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin:/opt/homebrew/bin"

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
		<string>${SCRIPT}</string>
		<string>--apply</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key>
		<string>${JOB_PATH}</string>
	</dict>
	<key>WorkingDirectory</key>
	<string>${REPO}</string>
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

# Reject a malformed plist now, not at the next scheduled run.
if ! plutil -lint "${PLIST}" >/dev/null; then
    echo "install-worktree-gc-schedule: WARNING — plutil rejected ${PLIST}" >&2
    exit 1
fi

DOMAIN="gui/$(id -u)"

# Re-bootstrap cleanly (idempotent). `bootout` may report "not found" the first
# time — that is fine.
launchctl bootout "${DOMAIN}/${LABEL}" 2>/dev/null || true
launchctl bootstrap "${DOMAIN}" "${PLIST}"

echo "install-worktree-gc-schedule: installed ${LABEL} (every ${INTERVAL_SECONDS}s, RunAtLoad=false)."
echo "  plist: ${PLIST}"
echo "  log:   ${LOG}"
echo "  verify: launchctl print ${DOMAIN}/${LABEL}"
