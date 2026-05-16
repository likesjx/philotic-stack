---
title: Philotic Task Runner Proposal
doc_type: proposal
domain: tooling-execution
status: accepted-current-slice
last_updated: 2026-04-24
tags:
- task-runner
- tooling
- execution
- workspace
- active-seam
related_docs:
- ARCHITECTURE_STATUS.md
- TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md
- TOOL_MANAGEMENT_PLANE_PROPOSAL.md
- RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md
- COMPUTER_USE_TASK_RUNNER_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: task-runner
implements: []
implemented_by:
- workspace-runner-overlay-slice
- workspace-runner-base-policy-slice
- desktop-observe-metadata-scaffold
active_seams:
- shell-runner-split
- desktop-runner-materialization
- runner-materialization-policy
- unreachable-runner-fallback
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Philotic Task Runner Proposal

## Goal

Define the execution model for filesystem- and shell-oriented work that must happen in a real environment rather than inside `philote`.

This proposal assumes:

- the loop should plan with abstract tools
- filesystem and shell execution are environment-bound
- the executor for those tools should be incarnation-specific
- failure and reachability policy should live above the runner, not inside it

## Disposition

Proposed and accepted as the intended direction for the next task-runner/tooling slices.

Current code already points this way:

- `workspace.*` tools are classified as `pinned`
- routed execution already carries runner/incarnation/hotel/environment metadata
- the current `tool-runner` is functioning as an initial external executor
- first coding slice landed:
  - route metadata can now carry `task_runner_kind`
  - execution payloads can now carry a `task_runner_overlay`
  - workspace execution uses that overlay to resolve the effective workspace binding
- second coding slice landed:
  - workspace runners now have a real base config surface
  - overlays can narrow allowed tools and execution limits
  - workspace read/search limits are enforced by runner policy instead of only by convention
- third coding slice landed:
  - workspace runner base policy now rides in hotel/session-driven route metadata
  - `tool-runner` treats env vars as fallback defaults instead of primary truth
  - canonical session snapshots now include workspace runner base config for routed workspace tools
- fourth coding slice landed:
  - `desktop.observe` is cataloged and routed as a pinned `desktop` task-runner tool
  - `tool-runner` advertises `desktop.observe` and returns metadata-only observation scaffolding
  - screenshot and input actions remain deferred behind approval and artifact-policy seams

Still pending:

- explicit task-runner specialization and configuration beyond the first `desktop.observe` scaffold
- shell runner split
- unreachable-incarnation fallback/materialization policy

Track follow-on work in [task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md) under `Next Project: Tool Assembly and Routed Execution`.

## Core Recommendation

Introduce a first-class `task-runner` concept for execution that is:

- environment-specific
- incarnation-bound
- agent/session constrained through overlays

Use it for:

- filesystem access
- shell execution
- computer-use/desktop automation
- other host/resource-bound work

Keep it separate from:

- `local_agent` tools
- generic capability-routed tools that are fungible across environments

## Why This Split Matters

`workspace.read` only looks like a generic capability until you remember that files insist on existing somewhere.

The same is true for:

- `workspace.list`
- `workspace.search`
- `workspace.write`
- `shell.exec`
- `desktop.observe`
- `desktop.screenshot`
- later `desktop.click`, `desktop.type`, `desktop.key`, and `desktop.scroll`

These tools are abstract for planning purposes, but they are pinned at execution time because they target:

- a concrete workspace root
- a concrete environment
- a concrete hotel or host
- often a concrete shell/runtime profile

If we blur that boundary, we will slowly reinvent “arbitrary host execution” under the friendlier name of “workspace tools,” which is a charming way to lose trust.

## Task Runner Layers

### 1. Capability Layer

This is what the loop and model see.

Examples:

- `workspace.read`
- `workspace.list`
- `workspace.search`
- `workspace.write`
- `shell.exec`

These remain abstract tool names.

### 2. Task Runner Layer

This is the concrete executor that actually performs the work.

A task runner is:

- a tool-runner specialization
- incarnation-bound
- attached to a real environment
- policy-constrained

