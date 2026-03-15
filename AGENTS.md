# AGENTS.md — Philotic Coding Agent Protocol

This file defines the default working protocol for coding agents in this repository. And a quick plug for appreciation of irony. Always good to spot it. And humor goes a long way to split up the tension.
Scope: entire repository.

## 0. Vocabulary

Terms like **proposal**, **slice**, **seam**, **work boundary**, **disposition**, and **workstream** have precise meanings here.
Before using them loosely, read the canonical definitions:

→ [docs/architecture/GLOSSARY.md](docs/architecture/GLOSSARY.md)

When usage elsewhere drifts from those definitions, fix the usage — not the glossary.

## 1. Project Snapshot

Philotic is a distributed AI agent operating system built around:

- `ansible` as the hotel daemon and canonical context owner
- materialized guest processes such as `membrane`, `agent-core`, `model-router`, and tool runners
- a graph-backed context and session model
- explicit runtime boundaries between cognition, routing, execution, and persistence

Read [CLAUDE.md](/Users/jaredlikes/code/philotic-stack/CLAUDE.md) for the concise repository map and command inventory.

## 2. Working Principles

### 2.1 One Canonical Owner Per State Type

When a kind of state exists, preserve one authority.

Examples:

- canonical session state in the context graph
- derived recovery checkpoints in apartments
- live routing/materialization state in the hotel runtime
- local working turn state inside `agent-core`

If two places appear to own the same thing, stop and resolve the boundary before extending behavior.

### 2.2 Build the Boundary Before the Abstraction Story Gets Too Fancy

Prefer real boundaries over conceptual ones.

Examples:

- framed IPC before trusting concurrent request/push behavior
- external tool runners before claiming tool execution is abstract
- canonical session snapshots before treating apartment recovery as architecture

### 2.3 Transitional Architecture Is Allowed, But Must Be Named

Small transitional slices are acceptable when they move the system forward, but they must be labeled as transitional in docs and close-out notes.

Do not let scaffolding quietly become implied final architecture.

### 2.4 Proven, Inferred, and Intended Are Different

Keep a clear distinction between:

- proven behavior
- inferred behavior
- intended future design

Do not collapse those categories in explanations, docs, or validation claims.

### 2.5 Honest Pushback Is Required

You are not here to be a yes-person.

If a proposed design has:

- a better alternative
- hidden cost
- authority confusion
- operational risk
- testability problems

say so clearly and propose the alternative.

## 3. Slice Contract

Each coherent implementation slice should produce:

1. a succinct proposal/spec update or disposition update
2. the smallest coherent code change
3. relevant verification
4. a descriptive commit and push
5. a short reality-gap note and next seam

If one of these is intentionally missing, say which part is omitted and why.

## 4. Proposal Lifecycle

Architecture and process proposals in `docs/architecture/` should stay lightweight but active.

Active architecture/process docs should also carry lightweight frontmatter metadata according to
[DOC_TAGGING_FRONTMATTER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DOC_TAGGING_FRONTMATTER_PROPOSAL.md).

Each active proposal should prefer this structure:

- `Goal`
- `Core Recommendation`
- `Disposition`
- `Current Slice`
- links to active work items in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)

Recommended `Disposition` values:

- `proposed`
- `accepted for current slice`
- `implemented`
- `superseded`
- `deferred`

Update the disposition as each slice lands.

### 4.1 Domains And Metadata

Use domains as the primary scope organizer for active architecture/process docs.

Controlled domain vocabulary:

- `runtime-sessions`
- `membrane-transport`
- `mesh-placement`
- `memory-context`
- `tooling-execution`
- `operator-control-plane`
- `deployment-distribution`
- `migration-parity`
- `workflow-docs`

Metadata discipline:

- every active architecture/process doc should declare exactly one primary `domain`
- use frontmatter to declare `doc_type`, `status`, `last_updated`, lightweight `tags`, and cross-links
- use tags as retrieval aids, not as a second hidden taxonomy
- if a doc appears to need multiple primary domains, name that as a seam explicitly instead of smearing ownership

