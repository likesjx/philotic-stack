# Decisions fixtures

Inputs for the pure unit tests in `src/decisions.rs`. Provenance matters, because a
fixture only proves what it was made from.

| File | Provenance |
|---|---|
| `recorded_openrouter_noul.json` | **Recorded live 2026-09-19** from `POST https://openrouter.ai/api/alpha/decisions` with `typesafe/jev-1.13` and one `noul` question. Only the generation id is redacted. |
| `recorded_openrouter_mixed_request.json` | **The exact request body sent 2026-09-21** for the call below: one `noul`, one `choice`, one `score`. A test asserts our wire adapter reproduces it (JSON-equal, same key order). |
| `recorded_openrouter_mixed.json` | **Recorded live 2026-09-21**, the response to the request above (HTTP 200, 0.51 s). Only the generation id is redacted. Confirms the `choice` distribution, the `score` legend echo and index-keyed probabilities. |
| `docs_native_mixed.json` | **Built from TypeSafe's HTTP API docs** (`docs.typesafe.ai/api.md`) for the NATIVE transport, not recorded (no native key). Its answer shapes match the recorded OpenRouter body. Replace it if a native key is granted. |
| `canonical_request_heal_classify.json` | The canonical request from `docs/architecture/DECISIONS_MODEL_PROPOSAL.md`, with concrete strings. |

To record another body, use a **dedicated** key and one synthetic sentence (never operator
data): `PHILOTIC_DECISIONS_LIVE_KEY=<key> cargo test -p decisions-client --lib live_smoke -- --ignored --nocapture`
exercises the real client; for raw bodies, POST the request file with `curl`. Redact the
`gen-dec-…` id before checking a body in.

The parser maps a score `legend` by its text and falls back to position; a deviation sets
`legend_mismatch` rather than failing, so a body that disagrees with the recorded ones shows
up as a flag, not as silent fallbacks.
