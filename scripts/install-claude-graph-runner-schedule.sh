#!/usr/bin/env bash
# Install the launchd LaunchAgent that runs the Claude Code graph-task runner
# (scripts/claude_graph_runner.py once) on a fixed interval on this Mac.
#
# Installing does NOT turn the runner on. Each run exits immediately unless the
# enable switch exists:
#     touch ~/.config/philotic/claude-graph-runner.enabled   # on
#     rm    ~/.config/philotic/claude-graph-runner.enabled   # off (kill switch)
#
# Behavior:
#   - macOS only. Non-Darwin hosts print a note and exit 0.
#   - RunAtLoad is FALSE: installing does not trigger a run.
#   - Runs from the main checkout; each task gets its own sibling worktree.
#   - Idempotent: re-running rewrites the plist and re-bootstraps cleanly.
#   - Remove with: launchctl bootout gui/$(id -u)/com.philotic.claude-graph-runner
#
# Usage: scripts/install-claude-graph-runner-schedule.sh [interval-minutes]
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "install-claude-graph-runner-schedule: non-macOS host ($(uname -s)) — nothing to do."
    exit 0
fi

LABEL="com.philotic.claude-graph-runner"
REPO="${PHILOTIC_REPO:-/Users/jaredlikes/code/philotic-stack}"
SCRIPT="${REPO}/scripts/claude_graph_runner.py"
PLIST="${HOME}/Library/LaunchAgents/${LABEL}.plist"
LOG="${HOME}/.philotic/claude-graph-runner.launchd.log"
INTERVAL_SECONDS=$(( ${1:-30} * 60 ))

if [[ ! -f "${SCRIPT}" ]]; then
    echo "install-claude-graph-runner-schedule: ${SCRIPT} not found." >&2
    echo "  (The PR must be merged and present in the main checkout first.)" >&2
    exit 1
fi

CLAUDE_BIN="$(command -v claude || true)"
if [[ -z "${CLAUDE_BIN}" ]]; then
    echo "install-claude-graph-runner-schedule: claude not on PATH" >&2
    exit 1
fi
JOB_PATH="$(dirname "${CLAUDE_BIN}"):${HOME}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"

mkdir -p "$(dirname "${LOG}")" "$(dirname "${PLIST}")"

cat >"${PLIST}" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>${LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/bin/python3</string>
        <string>${SCRIPT}</string>
        <string>once</string>
    </array>
    <key>WorkingDirectory</key>
    <string>${REPO}</string>
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
echo "Installed ${LABEL} (every $((INTERVAL_SECONDS / 60)) min). Log: ${LOG}"
echo "It does nothing until you: touch ~/.config/philotic/claude-graph-runner.enabled"
