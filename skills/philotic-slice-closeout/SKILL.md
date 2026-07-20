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
3. If the slice implements a promoted operator idea (a LifeGraph `idea:<slug>`
   node with `graph_ref` pointing at this work), close the idea loop:
   `just idea-sweep ship idea:<slug> "<short note>"`. Aria delivers the shipped
   digest to the operator on her next relevant turn — do not ping per merge.
4. Update `docs/task.md` for completed work and real follow-ups.
5. Update `docs/DEFECTS.md` for any defects opened, progressed, or closed during this slice. Confirm closing commits carry `Fixes: DEF-NNN`.
6. If architecture docs moved, run the `architecture-docs-maintainer` pass:
   - update `ARCHITECTURE_STATUS.md` if current truth changed
   - update `ARCHITECTURE.md` if durable reference changed
   - ensure metadata/domains/links still align
7. Summarize the highest honest verification level.
   - If live validation depended on a supervised/installed runtime, confirm installed binary truth and process restart before calling anything `smoke-green` or `watched-live-green`.
8. Record assumption-vs-reality gaps exposed by the slice.
9. Write the Muninn memory delta for durable context only:
   - decisions
   - reality gaps
   - validation outcomes
   - next seams
   - operator preferences
10. Commit and push one coherent slice per the commit convention in `AGENTS.md §6`.
11. If a measured harness trial is open for this slice (see
    [$sver-harness](../sver-harness/SKILL.md)), close it now with an honest
    status and verification level:
    `just harness-trial-close completed <verification-level> "<summary>"`.
    If no trial was started, note that telemetry was skipped — for seam-scoped
    implementation slices, prefer starting one next time
    (`just harness-trial-start <seam-id>`); the trial ledger is the only
    per-slice record of tokens, elapsed time, and lines changed per harness.
12. State the next seam.

## Output Expectations

Report:

- what is working
- what remains intentionally incomplete
- the verification level
- the Muninn memory delta, or that no write was needed
- the commit hash
- the next seam
