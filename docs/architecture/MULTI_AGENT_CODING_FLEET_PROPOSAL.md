---
title: Multi-Agent Coding Fleet Proposal
doc_type: proposal
domain: workflow-docs
status: proposed
last_updated: 2026-04-01
tags:
- multi-agent
- orchestration
- workstreams
- verification
related_docs:
- AGENT_WORKFLOW_PROPOSAL.md
- ARCHITECTURE_STATUS.md
- docs/process/WORKFLOW.md
- AGENTS.md
task_refs:
- docs/task.md
proposal_id: multi-agent-coding-fleet
active_seams:
- cross-agent-seam-ownership
- role-charter-contract
- verification-custody
- handoff-packet-shape
---

# Multi-Agent Coding Fleet Proposal

## Goal

Define a parallel coding workflow where multiple coding agents (Codex, Claude Code, Gemini/Antigravity, and Copilot) operate concurrently without conflicting authority, duplicate edits, or verification drift.

## Core Recommendation

Adopt a custody-first fleet model:

1. One orchestrator agent owns seam arbitration and final architecture truth.
2. Specialist agents own bounded work packets by seam and truth level.
3. Verification and docs/state updates are explicit roles, not best-effort side effects.
4. Every delegated task uses a standardized handoff packet with scope, boundaries, and output contract.

This keeps throughput high while preserving SVE/SVER discipline and source-of-truth integrity.

## Disposition

`proposed`

No code-level implementation yet. This proposal establishes the operating model and rollout slices for cross-agent parallel execution.

## Current Slice

1. Define canonical fleet roles and authority boundaries.
2. Define a required handoff packet schema for delegated work.
3. Pilot one implementation cycle using two parallel implementers plus one verifier.
4. Capture observed failure modes and update workflow rules.

## Fleet Topology

### Role 1: Orchestrator

- owns seam selection and split strategy
- owns architecture arbitration and final merge truth
- resolves conflicting recommendations across agents

### Role 2: Implementer

- owns bounded code slices for assigned seams
- must not change cross-cutting architecture without orchestrator approval
- returns minimal coherent diffs plus local rationale

### Role 3: Explorer/Reviewer

- runs parallel design and risk exploration
- proposes alternatives with explicit tradeoffs
- does not override implementation ownership

### Role 4: Verifier

- runs verification ladder for changed seams
- reports honest confidence level (`test-green`, `smoke-green`, `watched-live-green`)
- blocks closure when evidence is insufficient

### Role 5: Docs/State Maintainer

- updates proposals, task surface, and disposition alignment
- keeps graph-facing metadata and workflow status consistent

## Delegation Contract

Every delegated packet should include:

- `seam_id`
- `truth_level` (`inspect`, `implement`, `verify`, `explore`)
- `in_scope`
- `out_of_scope`
- `success_condition`
- `output_contract`
- `verification_expectation`

Packets missing these fields are treated as invalid for parallel execution.

## Working Rules

1. One seam has one active code owner at a time.
2. Parallel work requires non-overlapping file ownership or explicit coordination.
3. Verification custody is independent from implementation custody.
4. Session/workstream open-close hygiene is mandatory to avoid stale graph state.
5. Final architecture truth remains with orchestrator custody.

## Risks

- false parallelism from overlapping edits
- context dilution from oversized delegation packets
- verification inflation when implementers self-certify without independent checks
- stale session/workstream records causing planning noise

## Validation Plan

Pilot with three concurrent lanes:

1. lane A: implementer on seam A
2. lane B: implementer on seam B
3. lane C: verifier on completed lane output

Track:

- time-to-merge per seam
- merge conflict rate
- verification rework count
- stale-session incidents

## Exit Criteria

Move disposition to `accepted for current slice` when:

1. one full pilot cycle completes with explicit handoff packets
2. verification evidence is recorded for each completed seam
3. docs/task/proposal state remains consistent throughout the cycle
4. at least one observed failure mode is converted into an explicit standing rule