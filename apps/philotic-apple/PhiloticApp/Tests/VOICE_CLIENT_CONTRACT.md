# Apple voice client handoff — 2026-10-08

Disposition: source-tested transitional client implementation. Backend transport
and trusted privacy installation remain required; this is not live voice parity.

Worktree: `codex/apple-voice-privacy`, based on develop `45b6aef9`, preserving
the initial controller commit `c4c3ff4d`. The interrupted uncommitted work was
inspected after mac-jane reconnected. Other Apple companion, edge/server and
privacy worker worktrees were inspected, never edited. Shared task, architecture
and defect updates remain with the parent. No registered defect was closed here.

Owned app edits: `Services/ChatSessionManager.swift`, `Services/VoiceController.swift`,
`Views/ChatView.swift`, `Views/ConnectionSettingsView.swift`, the three voice test
classes, this handoff/initial checkpoint, and `run-voice-source-tests.sh`.
Owned kit edits: `VoiceTurnGate.swift`, `VoiceProviderPreference.swift`, voice
additions to `EdgeProtocol.swift`, and their tests. `EdgeClient.swift` changes
are limited to voice cancellation classification, HelloAck feature arity and
ephemeral voice replay-cursor handling, with corresponding cursor/protocol tests
and one existing integration-test pattern match. No Rust files are owned here.

## Implemented client behavior

- Dictation requires available on-device recognition before requesting any
  permission. Permission results that arrive after stop/recipient change cannot
  start a stale capture. Denial never falls back to remote recognition.
- Raw PCM streaming, recording upload and hands-free remote STT are denied
  before capture or upload. A legacy stored preference cannot authorize them.
- Local transcripts use the existing philote text submission/routing path. The
  client does not add the `voice` modality marker that triggers persona TTS.
- Every submitted text turn has a unique request identity and local generation.
  Speech requires exact accepted request/conversation/turn correlation. A stale
  or uncorrelated Final cannot bind a turn or reopen an interrupted generation.
- Chunk admission checks exact turn, conversation, sequence and terminal state.
  Duplicate, skipped, stale, unknown and post-terminal chunks fail closed.
  Inline voice replies are ephemeral on the Rust edge and no longer advance
  the client's durable replay cursor, which could prune unprocessed text events.
- Interrupt, new submission, reconnect handshake, disconnect, recipient change
  and speaking-provider/profile change revoke local audio and speech timers.
- Cancellation is sent only after the server advertises `turn_cancel_v1`.
  Local revocation survives transport failure. Legacy servers get no unknown
  cancellation message; the UI explains local-only stopping.
  Ordinary silent text turns are not cancelled by the voice controls.
- Settings expose preferred provider (ElevenLabs or Apple), installed Apple
  voice selection, Apple preview and stop. ElevenLabs remains unavailable;
  preference falls back to Apple and grants no external eligibility. Preview
  is user-triggered; tests use fake speakers and never play a preview.
- iOS keeps capture and playback in `playAndRecord` / `voiceChat`; macOS retains
  its separate existing engine. macOS AEC remains disabled and is not fixed here.

Current servers do not echo request identity, so text chat and local dictation
submission remain usable, but automatic correlated speaking is unavailable.
Apple preview remains usable. This deliberate limitation is shown in chat and
settings, rather than treating an old turn's Final as a new turn.

## Exact transport contract required from the backend

Client v1 envelopes retain all existing fields. Optional additions preserve
legacy fixture encoding/decoding. Rust counterparts have not been edited here.

1. `turn_submit.request_id`: nonempty client-generated string, unique per
   submission. Bind it atomically to authenticated device, target, conversation
   and canonical turn before dispatching generation. It is correlation, never
   identity, privacy, authority or a caller-selected canonical turn ID.
2. Accepted `turn_event`: `event_kind: "status"`, `content: "accepted"`, exact
   `request_id`, `conversation_id`, and nonempty canonical `turn_id`. Subsequent
   token/Final/error/audio events must keep that `turn_id`. Acceptance must
   precede output delivery so the client can safely admit streamed audio.
3. `hello_ack.features`: optional string array, default empty. Advertise
   `"turn_cancel_v1"` only when cancellation is actually installed end-to-end.
4. `turn_cancel`: client-to-server payload:

