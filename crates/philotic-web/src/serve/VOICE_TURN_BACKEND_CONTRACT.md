# Voice turn backend admission seam

This patch provides a bounded, authenticated-device turn-binding ledger and
source tests. It is not wired into the WebSocket handler and does not advertise
`turn_cancel_v1`. It does not echo accepted `request_id` yet. Neither relay abort
nor outgoing chunk suppression is evidence of provider generation cancellation.

Production installation requires these interfaces, coordinated by the parent:

The ledger's string device field is only metadata: its type does not establish
authentication. At installation it must come from the edge's verified bearer
session. Hotel Register(GuestIdentity) currently copies caller-supplied identity;
that registration must not be treated as an authenticated principal. The IPC
owner must establish verified launch/session/peer binding before issuing runtime
authority. See the privacy owner's IPC_AUTHORITY_HANDOFF_REPORT.md for producer,
park/repark/flush, SDK and model-runtime ownership and coordinated tests.

1. Extend hotel `InboundTask` beyond `source_node`, `task_id`, `task_json` with a
   server-resolved authority handle preserved through parked delivery and SDK
   decoding. Caller JSON must never issue identity, cancellation or privacy.
2. Bind verified edge device, request ID, target node/agent, conversation and
   canonical turn before dispatch. Resolve scoped conversation once, on the
   server. Publish accepted `request_id`/`turn_id` before starting the job.
3. Invoke an authenticated runtime cancellation adapter using that immutable
   binding. It must revoke pending/retrying provider attempts and stop active
   generation/audio publication. Cancellation never rolls back committed tools.
4. Check the ledger immediately before each provider attempt and each outgoing
   event/chunk. Late acceptance and Final cannot revive a revoked binding.
5. Advertise `turn_cancel_v1` only after the adapter is installed. Reconnect must
   retain the device binding/tombstones until its authenticated session expires;
   capacity exhaustion rejects instead of evicting live or revoked request IDs.

Privacy integration uses the privacy owner's `DispatchPrivacyAuthority::context_for`
and `guarded_registry`, with SpeechToText/TextToSpeech operation checks. Its exact
contract lives in `crates/model-router/VOICE_PRIVACY_DISPATCH_CONTRACT.md` in the
privacy-integration worktree. Preferences/request IDs/cancellation grant no egress
eligibility. Native-live paths remain denied. Source manifests and current policy
must come from the trusted runtime issuer, which is still absent.

Ownership: this patch adds `serve/voice_turn_binding.rs` and this note, plus only
a module declaration in `serve.rs`. The active desktop-invite-login worktree owns
dirty login fencing in `serve.rs`; no files in that worktree were edited. The
privacy owner retains core/model-router/IPC authority work. Apple client code is
in the preceding isolated commits c4c3ff4d and 46f871d2.

Verification: compile the ledger with a wrapper module using `rustc --test` and
execute its tests. These establish admission state transitions only; they do not
establish WebSocket ordering, actual runtime/provider cancellation, privacy
authority installation, simulator audio routing, device AEC or live ElevenLabs.
ElevenLabs stays unavailable pending trusted dispatch; macOS AEC still requires
a shared duplex audio engine. iOS duplex configuration is in the client slice.
