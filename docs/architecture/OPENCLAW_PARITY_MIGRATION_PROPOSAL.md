---
title: "OpenClaw Parity And Migration Proposal"
doc_type: proposal
domain: migration-parity
status: proposed
last_updated: 2026-03-12
tags:
  - parity
  - migration
  - openclaw
  - evaluation
  - active-seam
related_docs:
  - ARCHITECTURE_STATUS.md
  - ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md
  - PERSONALITY_AND_CONTEXT_PROPOSAL.md
task_refs:
  - docs/task.md
proposal_id: openclaw-parity-migration
implements: []
implemented_by: []
active_seams:
  - parity-matrix
  - migration-readiness-gates
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# OpenClaw Parity And Migration Proposal

## Goal

Define what Philotic must finish to support base OpenClaw functionality well enough to migrate without pretending parity has been achieved because a few demos look charming.

## Disposition

`proposed`

Track related work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md), [ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md), and [PORT_BLUEPRINT.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PORT_BLUEPRINT.md).

## Core Recommendation

Treat parity and migration as a checklist of durable capabilities, not as a vague emotional sense that Philotic “basically feels ready.”

The migration question should be asked in two layers:

1. what is required for **base functional parity**
2. what is required for **operational migration confidence**

## Likely Parity-Critical Areas

- imported agent identity continuity
- deterministic context graph management
- admin/control surface
- tool and skill management
- memory handling
- transcription and voice flows
- plugin/extension hooks
- local degraded-mode operation

## Migration-Critical Areas

- operator workflows
- deploy and recovery paths
- clear authority boundaries
- parity validation and reality-gap reporting
- support for the day-two debugging that migrations always pretend not to need

## Recommendation

Create an explicit parity matrix:

- OpenClaw capability
- Philotic current owner
- current confidence (`test-green`, `smoke-green`, `watched-live-green`)
- blocking gaps
- migration risk

That matrix should become the honest answer to “are we ready to migrate?” rather than a mood.

## First Slice Recommendation

Build the first parity matrix around:

- agent continuity
- memory/context behavior
- admin controls
- media/transcription
- deploy/operator workflow
