# Philotic Agent Workflow Proposal

## Goal

Capture the working process that proved effective during this build stretch so Codex can operate as a reliable engineering partner for Philotic development.

This proposal is about how the agent should work with the repo and with the operator, not about product architecture.

## Disposition

Accepted and partially implemented.

Implemented so far:

- repo-root [AGENTS.md](/Users/jaredlikes/code/philotic-stack/AGENTS.md)
- per-slice commit/push discipline during recent work
- proposal/task tracking updates
- executable workflow commands in [justfile](/Users/jaredlikes/code/philotic-stack/justfile):
  - `verify-vertical-slice`
  - `operator-checklist`

Still pending in [task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md):

- proposal disposition rollout across remaining active docs
- lightweight skill/rules optimization loop
- watched-live recipe for supervised guest/runtime validation

## Core Recommendation

Adopt a layered agent workflow with five explicit phases:

1. inspect the current system before changing it
2. implement the smallest honest slice
3. verify at the lowest useful layer first
4. verify upward through real runtime behavior
5. capture the decision, status, and next seam in docs

The point is to keep momentum without letting "it seems fine" become the main testing framework.

Each slice should also leave behind a small but durable paper trail:

- a proposal or spec disposition update
- a commit and push
- assumption-vs-reality learnings
- a quick skill/rules optimization check

## Working Principles

### 1. One Canonical Owner Per State Type

When a kind of state exists, the agent should identify and preserve one authority.

Examples:

- canonical session state in the context graph
- derived recovery checkpoints in apartments
- live routing state in the hotel runtime
- session-local working state inside `agent-core`

If two places appear to own the same thing, the agent should stop and resolve the boundary before piling on more behavior.

### 2. Build the Boundary Before the Abstraction Story Gets Too Fancy

The agent should prefer real boundaries over conceptual ones.

Examples from this stretch:

- framed IPC before trusting concurrent push/request behavior
- external tool runners before pretending tool execution was abstract
- canonical session snapshots before treating apartment recovery as architecture

If the implementation and the docs disagree, fix the boundary or mark it transitional.

### 3. Transitional Architecture Is Allowed, But Must Be Named

The agent may build a transitional slice before the full system exists, but should say so explicitly in code comments, proposals, and close-out notes.

That keeps the team from confusing:

- "working scaffolding"
- "final authority"

which are not the same thing, however much software likes to roleplay.

## Standard Development Loop

### 1. Context Pass

Before substantial edits, the agent should:

- inspect the relevant code paths
- inspect related tests and docs
- identify the current boundary of truth
- note any existing unrelated changes in the worktree

### 2. Implementation Slice

The agent should default to the smallest slice that:

- proves the architectural direction
- can be tested meaningfully
- leaves the system in a coherent state

The slice should usually include:

- code
- tests or smoke coverage
- docs/task updates if the decision surface changed

### 3. Verification Ladder

Verification should happen bottom-up:

1. targeted crate tests
2. integration/e2e tests
3. binary smoke scripts
4. watched live run when runtime/protocol/materialization behavior changed

The agent should not stop at unit tests when the change affects:

- IPC
- concurrency
- process supervision
- routing
- delivery
- materialization

### 4. Decision Capture

When a durable architectural or process decision is made, the agent should update:

- a proposal/spec doc in `docs/architecture`
- `docs/task.md`

This should happen during the work, not as an aspirational future chore.

Every active proposal should carry a short `Disposition` section that is updated per slice.

Recommended statuses:

- `proposed`
- `accepted for current slice`
- `implemented`
- `superseded`
- `deferred`

### 5. Close the Slice

Before moving on, the agent should state:

- what is actually working
- what is intentionally incomplete
- what the next highest-value seam is

That makes the next step a choice instead of a scavenger hunt.

The close-out should also capture:

- assumption/reality gaps exposed during the slice
- whether any standing instructions, skills, or rules now need tuning

## Slice Contract

Each implementation slice should aim to produce:

1. a succinct proposal update or disposition change
2. the smallest coherent code change
3. relevant tests and smoke coverage
4. one descriptive commit and push
5. a short note about reality gaps and next seam

If a slice cannot satisfy all five, the agent should say which part is intentionally missing and why.

## Testing Policy

### Unit and Crate Tests

Use crate-level tests for:

- serialization/protocol contracts
- loop state transitions
- policy decisions
- snapshot composition
- selection logic

### Integration and E2E Tests

Use integration tests for:

- real request/response flows
- persistence expectations
- inter-component behavior inside one process tree

### Binary Smokes

Use smoke scripts for:

- real guest binaries
- real sockets/transports
- real process orchestration
- user-visible happy-path and interrupt-path flows

These are especially important for:

- approval flows
- tool routing
- commands
- pause/resume
- live registry behavior

### Watched Runs

The agent should perform a watched live run after substantial runtime changes, especially for:

- IPC framing changes
- supervised guest startup
- routing/materialization changes
- environment-specific execution changes

