# Voice turn backend admission seam

The initial ledger checkpoint (`9da6ba37`) was uninstalled. The follow-up installs
correlated edge admission and echoes accepted `request_id`/canonical `turn_id`
before spawning the reply relay. Request IDs are validated and reserved before
IPC validation; correlated validation runs in a separate task so the socket can
receive a pre-accept cancel. Duplicate IDs cannot dispatch a second turn.

Server-owned connection handles fence superseded sockets before submit/cancel and are rechecked before pending acceptance. This uses the verified edge session only; it does not authenticate a hotel agent. HelloAck advertises no features by default. `RuntimeTurnCancellation` is an
in-process adapter seam, present only in synthetic tests. `TurnCancel` otherwise
returns `turn_cancel_unavailable`. When a verified adapter is installed, exact
verified-device/request/target/conversation/optional-turn admission revokes the
ledger before invoking the adapter. Failure remains revoked and reports failure.
Its success contract requires pending/retry/active generation cancellation, not
merely aborting the web relay. No production implementation exists yet.

Cancelled audio/text is filtered before retention, live delivery and replay.
Reply metadata cannot replace the relay's canonical conversation/session/turn;
present mismatched or malformed IDs are dropped. These are correlation barriers,
not authenticated agent provenance. Bytes already admitted to a socket send
cannot be recalled; the client turn gate rejects stale playback independently.

Lifecycle follow-up preserves `ad13939b` and fixes its 256 lifetime-entry cliff.
There are separate bounds of 256 active entries and 256 terminal metadata entries
per device. Active pending/accepted and unconfirmed revoked jobs have no timer
eviction. Completed relays, failed preparation and confirmed cancellation release
active capacity; terminal metadata expires after 30 minutes of monotonic server
time or is evicted at its bounded terminal capacity. Expiry is checked lazily on
ledger access. Clock rollback is clamped and jumps remove only terminal entries.

Managed canonical turns use the server-only `operator-chat-edge-turn-` namespace.
An unknown/expired managed turn denies, rather than becoming permissive legacy
traffic. Completed output is sealed at its last admitted sequence: existing
queued/replayed frames within that cutoff remain valid; later output denies.
Revocation denies even previously queued frames. Ledger admission and ring
insertion now hold the same lock as cancellation, so frames are admitted before
revocation or rejected; previously admitted retained frames may remain in the
ring while delivery/replay suppress them.

Compact keyed request fingerprints remain bounded at 4,096 per device for the
process lifetime and reject ID reuse after terminal metadata expiry, eviction
or reconnect. A hash collision rejects safely. They are not authority handles.
That safe budget exhaustion deliberately fails closed; it is not silently reset
on reconnect or wall-clock expiry. Removing even this bound safely requires an
owner-provided verified epoch/durable request replay contract. This remains an
in-memory source checkpoint; restart durability belongs to that authority work.

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

Ownership: the initial ledger added its module and note; follow-ups add edge admission/lifecycle, an acceptance callback and canonical reply filtering in `serve.rs`, and configuration-only profile preparation. The active desktop-invite-login worktree owns
dirty login fencing in `serve.rs`; no files in that worktree were edited. The
privacy owner retains core/model-router/IPC authority work. Apple client code is
in the preceding isolated commits c4c3ff4d and 46f871d2.

Verification: 30 protocol tests (legacy fixtures unchanged), 30 edge unit tests,
ten ledger unit tests, three mock profile tests and all 12 fake-hotel loopback WebSocket tests pass. The lifecycle regression sends 270 mixed completed/failed requests and reconnects; unit fixtures cover 600 transitions, repeated cancellation, clock/expiry, stale frames and fail-closed budget exhaustion. Mock
adapter tests cover cancellation before acceptance, wrong device/target/agent/
conversation/turn, adapter failure, queued/replayed stale output and a running
synthetic job whose committed tool effect survives cancellation. These do not
establish real hotel/philote/provider cancellation, privacy authority installation,
physical-device audio, macOS AEC or live ElevenLabs. The existing client slice
verification remains unchanged. No external provider traffic or microphone use.

ElevenLabs profile preparation is described in [the profile handoff](ELEVENLABS_PROFILE_HANDOFF.md).
The guarded registry and verified dispatch context remain the only supported
privacy seam; no second trust mechanism or client eligibility flag was added.

Minimum owner interface request: [runtime authority/cancellation](VOICE_RUNTIME_AUTHORITY_REQUEST.md).
