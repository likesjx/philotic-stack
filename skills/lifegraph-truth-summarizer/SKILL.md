---
name: lifegraph-truth-summarizer
description: Use this skill when an agent summarizes the operator's LifeGraph, answers "what is in my LifeGraph?", or turns LifeGraph records into planning context. It requires provenance-aware summaries that separate confirmed graph facts from seeded placeholders, inferred intent, and recommended next structure.
catalog:
  skill_name: lifegraph.truth_summarizer
  implied_tools:
    - life.recall
    - graph.query
    - graph-datasource.query
  validation_state: proposed
  skill_markers:
    - governed
    - provenance_required
  field_sources:
    required_fields:
      - question
    optional_fields:
      - focus_label
      - focus_role
      - include_next_steps
    repo_skill_path: skills/lifegraph-truth-summarizer/SKILL.md
    workflow: "life.recall or graph query -> provenance audit -> truth-banded summary"
---

# LifeGraph Truth Summarizer

Use this skill when summarizing LifeGraph state for the operator or for another role.

## Purpose

LifeGraph summaries must be useful without pretending the graph is more complete than it is.

The agent's job is to translate graph state into a clear operating picture:

- what is confirmed in the graph
- what is seeded but incomplete
- what is inferred from schema, proposal, or conversation context
- what should be added or linked next

## Required Posture

Never collapse these categories:

- **Confirmed**: directly observed from current LifeGraph query results.
- **Seeded**: node or edge exists, but required human-readable or operational fields are missing.
- **Inferred**: plausible from labels, IDs, embeddings, schema, or prior conversation, but not directly stored as a graph fact.
- **Recommended**: suggested next structure or action, not current graph truth.

If you cannot query the graph, say so and label the answer as memory- or schema-derived.

## Workflow

1. Query the LifeGraph with the narrowest useful scope.
2. Count relevant labels and relationship types before making broad claims.
3. Sample key properties for the labels you mention, especially `id`, `name`, `status`, `claim_summary`, `source_membrane`, `provenance`, `confidence`, and `validation_state`.
4. Check representative relationships before claiming that roles, goals, systems, and habits are linked.
5. Draft the answer in truth bands:
   - Confirmed in the graph
   - Seeded but incomplete
   - Inferred or intended
   - Best next moves
6. When a claim matters, include a compact provenance phrase such as `from source_membrane=telegram`, `validation_state=proposed`, or `observed in live Memgraph`.

## Summary Template

```text
Confirmed in the graph:
- ...

Seeded but incomplete:
- ...

Inferred or intended:
- ...

Best next moves:
- ...
```

Omit empty sections. Keep the answer conversational unless the operator asks for an audit.

## Guardrails

- Do not describe smoke-test nodes as operator life goals.
- Do not infer `name` from an `id` without saying it is inferred.
- Do not call a role "fully online" unless it has enough properties and relationships to support that claim.
- Do not claim a habit, system, or goal exists unless the corresponding node exists.
- Do not claim relationships such as `SUPPORTS`, `TRIGGERS`, `HAS_GOAL`, or `RECURS` unless the edge exists.
- When the graph contains placeholder nodes, use "seeded" or "placeholder", not "active".
- Prefer a short, honest next action over a polished but ungrounded narrative.

## Minimum Verification For Current-State Claims

Before saying "right now" or "currently in your graph", verify at least one of:

- live `life.recall` result with source/provenance fields
- live graph-datasource/Memgraph query result
- a current graph context packet emitted by the runtime

Schema docs and proposals can explain what the LifeGraph is for, but they do not prove current contents.
