name: check-engine
description: End-of-session review. Sweeps Muninn for unstored session work, syncs MEMORY.md, surfaces open threads and next seams, and identifies SVE process gaps. Run at the end of every meaningful session.

# Check Engine

Scope: global, run at the end of any meaningful work session.

This is the **E** (End) step of the SVE loop. It is the forcing function that keeps memory, documentation, and process aligned across sessions.

## When to Run

- Whenever the user says `/check-engine`, `check engine`, or `end of day review`
- After any session involving architectural decisions, deployment changes, bug fixes, or process discoveries
- Before closing a long-running session that covered multiple topics

## The Five Checks

### 1. Memory Sweep

Ask: what happened in this session that isn't yet in Muninn?

Scan the conversation for:
- bugs found and root-caused
- architectural decisions made (with rationale)
- deployment procedures executed or discovered
- process gaps identified
- user preferences or constraints stated
- external system behaviors observed

For anything not yet stored: call `muninn_remember_batch` directly in the main thread (not via subagent — you need confirmation before the session closes). Keep each memory atomic: one concept, 1–3 sentences.

### 2. MEMORY.md Sync

Check `~/.claude/projects/<project>/memory/MEMORY.md`. Does it have pointers to everything relevant? Add any new memory files to the index. Remove stale pointers.

### 3. Open Threads

List explicitly:
- what is working / confirmed
- what is intentionally deferred (and where it's tracked)
- what the next highest-value seam is

### 4. Process Gaps

Ask: did the SVE process fail anywhere this session?

Common failure modes:
- session bootstrap skipped (Muninn recall not done at start)
- Muninn writes batched to end instead of during work
- file-based memory and Muninn drifted (one updated, not the other)
- slice closed without `philotic-slice-closeout` pass
- deployment touched both formulas but only one was updated

Name the gap and whether it needs a skill/protocol update.

### 5. Green Status

If any code was written or committed this session, confirm:
- `just check` passes
- relevant tests pass
- no stale zombie processes on either machine

## Output Format

```
## Check Engine — <date>

### Stored to Muninn
- <concept>: <one line>
- ...

### MEMORY.md
- <added / updated / no changes>

### Open Threads
- Next: <highest value seam>
- Deferred: <what and where tracked>

### Process Gaps
- <gap and whether a skill was updated>

### Green Status
- <check result or skipped if no code changed>
```

## Muninn Write Rule at Session Close

At session end, call `muninn_remember` / `muninn_remember_batch` **directly in the main thread**.

Do not delegate to a background subagent at close-out — the session may end before the subagent completes. Direct writes give you confirmation before the session closes.

The `muninn-memory-habit` subagent delegation rule applies **during active work** (non-blocking writes). At check-engine time, block and confirm.
