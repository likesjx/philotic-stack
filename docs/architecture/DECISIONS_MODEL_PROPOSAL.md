---
title: Decisions Model — a typed, calibrated System One judge beside model-router
doc_type: proposal
domain: tooling-execution
status: proposed
disposition: proposed
last_updated: 2026-09-19
verification_level: none
tags:
- decisions
- system-one
- jev
- typesafe
- openrouter
- model-router
- shadow-mode
- calibration
- envelope
related_docs:
- MODEL_GRAPH_AND_CONTEXT_1_PROPOSAL.md
- MODEL_CONTROLLER_PROPOSAL.md
- LOCAL_ONNX_INFERENCE_PROPOSAL.md
- LOCAL_ADMIN_FALLBACK_MODEL_PROPOSAL.md
- PHILOTIC_WEB_HARDENING_PROPOSAL.md
- ARCH_RULES.md
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md
proposal_id: decisions-model
implements: []
implemented_by: []
active_seams:
- decisions-envelope
- decisions-shadow-heal
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Decisions Model — a typed, calibrated System One judge beside model-router

## Goal

Add a fourth kind of model capability next to generate, transform and embed: a **decision**. The caller sends a piece of state and a set of typed questions (yes/no, choose one of N, score on an ordered scale). The provider returns typed answers with calibrated probabilities and no free text. The first concrete provider is TypeSafe AI's **Jev**, reachable natively or through OpenRouter. The capability is defined provider-neutrally so a local classifier can fill the same envelope later.

This is a research-backed proposal. **Slice D0 (the pure envelope types and wire adapters) is implemented in `ansible-mesh-core/src/decisions.rs`; nothing is wired into a runtime yet. OpenRouter access was verified with one probe on 2026-09-19 (see Verified access).**

## Why this matters

