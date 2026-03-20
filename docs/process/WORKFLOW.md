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

- do Muninn bootstrap and orientation
- identify the current owner of truth
- inspect the relevant code, tests, and nearby docs
- name the current slice and the next seam

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

## Skills Map

Use repo-local skills for process execution:

- `muninn-memory-habit`
- `runtime-debugger`
- `runtime-rollout-watch`
- `verification-ladder`
- `philotic-slice-closeout`
- `check-engine`
- `retrospective-workflow`

Use code-local docs, tests, and types for correctness rules.

If a process rule starts preventing real bugs, push the enforcement downward into code.
If a code rule keeps causing collaboration confusion, summarize it upward here.
