---
name: retrospective-workflow
description: Use this skill to run a Philotic retrospective after a meaningful slice, live-debug session, or end-of-day checkpoint. It structures the retro around what went well, what did not, how to do better, how SVE should change, and how to optimize SVE. Trigger when the user asks for a retrospective, retro, lessons learned, process reflection, or engine/process improvement.
---

# Retrospective Workflow

Scope: Philotic-specific.

Use this skill when the goal is to turn a session into operational learning instead of just a summary.

This skill owns:

- retrospective structure and facilitation
- separating code lessons from process lessons
- identifying what should change in SVE
- identifying what should become a skill, a process rule, or a code invariant

This skill does not own:

- generic slice closeout
- commit/push procedure
- deep runtime root-cause debugging

Use [$philotic-slice-closeout](../philotic-slice-closeout/SKILL.md) to finish a code slice.
Use [$check-engine](../check-engine/SKILL.md) for session-end memory/process hygiene.
Use [$runtime-debugger](../runtime-debugger/SKILL.md) when the main task is still finding the live failure.

## When To Run

- after a hard debugging session
- after a watched-live run with important surprises
- at end of day when the user asks for a retrospective
- when process gaps or workflow pain became part of the work

## Workflow

1. Reconstruct the session in seams, not just chronology.
2. Classify lessons into:
   - correctness/code
   - workflow/process
   - SVE/process-engine
3. Answer the five retro questions:
   - what went well
   - what did not
   - how can we do better
   - how should SVE change
   - how should SVE be optimized
4. Apply the rule-placement heuristic:
   - bug-preventing rule -> code/tests/crate docs
   - coordination rule -> workflow/process docs or skills
   - both -> enforce in code, summarize in process
5. Name concrete follow-ons:
   - code invariant
   - process doc update
   - skill update
   - new skill
6. Decide where each improvement lands in the SVE loop:
   - `S` start/bootstrap
   - `V` verification/live truth
   - `E` end/close-out
   - `R` retrospective itself
7. End with the next highest-value improvement seam.

## Output Shape

Use this exact structure unless the user asks for something else:

1. What went well
2. What did not
3. How can we do better
3a. How can we update SVE?
3b. How can we optimize SVE?

Then add:

- `Rule placement decisions`
- `SVE loop updates`
- `New skill opportunities`
- `Next improvement seam`

## Heuristics

- Prefer seam-based lessons over diary-style recap.
- Name truth drift explicitly: source truth, build truth, installed truth, live truth.
- If the same failure cost time twice, it probably deserves a skill or workflow rule.
- If the same failure could have been prevented by a type, parser, or test, it should move into code.
- Do not let “what went well” become vague morale paste; tie it to actual practices that helped.

## Common Philotic Retro Themes

- rollout truth vs source truth
- watched-live honesty
- tool projection policy
- provider contract clarity
- boundary ownership
- whether a rule is living too high in `AGENTS.md`

## Skill Creation Trigger

Create or update a repo-local skill when the retrospective identifies:

- a repeated multi-step workflow that cost time more than once
- a validation ritual that should be standardized
- a recurring debugging pattern with stable checkpoints
- a process gap that can be reduced by explicit workflow guidance
