#!/usr/bin/env bash
set -euo pipefail

# Source-only harness: no app entry point, production settings or live sender.
tests_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
app_root="$(dirname -- "$tests_dir")"
harness="$(mktemp -d /tmp/philotic-voice-source-tests.XXXXXX)"

python3 - "$app_root" "$harness" <<'PY'
import json
import shutil
import sys
from pathlib import Path

app, harness = map(Path, sys.argv[1:])
shutil.copytree(app / 'Sources/PhiloticApp', harness / 'Sources/PhiloticApp')
(harness / 'Sources/PhiloticApp/PhiloticApp.swift').unlink()
tests = harness / 'Tests/PhiloticAppTests'
tests.mkdir(parents=True)
for name in ['VoiceControllerTests.swift', 'VoicePrivacyBoundaryTests.swift', 'VoiceSessionTests.swift']:
    shutil.copy(app / 'Tests/PhiloticAppTests' / name, tests / name)
kit = json.dumps(str((app.parent / 'PhiloticKit').resolve()))
(harness / 'Package.swift').write_text('''// swift-tools-version: 6.0
import PackageDescription
let package = Package(name: "VoiceSourceTests", platforms: [.macOS(.v14)],
    dependencies: [.package(path: %s)],
    targets: [
        .target(name: "PhiloticApp", dependencies: [.product(name: "PhiloticKit", package: "PhiloticKit")], swiftSettings: [.swiftLanguageMode(.v5)]),
        .testTarget(name: "PhiloticAppTests", dependencies: ["PhiloticApp"], swiftSettings: [.swiftLanguageMode(.v5)])
    ])
''' % kit)
PY

echo "Voice source harness: $harness"
swift test --package-path "$harness" --scratch-path "$harness/build" --disable-sandbox
