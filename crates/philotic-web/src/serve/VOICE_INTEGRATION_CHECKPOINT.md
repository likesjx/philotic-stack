# Voice source integration checkpoint

Base: verified remote develop `689bfc7ddce26341cac994bb77d0027bea2ac98a`.
Branch: `codex/voice-integrated-689bfc7d`.

The seven commits through original checkpoint
`17a7838e2e57b3473471bd247fadc59986e290dd` were reapplied in order without
conflicts. Original branch/worktree remain unchanged. The only changed path shared
with intervening remote changes was `docs/task.md`; its remote task entries are
retained, with the scoped Apple voice section added. No iPad, notch or privacy
owner worktree/delta was combined. No unrelated remote change was cherry-picked.

Integrated validation uses the current dependency lockfile and current hotel IPC
module layout. All Rust commands use `--offline` and the existing
`/tmp/percival-integrated-hotel-target` build cache:

- `cargo test -p philotic-web --no-default-features`: 231 unit tests passed,
  one ignored; all 12 mocked hotel WebSocket integration tests passed.
- `cargo test -p philotic-edge-protocol -p ansible-mesh-core`: 30 protocol
  tests and 534 mesh unit/integration tests passed; doc tests passed.
- `cargo check -p philotic-web -p aiua --all-targets --no-default-features`:
  passed against current IPC/mesh/dependency source.
- `cargo test -p aiua --no-default-features service::ipc`: all 197 current
  hotel IPC tests passed, including delivery/repark regressions.
- `bash apps/philotic-apple/PhiloticApp/Tests/run-voice-source-tests.sh`:
  19 synthetic app source tests passed, compiling shared macOS sources.
- `swift test --package-path apps/philotic-apple/PhiloticKit --disable-sandbox`:
  134 passed, one live test skipped, zero failures.
- Generated project with existing XcodeGen; `xcodebuild` for scheme
  `PhiloticApp-iOS`, generic iOS Simulator, Debug, signing disabled: build
  succeeded for arm64/x86_64. No simulator execution or device installation.

All test suites above have zero failures. Existing live/ignored tests were not
enabled. `git diff --check` passed. The repository docs metadata checker ran in
warn-only mode and reported warnings outside the changed voice documents. Logs
are `/tmp/voice-integrated-{web,mesh-protocol,check,ipc,app,kit,ios,docs}.log`.

Production privacy dispatch and cancellation remain uninstalled. External
STT/TTS and handsfree deny without trusted eligibility; ElevenLabs preference and
profile metadata grant no egress. `turn_cancel_v1` remains unadvertised without a
genuine adapter that revokes pending/retry work and confirms active-stream and
publication quiescence. Committed tools are never rolled back. Remaining gates:
hotel/SDK authority-envelope integration, guarded provider dispatch, eligible
profile preview/dispatch, macOS duplex/AEC, physical permission/audio-route tests,
and browser parity. No live provider, microphone, credentials, signing, deployment
or publication was used.
