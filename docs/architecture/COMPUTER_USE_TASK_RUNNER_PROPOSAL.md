---
title: Computer Use Task Runner Proposal
doc_type: proposal
domain: tooling-execution
status: accepted-current-slice
last_updated: 2026-04-24
tags:
- cua
- computer-use
- desktop
- task-runner
- tooling
- active-seam
related_docs:
- ARCHITECTURE_STATUS.md
- TASK_RUNNER_PROPOSAL.md
- TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md
- DESKTOP_MEMBRANE_PROPOSAL.md
- CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
- RUNTIME_AUTHORITY_LEASES_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: computer-use-task-runner
implements:
- task-runner
implemented_by:
- desktop-observe-metadata-scaffold
active_seams:
- desktop-runner-materialization
- desktop-action-approval-policy
- desktop-observation-contract
- runner-materialization-policy
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Computer Use Task Runner Proposal

## Goal

Define the first honest architecture boundary for computer-use automation in Philotic.

Computer use means OS-level or desktop-session-bound actions such as:

- observing the current screen or application state
- taking screenshots
- clicking, typing, scrolling, and pressing keys
- routing those actions through explicit approval and lease policy

The goal is not to make the desktop membrane into an executor. The goal is to give Philotic a concrete desktop automation runner that can be routed, constrained, audited, and materialized like other host-bound task runners.

## Core Recommendation

Introduce a pinned desktop task-runner family for computer-use actions.

Recommended family names:

- `task-runner.desktop`
- `task-runner.cua`

Recommended abstract tools:

- `desktop.observe`
- `desktop.screenshot`
- `desktop.click`
- `desktop.type`
- `desktop.key`
- `desktop.scroll`

These tools should be model-visible only when the session posture, runner availability, and approval policy all allow them.

The desktop membrane remains the operator ingress, approval, and visibility surface. It should not own OS automation execution authority. That separation matters because a web UI that can directly move the mouse is not a membrane anymore; it is a tiny monarchy with CSS.

## Disposition

Accepted for the current design slice.

Current repo truth:

- generic `tool-runner` exists and already proves external routed execution for workspace and shell tooling
- `TASK_RUNNER_PROPOSAL.md` already defines pinned, environment-bound, policy-constrained runner families
- `DESKTOP_MEMBRANE_PROPOSAL.md` already defines desktop membrane as bounded operator ingress rather than privileged direct authority
- `desktop.observe` is now advertised as a pinned desktop task-runner tool and returns metadata-only observation scaffolding
- screenshot and input actions are not implemented

This proposal narrows the next implementation path while naming the first landed scaffold honestly.

## Current Slice

This slice records the CUA boundary in architecture and graph-managed proposal surfaces, then lands the first low-agency `desktop.observe` scaffold.

The landed implementation is deliberately low-agency:

1. `desktop.observe` is registered in the Philote catalog and hotel abstract tool catalog
2. `desktop.observe` routes as a pinned `desktop` task-runner tool
3. the current `tool-runner` advertises and subscribes to `tool.desktop.observe`
4. the result is metadata-only JSON with runner, hotel, environment, desktop-session, and redaction posture fields
5. screenshot, click, type, key, and scroll remain unavailable until the approval and artifact-policy seams are explicit

## Boundary Model

### Capability Layer

The model sees abstract desktop tools only when policy permits.

Example:

```json
{
  "tool_name": "desktop.screenshot",
  "policy_class": "desktop-observe",
  "execution_mode": "pinned",
  "tags": ["desktop", "observe", "computer-use"]
}
```

### Runner Layer

The desktop runner owns execution.

It should be:

- incarnation-bound
- tied to a real host and desktop session
- lease-aware
- policy-constrained
- auditable
- explicit about whether it can observe only or mutate input state

Recommended first runner identity:

```toml
[[task_runners]]
runner_id = "task-runner-desktop-local-01"
incarnation_id = "task-runner-desktop-local-01"
family = "desktop"
profile = "observe"
hotel_id = "local-aiua-01"
environment_id = "desktop://local/default"
allowed_tools = ["desktop.screenshot", "desktop.observe"]
materialization_mode = "manual_only"
idle_policy = "terminate_after"
availability_state = "materializable"
```

### Membrane Layer

The desktop membrane may:

- present observation results to the operator
- present approval requests
- show active desktop runner state
- show target host/session attribution
- revoke or pause desktop automation through the hotel/control plane

The desktop membrane should not:

- execute raw OS actions directly
- bypass tool assembly
- mint its own desktop automation authority
- treat local browser presence as approval
- keep acting after its lease is lost

## Approval Policy

Computer-use tools need a stricter posture split than ordinary workspace reads.

Recommended policy classes:

- `desktop-observe`: screenshot, observe, window/app metadata
- `desktop-input-low`: benign click/key paths that still affect state
- `desktop-input-high`: typing, destructive UI actions, submit/send/confirm flows
- `desktop-credential-risk`: any path likely to reveal or enter secrets

Initial rule:

- implement observation before input
- require explicit approval before any input mutation
- require a stronger target-scoped approval before typing or submit-like actions
- suppress desktop tools entirely on low-intent conversational turns

The irony is that the safest computer-use runner is initially the one that mostly refuses to use the computer. This is annoying, and also correct.

## Observation Contract

The observation result should carry:

- `runner_id`
- `incarnation_id`
- `hotel_id`
- `environment_id`
- `desktop_session_id`
- `tool_name`
- `captured_at`
- `content_type`
- artifact reference or redacted inline summary
- redaction posture
- approval/request id when applicable

Raw screenshots should be treated as sensitive artifacts. The model may receive a shaped observation or reference depending on policy; the operator surface may receive richer detail under its membrane lease.

## Materialization Policy

Desktop runners should start stricter than workspace runners.

Recommended v1:

- `materialization_mode = "manual_only"` for input-capable runners
- `materialization_mode = "on_demand"` is acceptable only for observe-only runners once the lease path is proven
- runner must bind to a concrete interactive desktop session
- remote desktop automation must fail closed unless target-hotel authority, reachability, and operator posture are explicit

This belongs to the routing/materialization layer above the runner. The runner executes; the hotel decides whether it may be materialized and which session it may bind.

## Relationship To Existing Proposals

This proposal refines [TASK_RUNNER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TASK_RUNNER_PROPOSAL.md) by adding a `desktop` runner family.

It refines [TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md) by naming computer-use tools as `pinned` rather than fungible capabilities.

It preserves [DESKTOP_MEMBRANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DESKTOP_MEMBRANE_PROPOSAL.md) by keeping the desktop membrane as operator surface and approval/visibility boundary, not executor.

## Open Questions

1. Should the first runner shell out to the local Codex/Computer Use substrate, or should Philotic own a native macOS accessibility/screenshot backend from the start?
2. What is the canonical artifact store for raw screenshots and observation frames?
3. How should remote desktop sessions be named when a hotel manages more than one interactive desktop?
4. Which approval UX should own high-agency CUA requests first: desktop membrane, Telegram, or the canonical session approval stream?
5. How much shaped observation should go to the model by default versus staying behind an operator-visible artifact reference?

## Reality Gap

There is no dedicated standalone CUA runner yet.

The current scaffold proves routing and metadata shape through the existing `tool-runner`, but click/type/key tools should remain unavailable until the approval and observation seams are explicit enough to prevent accidental ambient desktop authority.
