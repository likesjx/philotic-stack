---
title: Automatic recall outcomes for reasoning
doc_type: proposal
domain: memory-context
status: accepted-current-slice
last_updated: 2026-11-05
tags:
- memory
- philote
- recall
- provenance
related_docs:
- ARCHITECTURE_STATUS.md
- NATIVE_GRAPH_TOOLS_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: automatic-recall-status
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Automatic recall outcomes for reasoning

## Goal

Tell Philote reasoning whether automatic retrieval found matches, found no relevant
matches within the attempted scope, or was incomplete. An empty prompt must not
silently assert that information was never discussed or stored.

## Core recommendation

`WorkingTurn.automatic_recall` carries a bounded enum per existing lane:
`not_attempted`, `matched`, `no_match`, `unavailable`, `denied`, `error`, `partial`.
The Muninn lane records its scoped, relevance-gated result before prompt budgeting.
No backend is unavailable; failed evaluation is unavailable; token rejection after
the existing recovery path is denied; another terminal engine error is error.
Rejected/failed vaults on an otherwise returned result mean partial, even if empty.
These fields also preserve recent rejection/failure evidence when backoff skips
requests on a later query or freshly constructed engine with the same server/auth.
Confirmed-empty vault skips remain successful empty results; backoff does not add
HTTP retries or synthesize a new active token rejection.
Skipped/slash/distill turns retain not_attempted; no retrieval or write is added.

LifeGraph remains asynchronous. Its outcome describes fresh cached strategy packets,
not a search of the current prompt or the entire graph. Missing/stale cache is
unavailable. Valid empty cached packets mean no_match only for those strategies.
Any fresh nonempty packet means matched before deduplication/budgeting. Structured
hotel admission rejections are classified even when the IPC transport succeeds. A recorded
prefetch failure with usable matches means partial; without matches it retains
its classified denied/unavailable/error state. Prefetch failure state is live-only;
the existing cache stays checkpointed. No-match requires a successful envelope and
usable context packet. An absent/degraded/malformed packet is error.

The memory section renders fixed status text before the existing complete records.
The full marker, scope qualification and record metadata share the existing UTF-8
byte budget. A marker that cannot fit is omitted whole; no text is truncated.
A fitting status survives even if the record preamble does not fit. Matched with
no displayed records remains matched, never no_match. Existing record provenance,
qualifications, contradiction handling and relevance gates remain authoritative.
Old checkpoints default to not_attempted; a legacy degraded flag renders a fixed
warning without echoing its free-form reason.

The marker tells reasoning that new durable information may use the existing
`memory_candidate` path with provenance. It does not generate candidates, turn
every question into a write, change promotion authority, migrate data, or replace
Muninn/LifeGraph enrichment. Candidate selection remains the existing attend hook.

Automatic recall events retain counts, latency and fixed outcomes. The integration
trace canary exposed implicit query capture in memory-core’s `activate` span;
that span now explicitly skips context, including in nested transport logs. Query text,
concept summaries and raw errors are removed from the touched Philote auto-recall
logs/events. This is not a universal redaction boundary for the rest of the stack.

## Disposition

Implemented on the feature branch; independent review and remote exact-head CI
remain pending. Local qualification is recorded below.
No production deployment or live-runtime qualification.

## Current slice

Based on develop merge `8fa2a79f`. Uses the existing worktree and command-local shared
Cargo target with incremental compilation disabled. Loopback HTTP fixtures exercise
the real Muninn REST engine, turn hook, UDS client and prompt assembly. The hotel
responses and memory contents in these tests are synthetic, not production authority
or a real Muninn service. LifeGraph cache tests exercise real cache admission and
projection without changing the runner or Memgraph data. Local checks: 44
memory-core regressions and 690 Philote regressions, including four focused
outcome/privacy/budget/UDS tests, all passing. Formatting and diff
checks and affected-crate Clippy pass with existing repository warnings.
A later end-to-end runtime
qualification can verify model behavior; this change does not claim that a model
will always follow the marker's candidate guidance.