```json
{
  "type": "turn_cancel",
  "target_node_id": "hotel",
  "target_agent_id": "agent",
  "conversation_id": "conversation",
  "request_id": "request-uuid",
  "turn_id": "canonical-turn-id"
}
```

`turn_id` is omitted when interruption races acceptance. The backend must
cancel by the authenticated request binding in that case, including a pending
acceptance/dispatch. A provided turn ID must match the binding. Unknown,
cross-device, wrong-target or mismatched identities must deny, never select
the newest conversation turn. Cancellation stops generation/synthesis/audio
delivery for that exact generation; it never reverses committed tools.

The client requests cancellation; successful socket send is not confirmation
that a model/provider actually stopped. Backend cancellation acknowledgment
and provider task abort behavior remain backend-owned work.

For a future external STT path, `audio_stream_start.stream_id` must likewise
be echoed/bound to accepted request/turn identity; FIFO acknowledgment order
cannot correlate independently completing STT relays. That path stays disabled.

## Trusted privacy boundary

The confirmed server contract supplies stable authenticated actor identity,
authoritative roles, current policy revision and the complete immutable source
manifest. Copies, summaries and tool results inherit source restrictions;
read access is not egress permission. Private sources forbid external STT/TTS;
unknown provider endpoints deny; local failure never selects external fallback.

The staged `DispatchPrivacyAuthority` / guarded provider registry must be
installed through the authenticated IPC/session adapter, with every retry and
fallback candidate guarded, before external voice is enabled. No production
adapter exists yet. Client request IDs, provider preferences, user JSON and
server capability flags are not privacy grants. External profile catalog and
per-turn preference fields need coordination with that backend owner; no
ElevenLabs voice/profile IDs or credentials are fetched or transmitted here.

The client blocks its external voice paths and does not trigger voice-modality
TTS. It cannot certify all backend egress: an agent's independent always-speak
policy or reasoning/model dispatch still needs the guarded server installation.
This work therefore does not claim complete egress enforcement.

## Verification and remaining work

Source-only macOS harness: 19 tests passed, no failures. It copies exact app
source, excludes the app entry point, injects empty settings, disables reconnect
triggers, isolates preference storage, mocks permission requests, and replaces
transport and speaking effects. Run:

```sh
bash apps/philotic-apple/PhiloticApp/Tests/run-voice-source-tests.sh
env -u PHILOTIC_EDGE_URL -u PHILOTIC_EDGE_TOKEN swift test --package-path apps/philotic-apple/PhiloticKit --scratch-path /tmp/philotic-kit-voice-build --disable-sandbox
```

PhiloticKit: 135 tests, one existing transport integration test skipped, zero
failures. Coverage includes repeated interrupt/new-turn/old-Final races,
interruption before acceptance, cancellation transport failure, exact negotiated
cancellation, legacy silence, provider changes, reconnect, agent switch away
and back, fresh-session scope, chunk ordering, and mocked permission denial.

All macOS app source, including the entry point, typechecked successfully against
the compiled kit module. The full iOS app built for the generic simulator target
(arm64 and x86_64) with signing disabled. These are compiler checks, not simulator
execution. macOS unit harness compiled the app module excluding only its entry
point so test startup could not access operator settings or start connections.
Evidence logs
are `/tmp/philotic-voice-source-tests.log`, `/tmp/philotic-kit-voice-tests.log`,
`/tmp/philotic-voice-macos-typecheck.log` and
`/tmp/philotic-voice-ios-app-build.log`. Temporary logs can disappear on reboot;
the commands and test sources are the durable evidence.

No microphone, real permission dialog, audio route, external provider, billable
call, installed runtime, simulator execution or physical device was tested.
Backend generation cancellation, privacy installation, expressive external
profiles, correctly shared macOS AEC engine and device route testing remain.
Browser parity and broad iPad changes are deferred. Shared iOS source is checked
at compile time, with no claim of iPad device validation.

No push, merge, deployment or service restart was performed. Muninn remained
unavailable to this tool surface; source-only continuation was explicitly
authorized. No secrets were read. No telemetry/harness trial was started.
The durable reality gap is that existing turn IDs alone do not identify which
local submission an accepted/replayed event belongs to. The next work boundary
is backend request binding/cancellation plus trusted privacy adapter installation.
