# Minimum runtime authority/cancellation request for the parent

Owner: shared privacy/IPC authority worker. Voice owns the web adapter and
correlation ledger, not issuance of authenticated agent identity or policy.

The in-process seam is RuntimeTurnCancellation::cancel(&TurnBinding) ->
Future<Result<(), String>>. TurnBinding has verified edge device, request,
canonical turn, target node/agent and conversation, all bound by the server.
It contains no issuer credential, trusted policy or source manifest. Resolve it
through the owner's registry; do not make it Deserialize or infer authority from
these strings, GuestIdentity registration, roles in task_json or preferences.

Please provide these minimum owner interfaces (names are illustrative):

1. Register a turn from a verified server-owned web launch/session origin:
   origin connection/edge session reference, stable target agent + incarnation,
   canonical turn + cancellation generation, full typed task digest, complete
   immutable source manifest, intended consumer and expiry. Return an opaque
   authority handle outside task_json. The caller must already be authenticated
   by the owner's launch/session/peer mechanism; a guest cannot attest itself.
2. Resolve that handle on every dispatch attempt into the existing
   VerifiedDispatchContext, rechecking current policy, payload/source digest,
   intended consumer/incarnation, expiry and cancellation generation. Preserve
   the same handle/generation through direct delivery, park, loss/repark and SDK
   decode. Unknown, missing, stale or mismatched handles deny all protected paths.
3. Cancel that handle + canonical turn/generation from the verified originating
   session/device. Return confirmed stop, already-finished/already-cancelled, or
   failure. Confirmed stop must revoke pending/parked and retry attempts, abort
   active generation/provider streams, and fence all subsequent text/audio
   publication before returning. Tools committed before cancellation stay
   committed; no rollback operation belongs in this interface.
4. Supply an idempotent terminal signal for the canonical turn/generation so
   cleanup can distinguish completed/cancel-confirmed work from a failed or
   hanging cancellation. Revoked but still-running work is never timer-evicted.

The web adapter should resolve the opaque handle from a server registry keyed by
this immutable binding, invoke the typed cancellation request, and await its
confirmed response. No caller-provided handle or "private/egress" boolean is
accepted by the edge cancellation message. Capability negotiation remains empty
until that adapter and all generation/publication barriers are installed.

For ElevenLabs, use only DispatchPrivacyAuthority::context_for(&ControllerTask)
and guarded_registry. The actual endpoint is External and the operation is
TextToSpeech (SpeechToText for transcription). Profile/preview preference is
metadata, never eligibility. The web worker has no production egress gate or
provider adapter installation; the prepared profile handoff is configuration-only.

Required coordinated mocked tests: spoofed registration/session; wrong device,
agent/incarnation/target/turn/generation; modified task or source manifest;
park/repark/reconnect; cancel before acceptance; revocation between sentences,
retries and fallback; active stream quiescence; committed tool remains; absent
adapter advertises no capability and denial invokes zero external providers.

## Owner source disposition

The privacy owner's `LOCAL_AUTHORITY_INTEGRATION.md` now documents source APIs:
`LocalLaunchRegistry`, `LocalTaskEnvelope`, and `LocalTaskAuthority` issue/resolve
opaque handles against verified launch principals, payload digests, source manifests
and current policy. Reuse this issuer; do not create a parallel voice authority.
The owner still lists hotel/SDK/startup and guarded dispatch installation as pending.
Its cancellation tombstone denies future resolution but does not abort an active
provider stream. The voice adapter must compose that revocation with confirmed
stream/publication quiescence before advertising `turn_cancel_v1`. SQLite authority
work belongs on blocking workers. Restart-unknown handles deny and require new
trusted admission.
