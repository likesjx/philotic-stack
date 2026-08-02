#!/usr/bin/env bash
# Deploy the worktree's release build to the live mac-jane hotel Cellar.
# Backs up every existing binary first; restart is a separate step (printed
# at the end) so the operator controls the bounce.
set -euo pipefail

WT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Resolve the target directory (honours CARGO_TARGET_DIR / build.target-dir,
# falls back to "$WT/target"), then refuse to run without a release build.
# Previously a missing build was not an error: every binary fell through to
# "leaving legacy binary untouched" and the script still printed its success
# banner, so deploying from a worktree that had never been built installed
# nothing while reporting success (hit 2026-07-30).
TARGET_DIR="$(cd "$WT" && cargo metadata --format-version 1 --no-deps 2>/dev/null \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
[ -n "${TARGET_DIR:-}" ] || TARGET_DIR="$WT/target"
REL="$TARGET_DIR/release"
if [ ! -x "$REL/aiua" ]; then
  echo "✗ no release build at $REL — run: cargo build --release --workspace" >&2
  exit 1
fi

# Freshness guard: this script installs whatever target/release already
# holds, so both a stale HEAD and stale build artifacts can silently revert
# merged fixes (2026-07-14: PR #266/#272 reverted on mbp-jane by a stale
# push). PHILOTIC_DEPLOY_ALLOW_STALE=1 overrides the hard abort.
# shellcheck source=scripts/deploy-freshness-check.sh
source "$WT/scripts/deploy-freshness-check.sh"
assert_tree_fresh "$WT"
warn_stale_artifacts "$WT" "$REL/aiua"

CBIN=/opt/homebrew/Cellar/aiua/0.1.0-alpha/bin
CWEB=/opt/homebrew/Cellar/philotic-web/0.1.0-alpha/bin/philotic-web
BK="/opt/homebrew/Cellar/aiua/0.1.0-alpha/bin-backup-$(date +%Y%m%d-%H%M%S)"

echo "==> backing up to $BK"
mkdir -p "$BK"
cp -p "$CBIN"/* "$BK"/
cp -p "$CWEB" "$BK/philotic-web"
echo "    $(ls "$BK" | wc -l | tr -d ' ') binaries backed up"

# Prune old backups. Each one is ~414MB; they were never pruned, so 34 had
# accumulated (~14GB) and filled the disk on 2026-07-30. The hotel did not
# crash — aiua stayed alive but stopped logging and spawned zero philote
# guests, with no error anywhere, which reads as a code bug for a long time.
KEEP="${PHILOTIC_DEPLOY_KEEP_BACKUPS:-3}"
PRUNED=0
# BSD head has no `head -n -N`, so compute the drop count explicitly.
ALL_BK="$(ls -d "$(dirname "$BK")"/bin-backup-* 2>/dev/null | sort || true)"
TOTAL_BK="$(printf '%s\n' "$ALL_BK" | sed '/^$/d' | wc -l | tr -d ' ')"
if [ "$TOTAL_BK" -gt "$KEEP" ]; then
  for old in $(printf '%s\n' "$ALL_BK" | sed '/^$/d' | head -n "$((TOTAL_BK - KEEP))"); do
    rm -rf "$old" && PRUNED=$((PRUNED+1))
  done
fi
[ "$PRUNED" -gt 0 ] && echo "    pruned $PRUNED old backup(s), keeping newest $KEEP"
AVAIL_GB=$(df -g /opt/homebrew 2>/dev/null | awk 'NR==2{print $4}')
if [ -n "${AVAIL_GB:-}" ] && [ "$AVAIL_GB" -lt 5 ]; then
  echo "⚠ only ${AVAIL_GB}GB free — a wedged hotel is the usual symptom of a full disk" >&2
fi

echo "==> installing fresh binaries"
installed=0
for b in $(ls "$CBIN"); do
  if [ -f "$REL/$b" ]; then
    chmod u+w "$CBIN/$b" 2>/dev/null || true
    cp "$REL/$b" "$CBIN/$b"
    installed=$((installed+1))
  else
    echo "    (leaving legacy binary untouched: $b)"
  fi
done
chmod u+w "$CWEB" 2>/dev/null || true
cp "$REL/philotic-web" "$CWEB"
echo "    $installed guest binaries + philotic-web installed"

# Hash verification must run BEFORE the re-sign step: `codesign -f -s -`
# rewrites the signature blob in place, so a post-sign shasum of the installed
# binary never matches the built one — the old ordering false-positived
# ("aiua MISMATCH") on every deploy and aborted before printing the restart
# instructions (2026-07-19).
echo "==> hash verification (pre-sign)"
for b in aiua philote model-router model-controller-elevenlabs model-controller-gemini; do
  a=$(shasum "$REL/$b" | cut -d' ' -f1)
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
