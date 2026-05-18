---
name: philotic-muninn-memory-habit
description: Use at session start and closeout to retrieve and write durable Muninn continuity context.
---

# Muninn Memory Habit

This Windsurf skill is projected from the Philotic canonical harness layer.

## Operating Guidance

- For meaningful work, run the Muninn bootstrap/orientation before decisions, resumed work, or implementation.
- Prefer `just session-start`; otherwise run `python3 scripts/muninn_mcp.py bootstrap`, then retrieve self/user/topic context with the Muninn triad.
- If Muninn cannot bootstrap, stop and get explicit operator approval before continuing on repo/runtime truth only.
- At closeout, write only the durable Muninn memory delta: decisions, reality gaps, validation outcomes, next seams, or operator preferences.
- Do not store transcripts, routine task churn, noisy logs, or proposal/docs summaries already committed in the repo.

## Ownership

- Muninn stores continuity handles, not source-of-truth docs or transcripts.
- Repo docs/code store implemented truth.
- Intel Graph stores structure, seams, work coordination, decisions, and verification evidence.
- `docs/task.md` stores active execution work.
