#!/usr/bin/env bash
# Test gate for the apps/philotic-apple workspace:
#   1. `swift test` in PhiloticKit (unit tests; also runs the optional
#      EdgeClient e2e hook, which self-skips when PHILOTIC_EDGE_URL is unset).
#   2. Both PhiloticApp xcodebuild targets (macOS + iOS Simulator), reusing
#      the retry-on-flake build script — see its header comment for why the
#      iOS Simulator destination needs a retry loop on this class of host.
#   3. PhiloticApp's macOS-hosted unit tests for shared app/domain code.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "==> swift test (PhiloticKit)"
(cd "$ROOT_DIR/apps/philotic-apple/PhiloticKit" && swift test)

echo "==> apple-app-build.sh (macOS + iOS Simulator)"
"$ROOT_DIR/scripts/apple-app-build.sh"

echo "==> xcodebuild test (PhiloticApp shared app/domain code)"
(
    cd "$ROOT_DIR/apps/philotic-apple/PhiloticApp"
    xcodebuild -project PhiloticApp.xcodeproj -scheme PhiloticApp-macOS \
        -destination 'platform=macOS' test
)
