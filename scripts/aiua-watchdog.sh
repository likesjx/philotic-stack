#!/usr/bin/env bash
# aiua hotel watchdog — re-bootstraps a launchd-managed hotel that got UNLOADED
# (booted out, e.g. a parallel deploy's stop without a completed start) and thus
# can NOT be respawned by KeepAlive. Grace-period guarded so it never fights an
# in-progress deploy's stop->install->start (which completes in <2min).
# Usage: aiua-watchdog.sh <hotel>   (via launchd StartInterval)
set -uo pipefail
HOTEL="${1:?usage: aiua-watchdog.sh <hotel>}"
GRACE="${PHILOTIC_WATCHDOG_GRACE:-300}"
uid=$(id -u)
LABEL="com.philotic.aiua.${HOTEL}"
PLIST="$HOME/Library/LaunchAgents/${LABEL}.plist"
STATE="$HOME/.philotic/aiua-watchdog.${HOTEL}.unloaded-since"
LOG="$HOME/.philotic/aiua-watchdog.${HOTEL}.log"
ts() { date -u +%FT%TZ; }
[ -f "$PLIST" ] || { echo "$(ts) no plist $PLIST — skip" >> "$LOG"; exit 0; }
if launchctl print "gui/${uid}/${LABEL}" >/dev/null 2>&1; then
  rm -f "$STATE"   # loaded — KeepAlive owns process lifecycle; healthy
  exit 0
fi
now=$(date +%s)
if [ ! -f "$STATE" ]; then
  echo "$now" > "$STATE"
  echo "$(ts) ${LABEL} UNLOADED — grace timer started (${GRACE}s)" >> "$LOG"
  exit 0
fi
since=$(cat "$STATE" 2>/dev/null || echo "$now"); age=$(( now - since ))
if [ "$age" -ge "$GRACE" ]; then
  echo "$(ts) ${LABEL} unloaded ${age}s (>=${GRACE}) — re-bootstrapping" >> "$LOG"
  launchctl bootstrap "gui/${uid}" "$PLIST" >> "$LOG" 2>&1
  sleep 5
  p=$(pgrep -f "[a]iua --hotel ${HOTEL}")
  if [ -n "$p" ]; then echo "$(ts) ${LABEL} RECOVERED (pid $p)" >> "$LOG"; rm -f "$STATE"
  else echo "$(ts) bootstrap didn't bring aiua up — retry next run" >> "$LOG"; fi
fi
exit 0
