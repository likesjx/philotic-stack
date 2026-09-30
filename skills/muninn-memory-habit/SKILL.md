---
name: muninn-memory-habit
description: HIGH PRIORITY. Use this skill at the START of EVERY session to retrieve project context and during the session to store decisions. It is the mandatory bootstrap mechanism for continuity and the engine for project optimization. Retrieve before meaningful work, write back important decisions, and use revealed gaps to improve repo-local protocols.
---

# Muninn Memory Habit

Use this skill when Muninn is part of the active workflow.

## Goal

Treat Muninn as a working memory habit, not a ceremonial sidecar.

The objective is to test whether regular retrieval and write-back improve:
- continuity
- personalization
- design recall
- context compression

When Muninn MCP is configured and reachable, this habit is the MANDATORY BOOTSTRAP STEP defined in CLAUDE.md. It is not an optional flourish.

## Ownership Split

Muninn is the continuity layer. It is not the source of truth, the task tracker, or a transcript archive.

Use the right owner:

- repo docs and code store implemented truth
- Intel Graph stores structure, seams, work coordination, decisions, and verification evidence
- `docs/task.md` stores active execution work
- Muninn stores why something matters next time: compact decisions, learned preferences, reality gaps, validation outcomes, and continuity handles

This split matters because memory should make the next session sharper without becoming a second, fuzzier copy of the repository.

## The Bootstrap Sequence

At the start of every session (before responding to the user):
1.  **Bootstrap Muninn**: run `python3 scripts/muninn_mcp.py bootstrap`
2.  **If bootstrap fails**: alert the user/operator immediately and do not continue without explicit approval
3.  **Recall Self**: `muninn_recall(identity, operating_posture)`
4.  **Recall User**: `muninn_recall(user_preferences, collaboration_style)`
5.  **Recall Topic**: `muninn_where_left_off` + `muninn_recall(active_goal, architecture_decisions)`
6.  **Orient**: Summarize how the recalled context shapes your plan for the current turn.

## Failure Rule

If the shared helper or Muninn MCP is unavailable or cannot be recovered automatically:

- say so immediately
- do not imply memory retrieval occurred
- pause and require explicit approval before proceeding without Muninn
- if approval is granted, say clearly that you are continuing on observed repo/runtime truth only

## Memory Triad

Prefer organizing memory around three questions:

1. Who am I?
- agent identity
- stable operating style
- collaboration posture

2. Who am I talking to?
- user preferences
- collaboration fit
- recurring needs or dislikes

3. What matters about this topic right now?
- active goals
- architecture decisions
- unresolved seams
- relevant facts or constraints

## Default Habit

### During active work: delegate writes to background subagents

During an active session, delegate Muninn writes to background subagents so the main thread is never blocked.

**Exception — session close (check-engine):** call `muninn_remember` / `muninn_remember_batch` directly in the main thread. Do not delegate to a background subagent at close-out — the session may end before the subagent completes, losing the writes. Direct calls at check-engine time are correct and expected.

**Recall → foreground subagent** (result needed before responding):

Spawn a foreground Agent with the topic context. The agent runs `muninn_where_left_off` or `muninn_recall`, returns the relevant memories as a concise summary. Main thread uses that summary before continuing.

Trigger recall:
- at session start
- when the conversation shifts to a new topic
- before architectural decisions or proposals

But before any recall attempt, the main thread should first verify the bootstrap gate with:

- `python3 scripts/muninn_mcp.py bootstrap`

If that gate fails, the main thread should not spawn recall subagents until the user/operator has explicitly approved proceeding without Muninn.

**Write → background subagent** (fire and forget):

Spawn a background Agent after each substantive turn. Main thread does not wait.

**How to delegate writes:**

Spawn a background Agent with a prompt like:

```
You are a memory subagent. Store the following concepts from this conversation turn as atomic Muninn memories.

Rules:
- One concept per muninn_remember call
- 1-3 sentences max per memory
- Fire all writes in parallel
- Do not store anything already captured in a committed doc

Concepts from this turn:
<bullet list of key decisions, gotchas, preferences, and facts>

Vault: default
```

The main thread immediately continues. The subagent handles writes asynchronously.

**When to delegate inline instead:**

If the concept is urgent and must survive an immediate crash or context loss, write it directly in the main thread rather than delegating. Otherwise always delegate.

