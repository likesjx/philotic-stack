name: multi-agent-orchestration
description: Spin up multiple agents with different canonical profiles on the same workstream. Coordinate implementer, orchestrator, reviewer, and verifier agents working together.

# Multi-Agent Orchestration

Scope: coordinating multiple AI agents on shared work.

One agent can only hold one role at a time. Complex work requires multiple perspectives — implementation, review, verification — running concurrently or in sequence. This skill documents how to spin up and coordinate multiple agents on the same workstream.

## Supported Orchestration Patterns

- **Single-harness, multi-role**: one harness declares `desired.supported_roles` and the workflow starts role-specific sessions from that shared harness. See `$windsurf-harness-setup`.
- **Multi-harness**: separate harnesses exist per agent/role/runtime, and each session claims its own harness.

## When to Use

- **Complex proposals** requiring multiple perspectives
- **Review workflows** where implementer → reviewer → verifier handoff is needed
- **Parallel tracks** like frontend + backend implemented simultaneously
- **Coordination-critical work** where drift detection matters

## Prerequisites

Before orchestrating, ensure:

1. **Canonical profiles exist** for each role (`implementer`, `orchestrator`, `reviewer`, `verifier`)
2. **Workstream exists** with a valid seam
3. **Harnesses configured** — either one multi-role harness with `desired.supported_roles`, or separate harnesses with `desired.role_charter` set
4. **Session hygiene** — no stale sessions blocking the workstream

### Verify Setup

```bash
# Check harnesses have desired state
curl -s "http://localhost:8900/api/nodes?kind=harness" | jq '.[] | {id: .id, role: .properties.desired.role_charter}'

# Check profile definitions exist
curl -s "http://localhost:8900/api/nodes?kind=profile_definition" | jq '.[] | .id'

# Check role charters exist  
curl -s "http://localhost:8900/api/nodes?kind=role_charter" | jq '.[] | .id'
```

## The Orchestration Pattern

### Step 1: Create Workstream (once)

```bash
curl -s -X POST http://localhost:8901/mcp \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "tools/call",
    "params": {
      "name": "graph_create_node",
      "arguments": {
        "id": "workstream:feature-x",
        "kind": "workstream",
        "name": "Feature X Implementation",
        "properties": {
          "status": "active",
          "seam_id": "seam:feature-x"
        }
      }
    },
    "id": 1
  }'
```

### Step 2: Configure Harnesses (per agent)

Each agent needs a harness with `desired` properties:

```bash
# Implementer harness
curl -s -X POST http://localhost:8901/mcp \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "tools/call",
    "params": {
      "name": "graph_update_node",
      "arguments": {
        "id": "harness:windsurf-implementer",
        "properties": {
          "agent": "cascade",
          "runtime_kind": "windsurf",
          "canonical_profile": "implementer",
          "desired": {
            "role_charter": "implementer",
            "canonical_profile_name": "implementer",
            "skill_refs": ["session-hygiene", "graph-intelligence"],
            "projection_targets": [{"path": "/path/to/repo", "kind": "worktree"}]
          }
        }
      }
    },
    "id": 2
  }'
```

### Step 3: Start Sessions (per agent)

Each agent starts its own session against the same workstream:

```bash
# Agent 1: Implementer
curl -s -X POST http://localhost:8901/mcp \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "tools/call",
    "params": {
      "name": "session_start",
      "arguments": {
        "agent": "cascade",
        "session_id": "session:cascade:feature-x-impl",
        "workstream_id": "workstream:feature-x",
        "seam_id": "seam:feature-x",
        "harness_id": "harness:windsurf-implementer",
        "canonical_profile": "implementer"
      }
    },
    "id": 3
  }'

# Agent 2: Reviewer (can start immediately or after implementer completes)
curl -s -X POST http://localhost:8901/mcp \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "tools/call",
    "params": {
      "name": "session_start",
      "arguments": {
        "agent": "codex",
        "session_id": "session:codex:feature-x-review",
        "workstream_id": "workstream:feature-x",
        "seam_id": "seam:feature-x",
        "harness_id": "harness:codex-reviewer",
        "canonical_profile": "reviewer"
      }
    },
    "id": 4
  }'
```

### Step 4: Monitor Coordination Risk

