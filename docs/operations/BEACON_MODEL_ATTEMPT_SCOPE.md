# Beacon model-attempt preparation (local, not deployed)

Base: develop a81507134d19d3cc44b589f8e71911cc20730fc3. The deployed
57b13ba0087722a2de062ff86710ae64a510d445 is separately verified by the parent;
this branch has not been built, installed or used against personal sessions.

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

## Inventory-bound proposed local fallback and limits

Only Beacon's source model binding adds model.ollama=qwen2.5:1.5b, matching the
reported installed inventory. The source ladder stays OpenRouter then Ollama;
this does not rewrite the reported live OpenRouter/Gemini/Ollama ladder. No
model downloads, global-default rewrites or hardware changes are proposed.

Presence is not capability acceptance. This selected small model fails closed
before network for tools, structured response channels/format, or nontext
modalities. Wire size is conservatively bounded to8192 bytes and output to256
requested tokens. Existing context preparation/admission remains in force.
These are tighten-only limits, not proof of model capability or quality.
A normal tool-enabled/structured Beacon turn can therefore remain inadmissible;
this branch does not claim that the actual iPhone failure is repaired.

The heal classifier's gemma3:4b default remains unchanged. Its existing
PHILOTIC_HEAL_MODEL configuration can select another model, but setting it to
qwen requires independent classification/schema/quality acceptance first. No
such acceptance, provider outcome, Gemini cause or billing diagnosis is claimed.

## Verification and proposed publication scope

Local Python test verifies the actual public YAML binding and unchanged ladder.
Rust tests are authored for malicious diagnostic projection, hashing, bounded
retention/rate limits, missing-model classification, synthetic HTTP success and
404 sanitization, and unsupported tool/structured/budget requests. Existing
context-isolation/streaming helper fixtures remain wired to the updated helper.
No Rust compiler/cargo/rustfmt was found on PATH or inspected standard installed
locations. Rust compilation, formatting and test execution remain blocking gates.
Static checks do not substitute for them.

Proposed next approval: push only codex/beacon-model-attempts-local and open one
draft against develop, allowing existing credential-free PR Check/source tests
and scoped cargo tests for ansible-mesh-core, model-router and philote. No workflow
files, merge, dispatch, release, deployment, grants or live provider calls are
included. Verify hosted workflows on the exact proposed head before publishing.

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
