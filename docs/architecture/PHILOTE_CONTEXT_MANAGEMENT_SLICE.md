# Philote context management: bounded text-call slice

Status: isolated implementation; not deployed. Base: `78eb2f32303823379d6b8c089000e19bf1f69d62`.

## Problem and invariant

Philote already completed turns by recording the main `WorkingTurn.user_content`
and assistant reply. The context inflation originated in request assembly, not
that completion path. This change does not migrate or delete hotel history,
checkpoints or long-term memories. `recent_turns` remains a bounded working cache;
selection works on clones and never writes its trimmed view back to it.

Each request carries a raw current message and structured context. The router's
`ControllerTask::composed_prompt_text` is the provider renderer. Structured active
turns suppress the redundant legacy flat envelope. Dialogue lives in one
structured lane; recall does not also occupy generic memory; tool payloads live
in one call/result lane rather than the working-instruction projection.
Legacy flat-only callers remain supported.

Initial dispatch, tool/paracrine continuation, transcription re-entry, and
approval/denial steering all use the authority-aware request assembly. Runtime
re-entry supplies its actual projected tools; diagnostic projection builders
are not dispatched directly. Operational Rules (including exact role names),
approval guidance, and one UTC/operator-local clock item are mandatory structured
instructions. The optional Knowledge layer contains memory seed/stewardship
context rather than approval policy.

## Selection and accounting

The ordinary batch/stream dispatch retry loop calls
`invoke_ordinary_provider_attempt` for every attempt, including rotated providers.
That boundary prepares a fresh request-local clone before invocation; deterministic
selection leaves the original unchanged across retries and fallback attempts. HTTP text paths for OpenAI /
OpenRouter, Anthropic, Gemini batch/streaming, Ollama and MLX check their final
serialized request envelopes before generation. Native realtime/voice,
transcription, embedding and decision paths are outside this first slice.

`provider_options.context_limits` accepts positive integer category budgets:
`input_tokens`, `output_tokens`, `mandatory_tokens`, `history_tokens`,
`memory_tokens`, `tool_result_tokens`, `tool_schema_tokens`, `user_tokens`.
The same explicit overrides can be authored through the existing
`agent.configure` path `settings.context_request_limits` (null resets defaults);
this changes source behavior only, not production settings. Overrides persist
in session settings and flow into initial calls, retries and fallbacks.
The name denotes token budget units, but accounting is explicitly labeled
`conservative_utf8_bytes_plus_framing`, not an exact tokenizer count or billed
usage. Existing embedding tokenizers are not a valid tokenizer for arbitrary
remote generation models. No tokenizer downloads or model calls occur here.

Defaults: input 32768, output 4096; user and mandatory sections 16384 each;
history, tool results and schemas 8192 each; memory 4096. Input is additionally
bounded by the local model context ceiling minus the reserved output allowance.
Known model families have conservative context/output ceilings; unknown models
fall back to 16384 context / 4096 output and cannot request a larger unverified
window. These are local ceilings, **not live provider-capability attestations**.
Exact catalog-driven capabilities and tokenizers are integration dependencies.
Do not describe these hardcoded prefix matches as resolved model capabilities
or token-accurate limits. In particular, Gemini resolves its model in the URL;
final body accounting does not resolve that endpoint's configured model.
Final HTTP accounting also checks the serialized model and every configured
OpenRouter fallback candidate, and rejects output fields exceeding the reserve.

An explicit valid `max_tokens` or `max_completion_tokens` overrides output.
Conflicting limits, fractional/negative/zero output budgets and long-task
budgets exceeding the local context or output ceiling fail. A long task can
raise category budgets through `context_limits`; mandatory content is never
silently reduced to make a request appear successful.

The selector keeps coherent recent user/assistant pairs, deduplicates optional
memory, and drops whole old tool call/result pairs. The newest tool result,
current user message, identity/instructions and projected tool schemas are
mandatory in this slice. An oversized mandatory item fails before generation.
Schemas are not arbitrarily dropped because the selector cannot infer which
plan/tool dependencies are optional. History selection prioritizes recency,
not an unimplemented semantic salience model. Final JSON escaping, provider
wrappers, instructions and schemas are checked; a final accounting overrun
fails rather than sending an over-budget envelope.

