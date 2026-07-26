---
title: Using the Table Query Tools
doc_type: guide
domain: tooling-execution
status: current
last_updated: 2026-07-24
tags:
- table-datasource
- catalog
- query-builder
- agent-tooling
related_docs:
- ../architecture/DB_CATALOG_QUERY_TOOL_PROPOSAL.md
---

# Using the Table Query Tools

How an agent reads a database it has been granted, without shell access. Four
tools: `table.catalog`, `table.schema`, `table.build`, `table.query`.

Run the live tour yourself — it uses your real iMessage store when readable and
falls back to a synthetic one otherwise:

```bash
cargo run -p table-datasource --example walkthrough
```

The transcript below is that example's real output, with message bodies reduced
to lengths and handles masked.

## The mental model

The operator registers **known databases** in `db_catalog.json`; agents can
only reach what is registered and granted to them. Everything else — arbitrary
paths, unregistered names, write statements — is refused before it touches a
connection. Agents never need (and never get) shell access to read data.

## Operator side: registering a database

One file, `{profile}/db_catalog.json`. It hot-reloads on save — no restart:

```json
{
  "databases": [
    {
      "name": "imessage",
      "path": "~/Library/Messages/chat.db",
      "mode": "ro",
      "description": "macOS iMessage store (read-only, snapshot-on-read)",
      "snapshot_on_read": true,
      "snapshot_ttl_secs": 300,
      "agents": { "agent-ariel": ["read"] }
    }
  ]
}
```

`mode` is `ro` or `rw` and is enforced at the connection. `agents` is optional —
omit it and the entry is open to any agent (still mode-bound); include it and
only the listed agents can see or touch the database. `snapshot_on_read` copies
the source before reading, for databases another process writes live.

## Agent side: the four calls

### 1. `table.catalog` — what can I reach?

No arguments. The answer is scoped to the caller:

```json
{
  "agent": "agent-ariel",
  "databases": [
    {
      "db": "imessage",
      "description": "macOS iMessage store (read-only, snapshot-on-read)",
      "mode": "ro",
      "snapshot_on_read": true,
      "source": "catalog",
      "verbs": ["read"]
    }
  ]
}
```

### 2. `table.schema` — what is in it?

Omit the table name to list everything. This is how an agent learns a database
it has never seen:

```
25 tables: _SqliteDatabaseProperties, attachment, chat, chat_handle_join,
chat_lookup, chat_message_join, ..., handle, message, ...
```

Pass `graph_id` to get a single table's `CREATE TABLE` DDL.

### 3. `table.build` — query without writing SQL

Send a spec; the runner compiles it. Identifiers are validated, operators come
from a whitelist (`= != < <= > >= like in is_null not_null`), values bind as
parameters, and the result is always LIMIT-bounded.

```json
{
  "table": {"table": "message", "as": "m"},
  "columns": ["m.ROWID", "m.date", "m.is_from_me"],
  "where": [{"column": "m.is_from_me", "op": "=", "value": 0}],
  "order_by": [{"column": "m.date", "dir": "desc"}],
  "limit": 3
}
```

The response carries the compiled SQL next to the rows, so what ran is always
auditable:

```
SELECT m.ROWID, m.date, m.is_from_me FROM message AS m
  WHERE m.is_from_me = ?1 ORDER BY m.date DESC LIMIT 3
```

**Joining another database**: add `joins[].db`. The runner attaches that
catalog database read-only for the duration of the query — and only if the
caller also holds a read grant on it.

```json
{
  "table": {"table": "message", "as": "m"},
  "joins": [{"db": "contacts", "table": "handle", "as": "h",
             "type": "left", "on": {"left": "m.handle_id", "right": "h.ROWID"}}]
}
```

### 4. `table.query` — the power lane

Raw `SELECT` for what the builder cannot express. Still read-only-enforced.
iMessage is the motivating case: Apple stores dates as nanoseconds since 2001,
which needs real SQL arithmetic.

```sql
SELECT datetime(m.date/1000000000 + 978307200, 'unixepoch', 'localtime') AS sent_at,
       length(m.text) AS text_len,
       substr(h.id, -2) AS handle_tail
FROM message m JOIN handle h ON m.handle_id = h.ROWID
WHERE m.text IS NOT NULL
ORDER BY m.date DESC
```

Pass `limit` in `parameters` to cap rows, and `attach` (an array of catalog
database names) for cross-database queries.

## What gets refused

Every one of these is denied — verified in the example run:

| Attempt | Refusal |
|---|---|
| Write task kind on a `ro` database | `database 'imessage' is read-only` |
| Write smuggled through the read lane | `table.query/table.build are read-only — use the explicit write task kinds` |
| `ATTACH` an arbitrary file via SQL | `not authorized` |
| SQL injected through the builder | `invalid identifier: "message; DROP TABLE handle"` |
| A database the agent was not granted | `agent 'agent-jane' has no Read grant on database 'imessage'` |

An ungranted agent's `table.catalog` returns `"databases": []` — it cannot see
that the database exists at all.

## Gotchas

- **Reads never create.** A read against a name that is neither in the catalog
  nor an existing profile database errors. Only write kinds
  (`table.configure`, `table.insert`, …) bring a profile database into being.
- **Snapshots are real copies.** `snapshot_on_read` writes a full plaintext
  copy to `{profile}/.snapshots/{name}.db` (mode `0600`, directory `0700`),
  purged when the runner shuts down cleanly. Within the TTL you are reading the
  copy, so very recent writes to the source will not appear.
- **Grants are per-agent, not per-role.** They key off `agent_id` in the task
  identity envelope.
- **Editing the catalog needs no restart** — but it does drop pooled
  connections, so an in-flight query may see a one-off reconnect.

## See also

- [`DB_CATALOG_QUERY_TOOL_PROPOSAL.md`](../architecture/DB_CATALOG_QUERY_TOOL_PROPOSAL.md)
  — design, threat model, and the Ariel cutover runbook (including the Full
  Disk Access ordering constraint).
