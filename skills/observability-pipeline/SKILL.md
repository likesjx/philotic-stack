---
name: observability-pipeline
description: Use this skill when you want to capture structured event data into a queryable local table — e.g. logging model routing signals, transcription events, or any recurring envelope your agent-graph neighborhood should track across sessions.
catalog:
  skill_name: observability.pipeline
  implied_tools:
    - table.configure
    - table.add_listener
    - table.query
    - table.stats
    - table.rolloff
    - table.schema
    - graph.query
  validation_state: validated
  skill_markers:
    - self_directed
  field_sources:
    required_fields:
      - table_name
      - schema_sql
      - event_kind
    optional_fields:
      - schema_map
      - rolloff_max_rows
      - rolloff_max_age_secs
      - adapter_script
      - filter_keys
    workflow: "table.configure → table.add_listener → graph.query (store table_config node) → table.rolloff"
---

# Observability Pipeline

Use this skill when you want to capture a recurring event stream into a local table so you can query it, monitor trends, and keep it visible in your cognitive envelope across sessions.

## When to use

- You are receiving structured events (transcription captures, routing signals, sensor readings) and want them persisted and queryable
- You want live row counts and latest-event timestamps to appear in your context on every turn
- You want to set retention rules so the table doesn't grow unbounded

## Workflow

### Step 1 — Create the table

Call `table.configure` with a `CREATE TABLE IF NOT EXISTS` DDL statement. Always include a `timestamp INTEGER NOT NULL` column for rolloff support.

```sql
CREATE TABLE IF NOT EXISTS router_signals (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    provider_id TEXT NOT NULL,
    task_kind   TEXT NOT NULL,
    outcome     TEXT NOT NULL,
    latency_ms  INTEGER,
    model_id    TEXT,
    timestamp   INTEGER NOT NULL
)
```

### Step 2 — Register a listener

Call `table.add_listener` to wire the router-listener to write matching events into this table.

Required parameters:
- `table_name`: matches the table you just created
- `event_kind`: the `kind` field value on inbound envelopes (e.g. `"routing_signal"`)
- `schema_map`: maps event envelope fields to table columns (e.g. `{"provider": "provider_id", "latency": "latency_ms"}`)

Optional:
- `filter_keys`: only process events where these envelope fields match (e.g. `{"agent_id": "bjork-01"}`)
- `adapter_script`: path to a Python script for non-trivial field transformation
- `schema_sql`: pass the same DDL so it gets stored in your agent graph for context

### Step 3 — Register in your agent graph

Call `graph.query` to store a `TableConfig` node in your partition so the table appears in your cognitive envelope on future sessions:

```cypher
CREATE (t:TableConfig {
  id: 'table_config:router_signals',
  name: 'router_signals',
  schema_sql: '<your CREATE TABLE statement>',
  event_kind: 'routing_signal',
  rolloff_max_rows: 10000
})
```

### Step 4 — Set retention rules

Call `table.rolloff` with `max_rows` and/or `max_age_secs` to prune the table. Run this periodically (e.g., once per session start).

```json
{
  "graph_id": "router_signals",
  "parameters": { "max_rows": 10000, "ts_column": "timestamp" }
}
```

### Step 5 — Verify

Call `table.stats` to confirm data is flowing:
```json
{ "graph_id": "router_signals", "parameters": { "ts_column": "timestamp" } }
```

## Cognitive envelope

Any `TableConfig` nodes stored in your agent graph partition appear in your cognitive envelope on subsequent sessions as part of the `AgentGraph` context layer. The node's `schema_sql` field is what gives future-you the table shape at a glance.

If you see `TableConfig: {...}` entries in your context, those are your registered tables. Use `table.stats` to check liveness and `table.query` to inspect recent rows.

## Schema map rules

- Keys are event envelope field names; values are table column names
- If `schema_map` is omitted, the full envelope is passed through as-is
- Column names that don't exist in the table are silently dropped by the INSERT
- Use `adapter_script` when the envelope shape requires non-trivial transformation (the script receives raw event JSON on stdin and must emit transformed row JSON on stdout)

## Common event kinds

| kind | Source | Typical fields |
|------|--------|---------------|
| `transcription_capture` | model-router AudioTranscribe | `session_id`, `turn_id`, `agent_id`, `transcript`, `model_gen`, `blob_download_url` |
| `routing_signal` | model-router fan-out | `provider_id`, `task_kind`, `outcome`, `latency_ms`, `model_id` |
| Custom | Any EmitTask to `router-listener` role | Whatever you put in the envelope |

## Troubleshooting

- **No rows appearing**: Check that `PHILOTIC_ROUTER_CAPTURE_ENABLED=true` is set for routing signals; confirm the router-listener reloaded its config (it loads on connect; restart it or wait for the next reconnect cycle)
- **Table missing from context**: Confirm you ran `graph.query` CREATE to register the `TableConfig` node in your partition
- **Identifier error from table-datasource**: Table names and column names cannot contain `"`, `;`, `--`, or null bytes
