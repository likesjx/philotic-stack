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
