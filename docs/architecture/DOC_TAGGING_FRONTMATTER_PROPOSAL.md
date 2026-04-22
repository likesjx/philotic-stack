---
title: Documentation Tagging And Frontmatter Proposal
doc_type: proposal
domain: workflow-docs
status: accepted-current-slice
last_updated: 2026-03-31
tags:
- docs
- frontmatter
- tags
- domains
- source-of-truth
related_docs:
- README.md
- ARCHITECTURE_STATUS.md
- PROPOSAL_ORGANIZATION_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: docs-tagging-frontmatter
implements:
- proposal-organization
implemented_by:
- docs-frontmatter-pilot
active_seams:
- architecture-doc-metadata-rollout
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Documentation Tagging And Frontmatter Proposal

## Goal

Define a lightweight metadata strategy for Philotic docs so proposals, seams, slices, task tracking, and current-state reference docs can link together cleanly without turning markdown into ceremony theater.

## Core Recommendation

Adopt a small, consistent YAML frontmatter block for active architecture and process docs.

The metadata should do four jobs only:

1. classify a document by `domain` and `doc_type`
2. make lifecycle state explicit
3. link current-state docs to near-future work and vice versa
4. support retrieval, indexing, and later tooling without forcing a complex taxonomy

Do **not** use frontmatter as a substitute for readable sections in the document body. Metadata should help us find the right doc and understand its status quickly; the body should still carry the actual thinking.

## Disposition

`accepted for current slice`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Why This Matters

Right now the repo has proposals, task tracking, and current-state docs, but the linking grammar between them is still mostly social knowledge plus filename recognition.

That causes three problems:

- cross-cutting seams are easy to miss
- current truth and near-future design are linked inconsistently
- docs become harder to index once volume grows

The irony is familiar: the repo is trying to become more legible while relying on increasingly implicit metadata.

## Metadata Design Rules

### Keep it small

Required frontmatter should fit on screen without scrolling.

### Prefer stable identifiers over prose tagging

Use constrained keys like `domain`, `doc_type`, `status`, and `proposals` rather than free-form adjective soup.

### Domains are first-class

Every active architecture/process doc should declare exactly one primary `domain`.

Cross-domain relevance belongs in `tags` or `related_docs`, not in multi-owner ambiguity.

### Link upward and sideways

Docs should point to:

- their parent planning surface
- adjacent proposals or status docs
- task surface when the doc drives implementation

### Current truth and future intent stay distinct

- current-state docs use metadata to link to active proposals and seams
- proposal docs use metadata to link to the status doc they may eventually change

## Canonical Domains

Use these as the controlled primary-domain vocabulary:

- `runtime-sessions`
- `membrane-transport`
- `mesh-placement`
- `memory-context`
- `tooling-execution`
- `operator-control-plane`
- `deployment-distribution`
- `migration-parity`
- `workflow-docs`

Notes:

- `workflow-docs` is for repo/process docs like agent workflow and documentation process, not product architecture.
- If a doc feels like it needs two primary domains, that is usually a seam warning worth naming explicitly.

## Canonical Doc Types

Use these as the controlled `doc_type` vocabulary:

- `status`
- `reference`
- `proposal`
- `seam`
- `task-surface`
- `workflow`
- `historical`

## Canonical Status Values

Use `status` by doc type:

- for `proposal` and `seam` docs:
  - `proposed`
  - `accepted-current-slice`
  - `implemented`
  - `superseded`
  - `deferred`
- for `status`, `reference`, and `workflow` docs:
  - `active`
  - `historical`

## Recommended Frontmatter Schema

### Common minimum block

Use this on all active docs in `docs/architecture/` except purely historical artifacts:

```yaml
---
title: "Human readable document title"
doc_type: proposal
domain: runtime-sessions
status: accepted-current-slice
last_updated: 2026-03-12
tags:
  - sessions
  - approvals
  - routing
related_docs:
  - ARCHITECTURE_STATUS.md
  - SESSION_LOOP_PROPOSAL.md
task_refs:
  - docs/task.md#wi-1-session-management
---
```

### Proposal-specific fields

Use these when the doc is a proposal:

```yaml
proposal_id: session-loop
implements: []
implemented_by: []
active_seams:
  - session-leases
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
```

Meaning:

