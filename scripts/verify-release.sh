#!/usr/bin/env bash
# Prove a hotel runs a given release (proposal:release-train R7).
#
#   scripts/verify-release.sh <host> <tag>        (just verify-release <host> <tag>)
#
#   host: vps-jane | mac-jane | mbp-jane
#     vps-jane → ssh ${PHILOTIC_VPS_SSH_TARGET:-deploy@jane-vps}, linux-x86_64, /opt/philotic
#     mac-jane → ${PHILOTIC_MAC_JANE_TARGET:-local},             darwin-arm64, ~/.philotic
#     mbp-jane → ssh ${PHILOTIC_MBP_JANE_TARGET:-mbp-jane},      darwin-arm64, ~/.philotic
#
# Downloads philotic-<tag>-<platform>.manifest.json from the GitHub Release,
# hashes every binary installed under <base>/releases/<tag>/bin on the host and
# prints a PASS/FAIL table (scripts/release-manifest.py compare). Also checks
# that <base>/current points at <tag>, that the running aiua executable comes
# from that release, and reports `aiua --version`.
#
# Exit 0 only if every check passes, including exact manifest version and full
# commit SHA reported by the installed aiua. Missing build provenance fails.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="${PHILOTIC_GH_REPO:-likesjx/philotic-stack}"
SSH_OPTS=(-o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=4)

if [[ $# -ne 2 ]]; then
  sed -n '4,9p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
fi
HOST="$1"
TAG="$2"
if ! [[ "${TAG}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]]; then
  echo "✗ '${TAG}' is not a release tag (need vX.Y.Z or vX.Y.Z-rc.N)" >&2
  exit 2
fi

case "${HOST}" in
  vps-jane|jane-vps)
    HOST="vps-jane"; TARGET="${PHILOTIC_VPS_SSH_TARGET:-deploy@jane-vps}"
    PLATFORM="linux-x86_64"; BASE="/opt/philotic" ;;
  mac-jane)
    TARGET="${PHILOTIC_MAC_JANE_TARGET:-local}"; PLATFORM="darwin-arm64"; BASE="" ;;
  mbp-jane)
    TARGET="${PHILOTIC_MBP_JANE_TARGET:-mbp-jane}"; PLATFORM="darwin-arm64"; BASE="" ;;
  *)
    echo "✗ unknown host '${HOST}' (vps-jane | mac-jane | mbp-jane)" >&2
    exit 2 ;;
esac

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

# ── 1. Manifest from the release ─────────────────────────────────────────────
MANIFEST_ASSET="philotic-${TAG}-${PLATFORM}.manifest.json"
echo "▶ Fetching ${MANIFEST_ASSET} from ${REPO} release ${TAG}..."
MANIFEST_URL="$(gh api "repos/${REPO}/releases/tags/${TAG}" -q ".assets[] | select(.name == \"${MANIFEST_ASSET}\") | .browser_download_url")"
if [[ -z "${MANIFEST_URL}" ]]; then
  echo "✗ release ${TAG} has no ${MANIFEST_ASSET}" >&2
  exit 1
fi
curl -fsSL --connect-timeout 15 --retry 3 -o "${WORK}/manifest.json" "${MANIFEST_URL}"
case "${PLATFORM}" in
  linux-x86_64) MANIFEST_TARGET="x86_64-unknown-linux-gnu" ;;
  darwin-arm64) MANIFEST_TARGET="aarch64-apple-darwin" ;;
esac
python3 "${ROOT_DIR}/scripts/release-manifest.py" check-metadata \
  --manifest "${WORK}/manifest.json" --tag "${TAG}" --target "${MANIFEST_TARGET}"
mapfile -t BINS < <(python3 -c 'import json,sys; [print(b["name"]) for b in json.load(open(sys.argv[1]))["bins"]]' "${WORK}/manifest.json")
MANIFEST_SHA="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["sha"])' "${WORK}/manifest.json")"

# ── 2. Probe the host ────────────────────────────────────────────────────────
# Output lines: "C <current target>", "H <sha256>  <bin>", "I <sha256>  bin/<bin>",
# "P <sha256>  bin/<bin>" (pre-sign, from the tarball), "V <aiua --version line>",
# "R <running aiua executable>".
read -r -d '' PROBE <<'PROBE' || true
base="$1"; tag="$2"; shift 2
[ -n "$base" ] || base="$HOME/.philotic"
dir="$base/releases/$tag"
echo "C $(readlink "$base/current" 2>/dev/null || echo '-')"
if command -v sha256sum >/dev/null 2>&1; then hash() { sha256sum "$1"; }; else hash() { shasum -a 256 "$1"; }; fi
for b in "$@"; do
  if [ -f "$dir/bin/$b" ]; then echo "H $(hash "$dir/bin/$b" | awk '{print $1}')  $b"; fi
