# Agent Context Management Proposal

## Goal

Define how Philotic agents and operators should inspect and mutate agent-owned context graph state at runtime without falling back to ad hoc startup config edits.

This proposal focuses on:

- agent self-management of bounded profile/context state
- admin management of all agent and hotel context
- authority boundaries for self vs operator edits
- how runtime tools map onto canonical context graph records
- reducing dependence on `mesh-config.json` for live behavioral tuning

## Disposition

`accepted for current slice`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Current Slice

- pin the first implementation target as a hotel-mediated self-update path
- make the request contract explicit before wiring more runtime behavior onto local `agent.configure`
- defer broad admin surfaces until the self-update boundary is proven once

## Core Recommendation

Philotic should expose two related but distinct management surfaces:

1. **agent self-management**
2. **admin/operator management**

Both surfaces should write to canonical context graph records through hotel-owned APIs and policy checks.

The key distinction is:

- agents may manage their own bounded slice
- admins may manage any agent/hotel slice

That lets us preserve agent autonomy where it is useful without confusing that with global system authority.

## Why This Needs Its Own Plane

Recent runtime work has made the problem obvious:

- voice identity
- TTS policy
- media routing policy
- imported workspace identity bundles
- persona/profile tuning

all now exist as real runtime behavior, but too much of the operator path still depends on file edits plus restart cycles.

That is acceptable as transitional scaffolding, but it is not an honest final operating model.

If we keep pushing more agent behavior into startup overlays, the context graph stops being canonical and becomes a spectator wearing a lanyard.

## Management Surfaces

### 1. Agent Self-Management

The agent should have a bounded tool surface for managing its own context graph state.

Recommended first capabilities:

- inspect current profile/config projection
- update `identity_text`
- update `user_context_text`
- update `memory_summary`
- update `voice_response_policy`
- update `media_routing_policy`
- update session-scoped bindings and preferences

Recommended first constraints:

- self-only by default
- field allowlist
- rate/size limits for large text fields
- approval-gated for sensitive categories
- hotel validates writes before commit

This is the right home for “change my voice speed,” “switch my default speech mode,” or “update my self-description,” instead of forcing those through a config file and a process restart every time.

### 2. Admin / Operator Management

An admin surface should exist for hotel-wide or cross-agent authority.

Recommended first capabilities:

- inspect any agent profile/config
- update any agent profile/config
- inspect hotel manifests and guest bindings
- manage hotel-scoped transport config
- manage role/incarnation definitions
- inspect and repair drift between startup overlays and graph-backed truth

Recommended first constraints:

- explicit operator/auth scope
- audit logging
- stronger approval or credential requirements
- no accidental elevation through ordinary agent tools

This is where an operator should manage:

- Jane vs Aria voice identity
- Telegram policy
- imported workspace source
- default model bindings
- hotel-level routing and policy

## Authority Model

The hotel remains the write authority.

The context graph remains canonical state.

Management tools are request surfaces, not direct storage owners.

Recommended boundary:

- **agent-core** may request self updates through a hotel-mediated tool/API
- **membrane** may expose admin/operator entry points, but does not own policy truth
- **ansible** validates, authorizes, persists, and audits
- **context graph** stores canonical result

This prevents runtime convenience APIs from quietly becoming a second authority.

## Canonical Records

The first management plane should target records we already effectively have:

- agent profile
  - `persona_name`
  - `soul_text`
  - `identity_text`
  - `user_context_text`
  - `memory_summary`
  - `voice_response_policy`
  - `media_routing_policy`
- session bindings
- role/incarnation records
- hotel-scoped transport/model config where appropriate

Longer-term, those should become more explicit graph entities rather than a mix of startup seed and projected blob fields.

## Suggested Tool Split

### Agent-facing tool

Suggested initial name:

- `agent.context.update`

Shape:

- path-based patch/update interface for bounded self fields
- readable confirmations
- structured error envelope on denial or validation failure

Recommended first transport shape:

- agent issues a hotel-mediated request
- hotel validates self-targeting and field allowlist
- hotel rewrites canonical `AgentIdentityRecord.bundle_json`
- runtime gets an explicit refreshed projection instead of pretending local session mutation is canonical

Suggested first request envelope:

```json
{
  "agent_id": "agent-jane-01",
  "updates": [
    {
      "path": "voice_response_policy.speed_percent",
      "operation": "set",
      "value": 108
    }
  ]
}
```

Suggested first response shape:

```json
{
  "updated_paths": ["voice_response_policy.speed_percent"],
  "agent_profile": {
    "...": "refreshed canonical projection"
  }
}
```

The exact IPC shape may differ, but the semantic contract should stay the same.

Examples:

- `voice_response_policy.speed_percent = 108`
- `voice_response_policy.mode = "on"`
- `identity_text = "..."`

### Admin-facing tool

Suggested initial names:

- `admin.agent.inspect`
- `admin.agent.update`
- later maybe `admin.hotel.inspect` / `admin.hotel.update`

These should require stronger trust than ordinary agent-local tools.

## Relationship To Existing Proposals

- [docs/architecture/PERSONALITY_AND_CONTEXT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERSONALITY_AND_CONTEXT_PROPOSAL.md)
  - defines what the agent profile/context layers are
- [docs/architecture/AGENT_INCARNATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_INCARNATION_PROPOSAL.md)
  - defines role/incarnation authority and provisioning
- [docs/architecture/KEY_VAULT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/KEY_VAULT_PROPOSAL.md)
  - defines how secret-backed values should be managed separately from ordinary context state

This proposal is the live management/control-plane layer that sits on top of those state definitions.

## First Slice Recommendation

Land the first honest self-management seam before broad admin panels:

1. hotel-mediated `agent.context.update` request path
2. bounded allowlist:
   - `identity_text`
   - `user_context_text`
   - `memory_summary`
   - `voice_response_policy.*`
   - `media_routing_policy.*`
3. structured error envelope on denied/invalid updates
4. session/runtime refresh after successful write

Then add admin management on top of the same hotel-owned write path instead of inventing a second mutation system.

## First Slice Constraints

The initial implementation should stay narrower than the full proposal:

- self-only updates
- no `soul_text`
- no transport credentials
- no model-secret mutation
- no role/incarnation writes yet
- no direct file edits

This is intentionally not “full profile editing.”

It is the first proof that live agent configuration can flow through the hotel into canonical graph-backed state without relying on `mesh-config.json` edits and restart rituals.

## Canonical Write Path

Recommended ownership for the first live slice:

1. `philote` or another caller requests a bounded update
2. `aiua` validates:
   - caller identity
   - self-only scope
   - field allowlist
   - value type/shape
3. `aiua` loads current `AgentIdentityRecord`
4. `aiua` applies the patch into `bundle_json`
5. `aiua` persists via `upsert_agent_identity`
6. `aiua` returns the refreshed canonical projection
7. caller refreshes local runtime state from that canonical result

That keeps the hotel as write authority and prevents local runtime convenience APIs from quietly becoming a shadow database.

## Open Questions

- which profile fields should be self-editable vs admin-only?
- should agents be able to update `soul_text`, or is that operator-governed?
- how should graph-backed truth interact with startup overlay defaults when they disagree?
- what should be session-scoped override vs durable profile change?
- how should admin auth be surfaced through membrane or CLI?
