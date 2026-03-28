# Philotic Rust Forge Proposal

## Goal

Define a future `rust-forge` runner that can help create, build, test, publish, and optionally integrate Rust-based Philotic components without collapsing trust boundaries between code generation, execution, and deployment.

This proposal is intentionally about a future control-plane capability, not a near-term default power for ordinary agents.

## Disposition

Proposed and deferred.

This is a design extension of:

- [RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md)
- [TOOL_MANAGEMENT_PLANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_MANAGEMENT_PLANE_PROPOSAL.md)
- [TASK_RUNNER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TASK_RUNNER_PROPOSAL.md)

Track follow-on work in [task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md) under `Deferred Design Threads`.

## Core Recommendation

Treat `rust-forge` as a specialized task-runner family for controlled component authoring and release preparation.

It should be able to:

- write or patch Rust component code in a bounded workspace
- run formatting, lint, and tests
- compile artifacts
- register artifact metadata
- optionally prepare release or materialization requests

It should not, by default:

- hot-deploy arbitrary code directly to production hotels
- bypass trust, signing, or approval gates
- silently plug new components into the mesh without explicit policy

If it does not live under strong trust controls, it becomes a beautifully efficient machine for manufacturing supply-chain incidents.

## Why This Exists

Philotic already wants:

- runnable component artifacts
- materializable runners
- graph-managed system topology

So a natural future step is:

- a forge that can help create new components
- compile them
- package them
- hand them to the artifact/release plane

That would be useful for:

- new tool-runners
- transport components
- adapters
- internal utilities
- targeted one-off operators

The important line is:

- `rust-forge` prepares or proposes new capabilities
- the release/materialization plane decides what becomes trusted and deployable

## Capability Model

Suggested abstract tools:

- `rust_forge.scaffold_component`
- `rust_forge.patch_component`
- `rust_forge.format_component`
- `rust_forge.test_component`
- `rust_forge.build_component`
- `rust_forge.publish_artifact`
- `rust_forge.register_component`
- `rust_forge.prepare_mesh_integration`

These should remain abstract to the agent loop, but route to a highly constrained forge runner incarnation.

## Runner Family

`rust-forge` should be a dedicated task-runner family, separate from:

- workspace runners
- shell runners
- ordinary tool-runners

Why:

- it has stronger trust and sandbox requirements
- it touches source code and artifacts
- it may influence system topology
- its outputs can become future executable components

Suggested family:

- `forge`

Suggested profiles:

- `forge.local_dev`
- `forge.ci_managed`
- `forge.release_prep`

## Execution Surfaces

The forge likely needs several bounded surfaces:

### 1. Source Workspace

Used for:

- creating component files
- editing bounded project areas
- generating manifests

### 2. Build Environment

Used for:

- `cargo fmt`
- `cargo test`
- `cargo build`
- artifact packaging

### 3. Registry / Artifact Plane

Used for:

- publishing metadata
- registering artifacts
- preparing release candidates

### 4. Mesh Integration Plane

Used for:

- proposing guest records
- proposing tool registration
- proposing hotel materialization steps

This last surface should almost certainly be proposal-driven first, not auto-apply.

## Trust Model

This is the entire ballgame.

### 1. Sandbox First

Forge work must run in constrained environments:

- isolated workspace
- no ambient production secrets
- bounded network access
- bounded filesystem scope
- bounded resource usage

### 2. Proof Before Release

Before any artifact can leave the forge:

- formatting passes
- tests pass
- build succeeds
- provenance is recorded
- artifact hash is captured

### 3. Approval Before Broad Use

Human or strong policy approval should gate:

- artifact publication
- release promotion
- mesh registration
- hotel materialization

### 4. Trust the Artifact, Not the Prompt

The system should trust:

- signed artifact metadata
- test results
- approval state

not:

- “the model said it looked good”

## Suggested Lifecycle

1. agent or operator requests a new component
2. `rust-forge` scaffolds or patches code in a bounded workspace
3. forge runs validation
4. forge produces an artifact candidate
5. artifact is registered with provenance
6. release/materialization is proposed, not assumed
7. hotel/runtime may later materialize the approved artifact

That separates:

- authoring
- validation
- publication
- deployment

which is exactly the separation that tends to disappear when people get excited about self-improving systems.

## Mesh Integration Scope

The forge should eventually support two different modes:

### Proposal Mode

Outputs:

- component manifest
- guest record proposal
- route registration proposal
- artifact registration proposal

No automatic mesh mutation.

### Controlled Apply Mode

Allowed only under strong policy.

May:

- publish artifact metadata
- register a component in the context graph
- request materialization of the new component

Even here, “request” is safer than “do immediately.”

## Relationship to Other Planes

### Task Runner Plane

`rust-forge` is a specialized task-runner family.

### Artifact Build and Distribution Plane

`rust-forge` is one potential builder/packager, not the whole artifact lifecycle.

### Tool Management Plane

`rust-forge` may create runners or components that later appear in the tool management graph.

### Mesh/Hotel Runtime

`rust-forge` may propose or request integration, but it should not become the unreviewed emperor of hotel topology.

## Near-Term Recommendation

Do not build `rust-forge` into the main runtime flow yet.

Instead:

1. keep it as a pinned future runner family
2. require explicit trust and sandbox design first
3. keep initial scope to:
   - scaffold
   - patch
   - test
   - build
   - publish proposal

Only after that should we consider:

- release promotion
- automatic mesh registration
- automatic hotel materialization

## Recommendation

Adopt `rust-forge` as a deferred specialized task-runner concept for Rust component creation and release preparation.

It should be:

- highly sandboxed
- proposal-first
- artifact-aware
- mesh-aware
- approval-gated

That gives us a path toward agent-assisted component creation without immediately turning the Philotic web into a self-modifying optimism engine.
