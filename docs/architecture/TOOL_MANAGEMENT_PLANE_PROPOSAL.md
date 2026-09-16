---
title: Philotic Tool Management Plane Proposal
doc_type: proposal
domain: tooling-execution
status: accepted-current-slice
last_updated: 2026-09-16
tags:
- tools
- management-plane
- runners
- graph
- active-seam
related_docs:
- ARCHITECTURE_STATUS.md
- TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md
- TASK_RUNNER_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: tool-management-plane
implements: []
implemented_by: []
active_seams:
- tool-management-plane-records
- agent-default-toolsets
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Philotic Tool Management Plane Proposal

## Goal

Define the system-level tool management layer that sits above session-time tool assembly.

This proposal assumes:

- tool runners are known system artifacts
- environments are runtime-discovered
- agent toolsets should be established before a session begins
- sessions should usually narrow or hide tools, not invent the tool universe from scratch

That lets us keep building the current agent/runtime flow without prematurely building the entire management plane in code.

## Disposition

Proposed and accepted as the assumed future model.

This plane is not fully implemented yet. Current code now contains:

- an initial runner registry
- session-time tool assembly projection
- session-scoped allowed-incarnation bindings that can derive visible tools and execution routes
- initial execution taxonomy in the runtime (`local_agent`, `capability`, `pinned`)

Track the remaining work in [task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md) under `Next Project: Tool Assembly and Routed Execution` and `Deferred Design Threads`.

## Core Recommendation

Introduce a canonical tool management plane in the Context Graph with four layers:

1. system runner plane
2. incarnation and environment plane
3. agent default capability plane
4. session effective tool plane

The hierarchy should be:

- the system defines which runner artifacts exist
- incarnations define which runnable or materializable instances exist in which environments
- agents map to the incarnations and tools they may generally use
- sessions define what is visible and executable right now

## Why This Layer Exists

Session-time `ToolAssembly` is necessary, but it is not the whole story.

Without a system plane, the session has to answer questions that should already be known:

- which tool runners exist at all
- which abstract tools each runner offers
- which incarnations are actually runnable or materializable
- which environments are available on which hotels
- which agents should generally be allowed to use which incarnations and tools

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

### 2. Tool Runner Incarnation

An incarnation is a fully defined running or materializable instance of a tool runner.

Properties:

- `incarnation_id`
- `runner_id`
- `hotel_id`
- `environment_id`
- `status`
  - `live`
  - `dormant`
  - `materializable`
  - `unavailable`
- `transport_modes`
- `endpoint_ref`
- `policy_overrides`

This is the thing an agent can actually route to.

### 3. Tool

A `tool` is the abstract callable capability.

Properties:

- `tool_name`
- description
- input schema
- output schema
- policy class
- tags

This is what the model and agent should reason over.

### 4. Environment

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

### 5. Agent Default Toolset

An agent should have a durable default tool universe.

Properties:

- `agent_id`
- `allowed_incarnations`
- `default_tools`
- `default_tool_families`
- `default_environment_preferences`
- `default_incarnation_preferences`
- `default_policy_overrides`

This is the baseline used before any session exists.

The important distinction is:

- runners define what can exist
- incarnations define what can run
- agents define what they may generally use

Current transitional implementation note:

- sessions can already carry `allowed_tool_runner_incarnations`
- that set now acts as the executable boundary for visible tools when present
- the long-term management plane should move that default capability mapping up to the agent/system layer before session start

### 6. Session Effective Toolset

A session should generally narrow or hide tools relative to the agent default.

Properties:

- `session_id`
- `effective_tools`
- `hidden_tools`
- `tool_filters`
- `environment_override`
- `incarnation_override`

This exists for:

- context management
- token reduction
- task relevance
- safety narrowing
- reducing model confusion

## Graph Shape

Recommended graph entities:

- `tool_runner`
- `tool_runner_incarnation`
- `tool`
- `environment`
- `hotel`
- `agent`
- `session`

Recommended edges:

