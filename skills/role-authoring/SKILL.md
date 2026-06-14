---
name: role-authoring
description: Use this skill when authoring or revising an agent role lens before governed execution. It gathers the missing role definition inputs, explains the required payload shape, and prepares the payload for the role.create_or_update workflow.
catalog:
  skill_name: role.authoring
  implied_tools:
    - session.status
    - role.create_or_update
    - handoff.to_role
  validation_state: validated
  skill_markers:
    - governed
    - high_agency
  field_sources:
    required_fields:
      - role_name
      - toolset_profile
      - reasoning.purpose
      - reasoning.toolset_rationale
      - reasoning.handoff_posture_and_limits
    repo_skill_path: skills/role-authoring/SKILL.md
    workflow_handoff: role.create_or_update
    transitional_note: role.authoring remains prompt-facing while runtime execution still resolves through the low-level role.configure mutation surface under the governed role.create_or_update workflow.
---

# Role Authoring

Use this skill when an orchestrator agent needs to author or revise a role lens before running the governed `role.create_or_update` workflow.

## Purpose

`role.create_or_update` is the canonical role mutation tool. This skill supplies the authoring procedure and required payload shape so the agent does not guess and omit required fields before calling it.

## Required role.create_or_update payload shape

Always provide:

- `role_name`
- `toolset_profile`
- `reasoning.purpose`
- `reasoning.toolset_rationale`
- `reasoning.handoff_posture_and_limits`

Provide when available or needed:

- `role_identity_addendum`
- `role_manifest`
- `inactive_ttl_seconds`
- `iteration_cap`
- `approval_policy`
- `model_profile`
- `context_window_policy`
- `is_admin` only when the operator explicitly wants an admin role

## Procedure

1. Clarify the role if the request is incomplete.
2. Gather:
   - role name
   - purpose
   - toolset profile
   - rules / manifest
   - handoff posture
   - limits
3. Normalize the request into a complete `role.create_or_update` payload.
4. Call `role.create_or_update` with the complete payload.
5. Summarize what was created or changed.
6. If the user asked to use the new role immediately, hand off with `handoff.to_role`.

## Guardrails

- Do not execute the workflow without `role_name`.
- Do not create admin roles unless the operator explicitly requests admin authority.
- Prefer updating an existing role when the request is clearly a refinement.
- If the role purpose or toolset is ambiguous, ask the smallest question needed before calling the tool.

## Minimal valid example

```json
{
  "role_name": "researcher",
  "toolset_profile": "research",
  "reasoning": {
    "purpose": "Create a focused research role for bounded investigation tasks.",
    "toolset_rationale": "The research profile keeps the tool surface narrow while preserving session continuity.",
    "handoff_posture_and_limits": "The role should return concise findings to orchestrator custody when the investigation completes."
  }
}
```

## Full example (all optional fields)

```json
{
  "role_name": "code-reviewer",
  "toolset_profile": "codex",
  "role_identity_addendum": "You are a careful code reviewer who prioritizes correctness and security over speed.",
  "role_manifest": "Review all diffs for logic errors, security holes, and test coverage gaps. Surface findings as a numbered list with severity.",
  "inactive_ttl_seconds": 300,
  "iteration_cap": 10,
  "approval_policy": "auto",
  "model_profile": "balanced",
  "context_window_policy": "rolling_4k",
  "is_admin": false,
  "reasoning": {
    "purpose": "Specialist code review role for PR review sessions.",
    "toolset_rationale": "The codex profile gives workspace read access without shell execution authority.",
    "handoff_posture_and_limits": "Return the review list to orchestrator when complete. Do not push changes or create PRs."
  }
}
```

## Field reference

| Field | Required | Notes |
|---|---|---|
| `role_name` | Yes | Snake_case or kebab-case. Must be unique per agent. |
| `toolset_profile` | Yes | One of the seeded profiles: `orchestrator`, `admin`, `codex`, `research`, `utility`, `architect`, `brain`, or `virtuoso`. Prefer the narrowest profile that can serve the role. |
| `reasoning.purpose` | Yes | What this role is for — 1–2 sentences. |
| `reasoning.toolset_rationale` | Yes | Why this toolset profile was chosen. |
| `reasoning.handoff_posture_and_limits` | Yes | When and how to hand back to orchestrator. |
| `role_identity_addendum` | No | Short persona overlay, appended to soul_text for this role. |
| `role_manifest` | No | Detailed operating rules for this role. |
| `inactive_ttl_seconds` | No | Auto-expire idle role after N seconds (default: no expiry). |
| `iteration_cap` | No | Max tool iterations per turn before forced handoff. |
| `approval_policy` | No | `auto`, `operator`, or `escalate`. |
| `model_profile` | No | Override model selection for this role. |
| `context_window_policy` | No | Context truncation strategy. |
| `is_admin` | No | Grant admin authority. Only set when operator explicitly requests it. |
