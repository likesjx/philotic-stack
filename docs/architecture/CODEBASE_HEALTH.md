# Philotic Stack — Codebase Health Assessment

> **Status:** Living Document | **Last Updated:** 2026-03-10
> Generated from full static analysis of the codebase at commit `681d892`.

---

## Snapshot Metrics

| Metric | Value |
| ------ | ----- |
| Rust source files | 69 |
| Total source lines | ~47,500 |
| Test functions | 263 across 28 files |
| `unwrap()`/`expect()` calls (non-test) | ~119 |
| `todo!()` / `unimplemented!()` | 0 |
| Architecture proposal docs | 30 (~10,100 lines) |
| Active crates | 8 |

---

## Strengths

### Architecture vision is coherent
The hotel/guest metaphor is clear and consistently applied. Crate dependency order is sensible (`ansible-mesh-core` → `philotic-client` → guests). The 30+ proposal docs in `docs/architecture/` reflect genuine design thinking, not vaporware — most are tied to features currently in or near implementation.

### Zero stubs in the implementation
No `unimplemented!()` calls anywhere in the codebase. Only one `TODO` marker (a lint suppression in `robot-kit`). Code that exists is real code.

### Storage layer is properly abstracted
Three pluggable traits (`EventStorage`, `CursorStorage`, `GraphStorage`) with `Arc<dyn>` injection throughout. Swapping the SQLite backend for another engine requires one line change in `ansible/src/main.rs`.

### Meaningful test suite
263 test functions across 28 files, all embedded inline. Key files have proportional coverage: `ipc.rs` (23 tests / 4,911 lines), `runtime.rs` (22 tests / 2,444 lines), `main.rs` (14 tests / 2,892 lines). `ansible-mesh-core` has a dedicated `tests/` directory.

### Smoke scripts validate key paths
Shell-based end-to-end coverage exists for approval roundtrip, remote model roundtrip, session control, routed tool execution, and binary resolution. These are real validation, not CI stubs.

### Execution plane has real separation
UDP is used for control-plane gossip; TCP point-to-point is used for routed execution traffic. The two-hotel local smoke (`smoke-remote-model-roundtrip.sh`) proves this path works.

---

## Problems and Risk Areas

### 1. `ipc.rs` is a god file — HIGH risk
At **4,911 lines**, `service/ipc.rs` is the single most critical file in the stack and the most dangerous to maintain. It's the central IPC dispatch path where every guest interaction lands. Its size makes it:
- A guaranteed merge conflict hotspot on parallel workstreams
- Hard to reason about in review
- The most impactful place for a latent panic to surface in production

**Recommendation:** Split by concern — registration/heartbeat, event publish, apartment sync, model routing, tool dispatch — each into its own `service/ipc_*.rs` module.

### 2. 119 runtime `unwrap()` calls — MEDIUM risk
`ipc.rs` alone carries 195 `unwrap()`/`expect()` calls (some in test code, many not). A panicking `unwrap()` in an async tokio task silently kills that task. For a daemon that's supposed to be a stable hotel supervisor this is a reliability risk, not just a style issue.

**Recommendation:** Audit `ipc.rs` and `main.rs` for `unwrap()` on `Option`/`Result` in async task bodies. Replace with logged errors and graceful degradation.

### 3. Proposal backlog significantly outpaces implementation
~10,100 lines of architecture proposals exist for features not yet in code:

| Proposal | Status |
| -------- | ------ |
| Agent incarnation model | Docs only |
| Forked sessions | Docs only |
| Task runner / task lifecycle | Docs only |
| Voice machine | Docs only |
| Native overlay/VPN | Docs only |
| Slash commands | Docs only |
| Tool management plane | Docs only |
| Approval UX | Docs only |
| Muninn memory protocol | Docs only |
| Agent loop re-entry | Docs only |

This is not a criticism of the design process — the proposals are high quality. But the gap between `docs/architecture/` and `crates/` is wide, and it grows with each session.

### 4. No integration test directories for core crates
Only `ansible-mesh-core` has a `tests/` directory. `ansible`, `agent-core`, and `membrane` — which contain the most complex runtime behavior — have no integration test layer. The smoke scripts fill some of this gap but are environment-dependent and not CI-runnable.

**Recommendation:** Add at least one integration test per crate that exercises the happy path without a live running hotel where possible (e.g., `GuestManager` can be tested with a mock materializer).

### 5. `tool-runner` is seeded inactive
The `tool-runner` binary is registered in the hotel config seeding path with the comment: `// Not yet implemented — marked inactive so the hotel skips spawn`. It's 770 lines of real workspace tool logic (file read, search, sandboxed execution). Its status relative to the `TOOL_ASSEMBLY_EXECUTION_PROPOSAL` is unclear — it may be the implementation or may be superseded by that proposal.

**Recommendation:** Clarify whether `tool-runner` is the working implementation to be activated, or a placeholder to be replaced. If the former, close the activation gap. If the latter, document it as deprecated.

### 6. WebRTC is partial
`webrtc_guest.rs` imports the `webrtc` crate and defines the `WebRtcGuest` struct with connection setup scaffolding, but the ICE/signaling lifecycle is incomplete. It's listed in the Port Road Map as planned — it's actually in-progress but not yet usable.

### 7. Branch sprawl risk
Multiple `codex/*` workstreams have been active in parallel. The `CLAUDE.md` worktree discipline is good. Keep it — each branch should reach a merge decision before a third is opened.

---

## Overall Verdict

**Serious prototype, not yet production-ready infrastructure.**

The core hotel/guest/IPC loop works. Telegram media roundtrip is real. The mesh + TCP execution plane is real. The storage abstraction is solid. But:

- The main daemon has reliability risks (ipc.rs size + unwrap density)
- The system lacks reproducible integration tests
- The proposal-to-implementation ratio is lopsided and widening

The biggest risk is **proposal accumulation outpacing implementation velocity**. The architecture is coherent and the bones are good. The next priority should be closing existing features before adding new proposals.

---

## Priority Recommendations

1. **Split `ipc.rs`** into sub-modules by concern
2. **Audit and fix `unwrap()` density** in async task bodies in `ansible/`
3. **Activate or deprecate `tool-runner`** — resolve its status
4. **Add one integration test per core crate** (`ansible`, `agent-core`, `membrane`)
5. **Close the current branches** before opening new proposal workstreams
