#!/usr/bin/env bash
# Install the launchd LaunchAgent that SUPERVISES the MuninnDB daemon on this
# macOS user account (KeepAlive: crashes and exits restart, boots start it).
#
# WHY: no launchd job for the muninn DAEMON existed on either Mac — only
# com.muninn.mcp, which supervises the stdio MCP *proxy*, not the daemon. The
# daemon was hand-started, so on 2026-07-23 it died on mbp-jane and stayed down
# for three days. Every agent on that host ran memory-blind the whole time
# ("Auto recall skipped: no Muninn memory backend configured") while the heal
# circuit filed 919 critical entries and repaired nothing. See DEF-071.
#
# mac-jane only LOOKED healthy: `muninn mcp` proxies spawned by Claude desktop/
# CLI clients resurrect the daemon on demand (verified — SIGKILL with no launchd
# job produced a fresh ppid=1 instance in ~6s). The fleet's memory service was
# surviving on which desktop apps happened to be open. That is not supervision.
#
# Mirrors vps-jane's systemd muninn.service (Restart=always, RestartSec=5).
#
# SCOPE: this covers the daemon EXITING. A wedged-but-still-listening daemon
# needs a protocol probe with a restart action — FLEET_SUPERVISION_PROPOSAL.md
# slices S2/S3.
#
# Idempotent: re-running rewrites the plist and re-bootstraps.
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "install-muninn-daemon: non-macOS host ($(uname -s)) — nothing to do."
    echo "  On Linux, muninn is supervised by systemd (muninn.service)."
    exit 0
fi

LABEL="com.muninn.daemon"
BIN="${MUNINN_BIN:-/opt/homebrew/bin/muninn}"
DATA="${MUNINN_DATA:-$HOME/.muninn/data}"
PLIST="$HOME/Library/LaunchAgents/${LABEL}.plist"
LOG="$HOME/.muninn/muninn-daemon.log"
ERRLOG="$HOME/.muninn/muninn-daemon.err.log"

if [[ ! -x "$BIN" ]]; then
    echo "install-muninn-daemon: $BIN not found or not executable." >&2
    echo "  Install it first: brew install muninn (or set MUNINN_BIN)." >&2
    exit 1
fi

if [[ ! -d "$DATA" ]]; then
    echo "install-muninn-daemon: data dir $DATA does not exist." >&2
    echo "  Refusing to start against a fresh data dir — a new store invalidates" >&2
    echo "  already-issued tokens and a seq-ahead lobe can silently join a fresh" >&2
    echo "  cortex. Restore or point MUNINN_DATA at the real store first." >&2
    exit 1
fi

mkdir -p "$(dirname "$PLIST")"

cat >"$PLIST" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>${LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>${BIN}</string>
        <string>--daemon</string>
        <string>--data</string><string>${DATA}</string>
    </array>
    <key>WorkingDirectory</key><string>${HOME}</string>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>ThrottleInterval</key><integer>5</integer>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key><string>/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
        <key>HOME</key><string>${HOME}</string>
    </dict>
    <key>StandardOutPath</key><string>${LOG}</string>
    <key>StandardErrorPath</key><string>${ERRLOG}</string>
</dict>
</plist>
PLIST_EOF

# Pebble takes an EXCLUSIVE lock on the data dir. If a hand-started or
# mcp-proxy-spawned daemon still holds it, the launchd instance dies with
# "open pebble: resource temporarily unavailable" (exit 1) and retries forever
# without ever winning. Clear the field first, then bootstrap immediately — on a
# host running Claude clients an mcp proxy will respawn the daemon within ~6s and
# take the lock back.
launchctl bootout "gui/$(id -u)/${LABEL}" 2>/dev/null || true
sleep 1
if pgrep -f "muninn --daemon" >/dev/null 2>&1; then
    echo "Stopping unsupervised muninn daemon(s) to release the Pebble lock"
    pkill -9 -f "muninn --daemon" || true
    sleep 3
fi

launchctl bootstrap "gui/$(id -u)" "$PLIST"
sleep 8

if launchctl list | grep -q "${LABEL}"; then
    PID=$(pgrep -f "muninn --daemon" | head -1 || true)
    echo "Installed ${LABEL} (KeepAlive). pid=${PID:-none} data=${DATA}"
    echo "Logs: ${LOG} / ${ERRLOG}"
    echo "Verify supervision:  kill -9 \$(pgrep -f 'muninn --daemon') && sleep 15 && pgrep -f 'muninn --daemon'"
else
    echo "install-muninn-daemon: job did not load — check ${ERRLOG}" >&2
    exit 1
fi
