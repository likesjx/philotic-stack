---
name: mcp-endpoint-steward
description: Use this skill when a philote must expose itself (or a datasource it fronts) to an external MCP client such as Perplexity, Claude, Codex, or an automation runner, and answer the resulting tools/call requests deterministically first and by model inference only as the declared fallback. Covers audit, surface design, handler policies, provisioning, credentials, smoke, rotation, and retirement.
catalog:
  skill_name: mcp.endpoint_steward
  implied_tools:
    - mcp.status
    - mcp.provision
    - mcp.grant_token
    - mcp.rotate_token
    - mcp.revoke_token
    - mcp.revoke
    - session.status
  validation_state: validated
  skill_markers:
    - governed
    - membrane
    - boundary_hygiene
  field_sources:
    required_fields:
      - endpoint_id
      - intended_client
      - tools
      - exposure
    optional_fields:
      - handler
      - preapproval_rules
      - allotment
      - expires_at
    repo_skill_path: skills/mcp-endpoint-steward/SKILL.md
    workflow: "mcp.status → design surface + handler policies → mcp.provision → mcp.grant_token → smoke tools/list + tools/call → record grant"
---

# MCP Endpoint Steward

Use this skill to put an MCP endpoint in front of yourself safely, and to make sure the calls that arrive are answered by code before they are answered by you.

## Purpose

An MCP endpoint is an external protocol boundary. Anything you advertise there can be called by a machine at any hour, with no operator in the loop. The steward's job is therefore twofold:

1. **Expose the smallest surface, on the narrowest network, behind a credential** — and prove it with a smoke call before calling it done.
2. **Answer deterministically first.** Every philote-targeted tool carries a `handler` policy. Schema validation, static answers, and built-in reflexes run before any model turn; the cognitive loop is the *declared* fallback, never the default hot path.

Pair with [mcp-surface-hygiene](../mcp-surface-hygiene/SKILL.md) for the naming rules (which storage surface each tool touches) and with `docs/reference/MCP_CREDENTIAL_LIFECYCLE.md` for the credential runbook.

## How a call is handled

```
tools/call ──▶ membrane-mcp ──▶ bearer + allotment + preapproval check
                   │
                   ├─ target = datasource / tool  ──▶ runner answers; you are never involved
                   │
                   └─ target = philote ──▶ YOU, in this order:
                         1. validate_input   args vs input_schema  → isError on violation
                         2. steps[]          static → answer now
                                             reflex → echo | memory.recall | memory.capture
                                                      (escalate_on_empty → next step)
                         3. fallback         model {instructions}  → one cognitive turn
                                             error {message}       → isError, no model
```

Approval-gated calls (no matching `preapproval_rules` entry) still get schema validation and static answers, but reflexes are skipped and the call parks for operator approval before anything else runs.

## Workflow

