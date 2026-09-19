# Decisions fixtures

Inputs for the pure unit tests in `src/decisions.rs`. Provenance matters, because a
fixture only proves what it was made from.

| File | Provenance |
|---|---|
| `recorded_openrouter_noul.json` | **Recorded live 2026-09-19** from `POST https://openrouter.ai/api/alpha/decisions` with `typesafe/jev-1.13` and one `noul` question. Only the generation id is redacted. |
| `docs_native_mixed.json` | **Built from TypeSafe's HTTP API docs** (`docs.typesafe.ai/api.md`), not recorded. Covers `choice`, `noul` and `score` (index-keyed `legend` and `probabilities`). Replace it with a recorded body at the first D1 smoke. |
| `canonical_request_heal_classify.json` | The canonical request from `docs/architecture/DECISIONS_MODEL_PROPOSAL.md`, with concrete strings. |

When a live smoke records a real `choice` or `score` response, add it as `recorded_*.json`
and point the matching test at it. The score `legend` check is deliberately strict (it must
echo exactly the level text we sent); relax it only against a recorded body that disagrees.
