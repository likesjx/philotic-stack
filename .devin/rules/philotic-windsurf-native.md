---
trigger: model_decision
description: Philotic harness rule for the `windsurf-native` Windsurf workspace projection. Use when the task matches the active role or skills.
---

# Philotic Windsurf Harness: windsurf-native

This workspace uses Windsurf-native customization surfaces managed by `phil graph harness`.

## Active Role Charter

- `orchestrator`

## Active Skills

- muninn-memory-habit
- planning
- verification

## Notes

- Follow the workspace `AGENTS.md` instructions already present in the repository root.
- Prefer the matching Windsurf skills and workflows under `.windsurf/` when they apply.

## Muninn Memory

- For meaningful work, run the Muninn bootstrap/orientation before decisions, resumed work, or implementation.
- Prefer `just session-start`; otherwise run `python3 scripts/muninn_mcp.py bootstrap`, then retrieve self/user/topic context with the Muninn triad.
- If Muninn cannot bootstrap, stop and get explicit operator approval before continuing on repo/runtime truth only.
- At closeout, write only the durable Muninn memory delta: decisions, reality gaps, validation outcomes, next seams, or operator preferences.
- Do not store transcripts, routine task churn, noisy logs, or proposal/docs summaries already committed in the repo.
