# Philotic Workflow Guide

> Status: active workflow guidance

This document is the home for process rules: how we work, validate, close slices, and avoid lying to ourselves about live state.

## SVE Operating Loop

Philotic uses a lightweight SVE operating loop:

- `S` — Start
- `V` — Verify
- `E` — End
- `R` — Retrospective when the session exposed important surprises, process pain, or reusable lessons

The first three are the standing loop. Retrospective is not mandatory for every tiny slice, but it should run whenever the work changed how we think the engine ought to operate.

### S — Start

At session start or when resuming meaningful work:

- do Muninn bootstrap and orientation (`just session-start` — also runs harness drift check)
- when the graph server is reachable, `just session-start` also claims a visible graph session/workstream on the board using the `session-start-bootstrap-slice` seam
- identify the current owner of truth
- inspect the relevant code, tests, and nearby docs
- name the current slice and the next seam
- if starting focused work on a seam, open a harness trial: `just harness-trial-start <seam-id>`

Use:

- `AGENTS.md` for engineering protocol
- repo-local crate docs for local invariants
- repo-local skills when the workflow already exists

### V — Verify

During and after the slice:

- validate bottom-up using the verification ladder
- prove the installed/runtime truth gate before making live claims
- distinguish source truth, build truth, installed truth, and live truth

Use:

- `verification-ladder`
- `runtime-rollout-watch`
- `runtime-debugger` when the failure is still live and unresolved

### E — End

At slice close or end of session:

- capture durable decisions and outcomes
- update docs/tasks if current truth or active seams changed
- store durable memory to Muninn
- state what is working, what is incomplete, and the next highest-value seam
- close any open harness trial: `just harness-trial-close [status] [verified] [summary]`
- close any active workstream with explicit verification on completed closes: `just close-workstream`

Use:

- `philotic-slice-closeout` for implementation slice close-out
- `check-engine` for session-end memory and process hygiene

### R — Retrospective

Run a retrospective when:

- a watched-live run exposed surprises
- a debugging session revealed workflow pain
- the same failure cost time twice
- the session changed where a rule should live

Retrospectives answer:

1. what went well
2. what did not
3. how to do better
3a. how SVE should change
3b. how SVE should be optimized

Use:

- `retrospective-workflow`

## Rule Placement

Use this placement heuristic:

- If violating the rule creates a code bug, the rule belongs in code.
- If violating the rule creates team confusion, the rule belongs in workflow/process docs.
- If it does both, enforce it in code and summarize it in process guidance.

That means:

- bug-preventing rules should live in types, schemas, parsers, tests, and crate-local docs
- workflow rules should live in this guide, repo-local skills, and lightweight repo process notes
- top-level guidance should summarize where to look, not become the only place the rule exists

## Where Different Rules Belong

### Code Rules

Put code rules at the narrowest layer that can actually enforce them:

- type and schema invariants:
  - request/response payload shapes
  - tool argument requirements
  - provider output contracts
- module and boundary rules:
  - projection rules
  - re-entry rules
  - routing/selection policy
- crate-level behavior summaries:
  - crate README files
  - focused module docs near the owner boundary

Examples in this repo:

- tool projection policy lives in `crates/philote/src/session.rs` and its tests
- provider contract rules live in `crates/model-router/src/providers/` and controller serialization tests
- memory response shape belongs in `crates/model-router/src/controller.rs`, provider adapters, and `philote` reply handling

### Process Rules

Put process rules here and in repo-local skills:

- session bootstrap and Muninn habits
- verification and close-out flow
- rollout and watched-live discipline
- worktree/workstream habits
- harness attachment/refresh before any workstream starts
- operator and collaborator rituals

Examples in this repo:

- installed runtime truth gate
- slice close-out discipline
- check-engine expectations
- rollout/watch verification before claiming live green

## Runtime Truth Rule

When a slice depends on an installed or supervised runtime:

- source truth is not live truth
- local build truth is not installed truth
- watched-live claims require proof that the installed runtime actually changed

Minimum checks before claiming live validation:

- installed binary path changed
- supervisor or launch agent restarted
- running process uses the updated binary path
- observed behavior came from that updated process

## Tool Projection Rule

Tool visibility is a policy surface, not a passive mirror of the full binding set.

When the turn is conversational, social, gratitude-oriented, or acknowledgment-only:

- prefer projecting no tools
- especially suppress high-agency tools like delegation, reconfiguration, or background worker creation

Voice/transcription re-entry is a first-class policy boundary, not just “text with extra steps.”

## Graph Hygiene

The Intel Graph requires periodic maintenance to stay healthy. Without it, stale sessions block `graph_next_task` scoring, unembedded proposals degrade semantic search, and missing dispositions leave the pipeline ambiguous.

### Automated Maintenance

Run `just intel-graph-maintain` for a full sweep: scan → session cleanup → health check → embed proposals. This is safe to run at any time and is idempotent.

### Session Hygiene

Sessions are the coordination primitive for multi-agent work. Stale sessions (active but abandoned) pollute the dashboard and cause conflict-avoidance false positives.