The stack makes many small discrete judgments. Today each one is either a keyword/substring list, a hand-set threshold, or a full generative model turn that is then parsed. The keyword lists have a live defect history (DEF-093, DEF-113..117, DEF-156, DEF-158, DEF-159), and there is no calibrated confidence anywhere in the repo: no Platt scaling, no ECE, only hand-made bands (the `0.80 / 0.55` duplicate guard, Muninn's `relevance_band`, `ApprovalRiskHint`, autonomy postures).

A decisions provider gives a typed answer plus a distribution, which is exactly the shape the existing escalation bands want.

## What Jev is (external facts, vendor-sourced)

Sources are listed under References. Speed and cost numbers are **vendor claims** and unverified by us.

- Non-generative "System One" model from TypeSafe AI (announced 2026-09-15). Input is a `state` plus a map of typed questions. Output is a map of typed answers. No text generation, no parsing.
- Three primitives: `noul` (yes/no, returns a probability), `choice` (up to 255 options, returns the winner, the full distribution and a confidence), `score` (2–10 ordered levels, returns a weighted score, distribution and confidence). Many questions share one call and are evaluated in parallel and in isolation.
- Claimed 70–500 ms, $0.042 per million input tokens, free output, trained with "Reinforcement Learning for Calibrated Decisions". Limits: ~64 k tokens per request (~32 k for `state` plus the longest question), text only, 250 k tokens/s and 1,200 requests/min in early access.
- **Known failure modes** (TypeSafe's own Jev 1.13 page): literal reading; unreliable counting; arithmetic and date comparison; large irrelevant state hurts accuracy; **not resistant to injected instructions**; contradictory instructions and criteria confuse it; no invariants between question types.
- Independent coverage notes "hallucination-free" only means a valid type, not a correct answer.
- **Two transports, neither is chat completions:**
  - Native: `POST https://api.typesafe.ai/v1/systemone`, bearer key, model `jev-latest`.
  - OpenRouter (alpha): `POST https://openrouter.ai/api/alpha/decisions`, model `~typesafe/jev-latest` (the alias; note the tilde) or pinned `typesafe/jev-1.13`, 32 k context. **The bare `typesafe/jev-latest` that a third-party issue used returns HTTP 400 `does not exist` (verified).** Response adds `id`, `provider`, `usage.cost`. `noul` `criteria` requires both `true` and `false` when present (third-party report). Reported to return HTTP 400 from `/api/v1/chat/completions` (not tested by us).
- Availability: the native API is early access / waitlist. Zero data retention is enterprise-only. **Verified 2026-09-19: an ordinary OpenRouter key reaches Jev with no waitlist** (see Verified access).

## Verified access (2026-09-19)

One probe with the operator's own OpenRouter key and a single benign sentence (`"Help! My payouts have been failing for 3 days."`, one `noul` question):

- `POST /api/alpha/decisions` with model `typesafe/jev-latest` returned **HTTP 400** `Model typesafe/jev-latest does not exist`. The key was accepted; only the slug was wrong.
- The same request with `typesafe/jev-1.13` and with `~typesafe/jev-latest` returned **HTTP 200**, resolved to `typesafe/jev-1.13-20260917`, `answers.is_urgent.noul = 0.95`, `provider: "TypeSafe"`, and an `id` starting `gen-dec-`.
- Response body: `{"model", "answers", "usage": {"input_tokens": 307, "output_tokens": 23, "cost": 0.000012894}, "id", "provider"}`. `cost` equals input tokens × $0.042 / M. `output_tokens` is **non-zero but free**, so the trace must record both and must not assume zero.
- Latency was 0.29 s and 0.52 s wall time (curl total, including DNS and TLS from a Mac). Two successful calls, not a benchmark.

Consequences:

- The third-party issue's bare slug is wrong. The OpenRouter adapter maps to `~typesafe/jev-latest` or a pinned `typesafe/jev-1.13`.
- Shadow sites should **pin** `typesafe/jev-1.13` so a moving alias cannot change decisions silently, and every trace records the **resolved** `model` from the response (`typesafe/jev-1.13-20260917`). Calibration is only meaningful per model version, exactly like `model_gen` on embeddings.
- No waitlist is needed for an ordinary OpenRouter key. The native TypeSafe waitlist is now optional.

A second probe on 2026-09-21 (dedicated key, one synthetic sentence) sent a `noul`, a `choice` (3 options) and a `score` (3 levels) in one call and returned HTTP 200 in 0.51 s, resolved to `typesafe/jev-1.13-20260917`. It confirmed, against a **recorded** body: `choice` returns the full distribution including zero-probability options; `score` returns `legend` echoing exactly the level strings we sent, `probabilities` keyed by level index, and `score` equal to the probability-weighted index (1.88 = 0·0.01 + 1·0.10 + 2·0.89); usage was 436 input and 69 output tokens with `cost` $0.000018312, input-priced. The same request through the real `DecisionsClient` (an `#[ignore]`d live test) answered in 609 ms with no legend mismatch. Both bodies are checked in as fixtures.

Not yet tested: the native transport (no key), the 32 k state limit, error bodies for 422 / 429 / 529, and `noul` criteria with only one side present.

## What the repo has today (verified by reading code unless marked)

- `ModelProvider` (`crates/model-router/src/controller.rs:1763`) is `supports(&ControllerTask)` plus `invoke() -> ProviderOutput`. It is not text-shaped: `TaskKind::Embed` and `ProviderOutput::Embedding` prove a non-text kind works. The extension points are closed enums that check each other: `TaskKind` (`:11`), `RequestClass` (`:40`), `ControllerTask::validate` (`:760-802`), `ProviderOutput` (`:1224`), `ControllerResponseEnvelope::from_output` (`:1283`, exhaustive match), `AuxTaskKind::from_task_kind` (`aux_model.rs:75`, exhaustive), plus `model_oracle.rs` capability seeding.
- `ControllerTask` (`:252`) has **no `state` or `questions` field**. `Embed` borrows `prompt`. `provider_options` is provider-specific passthrough.
- `ResponseTrace` (`:1266`) carries only `provider`, `model`, `voice`. `usage`, `cost` and `latency` appear nowhere in the router.
- `emit_text_response` (`runtime.rs:1540`) sends every reply as an `EmitTask` with `action: "model_response"`, `agent_action.kind: "respond"` and hand-lists the `trace` fields (`:1565-1569`). Philote's turn handler calls `fail_active_turn` on any `model_result.error` (`turn_loop.rs:1584-1596`). **A decision reply on this path would fail the user's active turn when a decision errors.** Whether replies without a turn are filtered earlier was not traced.
- `isolate_aux_failure_from_cognitive_ladder` (`runtime.rs:1672`) lists exactly three aux capabilities. A new capability not on the list makes a failed decision escalate the conversational fallback ladder.
- `OpenAIProvider` hard-codes `/v1/chat/completions`, `/v1/embeddings` and `/v1/models` (`providers/openai.rs:1011, 1117, 1175`), so the existing `model-controller-openrouter` cannot reach `/api/alpha/decisions`. The API key, vault entry and base URL `https://openrouter.ai/api` are reusable.
- `serde_json` is `"1.0"` with no `preserve_order` in any direct `Cargo.toml`. A transitive dependency could still enable it; verify with `cargo tree -e features`. Ordered `score` levels must not depend on object key order.
- Precedent for rollout: the model-oracle shadow slices (PR #264, #287, #291). `PHILOTIC_SHADOW_ORACLE` (`model_oracle.rs:76`, default off, log-only) writes `oracle_pick` and `oracle_agreement` into `router_traces` (`router_trace.rs:53-63`); validated live on mbp-jane.
- Precedent for a typed advisory: `Context1Advisory { approval_risk_hint, recommended_preapproved_classes, rationale }` (`philote/src/session/types.rs:398`). It is only constructed in test modules; there is **no production producer**.
- No "system one", dual-process or cheap-tier doc exists in `docs/architecture` (checked by grep). Related but distinct: `LOCAL_ONNX_INFERENCE` (fast path for embeddings), `LOCAL_ADMIN_FALLBACK_MODEL`, `MODEL_GRAPH_AND_CONTEXT_1`.

## The envelope

One definition, two carriers. The request, response and error types live in `ansible-mesh-core` so the IPC controller and an in-process hot-path caller use the same structs.

### Request

Sent as the `task_json` of an `EmitTask` to a `model.decisions` controller role. `kind` is a new string parsed in `ControllerTask::from_value` next to `"text.embed"`. `ControllerTask` gains a typed `decisions: Option<DecisionsRequest>`; questions are **not** carried in `provider_options`.

```json
{
  "kind": "decisions.evaluate",
  "request_class": "judgment",
  "session_id": "…", "turn_id": "…",
  "provider": "typesafe",
  "model": "jev-latest",
  "decisions": {
    "site": "heal.classify",
    "state": { "guest": "…", "log_tail": "…" },
    "questions": [
      { "id": "severity", "type": "choice", "instructions": "…",
        "options": [ { "key": "critical", "description": "…" }, { "key": "high", "description": "…" } ] },
      { "id": "needs_restart", "type": "noul", "instructions": "…",
        "when_true": "…", "when_false": "…" },
      { "id": "harm", "type": "score", "instructions": "…",
        "levels": [ { "key": "none", "description": "…" }, { "key": "mild", "description": "…" } ] }
    ]
  },
  "deadline_ms": 1500,
  "effective_rights": ["decisions.evaluate"]
}
```

- `site` is **required**: the call-site id that keys shadow rows and traces, the same way `oracle_pick` is keyed today.
- Questions are an ordered array with unique ids. The transport adapter turns them into TypeSafe's map. `score` levels are an ordered array, never an object.
- `validate()` enforces: unique ids; choice ≤ 255 options; score 2–10 levels; two size bounds, both estimated at 3 bytes per token (conservative, so it rejects early; the provider's 422 stays authoritative): `state` plus the longest question ≤ 32 k tokens on every transport, and the whole request ≤ 64 k native / 32 k OpenRouter; `request_class: judgment` only with `decisions.evaluate`.

### Response

```json
{
  "capability": "decisions.evaluate",
  "content": "decisions:heal.classify",
  "result": {
    "site": "heal.classify",
    "answers": {
      "severity":      { "type": "choice", "choice": "high", "probabilities": { "high": 0.82, "critical": 0.11 }, "confidence": 0.82 },
      "needs_restart": { "type": "noul", "noul": 0.07 },
      "harm":          { "type": "score", "score": 0.4, "probabilities": { "none": 0.6, "mild": 0.4 }, "confidence": 0.7 }
    }
  },
  "artifacts": [],
  "trace": { "provider": "typesafe", "model": "jev-1.13.0", "transport": "native",
             "latency_ms": 118, "usage": { "input_tokens": 307, "output_tokens": 23, "cost_usd": 0.0000129 } },
  "provider_output": null
}
```

- `content` is a machine string, **never answer text** (`Embed` already puts `model_gen` there).
- The envelope carries **distributions, not verdicts**. The act / confirm / escalate mapping is deterministic code at each call site, in the style of the `0.80 / 0.55` duplicate guard. Thresholds are per question and are never shared across primitives (TypeSafe documents no invariants between them).
- The returned choice is validated against the request's options; an unknown choice is an error even though the vendor claims it cannot happen (a third-party probe lists "unknown choices" as a failure class).
- `ResponseTrace` grows `transport`, `latency_ms` and `usage`. This touches the struct and the hand-written field list in `emit_text_response`.

### Errors and the reply path

- Typed error classes: `unavailable`, `timeout`, `rate_limited` (429/529), `invalid_request` (400/422), `auth` (401/403), and `invalid_response` (the provider answered but the answer is unusable: malformed, unknown choice, missing or extra answers, out-of-range probability). Every one means **"use the deterministic decision"**; none may fail a turn. D0 implements these as `DecisionsErrorClass` with `classify_http_status`.
- `decisions.evaluate` joins the list in `isolate_aux_failure_from_cognitive_ladder`, and `AuxTaskKind::from_task_kind` gets an arm.
- Replies use a dedicated action (`decisions_response`) with a correlation id and a dedicated handler. They must never enter the `model_response` turn-reply path.
- **As built in D1** (`model-router/src/decisions.rs`, hook in `runtime.rs`): a decisions task is recognised from its **raw `kind`** before task parsing, provider-config load, the stub short-circuit and the fallback ladder, because `emit_failure` (which those stages call) always answers with `model_response`. So even an unparseable decisions task, or a failed config load, gets a typed decisions reply. The reply body has `action`, `capability`, `correlation_id`, `site` and a nested `decision: {status, result, trace | error}`; there is deliberately **no** top-level `agent_action`, `content` or `error`, and a test asserts it. The aux-isolation list entry is kept as belt and braces. The `model_response` → `fail_active_turn` hazard itself is still untested end to end; the design avoids it rather than proving it (open question 3).
- Provider timing: keep the invariant `attempt_policy().total_secs × retry_policy().max_attempts < 120 s`, but set decisions much tighter (single-digit seconds, one retry).

### Transport mapping

| Canonical | Native TypeSafe | OpenRouter |
|---|---|---|
| Path | `POST /v1/systemone` | `POST /api/alpha/decisions` |
| `model` | `jev-latest` | `~typesafe/jev-latest`, or pinned `typesafe/jev-1.13` |
| Questions | array → map by id | same |
| `choice` options | array of `{key, description}` → `criteria` **map** `key → description \| null` (order kept by a typed serializer) | same |
| `score` levels | array → `criteria` **array of strings** (the description, else the key); the answer returns `legend` and `probabilities` keyed by **level index**, which the adapter maps back to level keys and checks against what was sent | same |
| `noul` criteria | `when_true` / `when_false`, both optional | both required when present; fill neutral empty string |
| Cost | not returned | `usage.cost`, plus `id`, `provider` |
| Request budget | 64 k tokens | 32 k tokens |
| `state` + longest question | ≤ 32 k tokens (vendor docs) | ≤ 32 k tokens (enforced: it is the tighter bound) |

## Where it applies (candidates, ranked)

Blast radius abbreviated as BR. Line references were read at `d624e5b8`. Items marked *(survey)* come from a codebase survey and were not all individually re-read.

| # | Decision point | Today | Typed question | Hot path | BR / abstain value |
|---|---|---|---|---|---|
| 1 | Say-do gate `philote/src/turn_loop.rs:2915`, call site `:1744-1790` *(survey)* | ~120 substring phrases across four `reply_*` predicates | promise / past-claim / delivery-claim / none, plus P | one gate per text-only reply | live defects DEF-113..117, DEF-156. `SayDoDisposition::Trailer` is a ready abstain. |
| 2 | Should-plan and tool projection `philote/src/plan_eval.rs:1579`, `session/mod.rs:5674` *(survey)* | keyword lists | chitchat / question / request / multi-fact / outcome; tool relevance P | every user turn | DEF-093 class. Needs a fail-open tools threshold. |
| 3 | `memory.recall` MCP reflex `philote/src/mcp_handling.rs:166` | escalates only when results are empty | "do these engrams answer the query?" score | MCP ladder | the known relevance gap. The score exists in `memory-core/src/recall.rs` but the reflex ignores it. |
| 4 | Approval intent and risk tier `philote/src/tool_exec.rs:422-463` *(survey)* | substring "yes" (matches "yesterday") | approved yes/no + P; risk Low/Med/High | tool path | highest consequence per false positive; abstain means ask again. |
| 5 | LifeGraph duplicate guard and audit `closable` `data-memorygraphrag/src/hygiene.rs`, `audit.rs` *(survey)* | Jaccard 0.80 / 0.55, cosine 0.90 | same / related / distinct; closable / needs_judgment / leave | write-time and batch | DEF-158, DEF-159. Dates stay in code. |
| 6 | Lived-fact classifier `philote/src/life_capture.rs:255` *(survey)* | keyword vocabulary | OpenLoop / Commitment / Goal / Habit / Event / Decision / none | post-turn | already abstains on ambiguity. |
| 7 | Model difficulty for routing (`RouteNeed`, `model_oracle.rs:146`) | health-based only | difficulty score | routing | new capability; no defect history. See "Assessment: Jev as a model router" below. |
| 8 | Open-request, shame-tone, correction detectors *(survey)* | keyword lists | yes/no | post-turn | low BR. |

**Best first pilot: `heal-dispatcher` classify** (`crates/heal-dispatcher/src/main.rs:804`). It is off the hot path (30 s poll), already a typed JSON classifier (`gemma3:4b`, `format: json`) with a circuit breaker and a severity floor (`gate_llm_action`, `:854`), and needs no calibration data on day one.

**Not targets:** `philote/src/reflex.rs` (a typed rule table, no ambiguity), media and voice routing (config lookups), `prompt-guard` and `exec-guard` (deliberately model-free), cron payload validation (better fixed with a validator), zombie-turn and silence detection (time and count thresholds).

### Assessment: Jev as a model router (candidate 7, 2026-09-24)

The operator asked whether the decisions model could act as the model router. Short answer: **not as a synchronous per-turn router today. It can serve as a shadow-mode difficulty judge that informs routing later**, and that job needs its own slices. Findings (read at `61a4c4a8`):

- **Data policy blocks the obvious input.** The user's message is class C, and `decisions-client/src/gate.rs` hard-codes class C as never allowed. `SITES` lists only `heal.classify` and `smoke.live`. A routing site may send only class-B *features* derived from the ask, never its text: character or token length, attachment kinds, count of tools offered, whether the ask is a cron turn, the active role, and the fields `QueryModelRoute` already carries (`request_class`, `needs_tools`, `needs_structured`, `approx_context_tokens`, `latency_class`, `trust_ceiling`). Class B needs an explicit `operator_opt_in` row. **That is an operator decision and has not been made.** Features that thin may not beat a rule table, and the shadow run exists to measure exactly that.
- **Latency does not fit an inline router.** Measured round trips are 0.29-0.61 s over OpenRouter, while `MODEL_GRAPH_FLYWHEEL_PROPOSAL` Slice 9 budgets "<50ms local" for a routing oracle. An inline call would add that delay to every first dispatch. The call must be non-blocking, or parallel with a deadline and a deterministic fallback to the ladder.
- **A verdict has nowhere to land yet.** Every dispatch site overwrites `model_req.model` from `role_model_binding` unconditionally: `philote/src/runtime.rs:2918, :3734, :4256, :4986, :5619` and `turn_loop.rs:904, :2726, :2893, :3157`. The "per-ask" tier in `MODEL_REFLEX_PROPOSAL` (Seam A, keep a model the caller already set) does not exist. That seam is a prerequisite whoever makes the decision.
- **What already exists to build on.** The health oracle (`model_oracle.rs`, `rank_models`, IPC `QueryModelRoute`) filters by capability and health. `shadow_oracle_pick` (`turn_loop.rs:3700`, `PHILOTIC_SHADOW_ORACLE`) already logs oracle-vs-ladder disagreement, but hard-codes `approx_context_tokens: 0` and `needs_tools: true`. philote has no token estimator. The catalog (`model_catalog.openrouter`) holds tools/context/price per model, and `model_profile` holds live per-hotel health. Since DEF-202/203/204 both are fresh on every hotel.
- **What Jev should answer, and what it should not.** It fits the typed question "how hard is this ask?" (`score`: trivial / normal / hard / long-context) or "which tier?" (`choice` over the role's `fallback_tiers`). The concrete model is still picked in code from the tier, the catalog and the health rows, so Jev never names a model id and never becomes an authority. Thresholds stay in call-site code (invariant). An abstain or error means the ladder, as today.

**Proposed order (not yet slices; each needs the operator's go):**

1. **R0 — Seam A.** Dispatch sites keep a model already set on the request (`.or_else`), plus a `SelectionSource::PerAsk` variant. No behaviour change until something sets it.
2. **R1 — features.** A token estimate and a `RouteFeatures` struct built at `handle_user_message` (`runtime.rs:~3628`, before `resolve_model_execution_target`). Feed the real values into `shadow_oracle_pick` instead of the hard-coded ones.
3. **R2 — shadow site `route.difficulty`.** A class-B site, log-only, fired in parallel with dispatch, writing `decision_traces`. Compare its answers with `router_traces` outcomes (latency, failure, escalation). **Blocked on the operator's class-B opt-in.** A local classifier (open question 6, e.g. `gemma3` on Ollama) can fill the same envelope first with no data-policy question at all.
4. **R3 — promote.** Only if the traces show the verdict would have avoided failures or cost, set the per-ask tier from it, below the operator pin and sticky override and above the ladder primary. The ladder stays the fallback.

### Heal-queue drain and organize (slices H0-H3, 2026-09-24)

Operator ask: use the decisions call to drain and organize the self-heal queue. Live reading: nothing waits in `pending` (the dispatcher keeps up); the backlog is the `escalated` pile (mac-jane 402, vps-jane 83, mbp-jane 7), and it is mostly repeats of stopped patterns. The other gap is signal loss: with Ollama down, novel lines were resolved `unclassified`/noop (DEF-206), hiding a live Telegram poll conflict (DEF-207). So deterministic code does the organizing, and the judge answers only what code cannot.

| Slice | What | Status |
|---|---|---|
| H0 | Code only. Stale sweep: escalated rows whose `(guest, tag)` has not recurred within 48 h are closed `resolved` + `stale_closed` (hourly, `PHILOTIC_HEAL_STALE_ESCALATION_SECS`, `0` off). `occurrences` column keeps flood-collapse counts. ANSI stripped at push. Recurrence on the row's own timestamp (DEF-205). | test-green, `codex/heal-queue-organize` |
| H1 | Decisions **fallback**, same site `heal.classify` (class A, no gate change): when the incumbent has no verdict (Ollama down / breaker open), ask `severity` + `pattern` (a choice over a fixed tag list plus `other`). Tag applied at probability >= 0.6, else `unclassified`; action from `heal_action_for_pattern_tag`, **capped at `escalate`** (the judge labels and routes, never restarts). 30 calls/min, 4 s deadline, model-failure lines never sent, one trace per call keyed by heal row id. Flag `PHILOTIC_HEAL_DECISIONS_FALLBACK`, default off; shadow and fallback are independent modes of one `DecisionsJudge`. | test-green, same branch |
| H2 | Cluster triage (site `heal.triage`, class A, needs a `SITES` row): one question per duplicate *group* of escalations, batched in one request (32k OpenRouter cap): still-actionable / cleared / noise / needs-operator + urgency score. Pre-screen each group so one echoed payload cannot refuse the batch; age/date logic stays in code. | proposed |
| H3 | Ranked operator digest instead of per-row escalation pings; D4 calibration of H1/H2 verdicts against row outcomes; promotion via `ProposalOnly -> ConfirmFirst -> AutoWithAudit`. | proposed |

## Invariants

1. **Never an authorization authority.** Jev is not injection-resistant. Skill-register risk tiers, tool approval grants, `philotic-web` posture and everything on its route table stay deterministic; `philotic-web` is the only authz boundary (PHILOTIC_WEB_HARDENING). A decision may score and escalate; it may not grant.
2. **Shadow before in-path.** Every call site starts log-only behind a flag, default off, using the `PHILOTIC_SHADOW_ORACLE` pattern, and is measured against the incumbent before it can decide anything.
3. **Every call site has a deterministic fallback** and a decision failure never fails a turn. The provider is an external hop and the mesh includes a CGNAT Mac.
4. **Dates, counting and arithmetic stay in code.** The audit `closable` rule is "contradicted by completion + past-dated"; a decision gets only the semantic half.
5. **Thresholds are per question and calibrated on labelled data** from our own traces. No threshold is copied from vendor cookbooks or shared between primitives.
6. **Distributions out, policy in code.** The envelope never carries a threshold or a verdict.
7. **Privacy gate per site.** Every judged state leaves the mesh, and OpenRouter adds an intermediary. Operator message content and LifeGraph content are excluded from any site until retention terms are confirmed. Zero data retention is enterprise-only at TypeSafe.

## Data policy (recommended 2026-09-21; the operator asked for a recommendation, so this is NOT yet confirmed)

Every judged state leaves the mesh, and OpenRouter adds an intermediary. Zero data retention is enterprise-only at TypeSafe, and we have not verified OpenRouter's account-level logging and retention settings. So the default is narrow, and widening it is a visible diff.

**Data classes**

| Class | What | Policy |
|---|---|---|
| A | Machine-generated system telemetry: heal-queue failure text, error classes and codes, capability and tool names, counts, timings, status flags, and synthetic smoke strings | Allowed for shadow sites, after the redaction gate below |
| B | Features derived from operator content (length, language, a detected label), never the content itself | Allowed only per site, after an explicit operator opt-in recorded in the site table |
| C | Operator message text, LifeGraph / memory / Muninn content, session and turn text, anything from a persona's conversation, credentials, personal data, file contents | **Never**, until retention terms are confirmed in writing |

**Mechanics**

1. **Sites are allow-listed by id** in one const table in code, each with a declared `data_class`. An unknown site id is refused before any network hop. A site builds its `state` only from enumerated fields, so class C cannot arrive by accident through a "just send the whole row" call. Widening the policy means editing that table, which shows up in review.
2. **A redaction gate runs on every string that leaves**, even class A. Heal text is machine-generated but not safe by construction: DEF-089 is a live leak of bot tokens inside reqwest error URLs, and log lines carry URLs, keys and addresses. The gate strips URLs with credentials or query tokens, bearer and `sk-`-style keys, bot-token shapes (`<digits>:<token>`), long hex or base64 runs, email addresses and absolute home paths, then truncates to a fixed tail (heal text only needs the end). It ships with a test corpus, including the DEF-089 shape.
3. **Default off, with a kill switch.** `PHILOTIC_SHADOW_DECISIONS` is unset by default; a site table entry alone sends nothing.
4. **Audit without content.** Each call's trace row records `site`, `data_class`, the bytes that actually went out (the length of the request body as sent, measured at the wire, and zero when nothing was sent), the resolved model and the outcome. It never stores the text sent, and the schema has no column that could hold it. `heal-dispatcher --decision-summary` prints the totals, with errors, skips and agreement on separate lines.
5. **Provider.** OpenRouter is acceptable for class A only, using the hotel's existing OpenRouter key (the operator's choice; see "Key access" above for the shared spend and revocation trade-off). Before enabling in production, check the OpenRouter account's data-logging and retention settings; the native TypeSafe API is preferable if early access is granted and its retention terms are acceptable.
6. **A fail-closed payload screen** at the same egress point refuses state that looks like a conversation payload (`"role"`, `"messages"`, `"content"`, `"parts"`, `"prompt"`, `"system_instruction"`, matched even when JSON-escaped inside a log line). The allow-list keys on the site id, not on what the state holds, so this is the check that looks at content. A refusal is the error class `policy_refused`, sends nothing, and is recorded as a `skipped` row, never as an error or a disagreement.
7. **The heal-dispatcher pilot sends less than the gate would allow.** Rule-classified lines are never sent. Any line carrying a model-capability envelope (`[guest][text.generate] …`) is skipped: `model-router`'s `emit_failure` is the only source of untriaged model failures, and its text is a provider error body, which can echo the request that failed. And only the last 500 characters of a line are asked about.

**Residual risk, stated plainly.** The screen is a heuristic and redaction cannot recognise operator prose, so a line from another guest that quotes conversation text without those markers could still leave. The pilot's exposure is bounded (novel lines only, one class-A site, a 500-character tail, no model-controller lines) and observable (sent bytes and skips are in the summary), but it is not zero. The operator should accept that knowingly before enabling it on a hotel.

## Slices

| Slice | Content | Verification |
|---|---|---|
| D0 `decisions-envelope` | `ansible-mesh-core::decisions` types, validation, native and OpenRouter wire adapters as pure functions, unit-tested against fixtures. No network. Ordered-levels test. **Done 2026-09-19** (26 tests). The OpenRouter `noul` and a mixed `noul` + `choice` + `score` response are recorded live (2026-09-19 and 2026-09-21); only the native-transport fixture still comes from the vendor docs. The `score` legend is mapped by its text, falling back to position, with a `legend_mismatch` flag on the trace instead of an error. | test-green |
| D1 | model-router: `TaskKind::Decide`, `RequestClass::Judgment`, `ProviderOutput::Judgment`, `AuxTaskKind` arm, aux-isolation entry, `model_oracle` request-class mapping, a provider with a native / OpenRouter transport switch, `model-controller-decisions` bin (role `model.decisions`), `decisions_response` reply action, `ResponseTrace` growth. **Code done 2026-09-19, test-green** (23 new tests; the full `model-router` and `ansible-mesh-core` lib suites pass). `model.decisions` and `heal-dispatcher` are on the OpenRouter key's `allowed_roles`; an entry stored earlier lists only the old roles, so `aiua auth sync-roles --provider openrouter --db <context db>` adds them without decrypting the key. **Not done:** seeding the guest in the hotel (`aiua/src/main.rs`, a hot file and a deployment decision) and the live smoke with a dedicated key. | test-green, then smoke-green once a key exists |
| D2 `decisions-shadow-heal` | `heal-dispatcher` calls the provider beside `gemma3:4b`, log-only, writes a `decision_traces` row per site (agreement, probabilities, latency, cost, resolved model, `legend_mismatch`, and the **error class**). Error classes are counted separately from disagreement: a parse failure and a disagreeing judge look identical in a log-only run, and calibrating on the first as if it were the second would be calibrating on nothing. Flag `PHILOTIC_SHADOW_DECISIONS`, default off. **Code done 2026-09-21, test-green:** `heal-dispatcher/src/shadow.rs` runs in-process, fire-and-forget and bounded (4 in flight, 4 s, no retry, a burst is skipped), shadows only novel lines the incumbent classifies with `gemma3:4b` (rule-classified lines are not sent), and asks `severity` and `needs_restart`. The data-policy gate lives at the client's single egress point (`decisions-client/src/gate.rs`): allow-listed sites and redaction of every string in `state`. `decision_traces` is `ansible-mesh-core/src/decision_trace.rs`; its schema has no column that could hold sent text, and `summarize()` keeps error rows out of agreement. **Live on vps-jane since 2026-09-23** (operator confirmed the data policy and the shared OpenRouter key; `aiua auth sync-roles` granted `heal-dispatcher`). 2026-09-24: 128 traces, 124 ok, 1 timeout, 3 policy-skipped, $0.0028 total, `needs_restart` agreement 75/124 (60%). Traces carried only the guest id, so D4 could not join a judgment to its row's outcome; fallback traces now carry the heal row id. | watched-live-green on one hotel |
| D3 | In-process client with local fallback; shadow sites #3 (`memory.recall` relevance) and #1 (say-do gate), both non-blocking. | smoke-green, then watched-live-green |
| D4 | Calibration from `decision_traces`, `router_traces`, approve/deny events and `life.recall.feedback`; per-question thresholds; promotion via the existing autonomy postures (`ProposalOnly` → `ConfirmFirst` → `AutoWithAudit`, `ansible-mesh-core/src/autonomy.rs`). | field-evidence |
| D5 (optional) | Make the decisions provider the first production producer of `Context1Advisory`. | test-green |

### D2 shape (found while building D1)

- **`decisions_response` has no consumer yet.** `EmitTask` is fire-and-forget, the `correlation_id` has no correlator on the receiving side, and philote maps an unrecognised `action` to `IngressAction::Unknown`, which nothing handles. D1's reply path is therefore inert until D3 writes a consumer. It is correct and tested on the sending side only.
- **D2 is simplest in-process.** `heal-dispatcher` already holds its own `reqwest` client and calls Ollama directly; it is a 30 s poll loop with no inbox. It calls the decisions client directly rather than through the `model.decisions` controller. It cannot depend on `model-router` (ONNX, MLX and friends), so the HTTP hop and the key loader now live in the small shared crate `crates/decisions-client` (`DecisionsClient`, `load_decisions_config`), and `model-router`'s `DecisionsProvider` is a thin wrapper over it. **Done.**
- **Key access: the operator chose (b), a vault role grant (2026-09-21), on the EXISTING `openrouter` vault key.** The three options were (a) an environment variable on the unit, (b) a vault role grant, (c) routing through `model.decisions` over IPC. I first built (b) as a separate `decisions` vault entry, to avoid widening the key that also drives chat fallbacks; the operator preferred reusing the key already in the vault, so that entry was removed. The `openrouter` `ProviderKeySpec` now also lists `model.decisions` and `heal-dispatcher`. **Accepted trade-off:** chat and decisions share one spend, one rate limit and one revocation, and two more roles can read the key. The model is never read from `openrouter_default_model` (the chat model). Because a role list is fixed when an entry is stored, the entry already in the vault needs `aiua auth sync-roles --provider openrouter --db <context db>`, which adds the spec's roles without decrypting the key (ciphertext unchanged, additive only, refuses an empty role list or a guest-restricted entry). Run it once per hotel that runs the pilot; the vault registry does not replicate.
- **D2 logging waited for real `choice` and `score` bodies, which are now recorded** (2026-09-21), and the `score` legend held. It still needs the operator's confirmation of the data policy before anything is enabled live.

## Open questions

1. **Access. Resolved 2026-09-19:** Jev is reachable through an ordinary OpenRouter key with no waitlist (see Verified access). D1's smoke is unblocked; the native TypeSafe waitlist is optional.
2. **Capability name.** `decisions.evaluate` with model type `decisions` is the proposal; the operator may prefer another.
3. **Reply path.** A dedicated `decisions_response` action is recommended; confirm by test that reusing `model_response` really reaches `fail_active_turn`.
4. **Metering.** Grow `ResponseTrace` (recommended) or keep cost in a side table.
5. **Data policy.** Recommended in the "Data policy" section above (class A only for now, redaction gate, allow-listed sites). **Awaiting the operator's confirmation or amendment.**
6. **Local fill.** Whether an ONNX or local classifier should implement the same envelope so a site never depends on a hosted provider (`LOCAL_ONNX_INFERENCE` names the sidecar as the fast path for latency-sensitive consumers).
7. **Prior claims to re-verify.** The 67.8 % "agreement with references" figure attributed to TypeSafe in a third-party issue, and the 193.6× / 444.6× vendor benchmark, are unverified.

## Risks

- Alpha endpoint and early-access model: paths and schemas may change; pin the adapter behind fixtures.
- Vendor lock and availability on the turn path: mitigated by invariant 3.
- A calibrated-looking probability that is not calibrated on our data: mitigated by D4 and by shadow first.
- Cost is trivially low per call, so the risk is data exposure, not spend.

## References

- TypeSafe: [Introducing System One Models & Jev](https://typesafe.ai/blog/introducing-system-one-models-and-jev), [docs](https://docs.typesafe.ai/introduction), [HTTP API](https://docs.typesafe.ai/api.md), [models](https://docs.typesafe.ai/models.md), [Jev 1.13 known limitations](https://docs.typesafe.ai/model-jaggedness/jev-1.13.md), [skill suggestion cookbook](https://docs.typesafe.ai/cookbooks/skill_suggestion.md), [entity alignment cookbook](https://docs.typesafe.ai/cookbooks/entity_alignment.md), [LLM guardrails cookbook](https://docs.typesafe.ai/cookbooks/llm_guardrails.md), [confidence routing](https://docs.typesafe.ai/patterns/confidence-routing.md), [legal](https://docs.typesafe.ai/legal.md).
- OpenRouter: [Alpha.Decisions SDK docs](https://openrouter.ai/docs/client-sdks/typescript/sdks/decisions/README).
- Third-party integration notes: [oh-my-pi #12458](https://github.com/can1357/oh-my-pi/issues/12458), [tomcounsell/ai #3421](https://github.com/tomcounsell/ai/issues/3421).
- Independent coverage: [The Register](https://www.theregister.com/ai-and-ml/2026/09/16/typesafe-ai-debuts-model-for-machines-that-plays-doom/5296711), [Flavio Copes](https://flaviocopes.com/jev/).
