---
doc_type: defect-tracker
status: active
last_updated: 2026-03-17
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
