#!/usr/bin/env bash
set -euo pipefail
APPLE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APPLE_BUILD=/private/tmp/philotic-companion-build
APPLE_APP="$APPLE_BUILD/Build/Products/Debug/PhiloticApp.app"
APPLE_MODE="${1:-run}"
case "$APPLE_MODE" in run|--verify|--debug|--logs|--telemetry) ;; *)
  echo "Usage: $0 [run|--verify|--debug|--logs|--telemetry]" >&2
  exit 2 ;;
esac
pkill -x PhiloticApp || true
xcodegen generate --spec "$APPLE_ROOT/PhiloticApp/project.yml"
xcodebuild -project "$APPLE_ROOT/PhiloticApp/PhiloticApp.xcodeproj" \
  -scheme PhiloticApp-macOS -configuration Debug -destination 'platform=macOS' \
  -derivedDataPath "$APPLE_BUILD" build
case "$APPLE_MODE" in
  --debug) lldb -- "$APPLE_APP/Contents/MacOS/PhiloticApp" ;;
  --logs) open -n "$APPLE_APP"; /usr/bin/log stream --info --predicate 'process == "PhiloticApp"' ;;
  --telemetry) open -n "$APPLE_APP"; /usr/bin/log stream --info --predicate 'subsystem == "com.philotic.apple.mac"' ;;
  --verify) open -n "$APPLE_APP"; sleep 1; pgrep -x PhiloticApp ;;
  run) open -n "$APPLE_APP" ;;
esac
