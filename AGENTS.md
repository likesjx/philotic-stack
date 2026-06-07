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

- `aiua` as the hotel daemon and canonical context owner
- materialized guest processes such as `membrane`, `philote`, `model-router`, and tool runners
- a graph-backed context and session model
- explicit runtime boundaries between cognition, routing, execution, and persistence

Read [CLAUDE.md](/Users/jaredlikes/code/philotic-stack/CLAUDE.md) for the concise repository map and command inventory.

### Project Graph (Optional But Recommended)

The project intelligence graph (`phil graph`) is a powerful orientation tool when available.
It contains the full codebase structure (types, functions, traits), all proposals
and seams, git history, agent sessions, and a decision audit trail.

**The graph is not a hard dependency.** Agents can work effectively without it by reading
raw files, docs, and using standard search. When the graph IS available, prefer it — it's
faster and provides richer context than manual file reading.

Start the graph server with `just intel-graph-start`. MCP endpoint: `http://127.0.0.1:8901/mcp`.
REST API: `http://127.0.0.1:8900`. Use `just intel-graph-ensure` to start only if not already running.

**Standard agent workflow** (when graph is available — see `$graph-intelligence` skill):

1. `graph_next_task` → scored work recommendation with conflict avoidance
2. `graph_context_for` → one-call context: proposal + seams + code + verification + diagram
3. `session_start` → claim the work, visible on dashboard
4. *(do the work)*
5. `graph_impact` → blast-radius analysis before committing
6. `graph_record_test_run` → record test results (pass/fail counts) for verification tracking
7. `graph_decide` → record what you did and why
8. `session_close` → release claim
9. `graph_scan` → update graph and auto-persist PlantUML diagrams

**Test Recording** (automated via `just test-and-record`):

After running tests, record the results in the graph:

```bash
# Option C: Just recipe (runs tests + records to graph)
just test-and-record proposal:agent-onboarding

# Or manually via REST API:
curl -X POST http://127.0.0.1:8900/api/test-run \
  -H "Content-Type: application/json" \
  -d '{
    "target_id": "proposal:agent-onboarding",
    "test_count": 27,
    "pass_count": 27,
    "fail_count": 0,
    "duration_ms": 5000
  }'

# Or via MCP:
graph_record_test_run({
  "target_id": "proposal:agent-onboarding",
  "test_count": 27,
  "pass_count": 27,
  "fail_count": 0,
  "duration_ms": 5000
})
```

Required fields: `target_id`, `test_count`, `pass_count`.
Optional: `fail_count` (default 0), `coverage_pct`, `commit_sha`, `duration_ms`.

Quick orientation shortcuts:

- **Orient**: `graph_status` or `graph_digest` at session start
- **Inspect**: `graph_skeleton <crate>` for type diagrams, `graph_snippet` for code
- **Search**: `graph_search "<text>"` across code and docs
- **Dashboard**: `graph_agent_dashboard` to see who else is working

The graph gives you structural facts. Muninn gives you cognitive context
(learnings, preferences, patterns). Use both. See `$graph-intelligence` skill.

### Muninn Memory Contract

Muninn is the continuity layer, not the task tracker, source of truth, or transcript archive.

Use this split:

- **Repo/docs/code** store what is true.
- **Intel graph** stores structure, work coordination, seams, decisions, and verification evidence.
- **docs/task.md** stores active execution work.
- **Muninn** stores why something matters, what was learned, durable preferences, reality gaps, and compact continuity handles.

Default memory lanes:

1. **Session orientation**: before meaningful work, run the Muninn bootstrap and recall the triad: who am I, who am I talking to, what matters about this topic right now.
2. **Durable decisions**: use `muninn_decide` when there is a decision with rationale.
3. **Reality gaps**: use `muninn_remember` for mismatches between assumption and observed repo/runtime truth.
4. **Closeout bursts**: at slice/session end, store only the durable delta, not the whole story.

Use this closeout prompt shape and store only filled lines:

```text
Memory delta:
- Decision:
- Reality gap:
- Validation:
- Next seam:
- Operator preference:
```

Do not store long transcripts, command logs, proposal summaries that already live in docs, or routine task-list churn. If a memory wants to become a paragraph, split it or put it in the repo instead.

## 2. Working Principles

### 2.1 One Canonical Owner Per State Type

When a kind of state exists, preserve one authority.

Examples:

- canonical session state in the context graph
- derived recovery checkpoints in apartments
- live routing/materialization state in the hotel runtime
- local working turn state inside `philote`

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

### 2.6 Rule Placement

Do not let top-level repo guidance become the only home for bug-preventing rules.

Use this placement heuristic:

- if violating the rule creates a code bug, the rule belongs in code
- if violating the rule creates team confusion, the rule belongs in workflow/process docs
- if it does both, enforce it in code and summarize it in process guidance