- `tool_runner --OFFERS--> tool`
- `tool_runner --REQUIRES_ENV--> environment_trait`
- `tool_runner --CAN_MATERIALIZE_AS--> tool_runner_incarnation`
- `tool_runner_incarnation --RUNS_IN--> environment`
- `hotel --HAS_ENV--> environment`
- `hotel --CAN_MATERIALIZE--> tool_runner`
- `agent --CAN_USE_INCARNATION--> tool_runner_incarnation`
- `agent --DEFAULT_TOOL--> tool`
- `agent --PREFERS_ENV--> environment_trait`
- `agent --PREFERS_INCARNATION--> tool_runner_incarnation`
- `session --USES_TOOL--> tool`
- `session --HIDES_TOOL--> tool`

## Toolset Lifecycle

### System Level

Defines the superset of available capabilities:

- known runners
- possible runner incarnations
- known tools
- known implementation artifacts
- discovered environments per hotel

### Agent Level

Defines the default working tool universe:

- what incarnations this agent generally has access to
- what tools those incarnations expose
- preferred environments
- preferred incarnations or incarnation classes

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
- runner incarnations
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

## Skill Relationship

Skills should not collapse this hierarchy.

Current recommendation:

- skills may influence which tool families or incarnations are relevant
- but skills are not themselves the routing substrate

It is plausible that some incarnations may advertise runner-local skills later, but that should remain a capability annotation layered on top of runner/incarnation/environment modeling.

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

## Tool Catalog File (slice T1, 2026-09-16)

Operator direction: tool definitions are not hard-coded; they live in an
external file that can be edited and is loaded into the hotel graph.

State found before the slice: three compiled catalogs had drifted apart.
The philote's `catalog.rs` held 108 tools; the hotel's compiled
`seed_abstract_tool_catalog` held 49; the LifeGraph runner keeps its own
list. Only 36 names were in both philote and hotel catalogs, and only 3 of
those shared an identical description. The hotel already stored and projected
`abstract_tool` records, but the philote rebuilt its tool assembly from the
compiled table and ignored them.

Slice T1:

- **`catalog/tools.yaml`** is the single source: 121 tools (the philote's 108,
  whose descriptions/schemas/classes won every overlap, plus the 13 tools only
  the hotel seed defined, keeping its `tool_markers`), with shared schema
  fragments under `defs` referenced by `{"$ref": "#/defs/<name>"}`.
- **Hotel loader** (`crates/aiua/src/tool_catalog.rs`): the file compiled into
  aiua is the floor; `~/.philotic/<profile>/tool-catalog.yaml` and then
  `PHILOTIC_TOOL_CATALOG` override entries by `tool_name`. Loaded into
  `abstract_tool` records on every boot and `aiua load`, replacing the
  1 124-line compiled seed array. A broken override is logged and skipped,
  never fatal.
- **Record field `batch_of: {tool, items_pointer}`** — the first relationship
  expressed as data: `life.observe.batch` is a list of `life.observe` inputs at
  `/observations`.
- **Philote reads the records**: `GetToolCatalog` at startup fills a process
  registry; tool descriptions and schemas in both assembly builders, argument
  repair (including each batch item against the member schema), and plan
  verification (a batch call credits one member step per written item) read
  the records, with `catalog.rs` only as the fallback until the hotel answers.

Watched-live gate: edit a description in the profile override, restart the
hotel, see the edited text in a live prompt; a plan whose steps are bound to
`life.observe` verifies when one `life.observe.batch` call writes them.

Follow-on slices (not in T1), each replacing a compiled table with a record
field:

- `class`/approval from records (`tool_class`, `tool_requires_approval` still
  return compiled `&'static str`; approval is security-relevant, so it moves
  with its own tests).
- `mutates` / read-only: `plan_eval::READ_ONLY_SUFFIXES` and
  `turn_loop::LIFE_WRITE_TOOLS` disagree with each other and with the runner
  (`.feedback` listed read-only though `life.recall.feedback` writes;
  `graph.query`, a read, and `graph.mutate`, defined nowhere, listed as
  writes). The record field must pick one truth.
- Artifact-id argument pointers for write receipts (`uncited_writes`).
- Skill ownership / relevance (`tools_for_skill`, `skill_implied_tools`,
  `skill_is_relevant_for_turn`) from records.
- Delete `catalog.rs` once every consumer reads records; the LifeGraph runner's
  own tool list reads the same file.
- Merge policy for runtime tool edits (`ToolsetProfileRecord::reconcile_seed_with_existing`
  precedent) once a `tool.*` admin operation exists.

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
