---
doc_type: defect-tracker
status: active
last_updated: 2026-03-31
---

# Defects and Technical Debt

Tracked defects and known technical debt. Each entry carries status, severity, size estimate, and commit/seam cross-references.

**Status values**: `open` | `in-progress` | `fixed` | `deferred` | `wont-fix`
**Severity values**: `critical` | `high` | `medium` | `low`
**Size values**: `XS` | `S` | `M` | `L` | `XL`

---

## DEF-001: `local_capability_advertisements_include_hotel_scoped_incarnations` off-by-one

**Status**: open
**Severity**: low
**Size**: S
**Seam**: active-membrane-routing
**Found**: 2026-03-10

`tool-runner` is counted in hotel-scoped incarnation advertisements but is seeded as inactive. The test expects the active set only, producing an off-by-one failure. Pre-existing; does not affect runtime behavior since `tool-runner` is unused.

---

## DEF-002: `ansible` crate Gap 3 sqlite_storage incomplete

**Status**: open
**Severity**: medium
**Size**: M
**Seam**: tooling-execution
**Found**: 2026-03-10

`GraphStorage` trait methods for `AbstractToolRecord` (`upsert_abstract_tool`, `get_abstract_tool`, `list_abstract_tools`) are defined in the trait but not fully wired in `SqliteGraphStorage`. Causes compile error in the `ansible` crate. Does not affect `aiua`, `philote`, or `model-router` builds. In-progress slice (Gap 3).

## DEF-003: `aiua` has no library target — unit tests in binary modules cannot be run

**Status**: fixed
**Severity**: medium | **Effort**: 2pts

`aiua` is configured as a binary-only crate. Test mocks in the binary test target had pre-existing compilation failures (missing `OperatorTargetView` import, incomplete `WorkflowSkillRecord` mock impls on two `TestGraphStorage` structs), which prevented `cargo test -p aiua` from running any tests. Additionally, `context_graph_entries_support_hotel_level_telegram_overlay` was failing because `extract_context_graph_entries` didn't fall back to `hotels["default"]` when the named hotel was absent. All three issues fixed: import added to ipc.rs test module, stub methods added to both mock impls, fallback added to `extract_context_graph_entries`. `cargo test -p aiua` now passes 110 tests.
---

## DEF-004: Multi-tool synthesized response not surfacing

**Status**: open
**Severity**: high
**Size**: M
**Seam**: cognitive-loop-reentry
**Found**: 2026-03-30 (identified in MEMORY.md)

Agent re-entry loop completes multiple tool iterations, but the final synthesized summary response does not reach the user or surface in the final turn result. Re-entry loop works, but the final text generation step may be failing or its output is being dropped.

---

## DEF-006: Turn watchdog evicts WaitingModel turns instead of escalating to next fallback tier

**Status**: in-progress
**Severity**: high
**Size**: S
**Seam**: cognitive-loop-reentry
**Found**: 2026-06-27

When a model call hangs and the philote watchdog fires (`WAITING_MODEL_SECS = 300`), it evicts the turn and tells the user "I seem to have gotten stuck" instead of escalating to the next fallback tier. The root cause is a silent IPC drop between model-router and philote: model-router has a 70s budget (35s × 2 attempts) but if its IPC connection to aiua drops, no error signal reaches philote — so the 300s watchdog is the only backstop, and it was wired to give up rather than retry. The prior "fix" (`951e113`, bump 120s→300s) only delayed the give-up; it did not escalate. Fix: branch on `WaitingModel` phase in `evict_timed_out_turns` Step 3 and call `advance_turn_to_next_fallback_tier` instead of evicting. The 600s `CatchAll` ceiling remains a hard evict.

---

## DEF-005: `aiua` test suite — 10 hung tests, 2 unrelated failures on develop HEAD

**Status**: open
**Severity**: medium
**Size**: M
**Seam**: aiua-test-infra
**Found**: 2026-06-22

`cargo test -p aiua --release` on unmodified `develop` HEAD (`d2478e8`) hangs indefinitely on 10 tests: `desktop_membrane_lease_can_be_renewed_by_owner`, `desktop_membrane_lease_disconnect_allows_takeover`, `desktop_membrane_lease_release_allows_immediate_takeover`, `desktop_membrane_status_view_comes_from_hotel_record`, `desktop_membrane_target_guest_inventory_reports_failed_remote_query_when_unreachable`, `desktop_membrane_target_status_distinguishes_local_from_remote_observation`, `desktop_membrane_target_views_include_source_and_freshness_attribution`, `discord_lease_injects_membrane_binding`, `e2e_session_round_trip_persists_and_delivers_reply`, `e2e_structured_tool_call_round_trip_persists_and_delivers_reply`. Confirmed via isolated single-threaded run of one test alone (still hangs, so it is not parallel resource contention) and via `git stash` on the same commit (still hangs with the working tree clean). With those 10 skipped via `--skip`, two more fail: `service::ipc::tests::emit_task_falls_back_to_orchestrator_when_active_incarnation_is_unregistered` (timeout waiting for fallback delivery) and `tests::default_guest_seed_injects_hotel_socket_env` (`assertion left == right failed: left: 12, right: 11`) — both reproduce identically with or without unrelated changes, so likely order/state-dependent on which tests ran before them in the filtered set rather than newly broken. Not investigated further; out of scope for `codex/lifegraph-cross-hotel-park`. Suspect for the hangs: several of these tests bind `(dispatcher_tx, _dispatcher_rx)` and never drain `_dispatcher_rx`, so any code path that sends more than the channel capacity (8 or 16) worth of `LedgerCommand`s blocks forever on `dispatcher_tx.send(...).await` — worth checking first.