Code-facing rules should live as close as possible to the enforcing boundary:

- types and schemas
- parser/serializer logic
- nearby tests
- module docs
- crate READMEs

Process-facing rules should live in repo workflow docs and skills. See [docs/process/WORKFLOW.md](/Users/jaredlikes/code/philotic-stack/docs/process/WORKFLOW.md).

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
- `product-management-plane`
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

- call `graph_context_for` with the target proposal or seam to load context in one call
- or manually: inspect the relevant code paths, adjacent tests, and architecture/task docs
- identify the current owner of truth
- check for unrelated worktree changes
- check `graph_agent_dashboard` for active sessions that might conflict

### 5.1.1 Graph Session Protocol

When starting meaningful work on a proposal or seam:

1. call `session_start` with your agent name, session ID, and the target seam/proposal
2. call `session_activity` to report progress (files touched, phase changes)
3. call `session_close` when done

This creates visibility for all agents and prevents conflicting work.
The dashboard (`graph_agent_dashboard` or `GET /api/dashboard`) shows all active sessions.

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

### 5.3.1 Installed Runtime Truth Gate

When validation depends on an installed or supervised runtime, do not treat source edits or local test binaries as live truth.

Before claiming `smoke-green` or `watched-live-green` on an installed stack:

- verify the installed binary path actually changed
- verify the running process is using that path
- verify the relevant supervisor or launch agent actually restarted
- verify the observed behavior came from the updated runtime, not a stale process or stale cellar binary

If source is fixed but rollout is not proven, say so explicitly. That is `test-green`, not live-green with good intentions.

### 5.3.2 Tool Projection Is Policy

Tool availability is not the same as tool appropriateness.

When exposing tools to a model:

- treat projection as a policy surface, not a passive mirror of bindings
- suppress high-agency tools on conversational, gratitude, acknowledgment, or otherwise low-intent turns
- treat voice/transcription re-entry as a first-class policy boundary, not just “text with extra steps”

If a bad model action happened because an inappropriate tool was still visible, fix projection policy before blaming the model alone.

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
- the Muninn memory delta, if there was one

## 6. Commit and Push Discipline

Default to one commit/push per coherent slice. The quality control is slice size — if the slice is too big to explain cleanly, it is too big to commit cleanly.

### 6.1 Subject Line

```
type(scope): short description
```

**Types**: `feat`, `fix`, `chore`, `ops`, `docs`, `refactor`, `test`, `perf`
**Scope**: crate or area — `aiua`, `membrane`, `philote`, `model-router`, `philotic-web`, `ansible-mesh-core`, `phil`, `skills`

### 6.2 Body

1–5 bullet points. Focus on *why* and *what changed at the boundary* — not a changelog dump.

### 6.3 Trailers

Trailers are **additive** — include the ones that apply. Do not add empty or N/A placeholders.

| Trailer | When to include |
|---|---|
| `Slice: codex/<slug>` | Always, when on a named workstream |
| `Seam: <seam-id>` | When the change touches a known seam boundary |
| `Fixes: DEF-NNN` | When closing a tracked defect in `docs/DEFECTS.md` |
| `Refs: <short-name>` | Architectural cross-link; repeat the line for multiple refs |
| `Verified: <level>` | Always; be honest: `test-green`, `smoke-green`, `watched-live-green`, `check-only` |

Short names for `Refs:` are preferred — `TELEGRAM_POLL_LEASE_PROPOSAL`, `SEAM_REGISTRY`, `DEFECTS` — not full paths.

### 6.4 Examples

```
feat(membrane): graceful poll lease release on shutdown

- release lease explicitly on SIGTERM instead of relying on TTL expiry
- prevents ~30s standby takeover delay during intentional restarts

Slice: codex/telegram-poll-hardening
Seam: telegram-poll-lease
Verified: smoke-green (dual-poller handoff)
```

```
fix(aiua): dead guest with cleared PID drops poll lease correctly

- cleared PID no longer holds the poll lease open after supervisor reap
- lease drop fires before supervisor reschedules the slot

Slice: codex/telegram-poll-hardening
Seam: telegram-poll-lease
Fixes: DEF-001
Refs: TELEGRAM_POLL_LEASE_PROPOSAL
Refs: RUNTIME_AUTHORITY_LEASES_PROPOSAL
Verified: test-green
```

```
chore(skills): add check-engine skill, fix muninn-memory-habit subagent rule

Verified: check-only
```

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
- re-read [docs/architecture/ARCH_RULES.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCH_RULES.md)
- re-read [docs/architecture/ROADMAP.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROADMAP.md)
- re-read [docs/architecture/DOC_TAGGING_FRONTMATTER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/DOC_TAGGING_FRONTMATTER_PROPOSAL.md)
- re-read [docs/process/WORKFLOW.md](/Users/jaredlikes/code/philotic-stack/docs/process/WORKFLOW.md)
- apply the current repo-local SVE skill/process stack before continuing

