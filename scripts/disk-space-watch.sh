#!/usr/bin/env bash
# Periodic disk-space guard for a Philotic hotel host.
#
# Runs `phil doctor` and, when the `system.disk-space` check fires (WARNING or
# worse), appends a timestamped alert to ~/.philotic/<profile>/disk-space-alerts.log
# (and stderr, so a launchd StandardErrorPath captures it too).
#
# WHY: `phil doctor`'s system.disk-space check is on-demand — it only warns when
# someone runs it. This script, installed as a launchd StartInterval job
# (`just disk-watch-install`), turns it into an ACTIVE watcher so a filling disk
# is caught BEFORE ENOSPC wedges the hotel (the 2026-07-10 incident, where a
# 100%-full Air hard-blocked all work for an hour with no warning).
#
# Read-only + alert-only: it never deletes anything. The doctor check's repair is
# NeedsConfirm; remediation stays an operator decision (see the alert message).
#
# Usage: disk-space-watch.sh [profile]   (profile defaults to $PHILOTIC_PROFILE or bjork)
set -uo pipefail

PROFILE="${1:-${PHILOTIC_PROFILE:-bjork}}"
ALERT_LOG="${HOME}/.philotic/${PROFILE}/disk-space-alerts.log"
PHIL="${PHIL_BIN:-$(command -v phil 2>/dev/null || echo /opt/homebrew/bin/phil)}"

mkdir -p "$(dirname "$ALERT_LOG")" 2>/dev/null || true

# Doctor is read-only; a failure to run (e.g. binary missing) must never wedge
# the watcher — just exit quietly and try again next interval.
JSON="$("$PHIL" doctor --json --profile "$PROFILE" 2>/dev/null)" || exit 0

# The doctor JSON is passed via env, NOT stdin: `python3 - <<'PY'` already uses
# stdin for the program text, so a pipe into it would be swallowed by the heredoc.
DOCTOR_JSON="$JSON" python3 - "$ALERT_LOG" <<'PY'
import sys, os, json, datetime

alert_log = sys.argv[1]
try:
    doc = json.loads(os.environ.get("DOCTOR_JSON", ""))
except Exception:
    sys.exit(0)

findings = doc.get("findings") or []
if not isinstance(findings, list):
    sys.exit(0)

for f in findings:
    if f.get("check_id") != "system.disk-space":
        continue
    sev = str(f.get("severity", "")).lower()
    if sev not in ("warning", "error", "critical"):
        continue
    ts = datetime.datetime.now(datetime.timezone.utc).isoformat()
    line = f'{ts} [{sev}] {f.get("message", "")}\n'
    try:
        with open(alert_log, "a") as fh:
            fh.write(line)
    except Exception:
        pass
    sys.stderr.write(line)
PY
