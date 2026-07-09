#!/usr/bin/env bash
# Install a newsyslog drop-in that rotates Philotic hotel logs on macOS.
#
# Why: launchd's StandardOutPath never rotates — aiua.log hit 61MB on
# 2026-07-06 and once reached 952MB on mbp-jane. This writes
# /etc/newsyslog.d/philotic.conf rotating ~/.philotic/*/aiua*.log
# (aiua.log + aiua.err.log, every profile) at 50MB, keeping 5 compressed
# archives.
#
# Behavior:
#   - Linux hosts (vps-jane): journald self-rotates — prints a note, exits 0.
#   - /etc/newsyslog.d needs root. If sudo is not available non-interactively
#     (sudo -n), prints the exact manual one-liner and exits 0 so a deploy
#     that streams this script over ssh never fails on it.
#   - Idempotent: re-running with an up-to-date conf is a no-op.
#
# Usage: install-log-rotation.sh          # install (sudo -n) or print manual steps
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "install-log-rotation: non-macOS host ($(uname -s)) — journald self-rotates; nothing to do."
  exit 0
fi

CONF_PATH="/etc/newsyslog.d/philotic.conf"
LOG_GLOB="${HOME}/.philotic/*/aiua*.log"
OWNER="$(id -un):staff"

# newsyslog.conf(5) fields: logfilename [owner:group] mode count size(KB) when flags
#   G = logfilename is a glob;  J = bzip2-compress rotated archives
#   size 51200KB = 50MB;  when '*' = rotate on size only
# No signal flag: aiua holds the log fd via launchd StandardOutPath, so writes
# follow the renamed file until the next restart — deploys and kickstarts are
# frequent enough that this converges in practice.
CONF_CONTENT="# Philotic hotel log rotation (managed by scripts/install-log-rotation.sh)
# logfilename                              [owner:group]        mode count size when  flags
${LOG_GLOB}    ${OWNER}    644  5     51200 *     GJ"

if [[ -f "${CONF_PATH}" && "$(cat "${CONF_PATH}")" == "${CONF_CONTENT}" ]]; then
  echo "install-log-rotation: ${CONF_PATH} already up to date."
  exit 0
fi

if sudo -n true 2>/dev/null; then
  printf '%s\n' "${CONF_CONTENT}" | sudo -n tee "${CONF_PATH}" >/dev/null
  echo "install-log-rotation: wrote ${CONF_PATH} on $(hostname -s)"
  # Dry-run parse so a malformed entry is caught now, not at the next hourly run.
  if ! sudo -n newsyslog -n -f "${CONF_PATH}"; then
    echo "install-log-rotation: WARNING — newsyslog dry-run rejected ${CONF_PATH}; check the entry." >&2
  fi
else
  cat <<EOF
install-log-rotation: sudo is not available non-interactively on $(hostname -s).
Log rotation NOT installed (this does not fail the deploy). To install manually, run:

  sudo tee ${CONF_PATH} >/dev/null <<'CONF'
${CONF_CONTENT}
CONF

Then verify with:  sudo newsyslog -n -f ${CONF_PATH}
EOF
fi
exit 0
