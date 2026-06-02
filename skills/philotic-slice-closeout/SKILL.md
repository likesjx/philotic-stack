name: philotic-slice-closeout
description: HIGH PRIORITY. Use this skill to finish every implementation slice. It mandates "green status clearing" (verified build/test/smokes), updates to docs/task.md, and explicit disposition updates for touched architecture proposals. No slice is complete until this operational close-out is recorded and committed.

# Philotic Slice Closeout

Scope: Philotic-specific.

Use this skill only for end-of-slice operational closure in the Philotic repo.

This skill owns:

- Philotic proposal disposition updates tied to a just-completed slice
- `docs/task.md` updates tied to that slice
- verification-level summary for that slice
- reality-gap capture from that slice
- commit/push discipline for that slice
- naming the next seam after that slice

This skill does not own:

- general proposal editing across arbitrary projects
- deep runtime investigation
- choosing the verification strategy from scratch

Use [$proposal-maintainer](../proposal-maintainer/SKILL.md) for generic proposal upkeep.
Use [$architecture-docs-maintainer](../architecture-docs-maintainer/SKILL.md) when the slice changed architecture truth, active seams, frontmatter metadata, or docs entrypoints.
Use [$verification-ladder](../verification-ladder/SKILL.md) when the main job is deciding validation depth.
Use [$runtime-debugger](../runtime-debugger/SKILL.md) when the main job is finding a live runtime failure.

## Workflow

1. Identify the slice boundary.
2. Update the relevant Philotic proposal `Disposition`.
3. Update `docs/task.md` for completed work and real follow-ups.
4. Update `docs/DEFECTS.md` for any defects opened, progressed, or closed during this slice. Confirm closing commits carry `Fixes: DEF-NNN`.
5. If architecture docs moved, run the `architecture-docs-maintainer` pass:
   - update `ARCHITECTURE_STATUS.md` if current truth changed
   - update `ARCHITECTURE.md` if durable reference changed
   - ensure metadata/domains/links still align
6. Summarize the highest honest verification level.
   - If live validation depended on a supervised/installed runtime, confirm installed binary truth and process restart before calling anything `smoke-green` or `watched-live-green`.
7. Record assumption-vs-reality gaps exposed by the slice.
8. Write the Muninn memory delta for durable context only:
   - decisions
   - reality gaps
   - validation outcomes
   - next seams
   - operator preferences
9. Commit and push one coherent slice per the commit convention in `AGENTS.md §6`.
10. State the next seam.

## Output Expectations

Report:

- what is working
- what remains intentionally incomplete
- the verification level
- the Muninn memory delta, or that no write was needed
- the commit hash
- the next seam
