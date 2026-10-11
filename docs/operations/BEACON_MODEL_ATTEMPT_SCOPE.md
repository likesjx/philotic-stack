# Beacon model-attempt diagnostics (draft, not deployed)

Base: develop a81507134d19d3cc44b589f8e71911cc20730fc3. The deployed
57b13ba0087722a2de062ff86710ae64a510d445 is separately verified by the parent;
this branch has not been installed or used against personal sessions. Hosted
PR660 CI passed on e0303810 before the normal-request regression revision;
the revised head must independently pass CI.

## Change and diagnostic contract

Philote attaches projected observation-only metadata to ordinary model tasks:
initial, same-provider retry, fallback, or origin probe; route reason, actual
requested target role and bounded ladder position. This does not select routes,
change ordering, override pins or create dispatch authority. Controller parsing
projects a fixed label set and strips this metadata before provider invocation.
Old controllers can ignore the additive task field. New controllers receiving
old philote tasks honestly label route metadata unknown.

The ordinary model-router invocation boundary emits started plus success/failure
records; dropping a cancelled future emits cancelled_or_timeout, without claiming
an HTTP request reached a provider. Records have a random attempt identifier,
domain-separated hashed session/turn correlation and requested-model fingerprint,
optional resolved-model fingerprint from provider output metadata, fixed provider/
role/route/error labels, ordinal, HTTP status, retryability and terminal outcome
for that attempt. No prompt, reply, error.message, URL, header, raw model label,
credential or private memory content enters this projection. Provider-side hidden
OpenRouter fallback attempts cannot be reconstructed from this boundary; missing
resolved-model metadata remains null. These records do not prove reply delivery.

In-memory retention is at most128 records, with15-minute expiry checked on new
observations. Logging is capped at64 events per minute per controller; suppressed
counts appear on the next emitted record. Lock contention drops telemetry rather
than delaying routing. Sampling can omit events; an absent record is not proof
that no attempt occurred. Existing external log retention is unchanged and must
be reviewed before deployment; no new disk database or exporter is introduced.

Ollama retains at most 4096 diagnostic response bytes and emits fixed error markers
rather than provider response bodies. Recognized404 missing-model errors receive
model_unavailable/nonretryable/switch_provider. Endpoint404 stays invalid_request.
Neither case retries an identical missing request. Existing philote pin handling
still prevents provider escalation when explicitly pinned.

## Explicitly unavailable normal Beacon qwen fallback

This PR does not wire Beacon to qwen. The source model binding was removed
because normal Beacon fallback requires cognitive channels (spoken_text,
memory_candidate, active_plan), and can require tools. Plain-text Ollama cannot
satisfy that contract. Dropping channels/tools would change semantics, so no
such adaptation is attempted. Source role configuration matches the base;
the reported live ladder, overrides and provider defaults are not rewritten.

Presence is not capability acceptance. An explicitly requested small model fails closed
before network for tools, structured response channels/format, or nontext
modalities. Wire size is conservatively bounded to8192 bytes and output to256
requested tokens. Existing context preparation/admission remains in force.
These are tighten-only limits, not proof of model capability or quality.
A normal Beacon turn remains unavailable through qwen even without tools.
It receives explicit OLLAMA_FALLBACK_CAPABILITY_UNVERIFIED, classified as
unsupported_capability/nonretryable/switch_provider. Required channels remain
intact and HTTP invocation is not attempted. This PR fixes diagnostic visibility
and absent-model error classification, not the missing local model or actual
iPhone outcome. A usable cognitive local fallback requires a separately reviewed
contract-preserving adapter and capability acceptance.

The heal classifier's gemma3:4b default remains unchanged. Its existing
PHILOTIC_HEAL_MODEL configuration can select another model, but setting it to
qwen requires independent classification/schema/quality acceptance first. No
such acceptance, provider outcome, Gemini cause or billing diagnosis is claimed.

## Verification and proposed publication scope

Local Python test verifies no unverified qwen binding and unchanged source ladder.
Rust tests are authored for malicious diagnostic projection, hashing, bounded
retention/rate limits, missing-model classification, synthetic HTTP success and
404 sanitization, and unsupported tool/structured/budget requests. Existing
context-isolation/streaming helper fixtures remain wired to the updated helper.
The normal-Beacon regression serializes the actual ModelRequestPayload with
required channels, both without and with a tool. It checks explicit refusal,
zero HTTP requests and an unchanged original payload.
No Rust compiler/cargo/rustfmt was found on PATH or inspected standard installed
locations. Rust compilation, formatting and tests run in hosted CI; each amended
head needs independent review and exact-head CI evidence. Static checks do not
substitute for them.

Approved source scope: update codex/beacon-model-attempts-local and draft PR660
against develop; existing credential-free PR/source workflows run on updates.
No workflow files, merge, manual dispatch, release, deployment, grants or live
provider calls are included.

## Tested deployment scope and rollback gate

No deployment is presently test-green. After exact-head Rust CI and independent
review, publish immutable Linux artifacts for philote and affected model-controller
binaries. Old aiua can relay the additive task metadata; exact compatibility must
be proven in synthetic IPC acceptance before any live replacement. The current
codebase includes later changes beyond57b13ba, so do not roll out all develop by
implication. Use a reviewed hotfix-compatible patch or explicitly approved full
artifact with complete state/writer inventory and shared historical/global fences.

Before live approval, establish qwen capability/context acceptance using public
synthetic tasks, exact selected model configuration, log retention, byte hashes,
writer stop/restart scope and whole opaque state/binary rollback. Review any heal
model setting separately; do not apply units/accounts/network changes implicitly.
Retain previous binaries and exact role/model config; on failure restore both
approved state and config/binaries under the coordinated locks. Do not use broad
aiua load as a narrow model setter. Do not reset user sessions speculatively.
