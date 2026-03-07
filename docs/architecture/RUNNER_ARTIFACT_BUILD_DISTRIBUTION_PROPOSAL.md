# Philotic Runner Artifact Build and Distribution Proposal

## Goal

Define how tool runners and similar executable components can be:

- authored
- built
- tested
- trusted
- distributed
- materialized

without collapsing build, execution, and deployment into one blurry subsystem.

## Disposition

Proposed and deferred.

This plane is intentionally not implemented in the current runtime slices.

Track future work in [task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md) under:

- `Next Project: Tool Assembly and Routed Execution`
- `Deferred Design Threads` -> `Runner artifact plane`

## Core Recommendation

Treat artifact build and distribution as a separate control plane from tool execution.

The core entities should be:

- `tool_runner`
- `artifact`
- `build_job`
- `distribution_job`
- `runner_release`
- `builder`
- `materializer`

This keeps us from accidentally treating “the thing that runs a tool” and “the thing that creates or ships that executable” as the same concern.

## Why Separate the Planes

Tool execution answers:

- what capability should run right now
- which runner should handle it
- where is that runner

Artifact build/distribution answers:

- how is a runner created
- how is it verified
- how is it versioned
- how is it moved to a hotel
- who trusts it enough to materialize it

If these are not separated, the system quickly becomes a charming little machine for inventing and deploying its own mistakes at scale.

## Canonical Entities

### 1. Tool Runner

A `tool_runner` is the logical capability provider.

Properties:

- `runner_id`
- `implementation_kind`
- `source_ref`
- `build_strategy`
- `entrypoint`
- `offered_tools`
- `release_policy`

### 2. Artifact

An `artifact` is the concrete build output a hotel can materialize.

Properties:

- `artifact_id`
- `runner_id`
- `version`
- `platform`
- `hash`
- `signature`
- `blob_ref`
- `build_provenance`
- `test_status`

Artifacts, not raw source trees, should be the normal unit of distribution and materialization.

### 3. Build Job

A `build_job` is the controlled process that turns source/spec into artifact.

Properties:

- `build_job_id`
- `source_ref`
- `requested_by`
- `builder_environment`
- `status`
- `logs_ref`
- `artifact_id`

### 4. Distribution Job

A `distribution_job` moves an artifact to a hotel or deployment environment.

Properties:

- `distribution_job_id`
- `artifact_id`
- `target_hotel`
- `target_environment`
- `status`
- `logs_ref`

### 5. Runner Release

A `runner_release` links a logical runner to an artifact approved for materialization.

Properties:

- `runner_id`
- `artifact_id`
- `release_channel`
- `approved_for_hotels`
- `trust_level`

## Recommended Runtime Roles

### Builder

A builder is a controlled environment that can:

- write or patch code into a workspace
- compile
- run tests
- produce versioned artifacts

This can be:

- local dev builder
- remote build node
- CI-style builder

### Distributor

A distributor moves artifacts to execution environments.

This may:

- push to a hotel
- publish to a blob/object store
- instruct hotels to pull artifacts

### Materializer

A materializer launches an approved artifact into a target environment.

This may be:

- hotel local process materializer
- container materializer
- remote node materializer
- MCP bridge materializer

## Trust Model

This is the most important part if this ever becomes a product or platform.

The market question is interesting, but the gating question is trust.

Any viable system needs concrete answers for:

- who can request a build
- who can write source or patches
- what sandbox the builder runs in
- what tests must pass
- what signing/provenance exists
- who can approve release
- which hotels trust which artifacts

Recommended trust layers:

### 1. Sandbox

Builds run in constrained environments:

- isolated workspace
- limited secrets
- controlled network
- resource limits

### 2. Verification

Before release:

- compile passes
- tests pass
- static policy checks pass
- artifact hash and provenance recorded

### 3. Approval

At minimum for anything non-local:

- human approval or policy gate
- explicit release to a runner channel

### 4. Materialization Trust

Hotels should materialize:

- approved artifacts
- for allowed runners
- in allowed environments

not arbitrary source produced moments earlier by an optimistic model.

## Development Modes

### Dev Mode

Fast local iteration:

- write code
- build locally
- materialize locally

Useful for:

- experimentation
- prototypes
- private environments

### Managed Mode

Controlled production-ish flow:

- build in builder environment
- test and sign
- publish artifact
- hotel pulls approved release

Useful for:

- shared systems
- team trust
- auditable deployments

### Emergency Mode

Explicit break-glass behavior:

- build on or near the hotel
- high-visibility approval
- heavy audit trail

Useful for:

- incidents
- urgent hotfixes

but should never be the default operating model.

## Distribution Strategy

The ansible backbone should coordinate build/distribution, but it should not necessarily carry every binary payload over ordinary task IPC.

Recommended split:

- Context Graph stores metadata
- blob/object store stores large artifacts
- Ansible backbone carries:
  - build requests
  - state changes
  - manifests
  - distribution instructions
  - provenance and status

Large binaries can later move through:

- blob service
- chunked artifact transport
- hotel pull from approved source

This keeps the control plane elegant without forcing ordinary IPC messages to cosplay as a package manager.

## Model-Directed Runner Creation

This can exist, but only through a controlled chain.

Recommended flow:

1. model proposes new runner or patch
2. proposal becomes source patch/spec in controlled workspace
3. builder compiles and tests it
4. artifact is created and registered
5. approval/policy gate decides whether it can release
6. distributor makes it available
7. hotel materializes approved artifact only

So:

- model may assist authoring
- builder performs build
- release process establishes trust
- hotel materializes trusted artifact

That is much healthier than letting the same runtime both invent and deploy its own code path because it sounded efficient in the moment.

## Relationship to Tool Management Plane

This proposal complements, rather than replaces, the tool management plane.

- tool management plane defines logical runners, tools, and environments
- artifact plane defines how runner implementations are built and shipped

Together:

- management plane answers what exists logically
- artifact plane answers how implementations become real and trusted

## Relationship to Current Work

We do not need to implement this whole plane before finishing current agent/runtime functionality.

Near-term implication:

- keep current routed execution work focused on abstract tools, runner discovery, and selection
- assume that runners will eventually materialize from approved artifacts
- avoid baking “compile locally right here” assumptions into tool execution

## Product/Market Thought

There may indeed be a market here, but only if the trust story is real.

The valuable part is not merely:

- “AI can write tools”

It is:

- “systems can safely author, verify, distribute, and materialize executable capability with policy and auditability”

Without sandboxing, verification, provenance, and release trust, it is just a highly energetic supply-chain incident waiting for branding.

## Recommendation Summary

Introduce a future artifact plane with:

- build jobs
- artifacts
- releases
- distribution jobs
- trust gates

Keep it distinct from the tool execution plane.

Let the current runtime continue evolving under the assumption that:

- runners are logical capability providers
- artifacts are the thing hotels ultimately materialize
- build/distribution is a separate, policy-heavy subsystem

That is the right way to preserve momentum now without quietly coupling the future system to the most convenient prototype path.
