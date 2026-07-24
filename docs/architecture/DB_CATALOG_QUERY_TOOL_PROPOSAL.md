---
title: Database Catalog and Governed SQLite Query Tool
doc_type: proposal
domain: tooling-execution
status: in-progress
last_updated: 2026-07-21
tags:
- table-datasource
- catalog
- security
- least-privilege
- query-builder
- agent-grants
related_docs:
- AGENT_RESOURCE_MODEL_PROPOSAL.md
- TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md
- CAPABILITY_POOL_AND_PURPOSE_COMPOSITION_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: db-catalog-query-tool
disposition: accepted-current-slice
active_seams:
- db-catalog-registry
- readonly-authorizer-lane
- query-builder-attach
- table-identity-grants
- ariel-table-cutover
---

# Database Catalog and Governed SQLite Query Tool

## Goal

Replace `bash.exec`-style data access for agents with a governed SQLite query
surface: a hotel-owned **catalog of known databases**, enforced **read-only
connections**, a server-side **query builder**, and **per-agent grants**.

First consumer: `agent-ariel` (communications steward) reading the operator's
messages (macOS iMessage `chat.db`) with no shell access and no write path.

Origin: operator + Aria design conversation 2026-07-02 (mbp-jane, Telegram
turns 182443492–494) — the operator chose a read-only database runner over
granting Ariel `bash.exec`. Reaffirmed and expanded 2026-07-21: *"a tool that
will build a sqlite query against known databases, attach to the database."*
Graph node: `proposal:db-catalog-query-tool`.

## Problem

`table-datasource` before this proposal:

1. **Any `db` name auto-creates a file** under the profile dir — there is no
   notion of a *known* database; the namespace is whatever string a caller
   sends.
2. **Path traversal**: the path is built with
   `base_dir.join(format!("{}.db", name))`, so `db: "../../Library/Messages/chat"`
   escapes the profile directory — read-write.
3. **`table.query` accepts raw SQL with no read-only enforcement** and
   `table.exec` is arbitrary batch DDL. The only gate is the coarse `table`
   tool class; no per-database or per-verb control exists even though
   `DatasourceTask` already carries caller `identity`.

## Components

### 1. Database catalog (`crates/table-datasource/src/catalog.rs`)

`{base_dir}/db_catalog.json` (override: `PHILOTIC_TABLE_CATALOG`) registers
known databases:

```json
{
  "databases": [
    {
      "name": "imessage",
      "path": "~/Library/Messages/chat.db",
      "mode": "ro",
      "description": "macOS iMessage store",
      "snapshot_on_read": true,
      "snapshot_ttl_secs": 300,
      "agents": { "agent-ariel": ["read"] }
    }
  ]
}
```

- Catalog names resolve to their registered path with their registered mode.
- Non-catalog names fall back to the legacy profile mapping
  `{base_dir}/{name}.db`, now restricted to `[A-Za-z0-9_-]+` (closes the
  traversal hole; auto-create behavior for profile DBs is preserved for
  backward compatibility).
- The file is re-read whenever its mtime changes and the connection pool is
  reset — **grants and databases are editable at runtime with no deploy**
  (per the data-driven-tool-grants principle).

### 2. Read-only enforcement

`mode: "ro"` entries open with `SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only=ON`,
and every connection installs a rusqlite **authorizer** that denies
`ATTACH`/`DETACH` unless the runner itself is performing a catalog-resolved
attach (agent SQL can never attach a file). Write task kinds are refused
against `ro` databases before a connection is touched, and the shared read lane
(`table.query`/`table.build`) rejects any statement whose
`sqlite3_stmt_readonly` is false — writes cannot be smuggled through the query
tools on *any* database.

### 3. Query tool surface

- `table.catalog` — databases the calling agent can see: name, mode,
  description, granted verbs.
- `table.schema` — with `graph_id`: one table's DDL; without: every table's
  DDL (schema discovery for unfamiliar catalog databases).
- `table.build` — the structured builder: `{table, columns?, where?, joins?,
  order_by?, limit?, offset?}` compiled server-side into a parameterized
  single SELECT. Identifiers validated, operators whitelisted
  (`= != < <= > >= like in is_null not_null`), every value bound as a
  parameter, result always LIMIT-bounded. Returns the compiled SQL alongside
  the rows for auditability. `joins[].db` references another catalog database;
  the runner attaches it read-only under that alias for the duration of the
  query.
- `table.query` — the power lane: raw SQL, still read-only-enforced. Needed
  because real schemas (iMessage's Apple-epoch timestamps, `handle` joins)
  want expressions a builder cannot cover. `parameters.attach` lists catalog
  databases to attach read-only.

### 4. Per-agent grants

Grants key off `task.identity.agent_id` against the entry's `agents` map:
`read` / `write` verbs (`write` implies `read`). When `agents` is present only
listed agents may touch the database — others get a deny before any connection
opens, and `table.catalog` does not reveal the entry to them. When `agents` is
absent, the catalog entry itself is the grant (the operator registered it),
mode-bound. Write kinds always require a `rw` database.

The agent-resource-model broker (Seam 1 IPC, currently stubbed) is the
intended long-term authority for these grants; the catalog `agents` map is the
data-driven first slice and will become the broker's projection target.

### 5. Ariel slice (pending — live-hotel config)

- Catalog entry `imessage` → `~/Library/Messages/chat.db`, `ro`,
  `snapshot_on_read: true` (Messages writes WAL live; the snapshot — taken via
  the SQLite backup API into `.snapshots/` — avoids lock contention and torn
  reads), `agents: {"agent-ariel": ["read"]}`.
- Replace `bash.exec` in Ariel's toolset with
  `table.catalog` / `table.schema` / `table.build` / `table.query`.
- The runner process (aiua host) needs macOS **Full Disk Access** to read
  `chat.db`.
- Fix the identity bundle bug found during discovery: `agent_identity:agent-ariel`
  on mbp-jane carries `identity_text` that begins "You are Hermes".

## Seams

| Seam | Content | Status |
|---|---|---|
| `db-catalog-registry` | Catalog + ro-mode connections; traversal/auto-create killed | code-complete, test-green |
| `readonly-authorizer-lane` | Authorizer + `stmt.readonly()` read lane | code-complete, test-green |
| `query-builder-attach` | `table.build` compiler + catalog-gated cross-db attach | code-complete, test-green |
| `table-identity-grants` | Per-agent `{db, verbs}` grants from `task.identity` | code-complete, test-green (catalog slice; broker integration follow-up) |
| `ariel-table-cutover` | Live mbp-jane config: imessage entry, grant, bash.exec removal, identity fix | pending (operator ceremony) |

## Verification

`cargo test -p table-datasource` — 22 tests: the 9 pre-existing behavior tests
(kept green) plus builder unit tests and per-seam integration tests:
traversal rejection, unknown-catalog-database rejection, ro write-kind
rejection, smuggled-write rejection through the read lane, agent-SQL ATTACH
denial, builder end-to-end, cross-db join attach, per-agent grant
enforcement, catalog visibility filtering, snapshot-on-read stability.

## Follow-ups

- Datasource guests register `supported_tools: Vec::new()` in their IPC
  identity; the `tool_runner_registry` therefore advertises no capabilities
  for them. Thread the provider's supported kinds through
  `DatasourceGuestConfig` (5 construct sites).
- Broker integration: project `resource_grants` from the agent-resource-model
  broker into the catalog `agents` map instead of hand-editing JSON.
- `table.exec` on `rw` databases remains arbitrary DDL — consider folding it
  behind an explicit `admin` verb.
