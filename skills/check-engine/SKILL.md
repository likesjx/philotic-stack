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

### 2.5 Defect Sweep

Check `docs/DEFECTS.md`. For each defect touched this session:

- **New bug found or root-caused** → add a `DEF-NNN` entry with status, severity, size, seam, and found date. Store to Muninn.
- **Defect resolved** → update `Status: fixed` and add `Fixed: <commit-hash>`. Confirm the closing commit carries `Fixes: DEF-NNN`.
- **Defect progressed but not closed** → update status to `in-progress` and add a short note.

Ask explicitly: *did any commit this session close a known defect without updating DEFECTS.md or adding a `Fixes:` trailer?*

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

Also ask:

- did a rule live too high in top-level guidance and need to move into code?
- did a repeated process step deserve a skill or workflow update?
- should this session end with a retrospective instead of only a check-engine pass?

### 5. Green Status

If any code was written or committed this session, confirm:
- `just check` passes
- relevant tests pass
- no stale zombie processes on either machine

### 6. Graph Health Check

If the Intel Graph is running (`just intel-graph-status`), run the combined health check:

```bash
curl -s http://127.0.0.1:8900/api/health | jq .
```

Check for:
- **Stale sessions**: Active sessions older than 4 hours → auto-cleanup via `just intel-graph-session-cleanup`
- **Orphaned workstreams**: Workstreams with no active session → investigate and close
- **Missing dispositions**: Proposals without a disposition → flag for next session
- **Verification gaps**: High count of `verification_level: none` → prioritize in next slice
- **Embedding gaps**: Proposals without embeddings → run `just intel-graph-embed-proposals`

If the graph is not running, skip this check and note it in the output.

Also check:
- Was a `session_start` called at the beginning of this session? If not, note the gap.
- Was `session_close` called? If not, close it now.
- Were any test runs recorded? If code was tested, record results via `just test-and-record`.

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

### Graph Health
- Sessions: <active>/<total>, stale: <count>, cleaned: <count>
- Proposals: <total>, missing disposition: <count>, no verification: <count>
- Embeddings: <count> unembedded proposals
- Session protocol: <started/closed/gap noted>
```

## Relationship To Retrospectives

`check-engine` and `retrospective-workflow` are not the same thing.

Use `check-engine` to close the session cleanly:

- memory written
- open threads named
- process gaps surfaced
- green status stated

Escalate to [$retrospective-workflow](../retrospective-workflow/SKILL.md) when the session exposed:

- important surprises
- repeated workflow pain
- a new rule-placement lesson
- an SVE/process optimization opportunity

## Muninn Write Rule at Session Close

At session end, call `muninn_remember` / `muninn_remember_batch` **directly in the main thread**.

Do not delegate to a background subagent at close-out — the session may end before the subagent completes. Direct writes give you confirmation before the session closes.

The `muninn-memory-habit` subagent delegation rule applies **during active work** (non-blocking writes). At check-engine time, block and confirm.