### 4.2 Source-Of-Truth Split

Keep these document roles distinct:

- [ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md)
  - current implemented truth, transitional choices, and active seams
- [ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md)
  - durable architecture reference
- proposal docs in `docs/architecture/`
  - intended direction, accepted current slices, and deferred design
- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
  - active execution surface

Do not let one of these quietly impersonate another because it happens to be nearby and eloquent.

## 5. Standard Development Loop

### 5.1 Context Pass

Before substantial edits:

- inspect the relevant code paths
- inspect adjacent tests
- inspect relevant architecture/task docs
- identify the current owner of truth
- check for unrelated worktree changes

### 5.2 Smallest Honest Slice

Default to the smallest slice that:

- proves the direction
- can be tested meaningfully
- leaves the system coherent

### 5.3 Verification Ladder

Validate bottom-up:

1. targeted crate tests
2. integration/e2e tests
3. binary smoke scripts
4. watched live run for runtime/protocol/materialization changes

Do not stop at unit tests when the change affects:

- IPC
- concurrency
- process supervision
- routing
- delivery
- materialization
- environment-specific behavior

### 5.4 Decision Capture

When a durable architectural or workflow decision is made, update:

- the relevant proposal/spec in `docs/architecture/`
- [docs/architecture/ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md) if current truth or active seams changed
- [docs/architecture/ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md) if the durable reference changed
- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)

Capture decisions during the work, not later if convenient.

When active architecture/process docs are touched, use the repo-local
[$architecture-docs-maintainer](skills/architecture-docs-maintainer/SKILL.md) skill to keep
frontmatter, domains, and cross-links aligned.

### 5.5 Slice Close-Out

Before moving on, state:

- what is working
- what is intentionally incomplete
- what the next highest-value seam is
- what assumption-vs-reality gaps were exposed

## 6. Commit and Push Discipline

Default to one commit/push per coherent slice.

Commit messages should:

- describe the primary change in the subject
- list the core behavioral or architectural changes in the body
- mention important verification or operational consequences when relevant

Avoid vague progress commits.

The quality control is slice size. If the slice is too big to explain cleanly, it is probably too big to commit cleanly.

## 7. Testing and Validation Rules

### 7.1 Crate Tests

Use crate-level tests for:

- protocol/serde contracts
- loop transitions
- policy decisions
- snapshot composition
- routing and selection logic

### 7.2 Integration and E2E

Use integration/e2e tests for:

- request/response flows
- persistence expectations
- multi-component behavior inside one process tree

### 7.3 Binary Smokes

Use smoke scripts for:

- real binaries
- real sockets/transports
- real process orchestration
- user-visible flows

### 7.4 Watched Runs

Perform a watched live run after meaningful changes to:

- IPC framing or multiplexing
- guest supervision or bootstrap
- routing/materialization
- runtime environment selection

Live observation is a first-class validation tool, not an optional emotional support exercise.

### 7.5 Confidence Levels

Distinguish clearly between:

- `test-green`
- `smoke-green`
- `watched-live-green`

These are not interchangeable.

## 8. Communication Rules

While working:

- give short progress updates before major work
- explain assumptions after doing the work
- prefer the next reasonable action over broad exploratory questioning
- pause when a decision has non-obvious architectural consequences

### 8.1 SVE Refresh Shortcut

Use `SVE refresh` as the canonical shorthand for refreshing an open session onto the current Philotic SVE process.

Interpret `SVE refresh` as:

- re-read [AGENTS.md](/Users/jaredlikes/code/philotic-stack/AGENTS.md)
- re-read [docs/architecture/README.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/README.md)
- re-read [docs/architecture/ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md)
- re-read [docs/architecture/DOC_TAGGING_FRONTMATTER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DOC_TAGGING_FRONTMATTER_PROPOSAL.md)
- apply the current repo-local SVE skill/process stack before continuing

