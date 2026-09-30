---
name: session-hygiene
description: Monitor and maintain session health. Detect stale sessions, orphaned workstreams, and agent coordination conflicts. Run during session start, check-engine, or on-demand.
---

# Session Hygiene

Scope: agent sessions and workstream lifecycle.

Sessions are the coordination primitive for multi-agent work. Without hygiene, stale sessions block `graph_next_task` scoring, pollute the dashboard, and cause agents to avoid work that appears claimed but is actually abandoned.

## When to Run

- **Session start**: Quick health check before claiming work; `just session-start` also runs the LifeGraph idea sweep (`just idea-sweep`) — triage pending operator ideas per `$graph-intelligence` § Idea Sweep
- **Check-engine**: Full sweep as part of the E step
- **On-demand**: When the dashboard shows suspicious activity
- **Automated**: Via `just intel-graph-session-cleanup` (cron-friendly)

## Health Check

Query the session health endpoint:

```bash
curl -s http://127.0.0.1:8900/api/health/sessions | jq .
```

Or via MCP: use `graph_status` and inspect the session section.

### What It Reports

- **Stale sessions**: Active sessions with no activity for > 4 hours
- **Overloaded agents**: Agents with > 2 concurrent active sessions
- **Orphaned workstreams**: Workstream nodes with `status: active` but no corresponding active session

### Health Criteria

The system is **healthy** when:
- No stale sessions exist
- No agent has more than 2 concurrent sessions
- No orphaned workstreams remain

## Cleanup

### Automatic Cleanup

```bash
# Close sessions older than 4 hours (default)
curl -s -X POST http://127.0.0.1:8900/api/session/cleanup | jq .

# Close sessions older than 8 hours
curl -s -X POST http://127.0.0.1:8900/api/session/cleanup \
  -H "Content-Type: application/json" \
  -d '{"max_age_hours": 8}' | jq .

# Via justfile
just intel-graph-session-cleanup
```

### Manual Cleanup

For sessions that need manual review before closing:

```bash
# Close a specific session
curl -s -X POST http://127.0.0.1:8900/api/session/close \
  -H "Content-Type: application/json" \
  -d '{"session_id":"<id>","summary":"Manual cleanup: <reason>"}'
```

## Prevention Rules

### For Agents

1. **Always call `session_close`** at the end of work, even if the work is incomplete
2. **Report activity** via `session_activity` during long sessions to prevent false staleness
3. **Check the dashboard** before starting work to avoid claiming already-active seams
4. **Limit concurrency** to 2 sessions per agent identity

## Agent Session Protocol (CRITICAL)

### When Starting Work

Every agent MUST follow this exact sequence:

```bash
# 1. Check for existing work on the target
curl -s -X POST http://127.0.0.1:8901/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"graph_agent_dashboard","arguments":{}},"id":1}'

# 2. Start session with EXISTING seam
curl -s -X POST http://127.0.0.1:8901/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"session_start","arguments":{"agent":"<agent-name>","session_id":"<unique-session-id>","seam_id":"<EXISTING-seam-id>"}},"id":1}'

# 3. Report activity
curl -s -X POST http://127.0.0.1:8901/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"session_activity","arguments":{"session_id":"<session-id>","phase":"<phase>","files_touched":["<file-list>"]}},"id":1}'
```

### Workstream-Seam Linking Rules

1. **NEVER create workstreams linked to non-existent seams**
2. **Always verify the seam exists** before starting a session
3. **Use existing seams** when possible
4. **If no appropriate seam exists**, create the seam FIRST via proper graph operations

### Finding Available Seams

```bash
# List all existing seams
curl -s "http://127.0.0.1:8900/api/nodes?kind=seam" | jq '.[] | {id: .id, name: .name, status: .properties.status}'

# Search for seams by name
curl -s "http://127.0.0.1:8900/api/nodes?kind=seam" | jq '.[] | select(.name | contains("<keyword>")) | {id: .id, name: .name}'
```

### When Finishing Work

```bash
# Always close the session
curl -s -X POST http://127.0.0.1:8901/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"session_close","arguments":{"session_id":"<session-id>"}},"id":1}'
```

### Common Mistakes to Avoid

❌ **Creating workstreams with phantom seams**  
❌ **Starting sessions without checking existing work**  
❌ **Forgetting to close sessions**  
❌ **Using non-existent seam IDs**  

✅ **Verify seam exists first**  
✅ **Check dashboard for conflicts**  
✅ **Always close sessions**  
✅ **Use proper seam IDs**

### For Operators

1. Run `just intel-graph-session-cleanup` daily or after agent sessions
2. Monitor `GET /api/health/sessions` for drift
3. Review orphaned workstreams — they may indicate agent crashes

## Integration with Check-Engine

The `check-engine` skill should include session hygiene as part of its sweep:

```
### Session Hygiene
- Active sessions: <count>
- Stale sessions: <count> (auto-cleaned: <count>)
- Orphaned workstreams: <count>
```

If stale sessions are found, clean them and note the cleanup in the check-engine output.

## API Reference

| Endpoint | Method | Description |
|---|---|---|
| `/api/health/sessions` | GET | Session health report |
| `/api/health/proposals` | GET | Proposal pipeline health |
| `/api/health` | GET | Combined system health |
| `/api/session/cleanup` | POST | Auto-close stale sessions |
| `/api/session/close` | POST | Close a specific session |
| `/api/dashboard` | GET | Full agent activity dashboard |
