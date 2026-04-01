name: proposal-pipeline
description: Manage proposal lifecycle from creation through disposition to implementation. Track disposition transitions, ensure metadata completeness, and identify stuck or orphaned proposals. Use when working with architecture proposals.

# Proposal Pipeline

Scope: proposal lifecycle, disposition management, metadata hygiene.

Proposals are the primary planning artifact in Philotic. A healthy pipeline means every proposal has a clear disposition, domain assignment, and path to either implementation or deferral.

## When to Run

- **Creating a new proposal**: Ensure it gets proper metadata from birth
- **Changing disposition**: Validate the transition and record a decision
- **Pipeline review**: Audit the full proposal set for health
- **Check-engine**: Report pipeline status as part of close-out

## Proposal Lifecycle

```
proposed → accepted for current slice → in-progress → implemented
                                    ↘ deferred
                                    ↘ superseded
```

### Disposition Values

| Disposition | Meaning |
|---|---|
| `proposed` | Idea captured, not yet accepted |
| `accepted for current slice` | Approved for active work |
| `in-progress` | Actively being implemented |
| `implemented` | Complete — code landed, verified |
| `deferred` | Intentionally postponed with rationale |
| `superseded` | Replaced by another proposal |

## Required Metadata

Every active proposal should have:

1. **`disposition`** — Current lifecycle state
2. **`domain`** — Primary scope domain (from controlled vocabulary)
3. **`status`** — Implementation status
4. **`verification_level`** — SVER state
5. **`tags`** — Retrieval aids
6. **`last_updated`** — When last touched

### Controlled Domain Vocabulary

- `runtime-sessions`
- `membrane-transport`
- `mesh-placement`
- `memory-context`
- `tooling-execution`
- `operator-control-plane`
- `deployment-distribution`
- `migration-parity`
- `workflow-docs`

## Health Check

```bash
# Full proposal health report
curl -s http://127.0.0.1:8900/api/health/proposals | jq .

# Quick count
curl -s http://127.0.0.1:8900/api/health/proposals | jq '{
  total: .total_proposals,
  missing_disposition: .missing_disposition,
  no_verification: .no_verification,
  no_embedding: .no_embedding
}'
```

### Health Criteria

The pipeline is **healthy** when:
- Every proposal has a disposition
- Fewer than half of proposals have `verification_level: none`
- All proposals have embeddings (for semantic search)

## Disposition Transitions

When changing a proposal's disposition:

1. **Record the decision** via `graph_decide`:
   ```
   graph_decide({
     target_node: "doc:my-proposal",
     action: "disposition_change",
     from_value: "proposed",
     to_value: "accepted for current slice",
     reason: "Approved in planning review — addresses critical seam",
     agent: "cascade"
   })
   ```

2. **Update the graph node** via `graph_mutate` or the REST API

3. **Export to docs** via `graph_export_docs` to sync frontmatter

## Creating a New Proposal

When creating a new proposal doc:

1. Create the markdown file in `docs/architecture/`
2. Add frontmatter with all required fields:
   ```yaml
   ---
   doc_type: proposal
   domain: <domain>
   status: proposed
   disposition: proposed
   last_updated: <date>
   tags: [<relevant>, <tags>]
   ---
   ```
3. Run `graph_scan` to ingest it into the graph
4. Record the creation decision via `graph_decide`

## Pipeline Audit

Run periodically or during retrospectives:

1. **Stuck proposals**: `disposition: proposed` for > 2 weeks
2. **Orphaned proposals**: No linked seams or tasks
3. **Missing metadata**: No domain, no disposition, no tags
4. **Verification debt**: `accepted for current slice` with `verification_level: none`
5. **Embedding gaps**: Proposals without embeddings can't be found via semantic search

## Integration with Graph Intelligence

The proposal health endpoint feeds into:
- `graph_next_task` scoring (proposals with accepted disposition score higher)
- `graph_digest` reporting (per-domain proposal chains)
- `graph_context_for` context assembly (verification + decision history)
- `graph_export_docs` frontmatter sync
