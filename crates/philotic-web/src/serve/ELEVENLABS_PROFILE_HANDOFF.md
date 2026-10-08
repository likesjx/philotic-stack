# ElevenLabs profile adapter handoff — unavailable pending trusted dispatch

Existing provider code already maps ControllerTask voice/voice_id, model,
spoken_text and provider_options.voice_settings into ElevenLabs requests. Preserve
that adapter and its vault-owned credentials; no new endpoint, token handling or
external preview path is needed in web or Swift.

Server profile configuration should map a stable selection ID to display label,
configured voice ID, optional configured model and expressive voice_settings.
The client selects the stable ID; it cannot supply an endpoint, credential,
identity, source manifest, policy revision or egress permission. Do not seed
invented production voice IDs or assume a model supports every expressive option.
Apple-local remains the effective provider while trusted remote eligibility is
unavailable; a local failure never authorizes ElevenLabs fallback.

Prepare the existing voice.synthesize envelope only after resolving the profile
and the complete typed source manifest. The privacy owner's
DispatchPrivacyAuthority::context_for(&ControllerTask) must return fresh trusted
context, and guarded_registry must classify the actual ElevenLabs boundary as
External and authorize TextToSpeech on every invoke/stream/retry/fallback. Missing,
unknown, private, stale or cancelled context denies. Preview text needs the same
provenance, permission and cancellation checks; selecting a profile is not consent
or eligibility. Native-live still denies.

Cancellation adapter inputs are server-created TurnBinding metadata from the
verified edge device session. The runtime issuer must bind that origin to the
full task digest, source manifest, intended authenticated agent and canonical
turn/cancel generation. Incoming GuestIdentity registration and task JSON cannot
issue the authority. Coordinate this with the privacy/IPC owner before adding
profile fields to edge/Swift dispatch; no production egress gate is installed in
this branch and no billable test or credentials were used.

Required fixture matrix at integration: private and inherited-private TTS deny;
unknown/missing/stale context deny; explicit authorized nonprivate TextToSpeech
permits only configured endpoints; local failure never falls back externally;
revocation or cancellation between sentences blocks the next attempt; preview
and actual response use the same guarded registry. Use mock providers and verify
zero invocations on denial before any separately authorized live experiment.

Configuration-only source preparation now lives in voice_profile_catalog.rs:
server-owned stable selections map to existing voice/model/voice_settings hints.
It accepts no speech content, endpoint, credentials or eligibility flag and has
no send method or installed route. Three synthetic tests prove metadata mapping,
unknown/duplicate denial and malformed configuration rejection. Full task
assembly/preview dispatch remains blocked on the owner guard described above.