Original history remains available for future selections. No textual summaries
are generated. Any future summary must be a separate derived artifact with
source turn IDs, producing algorithm/version, source policy references and
inherited privacy; it must never replace original user/assistant records or be
stored as if the human said it. Count-only selection receipts are diagnostics,
not conversation messages.

## Privacy and integration

`RecallAuthority` is an adapter seam, not a new privacy-policy authority. It must
consult existing authenticated actor/resource/provider policy on every call.
No adapter, missing/blank principal, or a denied record means omit recall.
Vault names, memory tags and model-produced annotations cannot grant access.
Agent-graph projection requires its own explicit authorization.

The base does not contain the active privacy worktree's authority. Normal
assembly therefore passes no adapter and omits recalled memories / agent graph
while preserving their durable records. This safe default changes recall
availability and must be reviewed during integration. Wire the real authority
through `model_request_payloads_with_recall_authority` before re-enabling it;
policy-version invalidation and cross-provider reauthorization belong to that
adapter. Do not describe this isolated source change as production protection.

Source-side recall selection is item/byte bounded and deduplicates by
vault+record ID and normalized content. Records are admitted whole, including
provenance; no live recall, summarization or embedding is exercised in tests.
Rendering preserves complete content and stored metadata, including qualifiers.
It counts UTF-8 bytes for the preamble, full item, numbering and warnings; an
item that cannot fit is omitted whole. No 500-character slice or outer text
truncation remains. The existing `recalled_memory_chars` setting name is retained
for compatibility, but this render/admission boundary uses a conservative byte
budget.

The routing layer still needs catalog-backed resolved model facts rather than
local family ceilings, catalog-backed operator capability validation, and tighter native
realtime/voice accounting. No IPC dispatch/schema files are edited, preserving
the active architecture work. The separate Beacon refresh fix is not included.

## Diagnostics and verification

Assembly logs section counts, selected/dropped items, estimate method, input and
output limits, and serialized bytes. Existing attempt/dispatch timing remains
in the router. Touched provider/Philote debug paths no longer dump prompt bodies;
Gemini HTTP failure logs contain status/error code and byte count only.
This is not a repository-wide logging/privacy audit.

For ordinary text attempts, preparation injects `max_tokens`: OpenAI/OpenRouter
serializes it directly, Anthropic sends `max_tokens` for batch/streaming, Gemini
sends `generationConfig.maxOutputTokens` for batch/streaming, Ollama's compatible
HTTP endpoint sends `max_tokens`, and MLX passes `ChatRequest.max_tokens`.
OpenAI's realtime websocket text route can receive prepared options but does not
implement this slice's final accounting or enforce the injected shared output
cap. Native response/voice, media analysis, transcription, synthesis, embeddings
and decisions remain outside this slice. Local tests do not attest that any
remote endpoint or local inference server honors its serialized output field.

Regression coverage includes multi-turn raw-message persistence, retries,
repeat selection/recall, absent authority/identity, memory duplication, canonical
Philote-to-OpenRouter serialization, complete tool pairs, oversized current user,
latest result and schema failures, mandatory instruction preservation, explicit
long-output limits, unknown model overrides and final serialization boundaries.

The dispatch regressions enter the runtime ordinary-attempt boundary and capture
real local HTTP requests: configured OpenRouter default, 4096 output cap, repeated
failed attempts with identical wire payloads, and a different fallback model. A
streaming probe verifies the same preparation boundary and mandatory overflow.
They do not run the entire IPC controller or the Philote fallback-tier coordinator.
Re-entry regressions exercise the shared runtime assembly boundary with initial,
transcription, approval and denial fixtures. Additional cross-crate tests verify
that required Rules/policy/clock survive optional-memory selection and that
authorized records retain trailing qualifiers or are omitted whole with framing.

Local verification results are recorded with the source fingerprint in the
workspace verification artifact.
Tests use synthetic fixtures and local mock servers, not live model calls.
