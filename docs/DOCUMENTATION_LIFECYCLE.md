# Documentation Lifecycle Process

## Problem Statement

The repo has 81 documents in `docs/architecture/` with varying states:
- Proposals that should become architecture
- Outdated docs that need archiving
- Active work that needs tracking
- Missing cross-links between related concepts

## Lifecycle States

```
PROPOSED → ACCEPTED → IMPLEMENTED → ARCHITECTURE
    ↓           ↓            ↓
DEFERRED   SUPERCEDED    ARCHIVED
```

## State Definitions

| State | Meaning | Visibility |
|-------|---------|------------|
| `proposed` | Under discussion | Context search: yes |
| `accepted-current-slice` | Approved for implementation | Context search: yes |
| `accepted` | Approved, not started | Context search: yes |
| `in-progress` | Being implemented | Context search: yes |
| `implemented` | Code complete, not verified | Context search: yes |
| `verified` | Watched-live confirmed | Context search: yes |
| `architecture` | Moved to ARCHITECTURE.md or domain doc | Context search: yes |
| `superseded` | Replaced by newer proposal | Context search: no (unless asked) |
| `deferred` | Paused indefinitely | Context search: no (unless asked) |
| `archived` | Outdated, kept for history | Context search: no (unless asked) |

## Archive Process

### When to Archive

A doc should be archived when:
1. It's been `superseded` by a newer proposal (6+ months old)
2. It's `deferred` for > 6 months with no activity
3. It's been fully absorbed into `ARCHITECTURE.md`
4. It references deprecated systems (zeroclaw, etc.)

### Archive Steps

1. **Update frontmatter:**
   ```yaml
   status: archived
   disposition: archived
   archived_at: 2026-03-29
   archived_reason: "Superseded by NEW_PROPOSAL"
   ```

2. **Move file:**
   ```
   docs/architecture/OLD_PROPOSAL.md
   → docs/architecture/archive/OLD_PROPOSAL.md
   ```

3. **Create tombstone:**
   ```markdown
   # OLD_PROPOSAL (ARCHIVED)

   **Status:** Archived as of 2026-03-29
   **Reason:** Superseded by [NEW_PROPOSAL](NEW_PROPOSAL.md)

   See archive/OLD_PROPOSAL.md for historical reference.
   ```

4. **Update graph:**
   - Mark node `status: archived`
   - Add `superseded_by` edge to replacement

## Proposal → Architecture Promotion

### When to Promote

Promote when:
1. Proposal is `verified` (watched-live-green)
2. The pattern has been proven in 2+ implementations
3. No active dissent or open questions

### Promotion Steps

1. **Extract core concepts** into `ARCHITECTURE.md` relevant section
2. **Create domain doc** if new domain (e.g., `docs/architecture/RUNTIME_SESSIONS.md`)
3. **Update proposal:**
   ```yaml
   status: architecture
   disposition: architecture
   architecture_ref: "ARCHITECTURE.md#section"
   ```
4. **Cross-link:** Add bidirectional links
5. **Update graph:** Mark node as `architecture` state

## PlantUML Generation Process

### Where Diagrams Live

| Diagram Type | Location | Generated From |
|--------------|----------|----------------|
| System context | `docs/architecture/diagrams/context/` | Proposal `### Architecture` sections |
| Component | `docs/architecture/diagrams/component/` | Proposal `### Components` |
| Sequence | `docs/architecture/diagrams/sequence/` | Proposal flow descriptions |
| C4 Level 1-4 | Embedded in docs | `graph_skeleton` API + templates |

### Generation Flow

```
Proposal (markdown)
    ↓
Parse sections for diagram hints
    ↓
Query graph for node relationships
    ↓
Generate PlantUML
    ↓
Save to docs/architecture/diagrams/
    ↓
Embed in proposal with !include
```

### Auto-Generation Triggers

- New `seam` node created → Generate context diagram
- New `proposal` with `domain` → Generate domain overview
- `implements` edges added → Generate component diagram

## Next Doc Set to Process

### Tier 1: Active Proposals Needing Architecture

These are `accepted-current-slice` or `implemented` and should drive architecture docs:

1. **AGENT_WORKFLOW_PROPOSAL** → Extract into `AGENTS.md` + process section
2. **AGENT_WORKSTREAM_TRACKING_PROPOSAL** → Extract into operator control plane architecture
3. **MEMBRANE_COMPONENT_PROPOSAL** → Drive `MEMBRANE.md` architecture
4. **MUNINN_MEMORY_PROTOCOL_PROPOSAL** → Drive `MEMORY.md` architecture

### Tier 2: Superseded/Outdated (Archive Candidates)

1. **ZEROCALW_TO_PHILOTIC_BRIDGE_PROPOSAL** → `archive/` (migration complete)
2. **PORT_BLUEPRINT.md** → `archive/` (historical, superceded)
3. **PHILOTIC_AGENT_LOOP_SPEC.md** → Merge into `RUNTIME_SESSIONS.md`
4. **SANDBOX_ARCHITECTURE.md** → Merge into `OPERATOR_CONTROL_PLANE.md`

### Tier 3: Need PlantUML

1. **RUNTIME_AUTHORITY_LEASES_PROPOSAL** → Sequence diagram for lease lifecycle
2. **TELEGRAM_POLL_LEASE_PROPOSAL** → Component diagram for poll architecture
3. **AGENT_LOOP_PROPOSAL** → C4 diagrams for session loop

## Implementation Plan

### Phase 1: Archive Cleanup (1 session)

1. Create `docs/architecture/archive/` directory
2. Identify 10-15 clear archive candidates
3. Move and tombstone them
4. Update graph nodes to `archived`

### Phase 2: Architecture Extraction (2-3 sessions)

1. Create domain architecture docs:
   - `RUNTIME_SESSIONS.md`
   - `MEMBRANE_TRANSPORT.md`
   - `MEMORY_CONTEXT.md`
   - `OPERATOR_CONTROL_PLANE.md`
2. Extract patterns from Tier 1 proposals
3. Cross-link proposals to architecture

### Phase 3: PlantUML Generation (1-2 sessions)

1. Add diagram generation to `graph-intelligence` MCP tools
2. Generate diagrams for active proposals
3. Embed in docs
4. Create diagram index

### Phase 4: SVER Integration (ongoing)

1. Architecture docs must have `verified` status
2. Changes to architecture require proposal first
3. Workstreams track against architecture (not just proposals)

## Success Criteria

- [ ] Archive directory created with 10+ docs
- [ ] Domain architecture docs for 4+ domains
- [ ] All Tier 1 proposals linked to architecture
- [ ] PlantUML diagrams for 5+ key proposals
- [ ] Clear process documented in `AGENTS.md`

---

**Current Workstream:** `workstream:workstream-graph-audit-20260329`  
**Seams touched:** 2  
**Proposals touched:** 6  
**Next priority:** Archive cleanup → Architecture extraction
