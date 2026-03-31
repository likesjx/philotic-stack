---
doc_type: proposal
status: proposed
domain: runtime-sessions
last_updated: 2026-03-31
tags:
- ipc
- naming
- guest-identity
- role-disambiguation
refs:
- AGENT_INCARNATION_PROPOSAL.md
---

# GuestIdentity.role → GuestIdentity.component_type Rename Proposal

## Goal

Eliminate the naming collision between the IPC process type (`GuestIdentity.role`)
and the session-level agent persona role (e.g. `orchestrator`, `virtuosa`).

These two concepts are orthogonal but share the word "role", causing confusion in
code, logs, and reasoning — as demonstrated by the `ConfigureRole` bug where aiua
checked the IPC process type instead of the persona role for authority.

## Core Recommendation

Rename `GuestIdentity.role` → `GuestIdentity.component_type` across the entire
codebase. This makes the distinction explicit at every call site.

**IPC process types** (values for `component_type`):
- `"membrane"` — transport gateway
- `"agent"` — philote persona process
- `"tool-runner"` — tool execution runner
- `"graph-runner"` — graph execution runner
- `"model"` — model controller (gemini, elevenlabs)
- `"tool"` — legacy tool role

**Session persona roles** (separate concept, stored in graph):
- `"orchestrator"` — default managing persona
- `"virtuosa"`, `"developer"`, etc. — configured role incarnations

## Disposition

`proposed`

## Current Slice

Not yet started. Deferred from the `ConfigureRole` bug fix slice, which introduced
`calling_role: String` on `IpcRequest::ConfigureRole` as the immediate workaround
so aiua can receive the actual persona role from philote explicitly.

The workaround is safe and correct. This rename is a cleanup slice that removes the
underlying source of confusion.

## Scope

Files touched will include:
- `crates/philotic-client/src/lib.rs` — `GuestIdentity` struct definition
- `crates/aiua/src/main.rs` — all `guest.role` references for supervision/routing
- `crates/aiua/src/service/ipc.rs` — `identity.role` in IPC handlers
- `crates/aiua/src/service/guest_manager.rs`
- `crates/ansible-mesh-core/src/sqlite_storage.rs` — storage column
- `crates/model-router/src/runtime.rs`
- All example smoke drivers in `crates/philotic-client/examples/`
- All test sites using `GuestIdentity { role: ... }`

The SQLite column rename for `materialized_guests.role` may require a schema
migration or a column alias to preserve backward compatibility with existing
installed environments.

## Next Seam

- Write migration for `materialized_guests.role` column if needed
- Do rename as a single `chore(philotic-client): rename GuestIdentity.role to component_type` commit
- Update all aiua supervision logic to use `component_type`
- Verified: `check-only` or `test-green`
