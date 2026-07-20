#!/usr/bin/env bash
# Deploy the worktree's release build to the live mac-jane hotel Cellar.
# Backs up every existing binary first; restart is a separate step (printed
# at the end) so the operator controls the bounce.
set -euo pipefail

WT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Freshness guard: this script installs whatever target/release already
# holds, so both a stale HEAD and stale build artifacts can silently revert
# merged fixes (2026-07-14: PR #266/#272 reverted on mbp-jane by a stale
# push). PHILOTIC_DEPLOY_ALLOW_STALE=1 overrides the hard abort.
# shellcheck source=scripts/deploy-freshness-check.sh
source "$WT/scripts/deploy-freshness-check.sh"
assert_tree_fresh "$WT"
warn_stale_artifacts "$WT" "$WT/target/release/aiua"

CBIN=/opt/homebrew/Cellar/aiua/0.1.0-alpha/bin
CWEB=/opt/homebrew/Cellar/philotic-web/0.1.0-alpha/bin/philotic-web
BK="/opt/homebrew/Cellar/aiua/0.1.0-alpha/bin-backup-$(date +%Y%m%d-%H%M%S)"

echo "==> backing up to $BK"
mkdir -p "$BK"
cp -p "$CBIN"/* "$BK"/
cp -p "$CWEB" "$BK/philotic-web"
echo "    $(ls "$BK" | wc -l | tr -d ' ') binaries backed up"

echo "==> installing fresh binaries"
installed=0
for b in $(ls "$CBIN"); do
  if [ -f "$WT/target/release/$b" ]; then
    chmod u+w "$CBIN/$b" 2>/dev/null || true
    cp "$WT/target/release/$b" "$CBIN/$b"
    installed=$((installed+1))
  else
    echo "    (leaving legacy binary untouched: $b)"
  fi
done
chmod u+w "$CWEB" 2>/dev/null || true
cp "$WT/target/release/philotic-web" "$CWEB"
echo "    $installed guest binaries + philotic-web installed"

# Hash verification must run BEFORE the re-sign step: `codesign -f -s -`
# rewrites the signature blob in place, so a post-sign shasum of the installed
# binary never matches the built one — the old ordering false-positived
# ("aiua MISMATCH") on every deploy and aborted before printing the restart
# instructions (2026-07-19).
echo "==> hash verification (pre-sign)"
for b in aiua philote model-router model-controller-elevenlabs model-controller-gemini; do
  a=$(shasum "$WT/target/release/$b" | cut -d' ' -f1)
  c=$(shasum "$CBIN/$b" | cut -d' ' -f1)
  if [ "$a" = "$c" ]; then echo "    $b OK"; else echo "    $b MISMATCH"; exit 1; fi
done

# In-place cp over an existing executable corrupts the kernel's cached code
# signature (inode reuse) — macOS then kills the binary at spawn with
# OS_REASON_CODESIGNING, flakily. Ad-hoc re-sign everything we touched.
echo "==> re-signing (ad-hoc) to clear stale signature caches"
for b in "$CBIN"/*; do [ -f "$b" ] && codesign -f -s - "$b" 2>/dev/null; done
codesign -f -s - "$CWEB" 2>/dev/null
codesign -v "$CBIN/aiua" && echo "    signatures valid"

echo ""
echo "Install complete. Restart the hotel with:"
echo "  launchctl bootout gui/\$(id -u)/com.philotic.aiua.mac-jane"
echo "  launchctl bootstrap gui/\$(id -u) ~/Library/LaunchAgents/com.philotic.aiua.mac-jane.plist"
echo "(clean bootout+bootstrap, NOT kickstart — avoids the two-instance race)"
echo "Rollback: copy $BK/* back over $CBIN/ and restart again."
