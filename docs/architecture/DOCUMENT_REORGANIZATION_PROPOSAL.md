---
title: Architecture Documentation Reorganization Proposal
doc_type: proposal
domain: workflow-docs
status: proposed
last_updated: 2026-04-08
tags:
- docs
- metadata
- taxonomy
- frontmatter
related_docs:
- DOC_TAGGING_FRONTMATTER_PROPOSAL.md
- PROPOSAL_ORGANIZATION_PROPOSAL.md
- ARCHITECTURE_STATUS.md
task_refs:
- docs/task.md
proposal_id: document-reorganization
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
---

# Architecture Documentation Reorganization Proposal

## Disposition

`proposed`

## Current Slice

- normalize proposal and archive metadata so the graph scanner can classify docs reliably
- mark archive-only proposal narratives as historical instead of leaving them in proposal-shaped limbo
- keep the reorganization proposal itself explicit about what the repo is asking for right now

## Current State Analysis

### Document Inventory
- **~80 PROPOSAL.md files** (mix of proposed, accepted, implemented, deferred)
- **7 core reference docs** (GLOSSARY, ARCHITECTURE, DOMAIN_MAP, SEAM_REGISTRY, ROADMAP, ARCH_RULES, ARCHITECTURE_STATUS)
- **3 new GRAPH_* docs** (not yet fully integrated)
- **17 docs in archive/** (properly archived)

### Problems Identified

1. **SEAM_REGISTRY.md** - Hand-edited but should be generated from graph `seam` nodes
2. **ROADMAP.md** - Dependencies manually maintained; should be generated from graph edges
3. **ARCH_RULES.md** - Has 12 rules but 20+ accepted proposals exist; missing rules
4. **No clear taxonomy** - docs mixed together regardless of type
5. **GRAPH_* docs** - Not referenced in related_docs of older proposals
6. **Status inconsistency** - `accepted-current-slice` vs `accepted for current slice` vs `accepted — current slice`

---

## Proposed Document Taxonomy

### 1. Hand-Authored Source Documents

These are created and maintained by humans. The graph indexes them but does not generate them.

**Location**: `docs/architecture/` (root)

| Type | Pattern | Example | Purpose |
|------|---------|---------|---------|
| **Proposals** | `*PROPOSAL.md` | `SESSION_LOOP_PROPOSAL.md` | Intent/direction documents |
| **Reference** | `*_MAP.md`, `*_REGISTRY.md` | `DOMAIN_MAP.md` | Controlled vocabularies |
| **Process** | `AGENTS.md`, `README.md` | `AGENTS.md` | How we work |
| **Status** | `*_STATUS.md` | `ARCHITECTURE_STATUS.md` | Current implementation state |

### 2. Generated Aggregates (Graph → Markdown)

These are generated from graph state via `graph_writeback`. Humans do not edit directly.

**Location**: `docs/architecture/generated/`

| File | Source | Generation Trigger |
|------|--------|-------------------|
| `SEAM_REGISTRY.md` | All `seam` nodes + `applies_to` edges | On seam node creation/update |
| `ROADMAP.md` | Seams with `depends_on` edges | Manual or periodic regeneration |
| `ARCH_RULES.md` | Rules from `accepted`/`implemented` proposals | On proposal disposition change |
| `ARCHITECTURE_STATUS.md` | Aggregate proposal statuses | Periodic or on significant changes |

### 3. Archive

**Location**: `docs/architecture/archive/`

- Superseded proposals
- Historical reference (kept for audit trail)

---

## Normalization Actions

### 1. Standardize Status Values

Current inconsistency:
- `accepted-current-slice`
- `accepted for current slice`
- `accepted — current slice`

**Canonical**: `accepted-current-slice` (kebab-case, no spaces, no em-dash)

### 2. Extract Missing Rules to ARCH_RULES.md

Accepted proposals with no rules extracted yet:

| Proposal | Domain | Likely Rules |
|----------|--------|--------------|
| `GRAPH_INTELLIGENCE_PROPOSAL` | `workflow-docs` | Graph is canonical source of truth; agents mutate via MCP; writeback is optional |
| `DEV_ENGINE_OPTIMIZATION_PROPOSAL` | `workflow-docs` | (may be guidance-level only) |
| `DISTRIBUTED_CRON_PROPOSAL` | `deployment-distribution` | Cron jobs are hotel-scheduled, not agent-local |
| `HOMEBREW_DISTRIBUTION_PROPOSAL` | `deployment-distribution` | Release pipeline produces signed artifacts |
| `PLUGGABLE_CONTEXT_ENGINE_PROPOSAL` | `memory-context` | Context engines are runtime-pluggable |
| `PROPOSAL_ORGANIZATION_PROPOSAL` | `workflow-docs` | (process guidance) |
| ... | ... | Review all 20+ accepted proposals |

### 3. Add Graph Intelligence References

Proposals that should link to GRAPH_* docs in `related_docs`:
- All proposals with `active_seams` → `GRAPH_INTELLIGENCE_PROPOSAL.md`
- All proposals with `source_of_truth_targets` → `GRAPH_AS_SOURCE_OF_TRUTH.md`
- DOC_TAGGING_FRONTMATTER_PROPOSAL → `GRAPH_INTELLIGENCE_STATUS.md`

### 4. Create Generated/ Folder

```
docs/architecture/
├── generated/                    # Graph-generated, do not hand-edit
│   ├── SEAM_REGISTRY.md         # From seam nodes
│   ├── ROADMAP.md               # From seam dependencies
│   ├── ARCH_RULES.md            # From accepted proposal rules
│   └── ARCHITECTURE_STATUS.md   # Aggregate status
├── archive/                     # Historical
│   └── (existing 17 files)
├── (proposals - ~80 files)      # Hand-authored
├── (reference - DOMAIN_MAP, etc.) # Hand-authored
└── (process - AGENTS.md, etc.)  # Hand-authored
```

**Header for generated files**:
```yaml
---
title: "Seam Registry"
doc_type: reference
domain: workflow-docs
status: active
last_updated: 2026-03-29
generated_from: graph
generation_trigger: seam_node_updated
manual_edit: false
tags: [generated, seams, registry]
related_docs:
  - GRAPH_AS_SOURCE_OF_TRUTH.md
---

> **WARNING**: This file is AUTO-GENERATED from the graph intelligence database.
> Do not edit manually. Changes will be overwritten.
> To modify: update the source proposal's `active_seams` and call `graph_writeback`.
```

### 5. Frontmatter Field Cleanup

Ensure all active docs have consistent frontmatter:

**Required**:
- `title`
- `doc_type` (from canonical list)
- `domain` (from canonical list)
- `status` or `disposition`
- `last_updated`

**For Proposals**:
- `proposal_id` (kebab-case, matches filename)
- `active_seams` (array)
- `related_docs` (array)
- `source_of_truth_targets` (array)

**Optional**:
- `tags`
- `task_refs`
- `implements`
- `implemented_by`

---

## Implementation Plan

### Phase 1: Status Normalization (Immediate)
1. Update all `accepted for current slice` → `accepted-current-slice`
2. Update all `accepted — current slice` → `accepted-current-slice`
3. Ensure `GRAPH_INTELLIGENCE_PROPOSAL.md` stays `accepted-current-slice`

### Phase 2: Rule Extraction (Next)
1. Review each `accepted-current-slice` proposal for extractable rules
2. Add rules to `ARCH_RULES.md` with proper `rule_id`, `domain`, `source`, `level`
3. Mark proposals as `extracted-rules: true` in properties

### Phase 3: Graph Integration (Ongoing)
1. Add `GRAPH_*` docs to `related_docs` where relevant
2. Ensure all proposals with `active_seams` have proper edges in graph
3. Verify `seam` nodes exist for all listed `active_seams`

### Phase 4: Generation Setup (Future)
1. Create `docs/architecture/generated/` folder
2. Add `generated: true` property to frontmatter schema
3. Implement periodic writeback job for aggregate docs
4. Move `SEAM_REGISTRY.md`, `ROADMAP.md`, `ARCH_RULES.md` to `generated/`

---

## Graph Queries for Validation

```sql
-- Find proposals with inconsistent status values
SELECT id, properties->>'status' as status 
FROM nodes 
WHERE kind = 'proposal' 
  AND properties->>'status' LIKE '%accepted%' 
  AND properties->>'status' != 'accepted-current-slice';

-- Find seams not in SEAM_REGISTRY.md
SELECT id, name FROM nodes 
WHERE kind = 'seam' 
  AND id NOT IN (SELECT target_id FROM edges WHERE relation = 'applies_to');

-- Find proposals with active_seams but no applies_to edges
SELECT p.id, p.name 
FROM nodes p 
WHERE p.kind = 'proposal' 
  AND p.properties->'active_seams' IS NOT NULL 
  AND NOT EXISTS (
    SELECT 1 FROM edges e 
    WHERE e.source_id = p.id AND e.relation = 'applies_to'
  );

-- Find accepted proposals with no rules extracted
SELECT id, name FROM nodes 
WHERE kind = 'proposal' 
  AND properties->>'status' = 'accepted-current-slice'
  AND properties->>'rules_extracted' IS NULL;
```

---

## Success Criteria

- [ ] All status values normalized to kebab-case
- [ ] ARCH_RULES.md contains rules from all accepted proposals
- [ ] All proposals with `active_seams` have `applies_to` edges in graph
- [ ] GRAPH_* docs appear in `related_docs` of relevant proposals
- [ ] Clear distinction between hand-authored and generated docs
- [ ] Graph is source of truth for SEAM_REGISTRY, ROADMAP, ARCH_RULES

---

## Related Documents

- `GRAPH_AS_SOURCE_OF_TRUTH.md` - Architecture for graph-first approach
- `DOC_TAGGING_FRONTMATTER_PROPOSAL.md` - Frontmatter schema
- `ARCH_RULES_AND_ROADMAP_PROPOSAL.md` - Process for rules/roadmap
- `PROPOSAL_ORGANIZATION_PROPOSAL.md` - Proposal lifecycle

Last updated: 2026-03-29