- `proposal_id`: stable short ID for linking and later indexing
- `implements`: other proposal IDs this one depends on
- `implemented_by`: seam IDs or slice IDs once we start carrying them
- `active_seams`: current near-future boundaries driven by this proposal
- `source_of_truth_targets`: which truth docs must change when slices from this proposal land

### Seam-specific fields

When we introduce seam docs, use:

```yaml
seam_id: telegram-poll-lease
proposal_refs:
  - telegram-poll-lease
current_state_docs:
  - ARCHITECTURE_STATUS.md
slice_refs:
  - docs/task.md#telegram-poll-lease
verification_level: test-green
```

## Seam Doc Graduation Rule

Do not create seam docs by default.

Proposal docs, [SEAM_REGISTRY.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SEAM_REGISTRY.md), and [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md) should remain the normal home for most seams.

A seam should graduate into its own doc only when at least one of these is true:

- the seam spans multiple proposals and the boundary itself needs explanation
- the seam has a confusing current-state versus target-state story that keeps getting re-explained
- the seam needs its own verification contract or confidence language
- the seam is expected to absorb multiple implementation slices over time and its prose is getting duplicated across docs
- ownership, authority, or source-of-truth confusion keeps recurring at that seam

Length by itself is not the rule.

However, length can be a smell:

- if the seam explanation inside a proposal is getting hard to scan
- if task bullets keep needing prose paragraphs to stay intelligible
- if multiple docs are carrying near-duplicate seam explanations

then the seam is probably ready to graduate.

Default posture:

- seam docs are exception-based, not mandatory
- introduce them on pain, not on principle
- when in doubt, keep the seam in the proposal + registry + task surface until confusion proves otherwise

### Status-doc fields

For living truth docs like `ARCHITECTURE_STATUS.md`:

```yaml
doc_type: status
domain: runtime-sessions
status: active
tracks_domains:
  - runtime-sessions
  - membrane-transport
  - mesh-placement
```

This lets one status doc cover multiple domains without pretending it belongs equally to all of them as a planning unit.

## Tagging Strategy

Tags should stay lightweight and primarily help retrieval.

Use three tag families:

### Domain-adjacent tags

Examples:

- `sessions`
- `telegram`
- `routing`
- `egress`
- `deployment`

### Planning-shape tags

Examples:

- `active-seam`
- `transitional`
- `current-slice`
- `source-of-truth`
- `verification`

### Cross-cutting tags

Examples:

- `security`
- `approval`
- `observability`
- `operator-ux`
- `migration`

Rules:

- prefer 3-6 tags
- avoid synonyms like `ops` and `operations` both existing
- do not encode primary domain in tags if it already exists in `domain`

## Cross-Linking Rules

Every active proposal should link in both metadata and body to:

- one primary status doc
- one task surface
- adjacent proposals when relevant

Every status doc should link to:

- the hot proposals or seams it is summarizing
- the task surface that reflects immediate work

Every task surface should remain human-readable, but should eventually reference stable seam or proposal IDs in nearby prose.

Stable seam IDs live in [SEAM_REGISTRY.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/SEAM_REGISTRY.md). Proposal `active_seams` fields should reference IDs from that registry rather than inventing fresh synonyms.

## Recommended Rollout

First slice only:

1. define the controlled vocabularies
2. add frontmatter to the highest-value docs first:
   - [ARCHITECTURE_STATUS.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE_STATUS.md)
   - [ARCHITECTURE.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ARCHITECTURE.md)
   - active architecture proposals
3. add one seam template once seam docs become explicit
4. keep task linking lightweight until stable seam IDs exist

Do **not** try to retrofit every historical doc in one pass. That is how a metadata cleanup becomes archaeology with deadlines.

## Current Slice

- define the controlled domain, doc-type, status, and tag vocabularies
- define a minimal frontmatter schema that supports proposals, seams, and current-state docs
- keep the filesystem flat while letting metadata carry scope and linkage
- defer mass backfill until the schema proves useful on the active docs

## Relationship To Other Proposals

- [PROPOSAL_ORGANIZATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PROPOSAL_ORGANIZATION_PROPOSAL.md)
  - this proposal gives proposal organization a metadata contract
- [AGENT_WORKFLOW_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/AGENT_WORKFLOW_PROPOSAL.md)
  - the workflow proposal depends on crisp decision/status capture
- [PERIMETER_EGRESS_CONTROL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERIMETER_EGRESS_CONTROL_PROPOSAL.md)
  - cross-cutting proposals like egress are a good test for domain-plus-tags rather than folder sprawl