For older open sessions that may not yet know this shorthand, the operator should use the explicit long form once and ask the agent to restate the refreshed protocol before continuing.

### 8.2 Muninn Failure Rule

When Muninn is required by protocol for meaningful work:

- if the Muninn MCP surface or helper is unavailable, say so immediately
- do not silently continue as if recall happened
- pause and require explicit user/operator approval before proceeding without Muninn
- once approval is given, state clearly that the turn is continuing on observed repo/runtime truth only

Use the shared helper in [scripts/muninn_mcp.py](/Users/jaredlikes/code/philotic-stack/scripts/muninn_mcp.py).
The helper should attempt local Muninn recovery first during session bootstrap.
The `bootstrap` and `require` modes exist specifically to fail loudly when memory bootstrap is unavailable or unrecoverable.

## 9. Parallel Workstreams

When multiple conversations or workstreams are active in parallel:

- use one dedicated git worktree per active implementation thread; do not share one filesystem checkout across multiple active slices
- keep each conversation on one coherent slice or seam
- state the workstream name, current goal, and explicit out-of-scope items near the start
- avoid overlapping file ownership when possible, especially in:
  - [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
  - active architecture proposals
  - hot runtime files
- commit as soon as a slice becomes coherent so other threads can build on a stable checkpoint
- let one thread own architectural boundary changes while other threads implement within those boundaries
- capture assumption-vs-reality gaps quickly, because drift accelerates when work is parallel

Branch model: `codex/<slug>` branches PR into `develop` (integration edge); `develop` merges to `main` when stable. Never merge feature branches directly to `main`.

Use the repo-local worktree workflow and prefer:

- `just workstream-start <slug>`
- `just workstream-status <slug>`
- `just workstream-overlap <slug>`

If two threads need the same files and the same architectural decisions at the same time, that is usually a seam problem, not a coordination failure.

## 10. Skills and Rules Optimization Loop

At the end of a meaningful slice, perform a brief check for:

- instruction gaps in repo guidance
- reusable workflows that deserve a skill
- skills that are too broad or too vague
- recurring assumption/reality gaps that should become standing rules

Keep this lightweight. The process should improve continuously without becoming its own bureaucracy engine.

## 11. Repo-Local Specialized Skills

The repository contains specialized skills in `skills/` to standardize common workflows. Prioritize these over global skills for project-specific tasks.

| Skill | Purpose |
|---|---|
| `philotic-slice-closeout` | Finalizing implementation slices (tasks, proposals, commits) |
| `verification-ladder` | Deciding and reporting the honest validation level |
| `proposal-maintainer` | Architecture/process proposal and spec hygiene |
| `architecture-docs-maintainer` | Keeping architecture truth, domains, frontmatter, and cross-links aligned |
| `muninn-memory-habit` | Establishing consistent retrieval/write-back habits |
| `subagent-delegation` | Splitting large tasks into bounded sub-tasks |
| `runtime-debugger` | Diagnosing live multi-process/multimodal stack failures |
| `runtime-materialization` | Designing startup/wake/sleep and placement policy |
| `muninn-memory-protocol` | Client adapter contract for memory integration |

## 12. Repository-Specific Notes

- The legacy ZeroClaw/OpenClaw reference clone has been removed from this repo. Consult the original `zeroclaw` repository separately if needed.
- Prefer `rg` for file and text search.
- Use `apply_patch` for manual file edits.
- Keep architecture docs and task tracking current when decisions shift.
- Treat domains and frontmatter as standing architecture-doc metadata, not optional polish.
- When changing runtime boundaries, prefer proving them with code and smokes before broadening the design story.

## 12. Key References

- [CLAUDE.md](/Users/jaredlikes/code/philotic-stack/CLAUDE.md)
- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
- [docs/architecture/AGENT_WORKFLOW_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_WORKFLOW_PROPOSAL.md)
