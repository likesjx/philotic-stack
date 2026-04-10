# Agent Session Tracking Protocol

## Overview

Every agent work session MUST be tracked in the graph intelligence system. This provides real-time visibility into active work, progress metrics, and workstream lifecycle management.

## Session Lifecycle

### 1. Session Start (REQUIRED at beginning of work)

Call `session_start` at the very beginning of every agent work session:

```javascript
// Example session_start call
fetch('/mcp', {
  method: 'POST',
  body: JSON.stringify({
    jsonrpc: '2.0',
    method: 'tools/call',
    params: {
      name: 'session_start',
      arguments: {
        session_id: `session:${new Date().toISOString().slice(0,10).replace(/-/g,'')}-${Date.now()}-${agentId}`,
        agent: 'cascade-kimi-k2.5',           // Your agent identifier
        agent_model: 'kimi-k2.5',             // Model being used
        seam_id: 'seam:embeddings-intel-graph-ui-20260329',  // Workstream seam
        task_id: 'task:task-md-123',           // Specific task (optional)
        phase: 'started'                      // Initial phase
      }
    }
  })
});
```

### 2. Record Activity (during work)

Call `session_activity` to track progress:

**File edits:**
```javascript
session_activity({
  session_id: 'session:...',
  activity_type: 'file_edit',
  details: {
    files: ['ui/index.html', 'server/mcp.rs'],
    lines_changed: 45
  }
});
```

**Test runs:**
```javascript
session_activity({
  session_id: 'session:...',
  activity_type: 'test_run',
  details: {
    tests_passed: 12,
    tests_failed: 0
  }
});
```

**Phase changes:**
```javascript
session_activity({
  session_id: 'session:...',
  activity_type: 'phase_change',
  phase: 'testing'  // started → coding → testing → green
});
```

### 3. Session Close (REQUIRED at end)

Call `session_close` when work is complete:

```javascript
session_close({
  session_id: 'session:...',
  status: 'completed',  // or 'cancelled', 'blocked'
  verified: 'test-green', // test-green, smoke-green, watched-live-green
  summary: 'Fixed performance issues in workstreams dashboard, added live session tracking'
});
```

## Workstream Phases

| Phase | Meaning | When to use |
|-------|---------|-------------|
| `started` | Session created, initial planning | First 5-10 min of session |
| `coding` | Active code changes | When editing files |
| `testing` | Writing/running tests | When adding tests or verification |
| `green` | All tests passing | Ready for verification ladder |

## UI Views

- **Workstreams**: Card view of all workstreams with live sessions
- **Status Board**: High-density hospital-style board showing all active workstreams with alert levels
- **Timeline**: Mutation history with session attribution

## Alert Levels (Status Board)

- **Critical (red)**: No active session on workstream
- **Attention (amber)**: Session in `started` phase (idle/planning)
- **Stable (green)**: Active coding/testing session

## Required Actions Summary

1. ✅ Call `session_start` at beginning of work
2. ✅ Call `session_activity` after significant edits/tests
3. ✅ Call `session_close` at end with final status

## Example Complete Session

```javascript
// 1. START
const sessionId = `session:${Date.now()}-cascade`;
await session_start({
  session_id: sessionId,
  agent: 'cascade-kimi-k2.5',
  seam_id: 'seam:my-workstream',
  phase: 'started'
});

// 2. ACTIVITY - coding phase
await session_activity({
  session_id: sessionId,
  activity_type: 'phase_change',
  phase: 'coding'
});

// Edit files...
await session_activity({
  session_id: sessionId,
  activity_type: 'file_edit',
  details: { files: ['foo.rs'], lines_changed: 32 }
});

// 3. ACTIVITY - testing phase
await session_activity({
  session_id: sessionId,
  activity_type: 'phase_change',
  phase: 'testing'
});

await session_activity({
  session_id: sessionId,
  activity_type: 'test_run',
  details: { tests_passed: 8 }
});

// 4. CLOSE
await session_close({
  session_id: sessionId,
  status: 'completed',
  verified: 'test-green',
  summary: 'Implemented feature X, all tests passing'
});
```
