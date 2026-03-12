# Proposal Organization Proposal

## Goal

Define the first lightweight structure for organizing a growing proposal set in `docs/architecture/` without turning the repo into a wiki maze with better typography.

## Disposition

`proposed`

Track follow-on work in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md).

## Core Recommendation

Philotic should keep proposals as lightweight active documents, but it now has enough of them that simple filename sprawl is starting to become its own little distributed systems problem.

The first organization strategy should stay intentionally small:

1. keep proposals in `docs/architecture/` for now
2. group them conceptually by domain, not by deeply nested folders yet
3. add lightweight tags and explicit related-proposal backlinks
4. use disposition and current-slice sections consistently so proposal state is visible without archaeology

## Why This Is Better

This preserves the current low-friction authoring model while making it easier to answer:

- what proposal is active for this seam
- what adjacent proposals it depends on
- which proposals are implementation-driving versus future-facing
- where a newcomer should start without reading the whole cathedral

It also avoids the classic irony of solving “too many docs” by creating an even fancier directory tree that nobody can navigate after week two.

## First Organization Rule Set

Recommended first pass:

- filenames stay flat in `docs/architecture/`
- each proposal includes:
  - `Goal`
  - `Disposition`
  - `Current Slice`
  - task link
  - `Relationship To Other Proposals` when adjacent seams matter
- use lightweight conceptual tags in prose or frontmatter only if they help retrieval; do not build a taxonomy cult

Suggested first tag families:

- `control-plane`
- `runtime`
- `memory`
- `distribution`
- `operator-surface`
- `migration`
- `voice`

These are retrieval aids, not authorities.

## Folder Strategy

Do **not** move to deep folders yet.

Recommended trigger for folders:

- only introduce subfolders when the flat set becomes hard to scan even with tags, backlinks, and naming discipline
- if that threshold is crossed, prefer a small number of domain folders such as:
  - `runtime/`
  - `control-plane/`
  - `operator-surface/`
  - `migration/`

Do not organize by lifecycle folders like `proposed/`, `implemented/`, `deferred/`; disposition already carries that truth better than directory churn.

## Backlink Recommendation

Each proposal should explicitly link to:

- adjacent proposals
- the active work surface in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md)
- canonical implementation or spec references when useful

This should stay selective.

Backlinks are there to show the local neighborhood, not to create a giant bidirectional spiderweb.

## Current Slice

- pin the first organization strategy before proposal volume grows further
- prefer flat filenames plus domain naming, tags, and backlinks over new folders for now
- add a backlog item to revisit folders only if the flat architecture index becomes materially hard to use

## Relationship To Other Proposals

- [ROUTER_NATIVE_OBSERVABILITY_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ROUTER_NATIVE_OBSERVABILITY_PROPOSAL.md)
  - observability is a good example of a cross-cutting domain that benefits from tags and backlinks
- [OPENCLAW_PARITY_MIGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/OPENCLAW_PARITY_MIGRATION_PROPOSAL.md)
  - migration work will need consistent linkage across multiple domain proposals
- [CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md)
  - admin/operator surfaces will likely become one of the first dense proposal clusters