The workflow home for the SVE operating loop is [docs/process/WORKFLOW.md](/Users/jaredlikes/code/philotic-stack/docs/process/WORKFLOW.md).

For older open sessions that may not yet know this shorthand, the operator should use the explicit long form once and ask the agent to restate the refreshed protocol before continuing.

### 8.2 Muninn Failure Rule

The session bootstrap in CLAUDE.md is non-negotiable. It applies on every session start — including sessions resumed from a summary, continued mid-task, or picking up after context compression. A context summary is observed state only; Muninn recall is required for decision history.

When Muninn is required by protocol for meaningful work:

- if the Muninn MCP surface or helper is unavailable, say so immediately
- do not silently continue as if recall happened
- pause and require explicit user/operator approval before proceeding without Muninn
- once approval is given, state clearly that the turn is continuing on observed repo/runtime truth only

Use the shared helper in [scripts/muninn_mcp.py](/Users/jaredlikes/code/philotic-stack/scripts/muninn_mcp.py).
The helper should attempt local Muninn recovery first during session bootstrap.
The `bootstrap` and `require` modes exist specifically to fail loudly when memory bootstrap is unavailable or unrecoverable.

After bootstrap succeeds, use Muninn deliberately:

- retrieve concise continuity context before decisions or resumed work
- write back only decisions, reality gaps, validation outcomes, next seams, and operator preferences
- prefer `muninn_decide` for explicit decisions and `muninn_remember` for atomic facts or gaps
- keep each write short; Muninn should make future orientation faster, not recreate the transcript in another database

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
| `graph-intelligence` | **Graph as primary context source** — orientation, task selection, context loading, impact analysis, session lifecycle, diagrams |
| `check-engine` | **End-of-session review** — memory sweep, MEMORY.md sync, open threads, process gaps, green status |
| `philotic-slice-closeout` | Finalizing implementation slices (tasks, proposals, commits) |
| `verification-ladder` | Deciding and reporting the honest validation level |
| `proposal-maintainer` | Architecture/process proposal and spec hygiene |
| `architecture-docs-maintainer` | Keeping architecture truth, domains, frontmatter, and cross-links aligned |
| `sver-harness` | Harness trial mechanics, telemetry hygiene, explicit verification on close |
| `muninn-memory-habit` | Establishing consistent retrieval/write-back habits |
| `subagent-delegation` | Splitting large tasks into bounded sub-tasks |
| `runtime-debugger` | Diagnosing live multi-process/multimodal stack failures |
| `runtime-materialization` | Designing startup/wake/sleep and placement policy |
| `runtime-rollout-watch` | Proving installed/runtime rollout truth before claiming live validation |
| `retrospective-workflow` | Running seam-based retrospectives and turning lessons into code/process/SVE changes |
| `muninn-memory-protocol` | Client adapter contract for memory integration |
| `role-authoring` | Creating or updating agent roles through `role.configure` |
| `lifegraph-truth-summarizer` | Provenance-aware LifeGraph summaries that separate confirmed graph facts, seeded placeholders, inferred intent, and recommended next structure |

## 12. Repository-Specific Notes

- The legacy ZeroClaw/OpenClaw reference clone has been removed from this repo. Consult the original `zeroclaw` repository separately if needed.
- Prefer `rg` for file and text search.
- Use `apply_patch` for manual file edits.
- Keep architecture docs and task tracking current when decisions shift.
- Treat domains and frontmatter as standing architecture-doc metadata, not optional polish.
- When changing runtime boundaries, prefer proving them with code and smokes before broadening the design story.

## 13. Key References

- [CLAUDE.md](/Users/jaredlikes/code/philotic-stack/CLAUDE.md) — Claude Code session bootstrap and commands
- [CODEX.md](/Users/jaredlikes/code/philotic-stack/CODEX.md) — OpenAI Codex session bootstrap and commands
- [GEMINI.md](/Users/jaredlikes/code/philotic-stack/GEMINI.md) — Google Gemini session bootstrap and commands
- [skills/graph-intelligence/SKILL.md](/Users/jaredlikes/code/philotic-stack/skills/graph-intelligence/SKILL.md) — full MCP tool reference and agent workflow
- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md) — active execution surface
- [docs/process/WORKFLOW.md](/Users/jaredlikes/code/philotic-stack/docs/process/WORKFLOW.md) — SVE operating loop
- [docs/architecture/AGENT_WORKFLOW_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_WORKFLOW_PROPOSAL.md)
