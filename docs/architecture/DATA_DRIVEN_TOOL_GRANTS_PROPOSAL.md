---
title: Data-Driven Tool Grants (SkillDAG)
doc_type: proposal
domain: tooling-execution
status: proposed
last_updated: 2026-07-21
tags:
- tooling
- grants
- skilldag
- autopoiesis
---

# proposal:data-driven-tool-grants-skilldag — PROPOSED (spec stage)

Filed 2026-07-14 (handoff PR #282, architecture item). Operator-flagged
principle from that session: **"we should not have any tool hard coded."**
Motivating case: the inability that night to disable one tool
(`life.observe.batch`) without a code change and deploy.

Note: the handoff says "full pros/cons in the proposal node", but no
intel-graph node was ever created — the handoff paragraph
(HANDOFF-2026-07-14-lifegraph-batch.md, "Architecture item") is the
canonical source.

## Problem

Tool grants are compiled into the binaries across five surfaces:

1. `skill_implied_tools` in `catalog.rs`
2. `tools_for_allowed_class` in `ipc.rs`
3. seeded `implied_tools` in `main.rs`
4. runner `supported_tools` in `main.rs`
5. `tool_catalog()`

So any enable/disable/grant/re-route needs a deploy.

## Goal

Grants become graph/config data — seeded once, editable at runtime with
**no deploy**. The hardcoded lists demote to a first-boot seed + fallback.

## SkillDAG decision

Keep the **authoritative, hot-path grants in the LOCAL hotel context
graph** (fast, always-available, per-hotel). Do NOT put runtime tool
resolution behind the remote LifeGraph (Memgraph on vps-jane), or every
agent bricks when it is down — that session's failure mode. The LifeGraph
is only an **optional reasoning/design layer**: the agent proposes changes
against it, and those changes **compile down** to the local toolset
(compiler pattern; autopoiesis fit).

## Slices

1. **Grant registry in the context graph** — verify by disabling
   `life.observe.batch` at runtime with no deploy.
2. **Runner routing as data** — the PR #277 reconcile is a precedent.
3. **Governance/audit.**
4. **Later** — SkillDAG reflection in the LifeGraph.

## Precedent

The PR #277 runner-routing reconcile is cited in the handoff as a
precedent for slice 2, and DEF-057's reconciling seeder (found in the
2026-07-19 role/tool deep-dive audit, fixed via codex/role-admin-hardening)
proved runtime DB changes can survive restarts.
