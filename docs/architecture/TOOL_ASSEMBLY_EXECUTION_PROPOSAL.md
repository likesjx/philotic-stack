---
title: Philotic Tool Assembly and Execution Proposal
doc_type: proposal
domain: tooling-execution
status: accepted-current-slice
last_updated: 2026-03-31
tags:
- tools
- execution
- routing
- runners
- active-seam
related_docs:
- ARCHITECTURE_STATUS.md
- TASK_RUNNER_PROPOSAL.md
- TOOL_MANAGEMENT_PLANE_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: tool-assembly-execution
implements: []
implemented_by:
- tool-assembly-routing-slice
- allowed-runner-incarnations-slice
active_seams:
- route-readiness-checks
- runner-fallback-policy
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Philotic Tool Assembly and Execution Proposal

## Goal

Define a tool model where:

- the model sees abstract tools
- the agent invokes abstract tools
- the runtime resolves execution routing
- concrete tool runners perform the work

This keeps implementation details out of the model and out of most of `philote`.

The model should not know whether a tool is:

- local
- remote
- MCP-backed
- another guest
- lazily materialized
- composed from multiple underlying systems

If it has to know that, we have already leaked the wrong abstraction.

## Disposition

Accepted for the current slice and partially implemented.

Implemented so far:

- first-class `ToolAssembly`
- model-facing tool definitions
- runtime-facing execution routes
- externalized routed tool execution via `tool-runner`
- live vs materialization-required route signaling
- execution-mode taxonomy in the runtime:
  - `local_agent`
  - `capability`
  - `pinned`
- first basic tool-family split:
  - `session.status` stays `local_agent`
  - `echo` stays `capability`
  - `workspace.list`, `workspace.read`, and `workspace.search` are now treated as `pinned`
- incarnation-aware execution metadata in assembled routes
- session `allowed_tool_runner_incarnations` can now define visible tools and preferred execution routes
- preference-aware route ranking for:
  - preferred incarnation
  - preferred runner
  - preferred hotel
  - preferred environment
- explicit route `selection_reason` values now reflect when a preferred route wins even if it requires materialization
- `philote` now translates abstract tool calls into preassembled route envelopes instead of rediscovering routing at call time

Still pending in [task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md):

- materialization policy
- richer runner selection and fallback

## Core Recommendation

Introduce a first-class `ToolAssembly` layer between session bindings and tool execution.

That layer should:

1. determine which tools are eligible for the session
2. merge agent defaults and session overrides
3. resolve execution routes for each eligible tool
4. verify runner availability
5. optionally materialize missing runners
6. produce:
   - a model-facing abstract tool catalog
   - a runtime-facing execution routing table
   - policy annotations for approval/governance

Then:

- `philote` plans with the abstract catalog
- `philote` invokes tools through a runtime `ToolExecutor`
- `ToolExecutor` dispatches to the resolved runner
- the runner returns a normalized tool result

## Architectural Layers

### 1. Tool Definition Layer

This is the abstract tool contract.

Fields:

- `tool_name`
- description
- input schema
- output schema
- policy class
- capability tags

Example:

```json
{
  "tool_name": "workspace.read_file",
  "description": "Read a file from the active workspace",
  "input_schema": {
    "type": "object",
    "properties": {
      "path": { "type": "string" }
    },
    "required": ["path"]
  },
  "output_schema": {
    "type": "object",
    "properties": {
      "content": { "type": "string" }
    }
  },
  "policy_class": "workspace-read",
  "tags": ["workspace", "read-only"]
}
```

This is what the model should reason over.

## 2. Tool Assembly Layer

This is the runtime preparation phase that turns desired capability into executable capability.

Inputs:

- agent default tool families
- allowed runner incarnations
- session bindings
- session approval policy
- current hotel/runtime availability
- available tool runners and their advertisements

Outputs:

- `tools_for_model`
- `execution_routes`
- `policy_annotations`
- runner environment/materialization requirements

When a session has an explicit allowed-incarnation set, `ToolAssembly` should derive visible tools from those incarnations first and only use `effective_toolset` as a narrowing/hiding layer. That keeps capability visibility aligned with actual routable execution rather than treating the tool list as magical truth.

Example:

```json
{
  "tools_for_model": [
    {
      "tool_name": "workspace.read_file",
      "description": "Read a file from the active workspace",
      "input_schema": {
        "type": "object",
        "properties": {
          "path": { "type": "string" }
        },
        "required": ["path"]
      }
    }
  ],
  "execution_routes": {
    "workspace.read_file": {
      "target_node": "local-aiua-01",
      "target_role": "tool.workspace",
      "runner_id": "workspace-runner-1",
      "execution_mode": "ipc",
      "availability_state": "live"
    }
  },
  "policy_annotations": {
    "workspace.read_file": {
      "policy_class": "workspace-read",
      "approval_required": false
    }
  }
}
```

Only `tools_for_model` belongs in prompt/model context.

`execution_routes` and runner details are runtime concerns.

Current Philotic implementation note:

- route envelopes now carry runner/incarnation/hotel/environment metadata plus a `selection_reason`
- `philote` uses that prepared envelope when dispatching external tool calls
- `local_agent` tools are resolved entirely within the loop runtime and are intentionally limited to simple logic or self/session configuration behavior

`execution_mode` must stay general. A routed tool may execute over:

