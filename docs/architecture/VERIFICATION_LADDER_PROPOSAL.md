---
domain: runtime-sessions
doc_type: proposal
disposition: accepted-current-slice
status: process-documentation
last_updated: 2026-03-31
---

# Verification Ladder Tracking Proposal

## Goal

Close the loop between code, tests, and architecture. Every seam completion requires verification evidence tracked in the graph. Architecture updates flow through the graph, not around it.

Current reality: the graph already stores verification evidence nodes such as `TestRun`, `SmokeRun`, and `UatRun`. The empty ladder problem is therefore a presentation/state-assembly gap, not a lack of evidence.

---

## The Verification Ladder

```
PROPOSED → CODE-COMPLETE → TEST-GREEN → SMOKE-GREEN → UAT-GREEN → IMPLEMENTED
    │            │              │              │            │
    │            │              │              │            └─ Final: Architecture doc updated
    │            │              │              │               Graph shows full traceability
    │            │              │              │
    │            │              │              └─ Live validation passes
    │            │              │                 Integration verified
    │            │              │
    │            │              └─ Unit tests pass
    │            │                 Coverage thresholds met
    │            │
    │            └─ Slice landed
    │               All code committed
    │
    └─ Design approved
       Active seams defined
```

### Verification Levels Defined

| Level | Evidence Required | Who Validates | Graph State |
|-------|-------------------|---------------|-------------|
| **proposed** | Design doc, active seams | Proposal author | `status: proposed` |
| **code-complete** | Committed slice, PR merged | Agent + human review | `status: code-complete`, `slice_committed: <sha>` |
| **test-green** | `cargo test` passes, coverage ≥ threshold | CI + agent | `verification: test-green`, `test_count: N`, `coverage_pct: XX` |
| **smoke-green** | Smoke script passes | Agent + human | `verification: smoke-green`, `smoke_log: <path>` |
| **uat-green** | Live validation in target environment | Human operator | `verification: uat-green`, `uat_evidence: <link>` |
| **implemented** | Full proposal delivered | Architecture review | `status: implemented`, `verified_by: <agent>` |

---

## Graph Schema Extensions

### New Node Kinds

| Kind | Purpose | Properties |
|------|---------|------------|
| `test_run` | A test execution event | `test_count`, `pass_count`, `fail_count`, `coverage_pct`, `duration_ms`, `commit_sha` |
| `smoke_run` | A smoke test execution | `script_path`, `exit_code`, `log_path`, `duration_ms`, `environment` |
| `uat_run` | User acceptance validation | `environment`, `operator`, `evidence_link`, `notes` |
| `verification_ladder` | Ladder state per seam/proposal | `current_level`, `history[]`, `blockers[]` |

### New Edge Relations

| Relation | From → To | Purpose |
|----------|-----------|---------|
| `tested_by` | `seam` → `test_run` | Link seam to its test evidence |
| `smoked_by` | `seam` → `smoke_run` | Link seam to smoke validation |
| `uat_by` | `seam` → `uat_run` | Link seam to acceptance validation |
| `covers` | `test` → `function`/`type` | Which code a test covers |
| `validates` | `test_run` → `seam` | Inverse: this run validates this seam |
| `blocks` | `seam` → `seam` | Cannot advance until dependency seam green |
| `requires_verification` | `task` → `seam` | Task requires seam to reach specific level |

---

## Architecture Update Process

### The Rule

> **Every architecture change MUST flow through the graph.**

No direct file edits for:
- `status` updates
- `active_seams` modifications
- `last_updated` timestamps
- Verification ladder state

### The Flow

```
1. AGENT detects state change
        │
        ▼
2. AGENT calls graph_update_node()
   - Update status: code-complete
   - Add evidence: commit_sha
   - Record reason: "Slice landed"
        │
        ▼
3. GRAPH broadcasts change
   - WebSocket → UI live update
   - Mutation log → Audit trail
        │
        ▼
4. OPTIONAL: graph_writeback()
   - Serialize to markdown
   - Auto-commit with provenance
        │
        ▼
5. HUMAN reviews in UI
   - See verification ladder progress
   - See linked commits, tests, evidence
   - Approve advancement to next level
        │
        ▼
6. HUMAN calls graph_update_node()
   - Advance status: test-green
   - Record verified_by
        │
        ▼
7. GRAPH reflects truth
   - All views consistent
   - No stale markdown
   - Full traceability
```

---

## Verification Tracking per Seam

### Seam Node Properties

```json
{
  "seam_id": "telegram-poll-lease",
  "domain": "membrane-transport",
  "verification_ladder": {
    "current": "smoke-green",
    "history": [
      {"level": "proposed", "date": "2026-03-01", "by": "AGENT"},
      {"level": "code-complete", "date": "2026-03-05", "by": "codex", "evidence": "commit:abc123"},
      {"level": "test-green", "date": "2026-03-08", "by": "CI", "evidence": "test_run:tr-456"},
      {"level": "smoke-green", "date": "2026-03-12", "by": "jane", "evidence": "smoke_run:sr-789"}
    ],
    "target": "uat-green",
    "blockers": ["delegated-telegram-polling"]
  },
  "test_summary": {
    "unit_tests": 47,
    "coverage": 89.3,
    "last_test_run": "2026-03-12T10:30:00Z"
  }
}
```

### Task-to-Seam Linkage

