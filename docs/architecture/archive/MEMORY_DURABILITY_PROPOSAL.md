---
title: Memory Durability Proposal
doc_type: proposal
domain: memory-context
status: proposed
last_updated: 2026-03-31
tags:
- memory
- durability
- replication
- mesh
- muninn
- write-ahead-log
related_docs:
- PHILOTE_MEMORY_CORE_PROPOSAL.md
- ARCHITECTURE.md
- MULTI_HOTEL_COMPONENT_DISTRIBUTION_PROPOSAL.md
proposal_id: memory-durability
implements:
- philote-memory-core
---

# Memory Durability Proposal

## Design Intent

Memory is a shared cognitive resource, not per-hotel state. The same agent identity —
its preferences, decisions, learned context, and continuity with the user — must survive
the loss of any single hotel node. This proposal defines how Philotic Stack protects
memory against catastrophic loss while keeping the architecture simple and consistent
with the existing mesh infrastructure.

The primary threat model is **total host loss**: the hotel and its local MuninnDB
instance gone completely. Gradual decay is a secondary concern (and partially a
feature — ACT-R decay is intentional). The goal is that a peer hotel can fully
reconstitute the memory of a lost agent without manual intervention.

---

## Four Protection Layers

### L0 — Decay Resistance (Stability Pinning)

Not all memories should be equally mortal. The Attend step (post-turn
`CognitiveOutcome` extraction) sets `stability` proportional to the durability of
what was learned:

| Outcome type | Stability target |
|---|---|
| Resolved contradiction | HIGH (near-permanent) |
| Solidified belief / decision | HIGH |
| Metacognitive observation | MEDIUM |
| Rejected approach | MEDIUM |
| Working scratch / session notes | LOW (default) |

This is cheap and runs entirely inside MuninnDB. It won't survive a total loss but
prevents important memories from eroding during long idle periods.

### L1 — Write-Ahead Replication over the Mesh (Primary Durability)

Every write operation emitted by `memory-core` is also recorded as a `MemoryEvent`
in the hotel's `mesh_events` ledger. Peer hotels consume these events via the
existing `CursorStorage` cursor/ACK mechanism and replay them into their own local
MuninnDB instances.

**Replication is async.** Writes succeed locally immediately; mesh delivery is
best-effort with cursor-based replay for catch-up. A hotel that was offline replays
all missed `MemoryEvent`s on reconnection.

**A new hotel bootstraps by replaying from cursor 0.** Full reconstitution from the
mesh ledger requires no external restore procedure.

#### MemoryEvent Envelope

Mirrors the write side of `MemoryEngine` exactly. Read operations (activate, read,
traverse) do not emit events. Lens state is session-local and does not replicate.

```rust
pub enum MemoryOp {
    Remember {
        vault:   VaultId,
        concept: String,
        content: String,
        tags:    Vec<String>,
        id:      EngramId,   // assigned locally; peers upsert by id
    },
    Forget {
        id: EngramId,
    },
    Evolve {
        old_id:  EngramId,
        new_id:  EngramId,
        content: String,
        tags:    Vec<String>,
    },
    Link {
        from_id:  EngramId,
        to_id:    EngramId,
        relation: String,
    },
}

pub struct MemoryEvent {
    pub op:         MemoryOp,
    pub agent_id:   AgentId,
    pub hotel_id:   String,
    pub sequence:   u64,
    pub occurred_at: i64,  // unix ms
}
```

`Forget` replicates. A user asking to delete a memory expects it gone everywhere.

#### Integration Point

`MuninnRestEngine::remember` (and siblings) call through to a
`MemoryEventEmitter` trait that the hotel daemon satisfies by appending to
`mesh_events`. In single-agent-per-hotel deployments this is a direct call;
multi-tenant deployments fan out per-agent.

### L2 — Git-Tracked Vault Export (Single-Node + Audit Trail)

For single-hotel deployments (homebrew user, one MacBook) there are no peers to
replicate to. The fallback is a periodic vault export:

- Hotel daemon runs a scheduled export job (configurable interval, default: daily)
- Exports each vault to a JSON snapshot: `~/.aiua/memory-exports/{vault}/{date}.json`
- Export directory is a git repository; the daemon commits each snapshot automatically
- Restore is `remember_batch` over the snapshot file — same path as mesh replay

This gives the single-node operator:
- **Point-in-time restore**: check out any past commit and replay
- **Human-readable audit**: `git diff` to see what the agent learned over a period
- **Off-machine backup**: push the export repo to any remote

The export format is a simple JSON array of `MemoryEvent` records — the same envelope
as L1, so the replay path is identical.

### L3 — SQLite WAL Backup (Corruption Protection)

Protects against SQLite-level corruption (power loss during write, disk error). The
hotel daemon checkpoints the MuninnDB WAL and copies the SQLite file to a rolling
backup location at the same interval as the L2 export.

This is the lowest-effort layer: a single `sqlite3 source.db ".backup dest.db"` call.
It does not survive total host loss but protects against the most common data
corruption scenarios.

---

## What This Is Not

- **Synchronous replication.** There is no two-phase commit. The risk window between
  a local write and mesh delivery is accepted.
- **MuninnDB-native replication.** We do not modify or depend on MuninnDB internals
  for this. The ledger approach works with any MuninnDB version.
- **Real-time backup.** L3 is periodic, not continuous. For continuous SQLite
  backup (e.g. Litestream), that can be layered on top by operators — out of scope
  for this proposal.