The UI shows coordination risk when multiple sessions share a worktree:

```bash
# Check for coordination conflicts
curl -s http://localhost:8900/api/dashboard | jq '.active_sessions | group_by(.worktree_path) | map(select(length > 1)) | flatten | {conflict_count: length, conflicts: map({agent, worktree_path})}'
```

## Session Lifecycle for Multi-Agent

### Parallel Mode (simultaneous)

```
[Implementer] ──┐
                ├──→ [Workstream: feature-x]
[Orchestrator] ─┘
```

Both agents active simultaneously, coordinating via graph state.

### Sequential Mode (handoff)

```
[Implementer] → session_close → [Reviewer] → session_close → [Verifier]
```

Each agent starts only after previous completes.

### Hybrid Mode (review gates)

```
[Implementer] ──→ session_close → [Reviewer] ──→ approve ──→ [Implementer v2]
         ↑__________________________________________________________↓
```

Implementer responds to review feedback.

## Coordination Rules

### For Implementers

1. **Report telemetry** — files touched, lines changed, tests run
2. **Close cleanly** — don't leave stale sessions
3. **Document decisions** — use `graph_decide` for architectural choices
4. **Watch for reviewers** — check dashboard before claiming new work

### For Reviewers

1. **Start from implementation** — review the closed implementer session, not fresh
2. **Record findings** — use `graph_record_test_run` for verification
3. **Clear verdicts** — approve/reject with explicit reasoning
4. **Timebox** — don't block workstream indefinitely

### For Orchestrators

1. **Track the whole flow** — maintain awareness of all agents
2. **Resolve conflicts** — when two agents claim same seam
3. **Update workstream state** — record phase transitions
4. **Final closeout** — ensure all sessions closed, verification recorded

## UI Indicators

### Live Sessions Dashboard
- Shows all active sessions with their roles
- **Coordination risk badge** when >1 session shares worktree
- **Telemetry gaps** highlighted in amber

### Workstream Detail
- Shows linked sessions and their phases
- **Phase progress** bars for multi-step work

### Projected Profiles / Roles
- Count of harnesses per role shows team composition
- Runtime distribution across `windsurf`, `claude-code`, `codex`

## Cleanup

### End of Multi-Agent Work

```bash
# Close all sessions for workstream
curl -s "http://localhost:8900/api/nodes?kind=session" | jq '.[] | select(.properties.workstream_id == "workstream:feature-x") | .id' | while read id; do
  curl -s -X POST http://localhost:8901/mcp \
    -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"session_close\",\"arguments\":{\"session_id\":$id,\"summary\":\"Multi-agent work complete\"}},\"id\":99}"
done

# Update workstream status
curl -s -X POST http://localhost:8901/mcp \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "tools/call",
    "params": {
      "name": "graph_update_node",
      "arguments": {
        "id": "workstream:feature-x",
        "properties": {
          "status": "completed",
          "completed_at": "2026-04-06T12:00:00Z"
        }
      }
    },
    "id": 100
  }'
```

## Common Issues

### "Another agent is already working on this"

Check dashboard — either:
- Wait for them to finish (sequential mode)
- Coordinate parallel work on different seams
- Use `session_hygiene` skill to clean stale sessions

### Missing profile in Projected Profiles

Harness missing `desired` property. Fix:
```bash
curl -s -X POST http://localhost:8901/mcp \
  -H "Content-Type: application/json" \
  -d '{... update harness with desired.role_charter ...}'
```

### Coordination risk not showing

Sessions not sharing same `worktree_path`. Verify both sessions report same path in dashboard.

## API Reference

| Tool | Purpose |
|------|---------|
| `graph_create_node` | Create workstream |
| `graph_update_node` | Set harness desired state |
| `session_start` | Agent claims work |
| `session_activity` | Report progress |
| `session_close` | Release work |
| `graph_decide` | Record architectural decision |
| `graph_record_test_run` | Record verification |

| Endpoint | Purpose |
|----------|---------|
| `/api/dashboard` | View all sessions, coordination risks |
| `/api/nodes?kind=workstream` | List workstreams |
| `/api/nodes?kind=session` | List sessions |
| `/api/health/sessions` | Session hygiene check |
