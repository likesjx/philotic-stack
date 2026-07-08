#!/usr/bin/env bash
# Builds apps/philotic-apple/PhiloticApp for macOS and the iOS Simulator.
#
# The iOS Simulator destination is flaky on hosts where the installed
# CoreSimulator runtime trails the Xcode-bundled SDK (e.g. only iOS 26.2
# installed against a 26.5 SDK): xcodebuild intermittently reports
# "Supported platforms for the buildables in the current scheme is empty"
# and fails destination resolution before any compilation happens. That
# failure mode is a toolchain/runtime mismatch, not a project or code
# defect, so this script retries a bounded number of times and only
# restarts CoreSimulatorService (which requires shutting down any booted
# simulators) after an observed failure — never preemptively, since this
# runs against the operator's real Mac.
#
# The permanent fix is installing a simulator runtime that matches the
# active Xcode SDK version (Xcode > Settings > Components, or
# `xcodebuild -downloadPlatform iOS`) — that requires disk headroom (the
# runtime image is several GB) this host may not have.
set -euo pipefail

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/apps/philotic-apple/PhiloticApp"
MAX_ATTEMPTS="${PHILOTIC_APP_BUILD_ATTEMPTS:-8}"
SIM_NAME="${PHILOTIC_APP_SIM_NAME:-iPhone 17}"
SIM_OS="${PHILOTIC_APP_SIM_OS:-26.2}"

cd "$APP_DIR"
echo "==> xcodegen generate"
xcodegen generate

echo "==> xcodebuild -scheme PhiloticApp-macOS (platform=macOS)"
xcodebuild -project PhiloticApp.xcodeproj -scheme PhiloticApp-macOS -destination 'platform=macOS' build

echo "==> xcodebuild -scheme PhiloticApp-iOS (platform=iOS Simulator, OS=${SIM_OS}, name=${SIM_NAME})"
attempt=1
while :; do
    if xcodebuild -project PhiloticApp.xcodeproj -scheme PhiloticApp-iOS \
        -destination "platform=iOS Simulator,OS=${SIM_OS},name=${SIM_NAME}" build; then
        echo "==> iOS Simulator build succeeded on attempt ${attempt}"
        break
    fi

    if [ "$attempt" -ge "$MAX_ATTEMPTS" ]; then
        echo "!! iOS Simulator build failed after ${MAX_ATTEMPTS} attempts." >&2
        echo "!! This is very likely the CoreSimulator/SDK-runtime-mismatch flake described" >&2
        echo "!! at the top of this script, not a code regression. Consider installing a" >&2
        echo "!! matching simulator runtime (Xcode > Settings > Components) if it persists." >&2
        exit 1
    fi

    echo "-- attempt ${attempt} failed (destination resolution flake); restarting CoreSimulatorService and retrying"
    xcrun simctl shutdown all >/dev/null 2>&1 || true
    killall -9 com.apple.CoreSimulator.CoreSimulatorService >/dev/null 2>&1 || true
    sleep 5
    attempt=$((attempt + 1))
done
