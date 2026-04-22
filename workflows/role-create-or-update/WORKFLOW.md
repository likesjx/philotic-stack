---
name: role-create-or-update
workflow:
  workflow_name: role.create_or_update
  workflow_kind: role.configure
  owner_scope: orchestrator
  target_class: same_identity_role_definition
  description: Governed role-definition workflow that validates a role lens, mutates the role incarnation record, updates capability posture, optionally materializes the worker, and may hand off after success.
  target_selection_policy:
    inputs:
      - role_name
      - toolset_profile
      - role_manifest
      - reasoning
    selection_mode: same_agent_role_record
  context_requirements:
    required_fields:
      - role_name
      - toolset_profile
      - reasoning.purpose
      - reasoning.toolset_rationale
      - reasoning.handoff_posture_and_limits
    optional_fields:
      - role_identity_addendum
      - role_manifest
      - inactive_ttl_seconds
      - iteration_cap
      - approval_policy
      - model_profile
      - context_window_policy
    supporting_skill: role.authoring
  return_contract:
    ack: ConfigureRoleOk
    post_success_options:
      - stay_in_orchestrator
      - handoff.to_role
  governance:
    execution_surface: role.configure
    materialization: ensure_role_materialized_on_new_or_breaking_change
    source_workflow_path: workflows/role-create-or-update/WORKFLOW.md
    transitional_note: runtime execution still flows through role.configure until workflow invocation becomes first-class
  rollout_state: active
---

# Role Create Or Update

Use this workflow when the orchestrator has already assembled a target role lens and needs the hotel to mutate role state deliberately.

## Purpose

This workflow exists so role creation stays a governed sequence instead of a large prompt-facing skill pretending to be both thought and mutation.

## Contract

1. Validate the authored role lens.
2. Persist or update the role incarnation record for the same agent identity.
3. Refresh capability posture and worker materialization when the change is new or breaking.
4. Return a normal configure acknowledgment, with same-self handoff remaining an explicit post-success step.
