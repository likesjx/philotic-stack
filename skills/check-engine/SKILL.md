---
name: check-engine
description: End-of-session review under existing authorization. Reviews the durable Muninn delta, MEMORY.md, open threads, verification and process gaps; performs permitted closeout duties or records precise deferred status. Run at the end of every meaningful session.
---

# Check Engine

Scope: global, run at the end of any meaningful work session.

This is the **E** (End) step of the SVE loop. It is the forcing function that keeps memory, documentation, and process aligned across sessions.

## When to Run

- Whenever the user says `/check-engine`, `check engine`, or `end of day review`
- After any session involving architectural decisions, deployment changes, bug fixes, or process discoveries
- Before closing a long-running session that covered multiple topics

## Closeout Authority and Availability

Muninn remains the default continuity mechanism. Before any check or side effect below, apply the canonical [recovery/defer path](../muninn-memory-habit/SKILL.md#recovery-and-temporary-defer) and [approval handoff rules](../../AGENTS.md#83-approval-evidence-across-handoffs). Those rules govern every transcript, memory, file and graph operation here; Check Engine creates no new access or write authority. Do not repeat approval requests for already-authorized actions absent material scope change or required action-time confirmation.

When an operation is unavailable, prohibited or outside authorization, do not run it or substitute another surface. Continue the independent review using permitted evidence. In the existing closeout/task record, name the exact deferred action and target, reason, responsible owner and recheck trigger (before a dependent decision or at the next meaningful checkpoint). Distinguish `not run/deferred`, `failed`, `passed with evidence` and `not applicable`; no attempt is not a pass, and an unobserved write must not be reported as “stored.” No personal-memory or transcript access is implied by permission to repair guidance or review a read-only task.

## The Checks

### 1. Memory Sweep (Reflex Validation)

If transcript verification is authorized, ask: is there permitted evidence that `mempalace_reflex_hook.sh` captured this session? Otherwise record transcript capture as not checked; do not inspect or submit transcript contents.

Coding operators (Claude Code, Antigravity, Cursor) may have lifecycle hooks configured to capture into the `intel-graph` broker. Configuration alone does not prove delivery; do not claim success without evidence or change hook configuration as part of this review.

If your IDE runner does **not** natively support `Stop` or `PreCompact` hooks, and the exact transcript submission and target are authorized and available, the existing manual path is:
`bash scripts/mempalace_reflex_hook.sh overview.txt`
where `overview.txt` is the path to your current transcript or memory slice.

Muninn write-back is separate from transcript capture. After authorized recovery/readiness and within the permitted write scope, write only the durable memory delta directly from the main agent thread. Do not delegate these final writes to a background subagent. Otherwise record the write as deferred under the canonical path; do not redirect it into transcript capture or local memory as a workaround.

Use this filter:

- Decision
- Reality gap
- Validation
- Next seam
- Operator preference

Do not store transcripts, noisy logs, proposal summaries already committed in docs, or routine task-list churn.

### 2. MEMORY.md Sync

Only when personal-memory access is authorized, check `~/.claude/projects/<project>/memory/MEMORY.md` for relevant pointers. Add new memory files or remove stale pointers only within authorized write scope. Otherwise record the specific read or sync as not run/deferred; a repository-only task does not authorize this file access.

### 2.5 Defect Sweep

Review `docs/DEFECTS.md` using permitted repository evidence. For each defect touched this session, make the following updates only within authorized write scope; in a read-only task, report proposed updates as not applied:

- **New bug found or root-caused** → add a `DEF-NNN` entry with status, severity, size, seam, and found date. Include a durable Muninn delta only under the memory-write conditions above.
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
- session bootstrap skipped without recording the canonical recovery/defer decision (a recorded authorized defer is not itself a process failure)
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

If any code was written or committed this session, report the relevant authorized verification that actually ran:
- `just check` or the appropriate scoped check, with evidence
- relevant tests and their results
- process health only on targets whose inspection is authorized and relevant; no cross-machine access or cleanup is implied

Record checks not run with their reason; do not turn missing runtime evidence into green status.

### 6. Graph Health Check

Only if graph inspection is authorized for this task, establish availability with the permitted status check (for example `just intel-graph-status`) and, when running, use the existing health check:

```bash
curl -s http://127.0.0.1:8900/api/health | jq .
```

Check for:
- **Stale sessions**: Active sessions older than 4 hours → report candidates; use `just intel-graph-session-cleanup` only if its full affected scope is authorized
- **Orphaned workstreams**: Workstreams with no active session → investigate with permitted evidence and close only those within authorized scope
- **Missing dispositions**: Proposals without a disposition → flag for next session
- **Verification gaps**: High count of `verification_level: none` → prioritize in next slice
- **Embedding gaps**: Proposals without embeddings → run `just intel-graph-embed-proposals` only within authorized write scope; otherwise record the gap/defer

If unavailable or graph access is prohibited/outside scope, do not start it or probe it; record graph checks and writes as not run/deferred with owner and recheck.

Also check:
- Was a `session_start` called at the beginning of this session? If not, distinguish a process gap from an intentional scoped defer.
- Was this session's `session_close` called? If a session was started and its closure is authorized/available, close that session; otherwise record the exact deferred closure or “not applicable: no session started.” Do not close unrelated sessions.
- Were test runs recorded? If recording and the recipe's checks are authorized/available, use `just test-and-record`; otherwise report actual local test results and graph recording as not run/deferred. Do not rerun prohibited tests merely to populate the graph.

## Output Format

If graph inspection did not run, omit numeric fields or mark them unknown; never substitute zero. Record deferred actions once in the existing closeout/task record and reference them from the relevant sections.

```
## Check Engine — <date>

### Deferred Operations
- <action/target — not run/deferred; reason; owner; recheck trigger, or none>

### Muninn Delta — <stored with evidence / not run-deferred / failed / no durable delta>
- Decision: <one line, or none>
- Reality gap: <one line, or none>
- Validation: <one line, or none>
- Next seam: <one line, or none>
- Operator preference: <one line, or none>
- ...

### MEMORY.md
- <added / updated / checked-no changes / not run-deferred / failed>

### Open Threads
- Next: <highest value seam>
- Deferred: <what and where tracked>

### Process Gaps
- <gap and whether a skill was updated>

### Green Status
- <passed with evidence / failed / not run-deferred with reason / not applicable>

### Graph Health
- Inspection/recording: <performed with evidence / not run-deferred / failed>
- Sessions: <active>/<total>, stale: <count>, cleaned: <count>
- Proposals: <total>, missing disposition: <count>, no verification: <count>
- Embeddings: <count> unembedded proposals
- Session protocol: <started/closed/gap noted>
```

## Relationship To Retrospectives

`check-engine` and `retrospective-workflow` are not the same thing.

Use `check-engine` to close the session cleanly:

- authorized durable memory delta written with evidence, or precisely deferred
- open threads named
- process gaps surfaced
- green status stated

Escalate to [$retrospective-workflow](../retrospective-workflow/SKILL.md) when the session exposed:

- important surprises
- repeated workflow pain
- a new rule-placement lesson
- an SVE/process optimization opportunity

## Reflexive Write Rule at Session Close

At session end, apply the transcript conditions in Check 1; this section adds no separate submission duty. Verify receipt only through permitted evidence. Use the existing manual hook only when submission is authorized, available and needed; do not duplicate a confirmed hook delivery. Otherwise record the exact capture/verification action as not run/deferred with owner and recheck. Do not edit `.claude.json` or disable normal authorized hooks to satisfy this closeout.
