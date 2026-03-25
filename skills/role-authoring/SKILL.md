---
name: role-authoring
description: Use this skill when creating or updating agent roles through role.configure. It gathers the missing role definition inputs, explains the required payload shape, and optionally hands off to the new role after creation.
---

# Role Authoring

Use this skill when an orchestrator agent needs to create or update a role with `role.configure`.

## Purpose

`role.configure` is a low-level tool. This skill supplies the procedure and required payload shape so the agent does not guess and omit required fields.

## Required role.configure shape

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
3. Normalize the request into a complete `role.configure` payload.
4. Call `role.configure`.
5. Summarize what was created or changed.
6. If the user asked to use the new role immediately, hand off with `handoff.to_role`.

## Guardrails

- Do not call `role.configure` without `role_name`.
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
