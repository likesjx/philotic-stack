---
name: role-authoring
description: Use this skill when authoring or revising an agent role lens before governed execution. It gathers the missing role definition inputs, explains the required payload shape, and prepares the payload for the role.create_or_update workflow.
catalog:
  skill_name: role.authoring
  implied_tools:
    - session.status
    - role.configure
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
    transitional_note: role.authoring remains prompt-facing and still implies the low-level role.configure tool as a compatibility bridge until workflow invocation is surfaced directly.
---

# Role Authoring

Use this skill when an orchestrator agent needs to author or revise a role lens before running the governed `role.create_or_update` workflow.

## Purpose

`role.configure` is a low-level mutation tool. This skill supplies the authoring procedure and required payload shape so the agent does not guess and omit required fields before the workflow executes that mutation.

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
4. Execute the governed `role.create_or_update` workflow, which currently runs through `role.configure` as a transitional execution surface.
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
