#!/usr/bin/env bash
# Answer "was this tool ever actually called, and did it work?" from the hotel's
# DURABLE record instead of from logs.
#
# WHY THIS EXISTS
# ---------------
# Establishing whether `life.observe.batch` was being used at all cost hours of
# journald forensics during the 2026-07 investigation, and the answer that came
# back was wrong twice over:
#
#   1. journald retention on vps-jane is ~6 days, so "no hits" only ever means
#      "not in the last week" — never "never".
#   2. the hotel logs model prompt and tool-catalog text VERBATIM, so a keyword
#      grep matches the tool's own schema far more often than any real call, and
#      the structured log lines carry ANSI colour codes that silently break the
#      obvious `grep 'capability="life.observe.batch"'`.
#
# The hotel already appends a durable `session_event` per emitted task, carrying
# the tool name — full history, no retention limit, no prompt pollution. This
# reads that. Prefer it over `journalctl` for any "is this tool used?" question.
#
# Usage:
#   scripts/tool-usage.sh                       # all tools, by call count
#   scripts/tool-usage.sh life.observe.batch    # one tool: totals, daily, errors
#
# Env:
#   PHILOTIC_DB   path to the hotel context DB
#                 (default: /opt/philotic/data/aiua_context.db — the vps layout;
#                  the Macs use ~/.philotic/<profile>/context.db)
#   SQLITE        sqlite3 invocation (default: "sudo sqlite3", as the vps DB is
#                 owned by the philotic user)

set -euo pipefail

DB="${PHILOTIC_DB:-/opt/philotic/data/aiua_context.db}"
SQLITE="${SQLITE:-sudo sqlite3}"
TOOL="${1:-}"

if ! $SQLITE "$DB" 'select 1;' >/dev/null 2>&1; then
  echo "cannot read hotel DB at $DB" >&2
  echo "set PHILOTIC_DB (Macs: ~/.philotic/<profile>/context.db) or SQLITE" >&2
  exit 1
fi

# json_extract path to the tool name recorded on each emitted task.
TOOL_PATH='$.payload_json.tool_name'

if [[ -z "$TOOL" ]]; then
  echo "== tool calls, all time (durable session_event record) =="
  $SQLITE -column -header "$DB" "
    select json_extract(data_json,'$TOOL_PATH') as tool,
           count(*)                             as calls,
           date(min(json_extract(data_json,'\$.created_at')),'unixepoch') as first_seen,
           date(max(json_extract(data_json,'\$.created_at')),'unixepoch') as last_seen
      from graph_nodes
     where kind='session_event'
       and json_extract(data_json,'$TOOL_PATH') is not null
     group by tool
     order by calls desc;"
  exit 0
fi

echo "== $TOOL =="
echo
echo "-- totals --"
$SQLITE -column -header "$DB" "
  select count(*) as calls,
         date(min(json_extract(data_json,'\$.created_at')),'unixepoch') as first_seen,
         date(max(json_extract(data_json,'\$.created_at')),'unixepoch') as last_seen
    from graph_nodes
   where kind='session_event'
     and json_extract(data_json,'$TOOL_PATH')='$TOOL';"

echo
echo "-- calls per day --"
$SQLITE -column -header "$DB" "
  select date(json_extract(data_json,'\$.created_at'),'unixepoch') as day,
         count(*) as calls
    from graph_nodes
   where kind='session_event'
     and json_extract(data_json,'$TOOL_PATH')='$TOOL'
   group by day order by day;"

echo
echo "-- outcome of responses carrying this capability --"
$SQLITE -column -header "$DB" "
  select case when data_json like '%\"error\":{%' then 'error' else 'ok' end as outcome,
         count(*) as n
    from graph_nodes
   where kind='session_event'
     and json_extract(data_json,'\$.payload_json.capability')='$TOOL'
   group by outcome;"

echo
echo "-- most recent error, if any --"
$SQLITE "$DB" "
  select json_extract(data_json,'\$.payload_json.error.message')
    from graph_nodes
   where kind='session_event'
     and json_extract(data_json,'\$.payload_json.capability')='$TOOL'
     and data_json like '%\"error\":{%'
   order by json_extract(data_json,'\$.created_at') desc
   limit 1;" | cut -c1-600