- local IPC
- remote hotel IPC
- mesh transport
- MCP bridge
- subprocess or container runtime

Tool assembly therefore also needs to answer:

- which environment the runner must materialize in
- which transport is required to reach it
- whether the runner is currently live or only registered/dormant

## 3. Agent Tool Abstraction

`philote` should not execute real tools directly.

Instead it should:

- receive an abstract tool call from the model
- validate that the tool exists in the assembled catalog
- hand it to a `ToolExecutor`
- wait for a normalized result

This means the agent should know:

- abstract `tool_name`
- normalized arguments

It should not know:

- process IDs
- hotel routing specifics
- whether the tool is local or remote
- how the runner was materialized

## 4. Tool Executor Layer

The `ToolExecutor` is the adapter between abstract tool calls and concrete execution routes.

Responsibilities:

- look up the route for an abstract tool
- confirm the route is still valid
- resolve the chosen incarnation, not just the abstract runner artifact
- request/ensure runner availability
- dispatch the call
- await a normalized result envelope

This can begin as a runtime helper in `philote`, but it should conceptually be an execution client to hotel-owned routing metadata, not a pile of local tool implementations.

The longer-term routing model should distinguish:

- runner artifact
- runner incarnation
- environment

so execution can target the correct runnable instance instead of pretending every offered tool maps directly to one generic runner.

It also needs enough policy to decide:

- whether to wait for materialization
- how long to wait
- whether to retry in another environment
- when to fail back to the agent with a materialization-needed result

## 5. Tool Runner Layer

A tool runner is a separate component/guest/process that actually performs tool work.

Examples:

- `tool.workspace`
- `tool.shell`
- `tool.remote`
- `tool.ansible`
- `tool.memory`

Responsibilities:

- advertise supported abstract tools
- accept structured execution requests
- execute the real work
- return structured results/errors

This is where implementation details live.

## Session Relationship

Session bindings define which tool families are intended to be available.

Tool assembly turns those bindings into executable reality.

So:

- session bindings = capability intent
- tool assembly = executable availability

That distinction matters because a session can ask for a tool family before a runner is online, before routing is known, or before policy/availability checks are satisfied.

## Approval Relationship

Approval should act on abstract tool calls and policy classes, not on runner implementation details.

Example:

- model asks for `workspace.read_file`
- assembly marks it `policy_class = workspace-read`
- approval layer decides whether that class requires pause, pre-approval, or denial

The user should approve:

- “read from workspace”

not:

- “send IPC to `tool.workspace` on `local-aiua-01` with runner lease `abc123`”

That would be technically rich and humanly useless.

## Proposed Runtime Flow

### 1. Session snapshot load

`philote` loads canonical session snapshot:

- session status
- approval policy
- effective bindings
- recent turns

### 2. Tool assembly

Hotel/runtime assembles the session tool environment:

- resolve tool definitions
- resolve routes
- verify runners
- produce `ToolAssembly`

### 3. Prompt construction

`philote` includes only abstract tool definitions in the model-facing context.

### 4. Tool call

Model returns:

```json
{
  "kind": "tool_call",
  "tool_name": "workspace.read_file",
  "arguments": {
    "path": "README.md"
  }
}
```

### 5. Runtime resolution

`ToolExecutor` uses `ToolAssembly.execution_routes["workspace.read_file"]`

### 6. Dispatch

Hotel routes to the concrete runner:

- `target_role = tool.workspace`
- `target_node = local-aiua-01`

### 7. Result normalization

Runner returns:

```json
{
  "tool_name": "workspace.read_file",
  "ok": true,
  "content": {
    "text": "..."
  }
}
```

### 8. Loop resume

`philote` consumes normalized tool result and continues.

## What Belongs Where

### `philote`

- tool planning
- abstract tool invocation
- waiting/resume behavior
- normalized result handling

### `aiua`

- canonical session and binding ownership
- tool assembly
- runner discovery / routing
- optional runner materialization
- event persistence

### tool runners

- concrete execution

## Skills in This Model

Skills should not be the same thing as tool runners.

Recommended distinction:

- skills
  - shape planning, strategy, and relevant capability families
- tools
  - executable abstract operations
- runners
  - concrete implementations of tools

So skills come into play:

1. during tool assembly
   - deciding which tool families are relevant for the session
2. during prompt construction
   - steering how the agent thinks/plans
3. later during loop policy
   - e.g. `planner`, `executor`, `analyst`, `operator`

Skills should not decide process routing directly.

## Recommended First Implementation Slice

### Phase 1

- keep the current abstract tool-call contract
- introduce a `ToolAssembly` struct
- add hotel-composed `tools_for_model` and `execution_routes`
- keep one simple externalized tool runner, even if it only wraps `echo`

### Phase 2

- replace local tool execution in `philote` with routed execution through `ToolExecutor`
- persist tool request/result events explicitly

### Phase 3

- add runner discovery/materialization checks
- add workspace tool family as a real external toolset

## Full Recommendation

- abstract tool use away from execution details
- add a first-class `ToolAssembly` layer
- make sessions define intended capability, not concrete implementation
- make the hotel own route resolution and runner readiness
- make tool runners separate components/processes
- keep `philote` focused on planning and normalized orchestration

That gives us the separation we actually want:

- the model chooses *what*
- the agent orchestrates *when and why*
- the runtime decides *where*
- the runner decides *how*
