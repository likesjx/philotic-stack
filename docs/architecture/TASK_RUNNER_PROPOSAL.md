# Philotic Task Runner Proposal

## Goal

Define the execution model for filesystem- and shell-oriented work that must happen in a real environment rather than inside `agent-core`.

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

Still pending:

- explicit task-runner specialization and configuration
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

### Agent/Session Overlay

Properties:

- `allowed_incarnations`
- `preferred_incarnation`
- `preferred_hotel_id`
- `preferred_environment_id`
- `workspace_override`
- `policy_override`

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
