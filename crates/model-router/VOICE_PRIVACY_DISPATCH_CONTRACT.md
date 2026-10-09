# Trusted voice privacy dispatch contract (runtime installation pending)

The local source authority is now implemented in
`ansible_mesh_core::privacy_local`. `LocalTaskAuthority::resolve` returns the
authenticated origin actor, current policy snapshot, sources, exact payload
digest and consumer incarnation after kernel/supervisor verification. Consumers
must bind the complete ControllerTask parsed from the exact envelope, not a
prompt-only digest. See `../ansible-mesh-core/LOCAL_AUTHORITY_INTEGRATION.md`.
The hotel/SDK resolve RPC and model-runtime adapter are still uninstalled.

Rust interface: `privacy_dispatch::DispatchPrivacyAuthority::context_for(&ControllerTask)
-> Option<VerifiedDispatchContext>`, with `actor: AuthenticatedAgent`,
`policies: PolicySnapshot`, and `sources: Vec<String>`. The server issues actor
identity/roles from its verified session, resolves current policy from PolicyStore,
and binds the complete typed task and immutable source manifest. Any summary,
tool definition/result, history, attachment, microphone input or synthesized text
must have its source provenance accounted for. Unknown, missing, stale or changed
task context returns None. JSON actor_id/private/egress/revision flags cannot issue
this context; policy read access never grants external processing.

Install `guarded_registry(Vec<(Arc<dyn ModelProvider>, ProviderBoundary)>,
Arc<dyn DispatchPrivacyAuthority>)` so all registry selection, retries and fallback
candidates are decorated. Boundary is the actual server-configured endpoint:
LocalTrusted, External or Unknown. A local proxy calling ElevenLabs is External;
Apple local execution is LocalTrusted only if it actually stays local. Unknown
denies. Preferences select among authorized paths and grant no eligibility.

`TaskKind::AudioTranscribe` checks ProcessingOperation::SpeechToText;
`TaskKind::VoiceSynthesize` checks TextToSpeech. `invoke` and `invoke_streaming`
check immediately before every attempt. A private source or inherited private
ancestor denies every external operation, including STT and TTS. Nonprivate
external operations require their explicit operation permission as well as read
ACL. A denied local or cloud attempt must never reach an undecorated fallback.
`VoiceDialogue` and `ResponseGenerate` native-live calls explicitly deny here;
those sessions need their own enforced boundary before activation.

`request_id` echo is correlation, not authorization. The authenticated server
must bind it to the trusted session, turn, full task digest and source manifest.
`turn_cancel_v1` must resolve only that session's authorized turn. A cancelled or
replaced turn must not start another provider attempt, publish more audio/text,
or resurrect on retry. Cancellation does not grant or relax privacy eligibility.
The client must not claim it authenticated the policy merely by echoing an ID.

Current decorator tests establish fresh authorization at each invocation and
revocation between attempts. They do not establish cancellation transport behavior
or a revocation barrier for an already running provider stream. The new
PolicyCommitLease ordering barrier is for canonical graph commits only. Voice
backend owners must explicitly choose and validate in-flight revocation/cancel
semantics instead of claiming that graph barrier protects voice streams.

IPC prerequisite: add a server-resolved authority handle to InboundTask and retain
it through parked-task delivery/SDK decoding. Existing source_node/task_id/task_json
alone cannot issue VerifiedDispatchContext. Production authority and registry
installation remain absent; fail closed rather than derive eligibility from the
client's selected voice, provider preference or request_id.