Memory atomicity rules (enforced by the subagent):
- **One concept per memory.** If there are five things, write five memories.
- **Short.** 1-3 sentences max. Aim for under ~300 characters when possible; do not exceed ~500 for ordinary `remember` writes.
- Let clustering happen naturally — do not manually group.

Recommended lightweight tags when helpful:
- `flesh-out`
- `decision`
- `reality-gap`
- `validation`
- `follow-up`
- `operator-preference`

Use tags sparingly. If a tag does not improve retrieval later, it is decorative bureaucracy.

Muninn v0.7 recall can filter server-side with `tags_all`, `tags_any`, and
`tag_filter`. Prefer these lane tags when the memory clearly belongs to a lane:

| Lane | Tags | Use |
| --- | --- | --- |
| Continuity | `continuity`, `operator-preference`, `decision` | cross-client orientation and durable preferences |
| Philotic runtime | `philotic-stack`, `runtime`, `validation`, `reality-gap` | repo/runtime work and live ops |
| MCP boundary | `mcp`, `perplexity`, `lifegraph`, `muninn-only` | external MCP surface hygiene |
| LifeGraph adjacent | `lifegraph`, `evidence`, `recall-feedback` | LifeGraph context without treating Muninn as LifeGraph truth |
| Client session | `codex`, `claude`, `perplexity` | client-specific working continuity |

Do not over-tag. Lane tags are retrieval hints, not an ontology or a proof of
truth. In particular, never use Muninn tags to imply confirmed LifeGraph truth.

Good atomic candidates:
- "active_incarnation_id is the load-bearing primitive for session ownership switching in Philotic."
- "Jared prefers soft toolset restrictions on the conversational incarnation, not hard-coded."
- "Worker readiness race: do not update active_incarnation_id until the worker registers."

Bad (too dense — don't do this):
- "The incarnation model has three kinds with active_incarnation_id as the primitive, memory in four tiers, and HandoffToWorker/Back IPC..."

## What to Store

One concept. One memory.

Good candidates:
- a single architectural decision and its one-sentence rationale
- a single user preference
- a single gotcha or risk
- a single "do not do X" rule

Avoid:
- summaries of whole proposals (the proposal file already exists)
- long verbatim logs
- anything that duplicates what's in a committed doc
- routine task-list churn
- whole transcripts

## Memory Delta Closeout

At closeout, write a Muninn memory delta only for durable new context.

Use this shape when it helps:

```text
Memory delta:
- Decision:
- Reality gap:
- Validation:
- Next seam:
- Operator preference:
```

Each filled line should usually become one atomic `muninn_remember` or `muninn_decide` call. Omit empty lines. If all lines are empty, say no Muninn write was needed.

Do not store:

- transcript summaries
- noisy logs
- proposal summaries already present in committed docs
- routine "completed task X" entries unless the outcome changes future behavior

## Retrieval and Projection

When Muninn returns relevant memory, treat it as projected context, not absolute truth.

- summarize it succinctly
- use it to shape the answer or plan
- keep a distinction between:
  - recalled memory
  - current observed repo/runtime truth

If recalled memory conflicts with current code or docs, trust current observed truth and note the mismatch.

## Suggested Workflow

1. Determine whether the turn is meaningful enough for retrieval.
2. Retrieve along the triad:
- self
- user
- topic
3. Use the recalled memory to shape planning or explanation.
4. After the turn, write back any durable learning.

## The Optimization Loop

Use memory to proactively improve the repository:

1.  **Identify Protocol Gaps**: If a memory surfaces a recurring error (e.g., "IPC framing is brittle in test X"), do not just fix the code.
2.  **Evaluate Protocol**: Check if `AGENTS.md` or a `SKILL.md` needs a new rule to prevent the error.
3.  **Update Repo**: Apply the improvement to the local markdown documentation.
4.  **Note the Optimization**: Store the update as a new memory (e.g., "Updated verification-ladder skill with IPC framing rule based on recurring test failures").

## Honesty Rule

Do not claim Muninn improved continuity unless retrieval actually influenced the turn.

If Muninn was not consulted, say so.
If Muninn returned irrelevant memory, say so.
If the habit is too heavy, say so.
If Muninn was unavailable, say so immediately and note whether the user approved continuing without it.

The experiment is about whether the memory helps, not about pretending it helped.
