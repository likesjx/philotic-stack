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

### 1. Memory Sweep (Reflex Validation)

Ask: has the `mempalace_reflex_hook.sh` correctly captured the transcript of this session?

For coding operators (Claude Code, Antigravity, Cursor) that support lifecycle hooks, this occurs automatically on session end into the `intel-graph` broker.

If your IDE runner does **not** natively support `Stop` or `PreCompact` hooks:
Execute the bash hook directly as the final step of the session:
`bash scripts/mempalace_reflex_hook.sh overview.txt`
where `overview.txt` is the path to your current transcript or memory slice.

Muninn write-back is separate from transcript capture. At check-engine time, write only the durable memory delta directly from the main agent thread. Do not delegate these final writes to a background subagent.

Use this filter:

- Decision
- Reality gap
- Validation
- Next seam
- Operator preference

Do not store transcripts, noisy logs, proposal summaries already committed in docs, or routine task-list churn.

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
- session bootstrap skipped (Mempalace wake-up recall not done at start)
- file-based memory and graph memory drifted (one updated, not the other)
- Mempalace hook failed to fire or dropped payload without alerting
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
- Decision: <one line, or none>
- Reality gap: <one line, or none>
- Validation: <one line, or none>
- Next seam: <one line, or none>
- Operator preference: <one line, or none>
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

## Reflexive Write Rule at Session Close

At session end, ensure the Mempalace broker received the latest context slice.
If you are operating in `Claude Code`, this is handled natively by `.claude.json`.
If you are operating directly from an operator layer without lifecycle hooks, manually wrap up the terminal loop by invoking:
`bash scripts/mempalace_reflex_hook.sh <transcript_path>`
