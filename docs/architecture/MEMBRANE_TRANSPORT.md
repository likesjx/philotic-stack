---
doc_type: architecture
domain: membrane-transport
domain: membrane-transport
status: draft
last_updated: 2026-03-29
tags:
  - membrane
  - transport
  - ipc
  - framing
  - telegram
refs:
  - MEMBRANE_COMPONENT_PROPOSAL
  - MEMBRANE_EXTERNAL_AGENT_AND_EVENT_TRANSPORT_PROPOSAL
  - TELEGRAM_POLL_LEASE_PROPOSAL
  - TELEGRAM_INTEGRATION_PROPOSAL
---

# Membrane Transport Architecture

## Overview

**Membrane** is the IPC and event transport layer for Philotic. It handles:
- Framed message passing between hotel and guests
- External transport (Telegram, WebSocket, etc.)
- Event routing and subscription
- Lease-coordinated poll management

```plantuml
@startuml
!theme plain
skinparam backgroundColor transparent

package "Membrane Transport" {
  [Framing Layer] as Framing
  [Router] as Router
  [Event Bus] as Events
  [External Adapter] as External
}

package "Transports" {
  [Telegram] as Telegram
  [WebSocket] as WS
  [Unix Socket] as Unix
}

Framing --> Router : decoded messages
Router --> Events : publish/subscribe
External --> Telegram : bot API
External --> WS : browser clients
External --> Unix : local guests
@enduml
```

## Core Concepts

### Framing

All IPC uses **explicit framing** with:
- Length-prefixed messages
- Correlation IDs for request/response matching
- Lease context propagation

```rust
// Pseudo-code
struct Frame {
    correlation_id: Uuid,
    lease_id: Option<String>,
    payload: Bytes,
}
```

### Event Bus

Publish/subscribe system for:
- Session state changes
- Lease acquisitions/releases
- External message arrival
- System health events

### External Adapters

Pluggable adapters for external systems:
- **Telegram**: Bot API with poll lease coordination
- **WebSocket**: Browser UI real-time updates
- **Unix**: Local guest process communication

## Telegram Integration

```plantuml
@startuml
!theme plain
skinparam backgroundColor transparent

actor User
participant "Telegram" as TG
participant "Membrane" as M
participant "aiua" as Hotel
participant "philote" as Guest

User -> TG : sends message
TG -> M : webhook/poll
M -> Hotel : route to session
Hotel -> Guest : materialize turn
Guest -> Hotel : response
Hotel -> M : send reply
M -> TG : bot API
TG -> User : receives reply
@enduml
```

### Poll Lease Protocol

Critical for avoiding double-processing:

1. Guest acquires poll lease
2. Lease has TTL (e.g., 30s)
3. Guest must heartbeat or release
4. If lease expires, another guest can take over

```plantuml
@startuml
!theme plain
skinparam backgroundColor transparent

Guest1 -> LeaseMgr : acquire_poll_lease()
LeaseMgr -> Guest1 : granted (TTL=30s)

loop heartbeat every 10s
  Guest1 -> LeaseMgr : renew_lease()
end

Guest1 -> LeaseMgr : release_lease()
note right: Graceful shutdown

Guest2 -> LeaseMgr : acquire_poll_lease()
LeaseMgr -> Guest2 : granted
@enduml
```

## Implementation

### Key Crates

- `membrane/` — Transport layer
- `membrane/src/framing.rs` — Frame codec
- `membrane/src/telegram.rs` — Telegram adapter
- `membrane/src/event_bus.rs` — Event routing

### Critical Files

- `membrane/src/lease_aware_poll.rs` — Poll lease coordination
- `membrane/src/external_adapter.rs` — Adapter trait
- `membrane/src/router.rs` — Message routing

## Active Seams

- `seam:telegram-poll-lease` — Poll lease implementation
- `seam:active-membrane-routing` — Live routing
- `seam:handoff-skill` — Graceful guest handoff

## Related Proposals

- [MEMBRANE_COMPONENT_PROPOSAL](../architecture/MEMBRANE_COMPONENT_PROPOSAL.md)
- [TELEGRAM_POLL_LEASE_PROPOSAL](../architecture/TELEGRAM_POLL_LEASE_PROPOSAL.md)
- [TELEGRAM_INTEGRATION_PROPOSAL](../architecture/TELEGRAM_INTEGRATION_PROPOSAL.md)

---

**Status:** Draft — extracting from implemented patterns  
**Next:** Add detailed framing protocol spec
