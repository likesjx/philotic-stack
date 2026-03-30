---
doc_type: architecture
domain: runtime-sessions
status: draft
last_updated: 2026-03-29
tags:
  - runtime
  - sessions
  - aiua
  - philote
  - leases
refs:
  - RUNTIME_AUTHORITY_LEASES_PROPOSAL
  - AGENT_WORKFLOW_PROPOSAL
  - AGENT_WORKSTREAM_TRACKING_PROPOSAL
---

# Runtime Sessions Architecture

## Overview

The Philotic runtime is organized around **sessions** — contexts for agent execution with explicit lifecycle, state ownership, and recovery boundaries.

```plantuml
@startuml
!theme plain
skinparam backgroundColor transparent

package "Runtime Sessions" {
  [aiua] as Hotel <<Container>>
  [philote] as Guest <<Container>>
  [membrane] as Transport <<Container>>
  [model-router] as Router <<Container>>
}

package "Authority & Leases" {
  [Lease Manager] as Leases
  [Poll Registry] as Polls
  [Session Store] as Sessions
}

Hotel --> Guest : materializes
Guest --> Router : routes cognition
Guest --> Transport : sends/receives
Transport --> Leases : acquires/releases
Leases --> Polls : manages
Leases --> Sessions : tracks
@enduml
```

## Core Concepts

### Session

A **session** represents a bounded period of agent activity with:
- Unique identifier (`session:YYYY-MM-DD-{agent}`)
- Associated `seam` (where work happens)
- Associated `proposal` (what work achieves)
- Activity log (file edits, tests, verification)
- Verification level (`test-green`, `smoke-green`, `watched-live-green`)

### Lease

A **lease** is time-bounded authority for:
- Poll registration (e.g., Telegram bot polling)
- Resource access (e.g., model routing priority)
- State ownership (e.g., session checkpoint)

Leases prevent split-brain and enable graceful handoff.

### State Ownership Boundaries

| State Type | Canonical Owner | Derivation |
|------------|-----------------|------------|
| Session context | Graph (aiua) | Recovery checkpoints |
| Live routing | Hotel runtime | Materialized guests |
| Working state | Philote | Turn-local only |

## Session Lifecycle

```plantuml
@startuml
!theme plain
skinparam backgroundColor transparent

[*] --> Started : session_start
Started --> Coding : activity
Coding --> Testing : test_run
Testing --> Green : verified
Green --> Closed : session_close
Green --> Coding : regression
Started --> Closed : abort
@enduml
```

## Verification Ladder

Every session must progress through verification:

1. **test-green** — Crate tests pass
2. **smoke-green** — Binary smoke tests pass
3. **watched-live-green** — Live run observed

No runtime change is "done" until watched-live confirmed.

## Implementation

### Key Crates

- `aiua/` — Hotel daemon, session authority
- `philote/` — Guest runtime, turn execution
- `membrane/` — Transport layer, lease framing
- `model-router/` — Cognitive routing

### Critical Files

- `aiua/src/session.rs` — Session lifecycle
- `aiua/src/lease.rs` — Lease management
- `membrane/src/framing.rs` — IPC framing

## Active Seams

- `seam:session-leases` — Lease protocol implementation
- `seam:runtime-authority-leases` — Authority delegation
- `seam:handoff-skill` — Graceful handoff

## Related Proposals

- [RUNTIME_AUTHORITY_LEASES_PROPOSAL](../architecture/RUNTIME_AUTHORITY_LEASES_PROPOSAL.md) — Lease design
- [AGENT_WORKFLOW_PROPOSAL](../architecture/AGENT_WORKFLOW_PROPOSAL.md) — Session workflow
- [AGENT_WORKSTREAM_TRACKING_PROPOSAL](../architecture/AGENT_WORKSTREAM_TRACKING_PROPOSAL.md) — Tracking integration

---

**Status:** Draft — extracting from implemented patterns  
**Next:** Add sequence diagrams for lease acquisition/handoff