Examples:

- `task-runner.workspace.local`
- `task-runner.workspace.repo-a`
- `task-runner.shell.devbox`

### 3. Overlay Layer

This is where agent/session-specific behavior belongs.

Base runner config should define:

- workspace root
- environment identity
- hotel identity
- shell availability
- filesystem policy
- command policy
- materialization characteristics

Agent/session overlays should define:

- which runner incarnations are allowed
- preferred workspace/environment
- allowed tool subset
- approval mode
- command/profile restrictions

That keeps us from needing one unique runner identity per agent unless we deliberately want hard isolation.

## Recommended Taxonomy

### Agent Tools

These remain `local_agent`.

Good uses:

- session inspection
- session config mutation
- agent config mutation
- lightweight deterministic logic

These should not be allowed to quietly become host execution with better branding.

### Capability Tools

These stay abstract and fungible.

Good examples:

- `memory.recall`
- `graph.read`
- maybe some remote search or retrieval tools

These should route to the best eligible provider without requiring a specific host environment.

### Task Runner Tools

These are planned abstractly but executed as `pinned`.

Good examples:

- `workspace.list`
- `workspace.read`
- `workspace.search`
- `workspace.write`
- `shell.exec`

They should route through a preassembled execution envelope that already identifies the chosen incarnation and environment.

## Workspace vs Shell

Even if one binary implements both, they should be treated as different capability families.

Recommended split:

- first split by execution surface:
  - `workspace`
  - `shell`
- then split by trust/policy profile within each family
- do not split by agent identity unless hard isolation is actually required

That gives us a sane progression:

1. one workspace family
2. one shell family
3. multiple incarnations or policy profiles per family as needed

This avoids two common mistakes:

- one giant runner that does everything badly
- one runner per agent, which looks tidy until operational life begins

### Workspace Family

- `workspace.list`
- `workspace.read`
- `workspace.search`
- `workspace.write`

Policy focus:

- root/path constraints
- file size limits
- write restrictions
- binary/text handling

### Shell Family

- `shell.exec`
- possibly `shell.background`
- possibly `shell.stream`

Policy focus:

- command allow/deny lists
- timeout and output limits
- environment variables
- working directory
- streaming/interruption behavior

This split matters because “list files” and “run shell commands” should not share the same trust posture just because both happen on a laptop.

### Desktop / CUA Family

- `desktop.observe`
- `desktop.screenshot`
- later `desktop.click`
- later `desktop.type`
- later `desktop.key`
- later `desktop.scroll`

Policy focus:

- desktop-session binding
- operator/session approval posture
- observation redaction
- screenshot artifact handling
- high-agency input gating
- lease-aware materialization

Computer-use automation should be a pinned desktop runner family, not a desktop membrane shortcut. The membrane can present the approval and observation surface; the runner owns execution.

## Recommended Family Split

### Workspace Runner

Purpose:

- operate on files rooted in a specific workspace or mount

Core tools:

- `workspace.list`
- `workspace.read`
- `workspace.search`
- `workspace.write`

Optional future tools:

- `workspace.stat`
- `workspace.move`
- `workspace.delete`
- `workspace.patch`

Recommended profiles:

- `workspace.readonly`
- `workspace.edit`
- `workspace.artifacts`

### Shell Runner

Purpose:

- execute commands in a specific working environment

Core tools:

- `shell.exec`

Optional future tools:

- `shell.background`
- `shell.stream`
- `shell.cancel`

Recommended profiles:

- `shell.readonly_inspect`
- `shell.dev_exec`
- `shell.privileged`

The important distinction is:

- workspace runners are primarily about files and path-scoped resources
- shell runners are about process execution and command policy

They may share an implementation binary later, but they should not share a default trust model.

## Configuration Model

### Runner Base Config

Properties:

- `runner_id`
- `incarnation_id`
- `kind`
  - `workspace`
  - `shell`
- `hotel_id`
- `environment_id`
- `workspace_root`
- `shell_profile`
- `filesystem_policy`
- `command_policy`
- `availability_state`

