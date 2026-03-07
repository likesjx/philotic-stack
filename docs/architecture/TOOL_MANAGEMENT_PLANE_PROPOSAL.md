# Philotic Tool Management Plane Proposal

## Goal

Define the system-level tool management layer that sits above session-time tool assembly.

This proposal assumes:

- tool runners are known system artifacts
- environments are runtime-discovered
- agent toolsets should be established before a session begins
- sessions should usually narrow or hide tools, not invent the tool universe from scratch

That lets us keep building the current agent/runtime flow without prematurely building the entire management plane in code.

## Core Recommendation

Introduce a canonical tool management plane in the Context Graph with three layers:

1. system tool plane
2. agent default tool plane
3. session effective tool plane

The hierarchy should be:

- system defines what exists
- agent defines what it can generally use
- session defines what is visible and executable right now

## Why This Layer Exists

Session-time `ToolAssembly` is necessary, but it is not the whole story.

Without a system plane, the session has to answer questions that should already be known:

- which tool runners exist at all
- which abstract tools each runner offers
- which runner implementations are known artifacts
- which environments are available on which hotels
- which agents should generally be allowed to use which tools

That is asking the session layer to do ontology, discovery, and routing preparation all at once. It can do that, but only in the same way a backpack can technically be a filing cabinet if you stop caring about access patterns.

## Canonical Model

### 1. Tool Runner

A `tool_runner` is a known implementation artifact.

Properties:

- `runner_id`
- `implementation_kind`
  - `rust`
  - `python`
  - `node`
  - `mcp`
  - `remote-service`
- `artifact_ref`
- `version`
- `materialization_modes`
- `supported_transports`
- `default_environment_requirements`
- `status_policy`

This is the thing we can know ahead of time.

### 2. Tool

A `tool` is the abstract callable capability.

Properties:

- `tool_name`
- description
- input schema
- output schema
- policy class
- tags

This is what the model and agent should reason over.

### 3. Environment

An `environment` is a discovered execution substrate associated with a hotel/runtime.

Properties:

- `environment_id`
- `hotel_id`
- `kind`
  - `local-os`
  - `workspace`
  - `container`
  - `remote-node`
  - `mcp-host`
  - `credential-zone`
- `capabilities`
- `constraints`
- `transports`
- `mounts` or `resource_refs`

This is not fully knowable ahead of time in the same way runners are.

### 4. Agent Default Toolset

An agent should have a durable default tool universe.

Properties:

- `agent_id`
- `default_tools`
- `default_tool_families`
- `default_environment_preferences`
- `default_runner_preferences`
- `default_policy_overrides`

This is the baseline used before any session exists.

### 5. Session Effective Toolset

A session should generally narrow or hide tools relative to the agent default.

Properties:

- `session_id`
- `effective_tools`
- `hidden_tools`
- `tool_filters`
- `environment_override`
- `runner_override`

This exists for:

- context management
- token reduction
- task relevance
- safety narrowing
- reducing model confusion

## Graph Shape

Recommended graph entities:

- `tool_runner`
- `tool`
- `environment`
- `hotel`
- `agent`
- `session`

Recommended edges:

- `tool_runner --OFFERS--> tool`
- `tool_runner --REQUIRES_ENV--> environment_trait`
- `hotel --HAS_ENV--> environment`
- `hotel --CAN_MATERIALIZE--> tool_runner`
- `agent --DEFAULT_TOOL--> tool`
- `agent --PREFERS_ENV--> environment_trait`
- `agent --PREFERS_RUNNER--> tool_runner`
- `session --USES_TOOL--> tool`
- `session --HIDES_TOOL--> tool`

## Toolset Lifecycle

### System Level

Defines the superset of available capabilities:

- known runners
- known tools
- known implementation artifacts
- discovered environments per hotel

### Agent Level

Defines the default working tool universe:

- what this agent generally has access to
- preferred environments
- preferred runner classes

This should be computed before a session starts.

### Session Level

Defines the scoped projection:

- which tools are visible to the model
- which tools are intentionally hidden
- which environment/runner preferences are overridden

The session should not usually be choosing from the whole system universe directly.

## Tool Assembly Relationship

`ToolAssembly` remains necessary, but it becomes a projection of the management plane rather than its substitute.

Inputs should become:

- system tool plane
- discovered hotel environments
- agent default toolset
- session filters/overrides
- runner liveness/materialization state

Outputs stay:

- `tools_for_model`
- `execution_routes`
- `policy_annotations`

So:

- tool management plane = canonical capability topology
- tool assembly = per-session executable projection

## Environment and Transport Rules

A runner may need a specific environment and may not execute over local IPC.

Therefore routing must stay transport-agnostic.

Examples:

- `execution_mode = ipc`
- `execution_mode = mesh`
- `execution_mode = mcp`
- `execution_mode = remote_http`
- `execution_mode = subprocess`

Environment examples:

- local workspace mounted on hotel A
- shell access only on hotel B
- credentialed remote deployment environment on hotel C

So a route must answer:

- where should this runner materialize
- what transport reaches it
- is it live, dormant, or unavailable

## Recommended Near-Term Behavior

We do not need to implement the full system plane before finishing the current agent/runtime work.

Near-term assumptions:

1. treat the system tool plane as canonical future architecture
2. continue using the current graph-backed `ToolAssembly` and runner registry as a transitional implementation
3. keep agent logic working against abstract tools only
4. avoid hardcoding local-IPC-only assumptions into the agent
5. let session bindings continue to narrow/hide tools while the system plane is formalized

## Implementation Phasing

### Phase A

Continue current work:

- finish agent loop/tool runtime functionality
- keep `ToolAssembly`
- keep durable runner registry
- keep dormant/live signaling

### Phase B

Add canonical management-plane records:

- `tool_runner`
- `tool`
- `environment`
- agent default toolset bindings

### Phase C

Refactor `ToolAssembly` to derive from:

- system tool plane
- agent default toolset
- session filters
- runner/environment availability

### Phase D

Add materialization and routing policy:

- runner selection rules
- environment preference rules
- wake/sleep policy
- fallback behavior

## Recommendation Summary

The system should not treat session-time tool assembly as the place where the world is discovered.

Instead:

- runners are known
- environments are discovered
- agents have default toolsets before sessions begin
- sessions narrow or hide tools for relevance and safety
- `ToolAssembly` is the executable projection of that larger management plane

That is the architecture we should now assume while we keep finishing the functionality we are already building in the agent/runtime path.
