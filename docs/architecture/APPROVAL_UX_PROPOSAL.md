---
title: Philotic Approval UX Proposal
doc_type: proposal
domain: runtime-sessions
status: accepted-current-slice
last_updated: 2026-03-31
tags:
- approval
- ux
- sessions
- telegram
- active-seam
related_docs:
- ARCHITECTURE_STATUS.md
- SESSION_LOOP_PROPOSAL.md
- AGENT_LOOP_PROPOSAL.md
- TELEGRAM_INTEGRATION_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: approval-ux
implements:
- session-loop
implemented_by:
- approval-interrupt-history-slice
active_seams:
- approval-card-ux
- session-preapproval-ux
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Philotic Approval UX Proposal

## Goal

Define a practical approval experience for Philotic that works well in constrained chat interfaces like Telegram while still preserving canonical session policy, auditability, and future richer control surfaces.

The important rule is simple:

- prompt guidance can reduce unnecessary stops
- commands can change approval behavior quickly
- canonical session policy remains the actual authority

Anything else is just letting vibes cosplay as governance.

## Problem

Approval interrupts are now part of the loop, but the user experience is still very literal:

- the agent requests approval
- the session pauses
- the user replies `/approve` or `/deny`

That works, but it is too coarse for longer-running sessions and too friction-heavy for routine safe actions.

We need a path for:

- one-off approval
- pre-approval without stopping the loop
- visibility into what is currently allowed
- eventual richer interfaces beyond Telegram

## Core Recommendation

Use a layered approval model:

1. Prompt guidance
2. Slash-command UX
3. Canonical session policy
4. Runtime enforcement

These layers serve different jobs:

- prompt guidance helps the model avoid asking unnecessarily
- slash commands give the human a usable control surface in chat
- session policy stores the truth durably
- runtime enforcement prevents the system from becoming confidently wrong

## Approval Layers

### 1. Prompt Guidance

The easiest near-term optimization is to include pre-approved behavior in the session prompt.

Example guidance:

- pre-approved actions for this session: read-only workspace inspection, `echo`, metadata lookups
- do not ask for approval for pre-approved actions
- do ask for approval for external side effects, destructive changes, or anything outside the approved set

Why this helps:

- fewer useless interruptions
- better model planning
- better fit for chat interfaces

Why this is not enough:

- a prompt is advice, not authority
- it should reduce friction, not replace enforcement

## 2. Slash Commands

For Telegram and similarly constrained transports, slash commands should be the primary approval UX.

### Baseline commands

- `/approve`
  - approve the currently waiting action once
- `/deny`
  - deny the currently waiting action once

### Recommended next commands

- `/preapprove <scope>`
  - adds a pre-approval rule to the current session
- `/approval status`
  - shows current approval posture for the session
- `/approval reset`
  - clears session-level pre-approvals

### Suggested first scopes

- `/preapprove this-session`
  - temporary permissive mode for the session
- `/preapprove workspace-read`
  - allow read-only workspace access
- `/preapprove tool:echo`
  - allow a named tool
- `/preapprove class:read-only`
  - allow a capability class

This gives Telegram a workable control plane without waiting for richer UI surfaces.

## 3. Canonical Session Policy

All approval state should live canonically in the session graph, not inside prompt text and not only in agent memory.

The session should own an approval policy object such as:

```json
{
  "approval_policy": {
    "auto_approve_all": false,
    "preapproved_tools": ["echo"],
    "preapproved_classes": ["workspace-read"],
    "require_each_time": ["shell-write", "network-send", "deploy"],
    "forbidden": ["credential-export"]
  }
}
```

This policy should be:

- durable
- session-scoped
- visible in session snapshots
- reflected into prompt guidance
- enforced at runtime

## 4. Runtime Enforcement

Runtime behavior should be:

- if action is allowed by policy:
  - record `approval_requested`
  - immediately record `approval_resolved` with `resolution_mode = "preapproved"` or `policy_auto`
  - continue without pausing
- if action is not allowed:
  - enter `waiting_approval`
  - notify the user
- if action is forbidden:
  - fail without offering approval

This keeps the history honest. Pre-approval should mean “auto-resolved and recorded,” not “never existed.”

## Approval Event History

Approval should be a first-class part of the session timeline.

Recommended event kinds:

- `approval_requested`
- `approval_resolved`
- `approval_denied`
- later:
  - `approval_expired`
  - `approval_cancelled`
  - `approval_policy_changed`

Recommended fields:

- `approval_id`
- `session_id`
- `turn_id`
- `reason`
- `requested_action`
- `decision`
- `resolution_mode`
- `resolved_by`
- `created_at`
- `resolved_at`

## Telegram UX Recommendation

For Telegram specifically, prioritize:

1. `/approve`
2. `/deny`
3. `/preapprove <tool|class|this-session>`
4. `/approval status`
5. `/approval reset`

This is enough to keep the loop usable without pretending Telegram is a full orchestration console.

## Future Communication Planes

As richer communication surfaces arrive, they should project onto the same session policy model rather than inventing a new approval system per transport.

Good future UX options:

- inline approve/deny buttons
- “always allow this tool in this session” buttons
- approval profiles like `safe`, `dev`, `autonomous`
- temporary grants with TTLs
- policy configuration panels

These are interface improvements, not model changes.

## Recommended Implementation Order

### Phase 1

- keep `/approve` and `/deny`
- reflect current session pre-approvals into the prompt
- keep runtime policy as the backstop

### Phase 2

- add `/preapprove`
- add `/approval status`
- persist `approval_policy_changed` session events

### Phase 3

- add fine-grained scopes and classes
- add richer transport UX on top of the same policy object

## Full Recommendation

- use slash commands as the main approval UX for Telegram
- use prompt guidance to reduce unnecessary approval requests
- store approval rules canonically in session policy
- enforce approval at runtime
- record pre-approvals as explicit approval events rather than skipping history

That gives us the shortest path to a usable approval system today without painting ourselves into a very chatty corner tomorrow.