Recommended v1 additions:

- `family`
  - `workspace`
  - `shell`
- `profile`
  - `readonly`
  - `edit`
  - `dev_exec`
  - etc.
- `allowed_tools`
- `materialization_mode`
  - `always_on`
  - `on_demand`
  - `manual_only`
- `idle_policy`
  - `keep_warm`
  - `sleep_after`
  - `terminate_after`

### Agent/Session Overlay

Properties:

- `allowed_incarnations`
- `preferred_incarnation`
- `preferred_hotel_id`
- `preferred_environment_id`
- `workspace_override`
- `policy_override`

Recommended v1 additions:

- `allowed_tools`
- `approval_profile`
- `max_read_bytes`
- `max_search_results`
- `allowed_commands`
- `denied_commands`
- `working_directory_override`
- `environment_variable_whitelist`

The overlay should narrow and tune. It should not silently widen a runner past its base policy.

## Minimal Config Shape (V1)

```toml
[[task_runners]]
runner_id = "task-runner-workspace-01"
incarnation_id = "task-runner-workspace-01"
family = "workspace"
profile = "readonly"
hotel_id = "local-aiua-01"
environment_id = "workspace://main"
workspace_root = "/srv/philotic/workspaces/main"
allowed_tools = ["workspace.list", "workspace.read", "workspace.search"]
materialization_mode = "on_demand"
idle_policy = "sleep_after"
availability_state = "materializable"

[task_runners.filesystem_policy]
allow_absolute_paths = false
allow_parent_traversal = false
max_read_bytes = 262144
max_search_results = 50

[[task_runner_overlays]]
agent_id = "agent-jane-01"
session_id = "telegram:123:agent-jane-01"
incarnation_id = "task-runner-workspace-01"
workspace_override = "workspace://main"
allowed_tools = ["workspace.list", "workspace.read", "workspace.search"]
approval_profile = "workspace_readonly"
max_search_results = 25
```

For shell:

```toml
[[task_runners]]
runner_id = "task-runner-shell-dev-01"
incarnation_id = "task-runner-shell-dev-01"
family = "shell"
profile = "dev_exec"
hotel_id = "local-aiua-01"
environment_id = "env://devbox"
workspace_root = "/srv/philotic/workspaces/main"
allowed_tools = ["shell.exec"]
materialization_mode = "manual_only"
idle_policy = "terminate_after"
availability_state = "dormant"

[task_runners.command_policy]
allowed_commands = ["ls", "rg", "cat", "git status"]
denied_commands = ["rm", "sudo", "shutdown"]
default_timeout_seconds = 15
max_output_bytes = 65536
```

## Unavailable Incarnation / Unreachable Hotel

This must be handled outside the task runner itself.

The runner should not be responsible for deciding:

- whether to materialize elsewhere
- whether to reroute to a less preferred environment
- whether to fail closed
- whether to wait for a hotel to return

That belongs to the routing/materialization layer above it.

The route/materialization policy should distinguish:

- `live`
- `dormant`
- `materializable`
- `unreachable`
- `unavailable`

And should decide, based on policy:

- materialize the preferred incarnation
- reroute to another eligible incarnation
- fail cleanly
- wait/retry within bounds

The important rule is:

- task runners execute
- routing/materialization policy decides what to do when the intended executor is not reachable

## Proposed Near-Term Path

1. Keep current `workspace.*` tools pinned.
2. Treat the current external `tool-runner` as the first task-runner scaffold.
3. Split the conceptual families:
   - workspace runner
   - shell runner
4. Add explicit runner base config plus agent/session overlays.
5. Define fallback/materialization policy in the routing layer, not the runner.

## Recommendation

Adopt `task-runner` as the conceptual home for filesystem and shell execution.

That means:

- workspace/bash work is incarnation-bound
- agent/session policy overlays constrain access
- execution remains abstract to the loop
- failure handling for unreachable incarnations is handled above the runner

This is the right place to be strict. The minute host-bound execution stops feeling explicit, the system starts lying about where side effects really come from.