---

## Vault Bootstrap Protocol

For L1 replication to work cleanly, every hotel must hold a local MuninnDB token for
every vault it will receive `MemoryEvent`s for. This is solved by vault bootstrap —
not by replicating secret material across hotels, which `KEY_VAULT_PROPOSAL.md`
explicitly prohibits.

### How it works

Each hotel runs its own local MuninnDB instance. When a vault is created anywhere in
the mesh, a `VaultAdvertisement` is broadcast — metadata only, no credentials. The
receiving hotel creates a matching vault in its own local MuninnDB, receives its own
local token, and maps it. `MemoryEvent` replay for that vault then works immediately
with the correct token. No fallback loop, no cross-hotel secret transfer.

```
Hotel A: vault self_philote-1 created
  → token mk_abc... stored in Hotel A key vault (local only)
  → VaultAdvertisement broadcast over mesh:
      { vault: "self_philote-1", agent_id: "philote-1", owning_hotel: "hotel-a" }

Hotel B receives VaultAdvertisement
  → creates self_philote-1 in its own local MuninnDB
  → receives its own token mk_xyz... (completely different from Hotel A's)
  → stores mk_xyz... in its own local key vault
  → maps self_philote-1 → mk_xyz... in its MuninnRestEngine config

MemoryEvents for self_philote-1 arrive on Hotel B
  → token already known, direct write, no fallback
```

### VaultAdvertisement message shape

```rust
pub struct VaultAdvertisement {
    pub vault_name:    VaultId,       // e.g. "self_philote-1"
    pub agent_id:      Option<AgentId>,
    pub user_id:       Option<UserId>,
    pub owning_hotel:  String,
    pub advertised_at: i64,           // unix ms
}
```

Carries only what the receiving hotel needs to create the local vault. No tokens,
no ciphertext, no secret material. Fully consistent with `KEY_VAULT_PROPOSAL.md`'s
remote ownership rule.

### Relationship to KEY_VAULT_PROPOSAL

- Secret material (vault tokens) never leaves the owning hotel
- `VaultAdvertisement` is metadata only — vault name and ownership
- Each hotel mints its own token for its own local MuninnDB copy
- The mesh carries vault *existence*; each hotel manages its own *credentials*

### Cold-start bootstrap

A hotel joining the mesh for the first time replays all `VaultAdvertisement`s from
the mesh ledger before replaying `MemoryEvent`s, ensuring its token map is fully
populated before any memory writes arrive.

---

## Topology

The **Context Graph is the unit of identity and memory**, not the hotel. Hotels are
compute — they materialize guests and own a Context Graph. Two hotels sharing the
same Context Graph share the same MuninnDB instance and vaults; replication between
them is a no-op because they are already the same store. L1 replication only activates
across Context Graph boundaries.

### Multi-Context Graph (separate identities, full replication)

```
Context Graph A                      Context Graph B
  Hotel (vps)                          Hotel (macbook)
  ├── aiua daemon                      ├── aiua daemon
  ├── MuninnDB guest                   ├── MuninnDB guest
  │     vault: self_philote-1                vault: self_philote-1 (replica)
  │     vault: user_jared                    vault: user_jared (replica)
  └── mesh_events                      └── mesh_events
        MemoryEvent stream ──────────────▶   CursorStorage replay
                          ◀──────────────    MemoryEvent stream
```

Either hotel can serve as the recovery point. Loss of one → the other has a full
live copy and continues serving. The recovering hotel replays from its last cursor.

### Single Context Graph (shared identity, shared store)

```
Context Graph A
  Hotel A (vps)     Hotel B (macbook)
  ├── aiua daemon   ├── aiua daemon
  └── ─────────────────────────────▶ shared MuninnDB
                                       vault: self_philote-1
                                       vault: user_jared
```

Hotels within the same Context Graph write to a shared MuninnDB directly. No
`MemoryEvent` replication needed — they are already the same store. The shared
MuninnDB is a conscious operator choice; L3 (SQLite backup) still applies, but
L1 replication does not. The operator has traded replication complexity for
simplicity, and accepts that the shared MuninnDB is a single point of failure.

---

## Implementation Phases

| Phase | Deliverable | Depends on |
|---|---|---|
| 1 (now) | `MemoryEvent` + `VaultAdvertisement` types in `ansible-mesh-core` (→ `aiua-mesh-core` post-rename) | — |
| 2 | `MemoryEventEmitter` trait + hotel daemon appends to `mesh_events` on every write | Slice C (hotel config keys) |
| 3 | `VaultAdvertisement` broadcast on vault creation + receiving hotel bootstrap handler | Phase 2 |
| 4 | Peer hotel cursor replay of `MemoryEvent`s into local MuninnDB | Phase 3, multi-hotel mesh live |
| 5 | L2 git export job in hotel daemon | Phase 2 |
| 6 | L3 SQLite backup job | Phase 2 |

L0 (stability pinning) lands in the Attend step implementation — independent of
this phase sequence.

---

## Open Questions

- **Conflict resolution on replay**: if Hotel A and B both wrote to the same vault
  concurrently and A replays into B, which version wins? Current answer: Last-Writer-Wins
  by `occurred_at` timestamp, consistent with the stack's existing LWW memory consistency
  model. Multi-primary is acceptable given LWW — no home hotel required for writes.
