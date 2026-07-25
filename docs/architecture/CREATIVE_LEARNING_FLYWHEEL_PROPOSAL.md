---
title: Creative Learning Flywheel Proposal
doc_type: proposal
domain: memory-context
status: implemented
last_updated: 2026-07-25
tags:
  - creativity
  - learning
  - experimentation
  - compounding-growth
related_docs:
  - KNOWLEDGE_ARCHITECTURE_PROPOSAL.md
  - LIFE_GRAPH_OS_PROPOSAL.md
  - MEMPALACE_EPISODIC_MEMORY_PROPOSAL.md
  - OBSIDIAN_KNOWLEDGE_GARDEN_PROPOSAL.md
task_refs:
  - docs/task.md#creative-learning-flywheel
proposal_id: creative-learning-flywheel
implements:
  - cross-agent-knowledge-architecture
  - life-graph-os
active_seams:
  - creative-learning-flywheel
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# Creative Learning Flywheel Proposal

## Goal

Turn captured experience and knowledge into a compounding cycle of curiosity, connection, creation, experimentation, reflection, and reuse.

The intended outcome is not a perfectly organized archive. It is more ideas developed, more things made, faster learning, better cross-domain connections, and less energy lost to re-entry.

## Core Recommendation

Build one visible loop:

```text
Discover → Capture → Connect → Make → Test → Reflect → Share or Apply → Reinforce
```

Each pass should leave behind reusable structure that makes the next pass easier:

- a question becomes an idea
- an idea becomes an experiment
- an experiment becomes an artifact
- the artifact produces learning
- the learning changes a goal, method, or future idea
- agents recall the right context when the thread resumes

Organization serves motion through this loop. If the system creates more categories than artifacts, it is succeeding at the wrong thing.

## Disposition

`implemented`; four-week pilot active

This proposal does not create another memory store. It defines the behavior, graph vocabulary, prompts, reviews, and measures that turn the existing stores into a personal growth system.

The first implementation slice uses `life.capture`,
`life.flywheel.brief`, and `life.flywheel.review` over the canonical
LifeGraph. All captures enter as proposed evidence; the brief and review are
read-only. V006 is installed on the live Memgraph, the `mac-jane` catalogs and
parser are installed, and the updated runner is installed under the supervised
`vps-jane` hotel. The production route is watched-live-green from `mac-jane`
through the remote runner and live Memgraph/ONNX services and back to
`mac-jane`.

The idempotent cadence installer registers:

- `lifegraph-flywheel-daily:mac-jane` at `12:00 UTC` daily
- `lifegraph-flywheel-weekly:mac-jane` at `22:00 UTC` on Sunday

Both jobs target Björk's orchestrator role, preapprove only their read-only
flywheel tools, run in isolated cron sessions, and deliver one bounded result
to the operator's Telegram. Both completed a forced first fire on 2026-07-25.
The scheduler currently stores UTC expressions, so the displayed local time
shifts by one hour when America/New_York enters or leaves daylight saving time.

## Ownership By Stage

| Stage | Canonical owner | Supporting context |
| --- | --- | --- |
| Discover | LifeGraph question or opportunity | sources, conversations, observed interests |
| Capture | MemPalace episode or quick LifeGraph signal | client and provenance |
| Connect | LifeGraph relationships | Obsidian links and semantic retrieval |
| Make | Obsidian or repository artifact | LifeGraph idea and project context |
| Test | LifeGraph experiment | runtime, project, or real-world evidence |
| Reflect | Obsidian learning note plus LifeGraph learning | MemPalace episode and Muninn continuity |
| Share or Apply | canonical artifact or commitment | audience, goal, and feedback |
| Reinforce | Muninn compact lesson and LifeGraph relationship updates | retrieval feedback |

## Minimal Growth Vocabulary

The first useful ontology extension should remain small:

- `Question`: something worth understanding
- `Idea`: a possible connection, approach, or creation
- `Experiment`: a bounded way to test or explore
- `Artifact`: something made, published, performed, or shipped
- `Learning`: a reusable conclusion with evidence
- `Source`: material that informed the work

Suggested relationships:

```text
Question --INSPIRES--> Idea
Source --INFORMS--> Question | Idea
Idea --TESTED_BY--> Experiment
Experiment --PRODUCES--> Artifact | Learning
Artifact --EXPRESSES--> Idea
Learning --REFINES--> Idea | Goal | Method
Artifact --SHARED_WITH--> Person | Audience
```

These labels and relationships are accepted for the current slice through
`V006__creative_learning_flywheel.cypher` and the server-side LifeGraph
allowlists. They remain governed by the normal proposed-write and confirmation
path.

## Agent Behaviors

Every knowledgeable agent should be able to:

- capture a question or idea in under ten seconds
- resume an active creative thread with a bounded context packet
- suggest surprising but provenance-backed connections
- turn a vague idea into the smallest useful experiment
- help create an artifact, not only discuss one
- close a loop with reflection and an explicit reusable learning
- ask whether a learning changed future behavior

Agents should not:

- generate unsolicited project sprawl
- mistake semantic similarity for a meaningful connection
- optimize node counts or note counts
- promote speculative ideas into confirmed commitments
- interrupt flow with mandatory taxonomy work

## Operating Rhythms

### In the moment

Provide a universal quick-capture path for a question, idea, source, or observation. Default to an inbox state; classification can happen later.

### Daily

Surface at most:

- one thread worth resuming
- one small experiment or creative action
- one open loop that is blocking momentum

### Weekly

Review:

- questions explored
- ideas advanced
- experiments run
- artifacts created or shared
- learnings reused
- surprising cross-domain connections
- stale threads to pause deliberately

### Monthly

Evaluate whether the system increased output, learning, and creative range. Remove rituals that produce maintenance without momentum.

## Measures That Matter

Primary measures:

- artifacts completed or shared
- idea-to-artifact conversion rate
- experiments completed
- time from capture to first action
- learnings reused in a later context
- useful cross-domain connections accepted
- recall helpfulness and time-to-re-entry

Guardrail measures:

- maintenance time
- duplicate captures
- stale inbox age
- suggestions dismissed as irrelevant
- artifacts abandoned because the system added friction

Node count, note count, and captured-token count are diagnostics, not success measures.

## Current Slice

Run one four-week pilot in the `LifeGraph creative systems` domain:

1. Use the accepted minimal growth vocabulary behind proposed-write gates.
2. Capture with `life.capture` or the model-bypass prefixes `capture:`,
   `question:`, `idea:`, `source:`, `experiment:`, `artifact:`, and
   `learning:`.
3. Generate a daily bounded “resume, make, unblock” brief with
   `life.flywheel.brief`.
4. Select one idea each week and define its smallest experiment.
5. Produce at least one artifact.
6. Close each experiment with a learning and retrieval-usefulness signal.
7. Review the measures with `life.flywheel.review` and remove friction before
   expanding scope.

## Success Measures For The Pilot

- at least four ideas receive a concrete next action
- at least two experiments complete
- at least one artifact is created or shared
- at least one learning is reused in a later decision or creation
- median re-entry time decreases
- weekly maintenance stays below 20 minutes

## Intentionally Incomplete

- autonomous goal selection
- gamified productivity scoring
- broad life optimization from unreviewed inference
- expanding to every domain before one loop compounds

## Next Seam

Run the four-week `LifeGraph creative systems` pilot using the active daily
brief and weekly review. Expand only after the review shows artifacts,
learning reuse, and lower re-entry cost without excess maintenance. Treat
timezone-aware cron scheduling as a follow-on only if the UTC-backed cadence's
one-hour seasonal shift creates real friction.
