---
title: Philotic Dev Engine Optimization
doc_type: proposal
domain: workflow-docs
status: accepted-current-slice
last_updated: 2026-04-09
tags:
- workflow
- engine
- muninn
- bootstrap
- active-seam
related_docs:
- ARCHITECTURE_STATUS.md
- AGENT_WORKFLOW_PROPOSAL.md
- DOC_TAGGING_FRONTMATTER_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: dev-engine-optimization
implements: []
implemented_by:
- engine-check-slice
- session-start-bootstrap-slice
active_seams:
- engine-bootstrap-routine
- reality-gap-consolidation
- session-start-bootstrap-slice
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# PROPOSAL: Philotic Dev Engine Optimization

## Goal
Transform the development environment from a collection of scripts into a high-leverage, self-optimizing "Agent Engine." The objective is to maximize agent continuity, minimize coordination tax, and ensure that every session builds on a verified semantic baseline.

## 1. Core Recommendations

### 1.1 Mandatory Session Bootstrap
Standardize the "entry handshake" for any agent entering the repository.
- **Status Clear**: Automatic verification of build/test baseline.
- **Semantic Recall**: Mandatory Muninn retrieval (Identity, User, Topic).
- **Protocol Orientation**: Forced alignment with `AGENTS.md` and active proposal dispositions.

### 1.2 Repo-Local Skills & Protocol
Treat development skills (`skills/`) as versioned code.
- No absolute paths to local user directories.
- Highly opinionated "Success Rungs" (Verification Ladder).
- Mandatory Operational Close-out (Disposition updates + Task alignment).

### 1.3 Semantic Optimization Loop
Use Muninn engrams to drive repository-level improvements.
- Capture "Reality Gaps" (failed assumptions) as tagged memories.
- Periodically consolidate recurring gaps into fresh rules in `AGENTS.md` or `SKILL.md`.

## 2. Infrastructure: The Muninn Memory Layer

### 2.2 Truth Sync (Local vs. VPS)
- **Local (Truth Cache)**: Low-latency personal preferences and local workspace state.
- **VPS (Project Truth)**: Shared architectural decisions, task history, and cross-agent coordination state.

## 3. Disposition
- **Status**: `accepted for current slice`
- **Current Slice**: `just engine-check` now validates Muninn reachability, repo-local bootstrap assets, and the cargo check/test baseline in one command; `just session-start` now also claims a visible graph session/workstream when the graph server is reachable.

## 4. Backlog / Next Seams

### 🔴 High Priority: Engine Automation
- [x] **`just engine-check`**: One-command verification of Muninn, repo-local bootstrap assets, and the cargo check/test baseline.
- [ ] **`just memory-consolidate`**: Tooling to triage "Reality Gap" engrams and suggest rule updates.
- [x] **`just session-start`**: Interactive (or prompt-based) agent bootstrap that runs recall and orientation automatically and claims a visible graph session/workstream when the graph is reachable.

### 🟡 Medium Priority: Infrastructure
- [ ] **VPS Muninn Deployment**: "Truth Cache" setup on `vps-jane` with automated sync.
- [ ] **Engram Schema Formalization**: Tagging system for `gap`, `decision`, `preference`, and `instruction-debt`.

### 🟢 Low Priority: Visibility
- [ ] **Dev Dashboard**: Simple CLI or markdown summary of current engine health and memory "hot spots."

## 5. Success Metrics
- **Zero Drift**: No session starts without an accurate read on "where we left off."
- **Self-Healing Protocols**: `AGENTS.md` rules evolve based on real implementation failures (Reality Gaps).
- **Contributor Ready**: A fresh clone can run `just engine-check` and have full context within one turn.

## 6. Task Links
- [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
