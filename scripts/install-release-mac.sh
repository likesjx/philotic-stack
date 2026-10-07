#!/usr/bin/env bash
# Install a Philotic GitHub Release on a macOS hotel (proposal:release-train R5/R6).
#
#   scripts/install-release-mac.sh <host|local> <tag> [--hotel NAME] [--keep N] [--no-restart]
#   scripts/install-release-mac.sh <host|local> --rollback [<tag>] [--hotel NAME]
#
#   <host>   ssh destination of the Mac (e.g. mbp-jane), or `local` for this Mac.
#   --hotel  hotel name for launchd label + profile (default: mac-jane for
#            `local`, otherwise <host>).
#
# Install:
#   1. resolve the darwin-arm64 tarball + outer SHA256SUMS of the release (gh api)
#   2. on the Mac: download into ~/.philotic/release-cache/<tag>/, verify the
#      tarball against SHA256SUMS, strip quarantine, unpack into a fresh dir
#      (new inodes — an in-place overwrite poisons the kernel's per-inode code
#      signature cache), verify the inner SHA256SUMS, `codesign -f -s -` every
#      binary, record post-sign hashes in INSTALLED_SHA256SUMS, move into
#      ~/.philotic/releases/<tag>/ — all while the hotel keeps serving
#   3. stop the hotel through launchd (bootout), flip ~/.philotic/current
#      atomically, clear the stale active_pid, start it back under launchd
#   4. keep the newest N (default 3) releases; never prune `current`
#
# Rollback: flip `current` to <tag>, or to the newest installed release that is
# not current, and restart the same way.
#
# This script never edits launchd plists. When the plist does not run from
# ~/.philotic/current/bin it prints the PlistBuddy commands to switch it, and
# skips the restart (a restart would not change which binaries run).
# See docs/process/RELEASE.md.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="${PHILOTIC_GH_REPO:-likesjx/philotic-stack}"
PLATFORM="darwin-arm64"
SSH_OPTS=(-o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=4)

usage() {
  sed -n '4,5p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

[[ $# -ge 2 ]] || usage
TARGET="$1"
shift
TAG=""
ROLLBACK=0
HOTEL=""
KEEP="${PHILOTIC_RELEASES_KEEP:-3}"
RESTART=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --rollback) ROLLBACK=1; shift ;;
    --hotel) [[ $# -ge 2 ]] || usage; HOTEL="$2"; shift 2 ;;
    --keep) [[ $# -ge 2 ]] || usage; KEEP="$2"; shift 2 ;;
    --no-restart) RESTART=0; shift ;;
    -h|--help) usage ;;
    -*) echo "unknown option: $1" >&2; usage ;;
    *) [[ -z "${TAG}" ]] || usage; TAG="$1"; shift ;;
  esac