This is how we found the push/reply collision bug and later the supervised guest registration issue. The tests were helpful; reality was unreasonably helpful.

## Commit and Push Discipline

The agent should commit when a slice has:

- coherent behavior
- verification evidence
- a clear explanatory commit message

Commit messages should:

- describe the primary behavior change in the subject
- explain the architectural reason in the body
- mention important verification or operational consequences when relevant

The default should be one commit/push per coherent slice.

Large feature runs should prefer multiple meaningful commits over one vague "progress" commit.

## Deployment and Live Validation

For deployable/runtime-facing changes, the agent should distinguish:

- test-green
- smoke-green
- watched-live-green

Those are separate confidence levels.

Recommended release gate for important runtime slices:

1. targeted tests pass
2. binary smoke passes
3. one watched live run is observed
4. caveats are documented if live behavior still exposes a known issue

## Communication Rules for Codex

The agent should:

- give short progress updates before major work
- explain assumptions after doing the work
- prefer doing the next reasonable step over asking broad questions
- pause only when a decision has real architectural consequences
- push back on design choices when there is a materially better option or a hidden cost

Agreement is not the goal. Honest engineering partnership is.

The agent should also keep a running distinction between:

- proven behavior
- inferred behavior
- intended future design

That distinction matters because codebases love to blur it and then act surprised later.

## Proposed AGENTS.md Improvements

The repo instructions for Codex should explicitly add:

- a verification ladder rule: tests -> integration -> smoke -> watched run
- a rule to capture architecture/process decisions in `docs/architecture` and `docs/task.md`
- a rule that each slice should disposition the relevant proposal as succinctly as possible
- a rule to commit/push per coherent slice with a specific commit body
- a rule to record assumption-vs-reality gaps when validation exposes them
- a rule to perform a quick skills/rules optimization check at the end of meaningful slices
- a rule that bidirectional or push-capable IPC must use framing/correlation assumptions by default
- a rule to treat state ownership boundaries as first-class design work
- a rule to label transitional implementations as transitional
- a rule to separate:
  - canonical authority
  - derived cache/checkpoint
  - live runtime state

## What Is Working in This Process

The following collaboration patterns worked especially well:

- pairing architectural proposals with immediate implementation slices
- proving each slice with real smokes instead of only code-level confidence
- using live observation to catch bugs hidden by green tests
- collecting reality-gap learnings from watched runs instead of treating them as embarrassing side quests
- splitting large design spaces into focused proposals instead of forcing one giant master spec
- carrying forward the "next seam" after every slice

## Proposal Lifecycle

Proposal docs should stay light, but they should not stay frozen.

Recommended shape:

- `Goal`
- `Core Recommendation`
- `Disposition`
- `Current Slice`
- `Links to active work items/tasks`
- optional supporting sections

Proposal docs should link directly to the relevant work tracking when that work exists.

This avoids the classic irony where the architecture doc explains the future beautifully while the task board is off in another room pretending not to know it.

## Skill and Rules Optimization Loop

At the end of a meaningful slice, the agent should do a brief check for:

- instruction gaps in repo guidance
- reusable behaviors that deserve a skill
- skills that are too broad, too vague, or not getting triggered when they should
- recurring assumption/reality gaps that should become explicit rules

This should stay lightweight unless a real pattern has emerged. The process should improve continuously, not become its own primary workload.

## Subagent Delegation

Subagents are useful when they reduce effort without spreading confusion.

Recommended rules:

- delegate a narrow seam, not a whole initiative
- assign an explicit truth level:
  - `inspect`
  - `implement`
  - `verify`
  - `explore`
- pass only the context needed for that seam
- keep final architectural judgment and synthesis in the main thread

Good subagent use cases:

- focused codepath inspection
- targeted test writing
- isolated implementation of a bounded slice
- runtime/log triage
- comparison of a few concrete alternatives

Bad subagent use cases:

- open-ended architecture ownership
- ambiguous product direction
- tasks that require the same large context package across many helpers
- final arbitration between competing truths

The context budget should stay intentionally small:

- a few directly relevant files
- one explicit objective
- one explicit success condition
- one proposal/task excerpt only if needed

If multiple subagents require the same giant context dump to be competent, the seam was not split cleanly enough.

## Near-Term Process Follow-Ups

1. Formalize this workflow in the repo's standing instructions.
2. Add a `just` target for the trusted vertical-slice verification suite.
3. Add a short "confidence level" section to close-out notes:
   - test-green
   - smoke-green
   - watched-live-green
4. Add a "known-good operator checklist" for user testing.
5. Add a stability board to `docs/task.md`:
   - proven working
   - working with caveats
   - intentionally incomplete
6. Add proposal `Disposition` sections and task links to the active architecture docs.
7. Add a watched-live recipe for supervised guest registration/runtime validation.

## Recommendation

Adopt this workflow as the default operating model for Codex in Philotic.

It preserves speed, but it also preserves the much rarer skill of not lying to ourselves about what is actually done.
