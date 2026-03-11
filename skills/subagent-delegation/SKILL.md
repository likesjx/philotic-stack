---
name: subagent-delegation
description: Use this skill when breaking a larger task into bounded subagent work. Trigger when the work would benefit from parallel investigation, focused implementation, test generation, or runtime triage, and you need to decide what context to pass, what authority to retain, and how to avoid overlap or confusion.
---

# Subagent Delegation

Scope: General.

Use this skill when the main question is how to divide work across helper agents without flooding them with unnecessary context or surrendering the architectural steering wheel.

## Goals

- Delegate narrow, high-leverage work.
- Pass only the context needed for the assigned seam.
- Keep final architectural judgment and synthesis in the main thread.
- Avoid duplicated effort, conflicting edits, and context bloat.

## Core Rule

Subagents should receive the smallest context that lets them succeed honestly.

Do not give every subagent the whole repo story by default. That is not collaboration. That is synchronized confusion with better CPU utilization.

## What Subagents Are Good For

- focused codepath inspection
- targeted test writing
- isolated implementation of a well-bounded slice
- log or runtime triage
- comparing a few concrete design alternatives
- documenting or summarizing a narrow subsystem

## What Subagents Are Bad For

- open-ended architecture ownership
- ambiguous product direction
- decisions that depend on many cross-cutting tradeoffs
- tasks where multiple subagents would need the same large context to be competent
- final truth arbitration between competing implementations

## Delegation Workflow

1. Name the seam.
   - What exact question or slice is being delegated?
   - What is out of scope?

2. Set the truth level.
   - `inspect`
   - `implement`
   - `verify`
   - `explore`

3. Pass only the required context.
   - relevant files
   - relevant proposal/spec sections
   - specific tests or logs
   - explicit assumptions

4. Define the output contract.
   - findings only
   - code change
   - test patch
   - recommendation with tradeoffs
   - evidence summary

5. Keep final synthesis centralized.
   - the main thread integrates results
   - the main thread resolves conflicts
   - the main thread owns final architectural decisions

## Context Budget Rules

Prefer passing:

- 1-5 directly relevant files
- one proposal or task section if needed
- one explicit objective
- one explicit success condition

Avoid passing:

- the entire architecture corpus
- unrelated work items
- broad motivational framing
- ambiguous authority like "figure out the best approach"

## Heuristics

- If two subagents would need the same big context, the seam is probably not split cleanly enough.
- If the delegated task can be evaluated with a yes/no result or a tight diff, it is a good candidate.
- If the task depends on hidden repo norms, include those norms explicitly instead of hoping the subagent intuits them.
- If the subagent returns something surprising, compare it against the original context budget before blaming the result.

## Output Expectations

When using this skill, define:

- the delegated seam
- the truth level
- the context package
- the expected output shape
- what authority remains with the main thread