- **Detection**: `GET /api/health/sessions` reports stale sessions (>4h), overloaded agents (>2 concurrent), and orphaned workstreams
- **Cleanup**: `POST /api/session/cleanup` auto-closes stale sessions with `timed_out` status
- **Prevention**: Always call `session_close` at session end, even for incomplete work
- **Justfile**: `just intel-graph-session-cleanup` (default 4h), `just intel-graph-session-cleanup 8` (custom)

See `$session-hygiene` skill for full protocol.

### Proposal Pipeline Hygiene

Every proposal should have a disposition, domain, and verification state.

- **Detection**: `GET /api/health/proposals` reports missing dispositions, verification gaps, and embedding coverage
- **Embedding**: `just intel-graph-embed-proposals` batch-embeds all proposals
- **Health criteria**: No missing dispositions, <50% proposals at `verification_level: none`

See `$proposal-pipeline` skill for lifecycle and metadata requirements.

### Verification Pipeline

Test results must be recorded in the graph to advance verification state.

- **Recording**: `just test-and-record <proposal_id>` runs tests and records results
- **Health**: `GET /api/health/proposals` shows verification distribution
- **Advancement**: Use `graph_advance_verification` only when evidence supports the new level

See `$verification-orchestrator` skill for the full ladder and evidence rules.

### Integration with SVE Loop

- **S (Start)**: Run `GET /api/health` to check system state before work
- **V (Verify)**: Record test runs via `POST /api/test-run`, advance verification via `graph_advance_verification`
- **E (End)**: `check-engine` includes Graph Health Check (Check 6) — stale sessions, proposal gaps, embedding coverage
- **R (Retrospective)**: Review `GET /api/dashboard` for agent coordination patterns

## Harness Management

Managed harnesses track desired/rendered/observed state for each coding agent runtime. Use these justfile recipes:

- `just harness-drift` — drift report for all managed harnesses
- `just harness-apply [harness] [profile]` — re-apply canonical profile and verify (default: `claude-local` / `philotic-operator`)
- `just harness-trial-start <seam-id> [harness] [profile]` — begin a measured trial; session ID written to `/tmp/philotic-harness-trial-session`
- `just harness-trial-report <activity-type> [phase] [tokens_in] [tokens_out] [elapsed_ms] [lines_changed] [files] [note]` — record activity against the active trial; empty reports are rejected
- `just harness-trial-close [status] [verified] [summary]` — close the active trial; completed trials must include a verification level

Canonical profiles for this repo:

| Profile | Role | Skills |
|---|---|---|
| `philotic-operator` | orchestrator | graph-intelligence, implementation, philotic-slice-closeout, verification-orchestrator, sver-harness, session-hygiene, muninn-memory-habit, check-engine |
| `philotic-implementer` | implementer | graph-intelligence, implementation, runtime-debugger, runtime-materialization, runtime-rollout-watch, subagent-delegation |
| `philotic-reviewer` | reviewer | graph-intelligence, review, verification-ladder, verification-orchestrator, architecture-docs-maintainer, proposal-maintainer |
| `philotic-orchestrator` | orchestrator | graph-intelligence, planning, multi-agent-orchestration, subagent-delegation, session-hygiene, proposal-maintainer, muninn-memory-habit |
| `philotic-verifier` | verifier | graph-intelligence, verification, verification-ladder, verification-orchestrator, runtime-rollout-watch, philotic-slice-closeout, check-engine |

Registered harnesses: `claude-local` (philotic-operator), `claude-native` (philotic-orchestrator), `windsurf-native` (orchestrator), plus codex harnesses.

## Skills Map

Use repo-local skills for process execution:

- `graph-intelligence` — graph as primary context source, MCP tool reference, agent workflow
- `session-hygiene` — session lifecycle monitoring, stale cleanup, coordination health
- `multi-agent-orchestration` — coordinating implementer/reviewer/verifier/orchestrator roles on one workstream
- `windsurf-harness-setup` — single-harness multi-role configuration for windsurf-native
- `verification-orchestrator` — SVER state, test-run pipeline, verification evidence
- `sver-harness` — harness trial mechanics, explicit verification on close, telemetry hygiene
- `proposal-pipeline` — proposal lifecycle, disposition management, metadata hygiene
- `check-engine` — end-of-session review (now includes graph health check)
- `philotic-slice-closeout` — closing implementation slices
- `verification-ladder` — deciding validation depth and SVER state
- `proposal-maintainer` — proposal and spec hygiene
- `architecture-docs-maintainer` — architecture truth, domains, frontmatter, cross-links
- `muninn-memory-habit` — retrieval/write-back habits
- `muninn-memory-protocol` — client adapter contract for memory integration
- `subagent-delegation` — splitting tasks into bounded sub-tasks
- `runtime-debugger` — diagnosing live multi-process failures
- `runtime-materialization` — startup/wake/sleep and placement policy
- `runtime-rollout-watch` — proving installed/runtime rollout truth
- `retrospective-workflow` — seam-based retrospectives
- `role-authoring` — creating or updating agent roles

Use code-local docs, tests, and types for correctness rules.

If a process rule starts preventing real bugs, push the enforcement downward into code.
If a code rule keeps causing collaboration confusion, summarize it upward here.
