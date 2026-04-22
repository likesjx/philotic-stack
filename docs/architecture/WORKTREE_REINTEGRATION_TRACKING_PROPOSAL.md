---
title: Worktree Reintegration Tracking Proposal
doc_type: proposal
domain: workflow-docs
status: proposed
last_updated: 2026-04-02
tags:
- worktree
- branch
- develop
- intel-graph
- sver
- reintegration
related_docs:
- GRAPH_INTELLIGENCE_PROPOSAL.md
- GRAPH_INTELLIGENCE_STATUS.md
- AGENT_WORKFLOW_PROPOSAL.md
- ARCHITECTURE_STATUS.md
- docs/process/WORKFLOW.md
task_refs:
- docs/task.md
proposal_id: worktree-reintegration-tracking
implements:
- graph-intelligence
implemented_by: []
active_seams:
- worktree-reintegration-tracking
source_of_truth_targets:
- GRAPH_INTELLIGENCE_STATUS.md
- ARCHITECTURE_STATUS.md
---

# Worktree Reintegration Tracking Proposal

## Goal

Make branch and worktree reintegration status visible in intel-graph and SVER so operators can tell, at a glance, whether a slice is:

- only in a side branch
- in an open PR
- merged to `develop`
- synced into the local main checkout

The specific failure to prevent is simple and expensive: testing or reasoning from a local `develop` checkout that is behind `origin/develop` while merged work already exists elsewhere.

## Core Recommendation

Treat worktree reintegration as first-class operational state, not social memory.

Intel-graph should track, for each active workstream/worktree:

1. the local worktree path
2. the active branch name
3. the intended base branch
4. PR state, when one exists
5. merge state relative to `develop`
6. local checkout freshness relative to `origin/develop`

SVER should surface this during:

- session start
- watched-live verification planning
- slice close-out
- retrospective

The graph already knows about sessions, workstreams, branches, and worktrees in fragments. The missing piece is explicit reintegration truth.

## Disposition

`proposed`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Why This Matters

Recent provider work exposed a recurring workflow gap:

- the real work landed in dedicated worktrees and PRs
- remote `develop` moved forward
- the main checkout stayed behind
- watched verification almost started from stale local truth

The irony is sharp: we are using intel-graph to make work visible while still relying on human recall to answer the very operator question that matters most near verification time:

`is this actually on develop here, or only merged somewhere else?`

That answer should be queryable.

## Current Problem

Today, the repo has pieces of the story but not the full reintegration state:

- git knows branches, remotes, and merge ancestry
- worktrees encode local checkout placement
- GitHub knows PR state
- intel-graph knows sessions, workstreams, seams, and proposals
- operators know some of the rest until they do not

This creates three recurring failure modes:

1. local `develop` silently drifts behind `origin/develop`
2. a workstream is merged remotely but still absent from the operator's main checkout
3. side branches remain active or preserved locally without clear reintegration status

## Proposed Model

Add explicit reintegration facts to graph/runtime orientation.

### Worktree / branch facts

For a workstream or worktree, track:

- `worktree_path`
- `branch`
- `base_branch`
- `head_commit`
- `origin_head_commit`
- `pr_url`
- `pr_state`
- `merged_to_base`
- `merged_commit`
- `local_checkout_synced`
- `local_checkout_head`
- `base_remote_head`

### Derived statuses

Project these into a small operator vocabulary:

- `in-flight`
- `pr-open`
- `merged-remote-not-local`
- `merged-and-local`
- `local-diverged`
- `orphaned-local-branch`

The important design choice is to keep these statuses derived from observable git/PR state rather than hand-edited prose.

## SVER Impact

### `S` Start

At session bootstrap, warn if:

- current checkout branch is `develop`
- local `develop` is behind `origin/develop`
- the requested seam/proposal was recently merged elsewhere but is absent locally

### `V` Verify

Before watched-live verification, require an explicit check:

- does this checkout contain the target slice?
- is the operator about to verify stale local truth?

### `E` End

At slice close-out, record reintegration status:

- branch pushed
- PR opened
- PR merged
- local main checkout synced or not

### `R` Retrospective

If multiple worktrees were involved, ask whether reintegration drift cost time or risk.

## Recommended UI / Query Surface

Add a graph/dashboard surface for:

- active worktrees
- branch -> base branch mapping
- PR state
- merge state to `develop`
- local checkout freshness relative to `origin/develop`
- reintegration alerts for merged-remote-not-local

Useful query examples:

- workstreams not yet merged to `develop`
- PR-merged slices not yet synced into local `develop`
- local `develop` behind remote while watched-live work is being planned

## Smallest Honest Slice

1. Record reintegration metadata on workstreams/sessions.
2. Add a graph query and dashboard view for reintegration status.
3. Add a startup warning when local `develop` is behind `origin/develop`.
4. Add close-out recording for `merged_to_base` and `local_checkout_synced`.

Do not start with auto-sync or branch mutation. First make drift visible.

## Rule Placement

- drift detection and merge-state calculation belong in code
- operator prompts and required workflow checks belong in SVER/process docs
- if a watched-live run can start from stale local truth, enforce that guard in code and summarize it in process

## Reality Gaps

- intel-graph already models worktrees and branches partially, but reintegration status is not yet first-class
- GitHub PR state and local git state are not joined into one operator view
- session/workstream close-out records progress, but not whether the work is back on `develop`

## Next Seam

`worktree-reintegration-tracking`

Specifically: add reintegration facts plus one dashboard warning for `merged-remote-not-local`.