Tasks in `docs/task.md` carry `requires_seam` and `required_verification`:

```yaml
---
title: "Implement graceful poll release"
status: in_progress
seam_ref: telegram-poll-lease
requires_verification: smoke-green  # Cannot complete until seam at this level
---
```

When task is marked complete:
1. Graph validates seam has reached `requires_verification` level
2. If not, task completion is blocked with clear message
3. Agent must advance seam verification first

---

## Impact Metrics (Tracked in Graph)

### Delivery Speed

| Metric | Source | Target |
|--------|--------|--------|
| Avg seam cycle time | `history[].date` deltas | < 5 days |
| Time in test-red | `verification_ladder` stalls | < 1 day |
| PR-to-merge time | `code-complete` → `test-green` | < 4 hours |

### Quality

| Metric | Source | Target |
|--------|--------|--------|
| Test coverage per seam | `test_summary.coverage` | > 85% |
| Smoke pass rate | `smoke_run` success ratio | > 95% |
| UAT first-pass rate | `uat_run` without rework | > 80% |
| Architecture drift | `status` mismatches | 0 |

### Quota Efficiency

| Metric | Source | Target |
|--------|--------|--------|
| Failed test runs before green | `test_run` retry count | < 2 |
| Smoke loops before pass | `smoke_run` retry count | < 2 |
| UAT cycles before accept | `uat_run` count | < 1.5 avg |
| Rework rate post-UAT | `verification: uat-green` → back | < 10% |

---

## MCP Tools for Verification

### Query

```
graph_verification_status    → Get ladder state for seam/proposal
graph_seam_blockers          → What's blocking advancement?
graph_test_coverage          → Coverage report per seam/crate
graph_impact_metrics         → Speed, quality, quota metrics
graph_stalled_seams          → Seams stuck > threshold
```

### Mutation

```
graph_record_test_run        → Log test execution results
graph_record_smoke_run       → Log smoke validation
graph_record_uat             → Log acceptance validation
graph_advance_verification   → Move seam to next level (with evidence)
graph_block_seam             → Mark blocker, prevent advancement
graph_unblock_seam           → Clear blocker
```

---

## Web UI Enhancements

### Seam Detail Page Additions

```
┌─────────────────────────────────────────┐
│  SEAM: telegram-poll-lease              │
├─────────────────────────────────────────┤
│                                         │
│  VERIFICATION LADDER                    │
│  ━━━━━━━━━━━━━━━━━━━━                   │
│                                         │
│  ☐ PROPOSED        ✓ 2026-03-01        │
│  ☐ CODE-COMPLETE   ✓ 2026-03-05        │
│  ☐ TEST-GREEN      ✓ 2026-03-08        │
│  ● SMOKE-GREEN     ✓ 2026-03-12  ←NOW   │
│  ○ UAT-GREEN       ○                   │
│  ○ IMPLEMENTED     ○                   │
│                                         │
│  [Advance to UAT-GREEN]  [Add Blocker] │
│                                         │
├─────────────────────────────────────────┤
│  EVIDENCE                               │
│  • Tests: 47 passed, 89.3% coverage    │
│  • Smoke: dual-poller handoff green    │
│  • Commit: abc123 (jane)               │
│                                         │
├─────────────────────────────────────────┤
│  BLOCKERS                               │
│  • delegated-telegram-polling (seam)   │
│                                         │
├─────────────────────────────────────────┤
│  METRICS                                │
│  Cycle time: 11 days | Tests: 2 retries │
│  Status: On track                       │
│                                         │
└─────────────────────────────────────────┘
```

### Dashboard Additions

- **Stalled Seams**: Table of seams stuck > 5 days at any level
- **Quality Radar**: Coverage, pass rates, rework %
- **Quota Impact**: Failed runs, retry counts per agent
- **Ready for UAT**: Seams at smoke-green awaiting human validation

---

## Implementation Phases

### Phase 1: Schema (This Slice)

- [ ] Extend `DocFrontmatter` with `verification_ladder` fields
- [ ] Add `test_run`, `smoke_run`, `uat_run` node kinds to scanner
- [ ] Create `verification_ladder` edge relations
- [ ] Update SEAM_REGISTRY.md format with ladder column

### Phase 2: MCP Tools (Next)

- [ ] Implement `graph_record_test_run`
- [ ] Implement `graph_advance_verification`
- [ ] Implement `graph_seam_blockers`
- [ ] Implement `graph_impact_metrics`

### Phase 3: UI (Future)

- [ ] Verification ladder widget on seam detail
- [ ] Evidence links (test logs, smoke output)
- [ ] Blocker management interface
- [ ] Metrics dashboard cards

### Phase 4: Process (Ongoing)

- [ ] Agent workflow: update graph before claiming complete
- [ ] Human review: verify in UI before advancing
- [ ] CI integration: auto-record test runs via MCP
- [ ] Blocker culture: no seam advance with unresolved blockers

---

## Success Criteria

- [ ] Every seam has verification ladder state in graph
- [ ] No architecture status updates without graph mutation
- [ ] Task completion checks seam verification level
- [ ] Metrics visible: speed, quality, quota impact
- [ ] UI shows real-time verification progress
- [ ] Full traceability: code → test → smoke → uat → architecture

---

## Disposition

`proposed` → seeking acceptance for current slice implementation.

Last updated: 2026-03-29
