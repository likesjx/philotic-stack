# Philotic Forked Sessions Proposal

## Goal

Define how Philotic should support forked sessions and multiple concurrent sessions without collapsing session state into one overwrite-prone blob.

This proposal finishes the current session-management work item by answering three questions:

- how should forked sessions work
- what do major platforms/frameworks appear to do today
- how should Philotic handle multiple sessions beyond the current chat-focused path

## Research Summary

### OpenAI

OpenAI's current platform model centers on conversations/responses and previously threads/runs.

What stands out:

- the model is fundamentally linear per conversation/thread
- official docs emphasize persistent conversation state and replay/appending
- they do not appear to expose a strongly first-class public tree/branch model

Interpretation:

- if a user wants to branch, the practical pattern is "start a new conversation from a prior state/prefix"
- branch identity is represented by a new conversation object, not by mutating one conversation into a tree

Relevant docs:

- [OpenAI Conversations API](https://platform.openai.com/docs/api-reference/conversations/create-item)
- [OpenAI Assistants deep dive](https://platform.openai.com/docs/assistants/how-it-works/managing-threads-and-messages)

### Anthropic

Anthropic's Messages API is stateless from the server's perspective.

What stands out:

- the caller resubmits the message list each turn
- branching is naturally modeled by copying a prior transcript prefix and appending a different next message

Interpretation:

- Anthropic implicitly supports branching well because history is a value, not a mutable server-owned object
- the burden of branch management is on the application

Relevant doc:

- [Anthropic Messages API](https://docs.anthropic.com/en/api/messages)

### LangGraph

LangGraph has the clearest explicit branching model among the systems reviewed.

What stands out:

- checkpoints are first-class
- "time travel" and resuming from an old checkpoint creates a fork
- the history is treated as immutable lineage with new execution branches

Interpretation:

- this is the strongest model for Philotic to emulate conceptually
- branches should be new session identities with lineage metadata, not in-place tree mutation on one live session

Relevant docs:

- [LangGraph time travel](https://docs.langchain.com/langgraph-platform/human-in-the-loop-time-travel)
- [LangGraph use time-travel](https://docs.langchain.com/oss/python/langgraph/use-time-travel)

## Recommendation

Philotic should model a fork as a **new session with lineage**, not as a mutable branch list hanging off one session record.

That means:

- `session_id` remains the identity of one linear branch
- `turn_id` remains the identity of one round trip within that branch
- a fork creates a new `session_id`
- the new session records where it came from

This is the cleanest fit for:

- recovery
- replay
- approvals
- apartment checkpoints
- cross-component routing
- eventual UI rendering

## Why Not Tree-In-One-Session

Putting multiple branches inside one mutable session sounds elegant until the rest of the system has to use it.

Problems:

- leases become ambiguous
- "latest turn" stops being a single thing
- checkpoints need branch selectors everywhere
- model-router, membrane, and approvals all need branch-aware routing rules
- apartment recovery becomes harder because one "session" no longer corresponds to one working thread

In other words, the irony is that a single "session tree" sounds simpler but forces every consumer to become a branch engine.

## Proposed Session Lineage Model

Add the following fields to `SessionRecord`:

- `root_session_id: Option<String>`
- `parent_session_id: Option<String>`
- `forked_from_turn_id: Option<String>`
- `fork_reason: Option<String>`
- `fork_label: Option<String>`

Semantics:

- `session_id`
  - identity of this exact branch
- `root_session_id`
  - original ancestor session for the whole lineage
- `parent_session_id`
  - immediate source branch
- `forked_from_turn_id`
  - turn in the parent from which this branch diverged
- `fork_reason`
  - optional machine-oriented reason (`user_branch`, `tool_retry`, `approval_alt_path`, `simulation`)
- `fork_label`
  - optional human-friendly display label

### Example

Original conversation:

- `session_id = telegram:123:agent-jane-01`

Fork created after turn 7:

- `session_id = fork:01HXYZ...`
- `root_session_id = telegram:123:agent-jane-01`
- `parent_session_id = telegram:123:agent-jane-01`
- `forked_from_turn_id = turn-7`
- `fork_reason = user_branch`
- `fork_label = "Try a more technical answer"`

The fork then continues linearly with new turn IDs of its own.

## Turn Semantics Under Forking

Turns remain linear inside a branch.

Rules:

- each `turn_id` belongs to exactly one `session_id`
- turns are immutable once completed
- forking never rewrites an existing turn
- the first turn in a fork can reference the parent turn via session lineage, not by dual ownership

This keeps event correlation simple:

- `session_id` = branch trace
- `turn_id` = request/response trace within that branch

## Apartment Strategy With Forks

Do not store all branch state in one agent-level short apartment.

Recommended structure:

- `short`
  - agent-level index of active/recent sessions
- `short_session:<session_id>`
  - branch-local checkpoint for that session

### `short` example

```json
{
  "active_sessions": [
    {
      "session_id": "telegram:123:agent-jane-01",
      "updated_at": 1710000000,
      "has_active_turn": true
    },
    {
      "session_id": "fork:01HXYZ",
      "updated_at": 1710000010,
      "has_active_turn": false
    }
  ]
}
```

### `short_session:<session_id>` example

```json
{
  "session_id": "fork:01HXYZ",
  "root_session_id": "telegram:123:agent-jane-01",
  "parent_session_id": "telegram:123:agent-jane-01",
  "forked_from_turn_id": "turn-7",
  "source": "telegram",
  "recent_turns": [],
  "active_turn": null,
  "summary": "Forked to explore a more technical path."
}
```

This gives us:

- one recovery checkpoint per branch
- no collision between concurrent sessions
- a small index for restore/discovery
- compatibility with snapshot-based `SyncApartment`

## When Forks Should Exist

Forking should be explicit, not accidental.

Good triggers:

- user says "branch from here" / "explore an alternative"
- tool workflow needs a what-if simulation
- approval flow requires an alternate path
- agent wants a scratch branch for reversible experimentation

Bad triggers:

- every retry
- every clarification
- every tool call

Default user conversations should stay linear unless there is a meaningful reason to branch.

## Proposed Work for This Feature

### Storage / Graph

- extend `SessionRecord` lineage fields
- add storage tests for fork creation and lineage traversal
- add helper queries:
  - `list_child_sessions(parent_session_id)`
  - `list_lineage(root_session_id)`

### Agent Logic

- allow `philote` to create a new session checkpoint from a parent checkpoint
- clone only the required prefix/summary into the new branch
- keep new turns and checkpoints isolated to the forked session

### Membrane / UX

- support explicit fork commands later
- display fork labels and branch ancestry in UI/logs

## Full Recommendation

Philotic should adopt this policy:

1. A session is one linear branch.
2. Forking creates a new session, never a tree inside one mutable session.
3. Session lineage is stored explicitly on the child session.
4. Turns remain linear and branch-local.
5. Apartment checkpoints are per-session, plus a small per-agent session index.
6. Recovery restores by `session_id` from the branch-local checkpoint.

This is the most robust fit for the architecture we are building:

- generalized session management in the hotel
- agent-owned cognitive checkpoints
- generic IPC/event plane
- future support for branching UIs, simulations, and approvals

## Multiple Sessions Beyond This Use Case

The current implementation path is chat-centric, but the multiple-session model should be broader.

Philotic should expect one agent to participate in many simultaneous sessions of different kinds:

- user chat sessions
- approval sessions
- WebRTC/voice streaming sessions
- tool workflow sessions
- monitoring or background automation sessions
- scratch/fork sessions

That suggests three operating rules:

### 1. One branch per session

No mixed trees inside one session.

### 2. One checkpoint per session

No single hot apartment blob for all active work.

### 3. One small index per agent

The agent needs a compact map of what is live, but not all data loaded at once.

That is the best general model even outside chat:

- voice sessions can branch
- workflow sessions can fork for retries/simulations
- approval sessions can split from a parent workflow

So the branching model is not just a chat feature. It is a general runtime pattern.
