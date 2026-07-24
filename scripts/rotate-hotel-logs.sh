#!/usr/bin/env bash
# User-level log rotation for Philotic hotel logs on macOS.
#
# WHY: the newsyslog drop-in from install-log-rotation.sh needs root
# (/etc/newsyslog.d), which is unavailable non-interactively on operator
# machines — so mac-jane's aiua.log grew unbounded (2.1GB by 2026-07-09,
# filling the disk during a release build). This script needs no sudo: the
# log files are user-owned, and launchd keeps an O_APPEND handle, so
# copytruncate (`: > log`) is safe — the writer's next append lands at the
# new end of file.
#
# Semantics mirror the newsyslog config: rotate at 50MB, keep 5 compressed
# generations. Installed as a launchd StartInterval job
# (~/Library/LaunchAgents/com.philotic.logrotate.plist, hourly).
#
# Usage: rotate-hotel-logs.sh [size_limit_mb]   (default 50)
set -uo pipefail

LIMIT_MB="${1:-50}"
LIMIT_BYTES=$((LIMIT_MB * 1024 * 1024))
KEEP=5

for log in "$HOME"/.philotic/*/aiua*.log; do
    [ -f "$log" ] || continue
    size=$(stat -f%z "$log" 2>/dev/null || echo 0)
    [ "$size" -ge "$LIMIT_BYTES" ] || continue

    # Shift older generations: .4.gz -> .5.gz (dropped), etc.
    rm -f "${log}.${KEEP}.gz"
    for ((i = KEEP - 1; i >= 1; i--)); do
        [ -f "${log}.${i}.gz" ] && mv "${log}.${i}.gz" "${log}.$((i + 1)).gz"
    done

    # Compress current content, then truncate in place (never rm/mv the live
    # file — launchd's writer keeps its handle and would keep writing to the
    # unlinked inode).
    gzip -c "$log" > "${log}.1.gz" && : > "$log"
    echo "$(date -u +%FT%TZ) rotated $log (${size} bytes)"
done
exit 0
