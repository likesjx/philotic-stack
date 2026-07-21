#!/usr/bin/env bash
# Install the launchd LaunchAgent that SUPERVISES the graph-intelligence server
# on this macOS user account (KeepAlive: crashes restart, boots start it).
#
# WHY: the server ran as a bare nohup process for months — once for 12 days
# from a deleted binary — because nothing restarted, upgraded, or observed it.
#
# The service runs ~/.philotic/bin/graph-intelligence (a stable path deploys
# copy into) with the live DB and the main checkout as the scan root. Binds
# loopback by default; export PHILOTIC_GRAPH_BIND/PHILOTIC_GRAPH_TOKEN in the
# plist for tailnet exposure.
#
# Upgrade flow: cp new binary to ~/.philotic/bin/graph-intelligence, then
#   launchctl kickstart -k gui/$(id -u)/com.philotic.intel-graph
#
# Idempotent: re-running rewrites the plist and re-bootstraps. If a manual
# (nohup) server is already running on the port, it is stopped first.
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "install-intel-graph-service: non-macOS host ($(uname -s)) — nothing to do."
    exit 0
fi

LABEL="com.philotic.intel-graph"
REPO="${PHILOTIC_REPO:-$HOME/code/philotic-stack}"
BIN="$HOME/.philotic/bin/graph-intelligence"
DB="$HOME/.local/share/philotic/graph.db"
PLIST="$HOME/Library/LaunchAgents/${LABEL}.plist"
LOG="$HOME/.philotic/logs/graph.log"

if [[ ! -x "$BIN" ]]; then
    echo "install-intel-graph-service: $BIN not found." >&2
    echo "  Deploy it first: cargo build --release -p graph-intelligence && cp target/release/graph-intelligence ~/.philotic/bin/" >&2
    exit 1
fi

mkdir -p "$(dirname "$LOG")" "$(dirname "$PLIST")"

# Stop any manually-started server so the supervised one can take the port.
EXISTING=$(pgrep -f "graph-intelligence --port 8900" || true)
if [[ -n "$EXISTING" ]]; then
    echo "Stopping unsupervised graph-intelligence (pid $EXISTING)"
    kill $EXISTING || true
    sleep 2
fi

cat >"$PLIST" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>${LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>${BIN}</string>
        <string>--port</string><string>8900</string>
        <string>--mcp-port</string><string>8901</string>
        <string>--db</string><string>${DB}</string>
        <string>--worktree</string><string>${REPO}</string>
    </array>
    <key>WorkingDirectory</key><string>${REPO}</string>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>ThrottleInterval</key><integer>30</integer>
    <key>StandardOutPath</key><string>${LOG}</string>
    <key>StandardErrorPath</key><string>${LOG}</string>
</dict>
</plist>
PLIST_EOF

launchctl bootout "gui/$(id -u)/${LABEL}" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"
echo "Installed ${LABEL} (KeepAlive). Log: ${LOG}"
echo "Upgrade: cp new binary to ${BIN}; launchctl kickstart -k gui/$(id -u)/${LABEL}"
