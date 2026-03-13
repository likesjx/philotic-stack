---
name: architecture-docs-maintainer
description: HIGH PRIORITY. Use this skill when changing Philotic architecture or workflow docs so domain, frontmatter, status truth, proposal links, and task links stay aligned automatically. Trigger for edits in docs/architecture/, docs/README.md, or docs/task.md when the work changes current truth, near-future seams, proposal status, or documentation structure.
---

# Architecture Docs Maintainer

Scope: Philotic-specific.

Use this skill when the work touches architecture or workflow documentation as a system, not just one paragraph in isolation.

Read these first:

- [docs/architecture/README.md](../../docs/architecture/README.md)
- [docs/architecture/ARCHITECTURE_STATUS.md](../../docs/architecture/ARCHITECTURE_STATUS.md)
- [docs/architecture/DOC_TAGGING_FRONTMATTER_PROPOSAL.md](../../docs/architecture/DOC_TAGGING_FRONTMATTER_PROPOSAL.md)

This skill owns:

- applying and maintaining the Philotic frontmatter schema
- assigning one primary `domain` and correct `doc_type`
- keeping `status` metadata aligned with body-level reality
- maintaining source-of-truth boundaries between status, reference, proposal, and task docs
- cross-linking current truth, active seams, related proposals, and task surfaces
- marking historical/non-authoritative architecture narratives clearly

This skill does not own:

- deep runtime investigation
- choosing validation depth
- end-of-slice commit/push workflow by itself

Use [$proposal-maintainer](../proposal-maintainer/SKILL.md) when the job is generic proposal tightening.
Use [$philotic-slice-closeout](../philotic-slice-closeout/SKILL.md) when the main job is closing a just-finished implementation slice.

## Trigger Conditions

Use this skill when any of the following are true:

- a doc in `docs/architecture/` is created or updated
- `docs/README.md` changes to reflect architecture entrypoints
- `docs/task.md` changes because architecture status or seams changed
- a proposal's `Disposition`, `Current Slice`, or active seam list changes
- current implementation truth changed and [ARCHITECTURE_STATUS.md](../../docs/architecture/ARCHITECTURE_STATUS.md) may need an update
- a doc needs frontmatter, domain assignment, or relationship links

## Workflow

1. Identify each touched doc's role:
   - `status`
   - `reference`
   - `proposal`
   - `workflow`
   - `historical`
2. Assign exactly one primary `domain` from the controlled vocabulary unless the doc is intentionally broad status/reference.
3. Ensure frontmatter exists and includes the minimum fields:
   - `title`
   - `doc_type`
   - `domain`
   - `status`
   - `last_updated`
   - `tags`
   - `related_docs`
   - `task_refs` when implementation work is relevant
4. Check that metadata and body agree:
   - proposal `status` matches the document's disposition language
   - status/reference docs describe current truth rather than aspiration
   - historical docs are labeled as such and not presented as active authority
5. Check the source-of-truth chain:
   - if current implemented behavior changed, update `ARCHITECTURE_STATUS.md`
   - if durable runtime structure changed, update `ARCHITECTURE.md`
   - if design direction or near-future work changed, update the relevant proposal
   - if execution sequencing changed, update `docs/task.md`
6. Add or refresh cross-links:
   - proposal -> status doc
   - proposal -> task surface
   - status doc -> active proposals/seams
   - docs index -> current entrypoints
7. Note stale prose or outdated references exposed by the metadata pass and either fix them or call them out explicitly.

## Controlled Vocabulary

Primary `domain` values:

- `runtime-sessions`
- `membrane-transport`
- `mesh-placement`
- `memory-context`
- `tooling-execution`
- `operator-control-plane`
- `deployment-distribution`
- `migration-parity`
- `workflow-docs`

Primary `doc_type` values:

- `status`
- `reference`
- `proposal`
- `seam`
- `task-surface`
- `workflow`
- `historical`

Primary `status` values:

- proposal or seam docs:
  - `proposed`
  - `accepted-current-slice`
  - `implemented`
  - `superseded`
  - `deferred`
- status, reference, workflow docs:
  - `active`
  - `historical`

## Non-Negotiable Rules

- One primary domain per active doc.
- Keep tags lightweight; do not build a taxonomy cult.
- `ARCHITECTURE_STATUS.md` is for current truth and active seams, not wishlist prose.
- `ARCHITECTURE.md` is for durable architecture reference, not live queue state.
- Proposal docs are design space unless clearly marked implemented.
- `docs/task.md` is the execution surface, not the architecture encyclopedia.

## Output Expectations

Report:

- which docs received metadata or linkage updates
- whether current truth, reference truth, and proposal truth still align
- any stale docs or historical narratives that remain risky
- the next doc seam worth tightening