1. **Audit** — `mcp.status` for every endpoint you already own. Do not create a second endpoint for the same client; re-provision the existing `endpoint_id` instead (the hotel rejects a takeover of someone else's).
2. **Design the surface** — one endpoint per authority level (`perplexity-memory` and `lifegraph-readonly` are separate on purpose). For each tool decide:
   - **Target.** Reads and writes that a runner owns go to `{kind:"datasource"}` or `{kind:"tool"}`; those never reach your cognitive loop. Only tools that genuinely need you get `{kind:"philote", agent_id:<you>}`. Use your materialized guest id (`agent-x:orchestrator`) when you want a specific incarnation; a bare agent id (`agent-x`) resolves to that agent's orchestrator incarnation. A philote target always reaches exactly one philote (DEF-110).
   - **Schema.** Declare `required` and `type` for every argument. The philote enforces them; a caller that violates the schema gets an `isError` result and no model turn.
   - **Handler policy** (philote targets only). Start with `validate_input: true`, then:
     - `static` for anything known at provisioning time (health, capability descriptor, fixed lookups),
     - `reflex` `memory.recall` with `escalate_on_empty: true` for "what do you know about X" tools,
     - `reflex` `memory.capture` for note/decision intake,
     - `fallback` `model` with concrete `instructions` (shape, length, what not to do) **or** `fallback` `error` when the tool must never reach inference.
   - **Preapproval.** One `action_pattern` per deterministic action you intend to serve unattended. Never `"*"` on a mesh or internet endpoint.
3. **Provision** — `mcp.provision` with `exposure` no wider than the client's real network position (`local` behind a same-host TLS proxy, `mesh` for Tailscale peers, `internet` only for a public frontdoor) and `default_auth: {scheme:"bearer_token", grants:[]}`. Do not set `allow_unauthenticated` unless the operator asked for an open endpoint in this conversation. The provisioning turn is the authorization event; expect an approval card.
4. **Credential** — `mcp.grant_token` per client with a `token_id` naming that client, an `allotment` (e.g. 120 calls / 3600 s), and an `expires_at` (30–90 days for readers, 7–30 days for writers). Relay the raw token to the operator **once**, with a storage warning, and never write it to memory, LifeGraph, or a document.
5. **Smoke** — from the client's network position: `tools/list` shows exactly the intended tools and nothing else; one `tools/call` per deterministic path returns the expected shape; one malformed call returns `isError` without a model turn (check `session.status` shows no new turn). Only then report the endpoint as live.
6. **Record** — token_id, scope, expiry, endpoint_id, and the smoke evidence. Labels and IDs only — never the secret.
7. **Maintain** — `mcp.rotate_token` on owner change, exposure, or expiry; `mcp.revoke_token` then `mcp.revoke` to retire. After each, prove the old credential fails.

## Handler policy examples

Capability descriptor, fully deterministic:

```json
{ "validate_input": true,
  "steps": [ { "kind": "static", "result": { "agent": "beacon", "surfaces": ["memory", "lifegraph"] } } ],
  "fallback": { "kind": "error", "message": "descriptor is static" } }
```

Memory lookup that escalates to reasoning only when memory is empty:

```json
{ "steps": [ { "kind": "reflex", "reflex": "memory.recall",
               "args": { "query": "${payload.question}", "limit": 5 },
               "escalate_on_empty": true } ],
  "fallback": { "kind": "model",
                "instructions": "Answer in at most three sentences from what you know. If you do not know, say so." } }
```

Note intake with no inference at all:

```json
{ "steps": [ { "kind": "reflex", "reflex": "memory.capture",
               "args": { "content": "${payload.note}", "category": "note", "tags": ["${payload.source}"] } } ],
  "fallback": { "kind": "error", "message": "capture failed" } }
```

Template rules: `"${payload}"` is the whole transformed payload, `"${payload.a.b}"` keeps the referenced JSON type, and `${payload.x}` inside a longer string interpolates as text.

## Guardrails

- A tool with `{kind:"philote"}` and **no** handler policy behaves as before: a full model turn per call. Treat that as a smell, not a default.
- `memory.recall` is capped at 25 results and reads only your own vault (or the shared user vault with `scope:"user"`). `memory.capture` writes only your own vault and tags the engram `mcp`.
- `escalate_on_empty` means **zero hits**. Muninn's activate returns nearest neighbours for almost any query, so with a populated vault the recall reflex answers rather than escalates; put a `model` fallback with instructions behind it only when a weak match is acceptable, otherwise design the tool as `static`/`error` or route it to a datasource.
- Reflex names are a closed list (`echo`, `memory.recall`, `memory.capture`). Provisioning with any other name fails in the provisioning turn, on both the philote and the hotel side.
- Exposure is validated against the hotel's perimeter ceiling; a tier the hotel cannot defend is a provisioning error.
- The hotel checks that `owner_agent_id` matches your registered identity. Do not provision on another agent's behalf; hand them this skill instead.
- Static results are served even to approval-gated callers (they were declared in an approved turn). Do not put anything sensitive in a static result.

## Anti-patterns

- Routing a read that a datasource owns through your cognitive loop.
- `fallback: model` with no `instructions`.
- `"*"` preapproval on anything beyond loopback.
- Reusing one bearer across clients or authority levels.
- Claiming an endpoint is live without a smoke call from outside the hotel.
- Storing the raw token anywhere but the operator's secret store.
