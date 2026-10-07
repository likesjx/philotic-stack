#!/usr/bin/env bash
# Read release/packages.toml — the single list of deployable binaries
# (proposal:release-train R3). Every workflow, deploy recipe and installer that
# needs "which packages / which binaries" must ask this script instead of
# hard-coding a list (the lists had drifted to 16 / 13 / 6 / 28 entries).
#
# Usage:
#   scripts/release-packages.sh cargo-flags   [--platform linux|darwin]   # -p a -p b ... (one line)
#   scripts/release-packages.sh packages      [--platform linux|darwin]   # one package per line
#   scripts/release-packages.sh bins          [--platform linux|darwin]   # one binary per line
#   scripts/release-packages.sh required-bins [--platform linux|darwin]   # one binary per line
#
# Without --platform, packages for any platform are listed.
# PHILOTIC_RELEASE_PACKAGES overrides the manifest path (used by tests).
# Needs only python3 >= 3.11 (tomllib); falls back to the `tomli` module.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="${PHILOTIC_RELEASE_PACKAGES:-${ROOT_DIR}/release/packages.toml}"

usage() {
  sed -n '7,11p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

[[ $# -ge 1 ]] || usage
CMD="$1"
shift
PLATFORM=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --platform) [[ $# -ge 2 ]] || usage; PLATFORM="$2"; shift 2 ;;
    --platform=*) PLATFORM="${1#--platform=}"; shift ;;
    -h|--help) usage ;;
    *) echo "release-packages: unknown argument: $1" >&2; usage ;;
  esac
done

case "${CMD}" in
  cargo-flags|packages|bins|required-bins) ;;
  *) echo "release-packages: unknown subcommand: ${CMD}" >&2; usage ;;
esac
case "${PLATFORM}" in
  ""|linux|darwin) ;;
  *) echo "release-packages: --platform must be linux or darwin (got '${PLATFORM}')" >&2; exit 2 ;;
esac
[[ -f "${MANIFEST}" ]] || { echo "release-packages: manifest not found: ${MANIFEST}" >&2; exit 1; }

python3 - "${MANIFEST}" "${CMD}" "${PLATFORM}" <<'PY'
import sys

try:
    import tomllib
except ModuleNotFoundError:  # python < 3.11
    try:
        import tomli as tomllib
    except ModuleNotFoundError:
        sys.exit("release-packages: python3 >= 3.11 (tomllib) or the tomli module is required")

path, cmd, platform = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path, "rb") as fh:
    data = tomllib.load(fh)

packages = data.get("package", [])
if not packages:
    sys.exit(f"release-packages: no [[package]] entries in {path}")

seen_pkgs, seen_bins = set(), set()
selected = []
for pkg in packages:
    name = pkg.get("name")
    if not name:
        sys.exit("release-packages: a [[package]] entry has no name")
    if name in seen_pkgs:
        sys.exit(f"release-packages: duplicate package {name}")
    seen_pkgs.add(name)
    bins = pkg.get("bins", [])
    required = pkg.get("required", [])
    for b in bins:
        if b in seen_bins:
            sys.exit(f"release-packages: binary {b} listed twice")
        seen_bins.add(b)
    stray = [r for r in required if r not in bins]
    if stray:
        sys.exit(f"release-packages: {name}: required bins not in bins: {stray}")
    if platform and not pkg.get(platform, False):
        continue
    selected.append(pkg)

if cmd == "cargo-flags":
    print(" ".join(f"-p {p['name']}" for p in selected))
elif cmd == "packages":
    for p in selected:
        print(p["name"])
elif cmd == "bins":
    for p in selected:
        for b in p.get("bins", []):
            print(b)
elif cmd == "required-bins":
    for p in selected:
        for b in p.get("required", []):
            print(b)
PY