done
if [ -f "$dir/INSTALLED_SHA256SUMS" ]; then sed 's/^/I /' "$dir/INSTALLED_SHA256SUMS"; fi
if [ -f "$dir/SHA256SUMS" ]; then sed 's/^/P /' "$dir/SHA256SUMS"; fi
if [ -x "$base/current/bin/aiua" ]; then
  "$base/current/bin/aiua" --version 2>&1 | head -n 3 | sed 's/^/V /'
fi
pid=""
if [ "$(uname -s)" = "Linux" ]; then
  pid="$(systemctl show -p MainPID --value philotic-hotel 2>/dev/null || true)"
  [ "$pid" != "0" ] || pid=""
  if [ -n "$pid" ]; then
    exe="$(readlink "/proc/$pid/exe" 2>/dev/null || sudo -n readlink "/proc/$pid/exe" 2>/dev/null || true)"
    echo "R ${exe:-?}"
  fi
else
  pid="$(pgrep -f 'aiua --hotel' 2>/dev/null | head -n 1 || true)"
  if [ -n "$pid" ]; then echo "R $(ps -o comm= -p "$pid" 2>/dev/null || echo '?')"; fi
fi
PROBE

echo "▶ Probing ${HOST} (${TARGET})..."
if [[ "${TARGET}" == "local" ]]; then
  bash -s -- "${BASE}" "${TAG}" "${BINS[@]}" <<<"${PROBE}" > "${WORK}/probe.txt"
else
  # shellcheck disable=SC2029  # args are %q-quoted for the remote shell on purpose
  ssh "${SSH_OPTS[@]}" "${TARGET}" "bash -s -- $(printf '%q ' "${BASE}" "${TAG}" "${BINS[@]}")" <<<"${PROBE}" > "${WORK}/probe.txt"
fi
sed -n 's/^H //p' "${WORK}/probe.txt" > "${WORK}/actual.txt"
sed -n 's/^I //p' "${WORK}/probe.txt" > "${WORK}/installed.txt"
sed -n 's/^P //p' "${WORK}/probe.txt" > "${WORK}/presign.txt"
CURRENT="$(sed -n 's/^C //p' "${WORK}/probe.txt" | head -n 1)"
VERSION="$(sed -n 's/^V //p' "${WORK}/probe.txt" | head -n 1)"
RUNNING="$(sed -n 's/^R //p' "${WORK}/probe.txt" | head -n 1)"

# ── 3. Compare ───────────────────────────────────────────────────────────────
FAIL=0
echo
if ! python3 "${ROOT_DIR}/scripts/release-manifest.py" compare \
     --manifest "${WORK}/manifest.json" --actual "${WORK}/actual.txt" \
     --installed "${WORK}/installed.txt" --presign "${WORK}/presign.txt" --label "${HOST}"; then
  FAIL=1
fi

row() { printf '  %-14s %-6s %s\n' "$1" "$2" "$3"; }
echo
echo "  CHECK          STATUS DETAIL"
if [[ "$(basename "${CURRENT}")" == "${TAG}" ]]; then
  row "current" "PASS" "${CURRENT}"
else
  row "current" "FAIL" "${CURRENT:-<missing>} (expected .../releases/${TAG})"
  FAIL=1
fi

if [[ -z "${RUNNING}" ]]; then
  row "running aiua" "FAIL" "no running hotel process found"
  FAIL=1
elif [[ "${RUNNING}" == *"/releases/${TAG}/bin/aiua" || "${RUNNING}" == *"/.philotic/current/bin/aiua" ]]; then
  row "running aiua" "PASS" "${RUNNING}"
else
  row "running aiua" "FAIL" "${RUNNING} (not from release ${TAG}; plist/unit not switched?)"
  FAIL=1
fi

printf '%s\n' "${VERSION}" > "${WORK}/version.txt"
if python3 "${ROOT_DIR}/scripts/release-manifest.py" check-version \
     --manifest "${WORK}/manifest.json" --output-file "${WORK}/version.txt"; then
  row "version/build sha" "PASS" "${VERSION}"
else
  row "version/build sha" "FAIL" "${VERSION:-<missing>}"
  FAIL=1
fi

echo
if [[ ${FAIL} -eq 0 ]]; then
  echo "✅ ${HOST} verified on ${TAG}"
else
  echo "❌ ${HOST} does NOT match release ${TAG}"
fi
exit "${FAIL}"
