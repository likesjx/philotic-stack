# Graph Audit and Update Plan

## Current State

### Proposal Coverage
- **69 proposals** in graph database
- **~50 proposals** with `status: unknown` — need disposition updates
- **~50 proposals** with `disposition: none` — need status review

### Critical Gaps
1. **No seam nodes** for most active proposals
2. **No task nodes** linked to proposals
3. **No active workstreams** except current session
4. **Proposal→Seam→Task hierarchy** not established

## Systematic Audit Process

### Phase 1: Proposal Triage (Quick Wins)

For each proposal in the graph, determine:

| Check | Action | Priority |
|-------|--------|----------|
| Has frontmatter `disposition`? | Update graph node property | High |
| Has `active_seams` in frontmatter? | Create seam nodes if missing | High |
| Has `task_refs` in frontmatter? | Create task nodes if missing | Medium |
| Is `implemented` or `superseded`? | Archive or mark complete | Low |

### Phase 2: Seam Creation

For each **active** proposal (disposition = `accepted` | `in-progress` | `proposed`):

1. Read proposal frontmatter for `active_seams`
2. For each seam mentioned:
   - Check if seam node exists
   - If not, create seam node with `part_of` edge to proposal
3. Create edge: `seam --implements--> proposal`

### Phase 3: Task Harvesting

For each active proposal:

1. Scan markdown for:
   - `## Current Slice` section
   - `### Tasks` or `### Next Steps`
   - Checkbox items `[ ]` or `[x]`
2. Create task nodes for unchecked items
3. Link tasks to seams via `part_of` edges

### Phase 4: Workstream Activation

For proposals currently being worked:

1. Create workstream node (if agent session active)
2. Link workstream to seam and proposal
3. Ensure session is tracking properly

## Implementation Order

**Start with high-impact, low-effort:**

1. **EMBEDDINGS_IN_GRAPH_PROPOSAL** — already in-progress, has active session
2. **AGENT_WORKFLOW_PROPOSAL** — process backbone, just updated
3. **AGENT_WORKSTREAM_TRACKING_PROPOSAL** — current work, already in graph
4. **ARCHITECTURE_STATUS.md** — central status doc, should be node

Then batch process by domain:
- `runtime-sessions` proposals
- `operator-control-plane` proposals
- `membrane-transport` proposals
- etc.

## Tools Needed

1. `graph_audit_proposals` — MCP tool to scan all proposals and report gaps
2. `graph_sync_proposal` — Update graph node from frontmatter
3. `graph_create_seam_for_proposal` — Create seam from proposal.active_seams
4. `graph_harvest_tasks` — Extract tasks from proposal markdown

## Immediate Next Steps

**Option A: Manual Audit**
- Read each active proposal
- Extract seams/tasks manually
- Use existing MCP tools to create nodes

**Option B: Batch Tool**
- Build `graph_audit_proposals` tool
- Run once to generate report
- Review and approve bulk updates

**Option C: Hybrid**
- Start manual with top 5 proposals
- Learn patterns
- Build tool for remaining

## Success Criteria

- All active proposals have seam nodes
- All active proposals have disposition in graph
- Active work has workstream + session tracking
- Status Board shows meaningful data

---

**Recommendation:** Start with **Option C** — manually process top 5 proposals to establish patterns, then build automation.

**Time Estimate:** 2-3 hours for manual top 5, then 1 hour to build batch tool, then 30 min to process remaining 60+.