done
if [[ ${ROLLBACK} -eq 0 && -z "${TAG}" ]]; then usage; fi
if [[ -n "${TAG}" ]] && ! [[ "${TAG}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]]; then
  echo "✗ '${TAG}' is not a release tag (need vX.Y.Z or vX.Y.Z-rc.N — three parts)" >&2
  exit 2
fi
[[ "${KEEP}" =~ ^[1-9][0-9]*$ ]] || { echo "✗ --keep must be a positive integer" >&2; exit 2; }
if [[ -z "${HOTEL}" ]]; then
  if [[ "${TARGET}" == "local" ]]; then HOTEL="mac-jane"; else HOTEL="${TARGET}"; fi
fi

# shellcheck source=scripts/lib/launchd.sh
source "${ROOT_DIR}/scripts/lib/launchd.sh"
LAUNCHD_TARGET="${TARGET}"
PROFILE="$(hotel_profile_for "${HOTEL}")"

# Run a multi-line script on the target with positional args.
on_target() {
  local script="$1"
  shift
  if [[ "${TARGET}" == "local" ]]; then
    bash -s -- "$@" <<<"${script}"
  else
    # shellcheck disable=SC2029  # args are %q-quoted for the remote shell on purpose
    ssh "${SSH_OPTS[@]}" "${TARGET}" "bash -s -- $(printf '%q ' "$@")" <<<"${script}"
  fi
}

# ── Target-side scripts ──────────────────────────────────────────────────────

read -r -d '' STAGE_SCRIPT <<'STAGE' || true
set -euo pipefail
tag="$1"; asset="$2"; tarball_url="$3"; sums_url="$4"
rel="$HOME/.philotic/releases"; cache="$HOME/.philotic/release-cache/$tag"
dest="$rel/$tag"; partial="$rel/.$tag.partial"
mkdir -p "$rel" "$cache"

if [ -f "$dest/INSTALLED_SHA256SUMS" ] && [ -x "$dest/bin/aiua" ] \
   && (cd "$dest" && shasum -a 256 -c INSTALLED_SHA256SUMS >/dev/null 2>&1); then
  echo "  ✓ $tag already installed and intact at $dest"
  exit 0
fi

cd "$cache"
curl -fsSL --connect-timeout 15 --retry 3 -o SHA256SUMS "$sums_url"
curl -fsSL --connect-timeout 15 --retry 3 -o "$asset.part" "$tarball_url"
mv -f "$asset.part" "$asset"
line="$(grep "  $asset\$" SHA256SUMS || true)"
if [ -z "$line" ]; then echo "  ✗ $asset is not listed in the release SHA256SUMS" >&2; exit 1; fi
if ! printf '%s\n' "$line" | shasum -a 256 -c - >/dev/null; then
  echo "  ✗ $asset does not match the release SHA256SUMS" >&2; exit 1
fi
echo "  ✓ tarball verified against release SHA256SUMS"
xattr -d com.apple.quarantine "$asset" 2>/dev/null || true

rm -rf "$partial"
mkdir -p "$partial"
tar -xzf "$asset" -C "$partial"
if ! (cd "$partial" && shasum -a 256 -c SHA256SUMS >/dev/null); then
  echo "  ✗ unpacked binaries do not match the tarball's SHA256SUMS" >&2; exit 1
fi
echo "  ✓ $(find "$partial/bin" -type f | wc -l | tr -d ' ') binaries verified against manifest (pre-sign)"
xattr -dr com.apple.quarantine "$partial" 2>/dev/null || true

# Fresh files from tar = new inodes. Re-sign ad hoc to clear any stale
# signature state, then record what is actually on disk: re-signing can change
# a binary's sha256, so verify-release accepts these as RESIGNED.
unsigned=0
for f in "$partial"/bin/*; do
  if ! codesign -f -s - "$f" >/dev/null 2>&1; then echo "  ⚠ codesign failed: $(basename "$f")" >&2; unsigned=1; fi
done
[ "$unsigned" -eq 0 ] || { echo "  ✗ some binaries could not be signed (macOS would SIGKILL them)" >&2; exit 1; }
(cd "$partial" && shasum -a 256 bin/* > INSTALLED_SHA256SUMS)
date -u +%Y-%m-%dT%H:%M:%SZ > "$partial/INSTALLED_AT"

if [ -e "$dest" ]; then
  rm -rf "$rel/.$tag.old"
  mv "$dest" "$rel/.$tag.old"
fi
mv "$partial" "$dest"
rm -rf "$rel/.$tag.old"
echo "  ✓ staged $dest"
STAGE

# Resolve the rollback target: $1 = wanted tag or empty for "previous".
read -r -d '' PICK_SCRIPT <<'PICK' || true
set -euo pipefail
want="$1"
rel="$HOME/.philotic/releases"; link="$HOME/.philotic/current"
[ -L "$link" ] || { echo "$link does not exist — no release installed" >&2; exit 1; }
current="$(basename "$(readlink "$link")")"
if [ -z "$want" ]; then
  # shellcheck disable=SC2012  # release names are validated vX.Y.Z tags; ls -t gives install order
  want="$(ls -1td -- "$rel"/*/ 2>/dev/null | sed -e 's#/$##' -e 's#.*/##' | grep -vxF -- "$current" | head -n 1 || true)"
fi
[ -n "$want" ] || { echo "no other installed release to roll back to (current: $current)" >&2; exit 1; }
[ -x "$rel/$want/bin/aiua" ] || { echo "release $want is not installed under $rel" >&2; exit 1; }
echo "$want"
PICK

# Flip ~/.philotic/current to releases/$1 atomically (rename over the link).
read -r -d '' FLIP_SCRIPT <<'FLIP' || true
set -euo pipefail
rel="$HOME/.philotic/releases"; link="$HOME/.philotic/current"; target="$rel/$1"
[ -x "$target/bin/aiua" ] || { echo "  ✗ $target/bin/aiua missing" >&2; exit 1; }
tmp="$link.tmp.$$"
ln -sfn "$target" "$tmp"
if mv --version >/dev/null 2>&1; then mv -Tf "$tmp" "$link"; else mv -fh "$tmp" "$link"; fi
echo "  ✓ ~/.philotic/current -> $target"
FLIP

# Keep the newest $1 releases (by install time); never prune `current`.
read -r -d '' PRUNE_SCRIPT <<'PRUNE' || true
set -euo pipefail
keep="$1"
rel="$HOME/.philotic/releases"; link="$HOME/.philotic/current"
current="$(basename "$(readlink "$link" 2>/dev/null || echo none)")"
n=0
# shellcheck disable=SC2012  # validated tag names; ls -t gives install order
ls -1td -- "$rel"/*/ 2>/dev/null | sed -e 's#/$##' | while IFS= read -r d; do
  n=$((n + 1))
  if [ "$n" -gt "$keep" ] && [ "$(basename "$d")" != "$current" ]; then
    rm -rf -- "$d" && echo "  – pruned $(basename "$d")"
  fi
done
PRUNE

# Print plist state: "<PHILOTIC_BIN_DIR>|<ProgramArguments:0>|<home>"
read -r -d '' PLIST_SCRIPT <<'PLIST' || true
plist="$HOME/Library/LaunchAgents/$1.plist"
pb=/usr/libexec/PlistBuddy
bin_dir="$("$pb" -c "Print :EnvironmentVariables:PHILOTIC_BIN_DIR" "$plist" 2>/dev/null || true)"
prog="$("$pb" -c "Print :ProgramArguments:0" "$plist" 2>/dev/null || true)"
echo "$bin_dir|$prog|$HOME"
PLIST

# ── Driver ───────────────────────────────────────────────────────────────────

echo "▶ Probing launchd service for '${HOTEL}' on ${TARGET}..."
LABEL="$(launchd_detect_label "${HOTEL}")"
if [[ -n "${LABEL}" ]]; then
  echo "  ✓ launchd-managed: ${LABEL}"
else
  echo "  – no launchd service found for ${HOTEL}; will flip current but not restart"
fi

if [[ ${ROLLBACK} -eq 1 ]]; then
  echo "▶ Choosing rollback target on ${TARGET}..."
  TAG="$(on_target "${PICK_SCRIPT}" "${TAG}")"
  echo "  → ${TAG}"
else
  ASSET="philotic-${TAG}-${PLATFORM}.tar.gz"
  echo "▶ Resolving release ${TAG} assets on ${REPO}..."
  RELEASE_JSON="$(gh api "repos/${REPO}/releases/tags/${TAG}")"
  TARBALL_URL="$(python3 -c 'import json,sys; a={x["name"]: x["browser_download_url"] for x in json.load(sys.stdin)["assets"]}; print(a.get(sys.argv[1], ""))' "${ASSET}" <<<"${RELEASE_JSON}")"
  SUMS_URL="$(python3 -c 'import json,sys; a={x["name"]: x["browser_download_url"] for x in json.load(sys.stdin)["assets"]}; print(a.get("SHA256SUMS", ""))' <<<"${RELEASE_JSON}")"
  if [[ -z "${TARBALL_URL}" || -z "${SUMS_URL}" ]]; then
    echo "✗ release ${TAG} has no ${ASSET} or SHA256SUMS asset (did release.yml finish?)" >&2
    exit 1
  fi
  echo "▶ Staging ${TAG} on ${TARGET} (hotel still running)..."
  on_target "${STAGE_SCRIPT}" "${TAG}" "${ASSET}" "${TARBALL_URL}" "${SUMS_URL}"
fi

# Does the plist actually run from ~/.philotic/current/bin?
PLIST_OK=0
if [[ -n "${LABEL}" ]]; then
  IFS='|' read -r PLIST_BIN_DIR PLIST_PROG TARGET_HOME <<<"$(on_target "${PLIST_SCRIPT}" "${LABEL}")"
  WANT_BIN="${TARGET_HOME}/.philotic/current/bin"
  if [[ "${PLIST_BIN_DIR%/}" == "${WANT_BIN}" && "${PLIST_PROG}" == "${WANT_BIN}/"* ]]; then
    PLIST_OK=1
  fi
fi

print_plist_instructions() {
  local plist="\$HOME/Library/LaunchAgents/${LABEL}.plist"
  cat <<EOF

⚠ ${LABEL} does not run from ~/.philotic/current/bin yet:
    PHILOTIC_BIN_DIR    = ${PLIST_BIN_DIR:-<unset>}
    ProgramArguments[0] = ${PLIST_PROG:-<unset>}
  The release is installed and ~/.philotic/current flipped, but restarting would
  not change the binaries. One-time switch (on ${TARGET}), then rerun this script:

    PLIST=${plist}
    /usr/libexec/PlistBuddy -c "Set :ProgramArguments:0 \$HOME/.philotic/current/bin/aiua" "\$PLIST"
    /usr/libexec/PlistBuddy -c "Set :EnvironmentVariables:PHILOTIC_BIN_DIR \$HOME/.philotic/current/bin" "\$PLIST"
    /usr/libexec/PlistBuddy -c "Print :EnvironmentVariables:PATH" "\$PLIST"   # if set, put ~/.philotic/current/bin first
    launchctl bootout gui/\$(id -u)/${LABEL}; launchctl bootstrap gui/\$(id -u) "\$PLIST"

  Keep the Cellar binaries for one release as the fallback (docs/process/RELEASE.md).
EOF
}

restart_hotel() {
  if ! hotel_clear_active_pid "${PROFILE}" "${HOTEL}"; then
    echo "⚠ Could not clear hotels.active_pid (continuing — aiua may refuse to start if a stale live PID matches)"
  fi
  launchd_start "${LABEL}"
  echo "  ✓ ${LABEL} started under launchd supervision"
}

if [[ ${PLIST_OK} -eq 1 && ${RESTART} -eq 1 ]]; then
  HOTEL_STOPPED=0
  on_exit_restart() {
    local rc=$?
    if [[ ${HOTEL_STOPPED} -eq 1 ]]; then
      HOTEL_STOPPED=0
      echo "⚠ Aborted after the hotel was stopped (exit ${rc}) — restarting it on whatever current points at." >&2
      restart_hotel || echo "❌ Automatic restart failed; bootstrap ${LABEL} on ${TARGET} by hand." >&2
    fi
    exit "${rc}"
  }
  trap on_exit_restart EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM

  echo "▶ Stopping ${HOTEL} (launchd bootout)..."
  STOP_EPOCH="$(date +%s)"
  HOTEL_STOPPED=1
  launchd_stop_hotel "${LABEL}" "${HOTEL}"
  on_target "${FLIP_SCRIPT}" "${TAG}"
  restart_hotel
  HOTEL_STOPPED=0
  trap - EXIT INT TERM
  echo "  hotel was stopped for $(( $(date +%s) - STOP_EPOCH ))s"
else
  on_target "${FLIP_SCRIPT}" "${TAG}"
  if [[ -n "${LABEL}" && ${PLIST_OK} -eq 0 ]]; then
    print_plist_instructions
  elif [[ ${RESTART} -eq 0 ]]; then
    echo "  – --no-restart: hotel not restarted"
  fi
fi

if [[ ${ROLLBACK} -eq 0 ]]; then
  echo "▶ Keeping the newest ${KEEP} releases..."
  on_target "${PRUNE_SCRIPT}" "${KEEP}"
fi

echo "▶ ~/.philotic/current/bin/aiua --version:"
launchd_exec "\$HOME/.philotic/current/bin/aiua --version" || echo "  ⚠ aiua --version failed"
echo "✅ ${HOTEL} on ${TARGET}: current = ${TAG}. Prove it: just verify-release ${HOTEL} ${TAG}"
