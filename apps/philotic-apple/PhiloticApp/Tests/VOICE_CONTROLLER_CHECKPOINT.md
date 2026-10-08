# Voice controller checkpoint — 2026-10-08

Base: develop `45b6aef9`. Workstream: `codex/apple-voice-privacy`.

This transitional source change owns `Services/VoiceController.swift` and
`Tests/PhiloticAppTests/VoiceControllerTests.swift` only. Existing dirty Apple
companion work and IPC worktrees were inspected and left untouched.

On-device dictation now refuses unsupported local recognition before requesting
permissions and always sets `requiresOnDeviceRecognition`. Recognition callbacks
carry a capture generation, so a cancelled capture cannot update a later capture.
Playback callbacks must match the current player before changing state. Stopping
playback also stops Apple speech, and pending reply audio includes Apple speech.

The iOS implementation uses `playAndRecord` / `voiceChat`, speaker default and
Bluetooth input for capture, recorded reply playback and Apple speech. Capture
teardown keeps the session active while another capture or playback is active.
macOS retains its separate existing input engine implementation: AEC is still
disabled pending a correctly shared capture/playback engine. No AEC fix is claimed.

## Verification

Three synthetic tests passed using a temporary SwiftPM harness containing the
exact controller source and test file (Swift language mode 5, macOS 14 minimum):
stale stopped-player callback, repeated stop/invalid chunk flows, and PCM frame/tail
draining. The controller also typechecked against the arm64 iOS 17 simulator target
with the installed iOS 27 SDK. This checks compilation, not simulator execution.
Evidence logs: `/tmp/philotic-voice-tests.log`,
`/tmp/philotic-voice-ios-typecheck.log`. `git diff --check` passed.

No full app/workspace build, simulator run, device, microphone, audio-route,
permission-dialog, installed runtime, external API, or billable validation ran.
Muninn tools were unavailable; source-only continuation was explicitly authorized.
Graph loopback was unavailable. No service restart or credential read was performed.

## Remaining work

This does not make the existing raw PCM/upload STT or server TTS paths private.
They still need trusted policy eligibility at the source and provider boundary;
preferences or user JSON must not grant that authority. External voice must remain
unavailable until the privacy worker's policy can be consumed. Apple dictation
is locally enforced here, but its submitted text follows existing philote routing.

The parent owns coordination with the privacy worker and subsequent end-to-end
turn cancellation, stale turn audio rejection, provider/profile selection and
preview, local-only speaking selection, and macOS duplex AEC. The existing
conversation-wide suppression race is not fixed by player identity checks.
Cancellation must not imply rollback of committed tools. Agent switch, fresh
session, reconnect, actual permission denial and device audio routes remain to
be tested with those larger changes. Browser parity and broad iPad changes are
outside this checkpoint. No push, merge, deployment or runtime restart is authorized.
