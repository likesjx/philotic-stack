name: windsurf-harness-setup
description: Configure windsurf-native harness with multi-role support. One harness declares all 4 canonical roles (implementer, orchestrator, reviewer, verifier) and auto-spins up sessions when workstream starts.

# Windsurf Harness Setup (Multi-Role)

Scope: Configure `harness:windsurf-native` as a multi-role harness using `desired.supported_roles`.

One harness can declare multiple roles it supports. When starting a workstream, this harness auto-creates sessions for all declared roles.

## Pattern: Multi-Role Single Harness

```
harness:windsurf-native
├── desired.supported_roles: ["implementer", "orchestrator", "reviewer", "verifier"]
├── desired.projection_targets: [{"path": "/repo", "kind": "worktree"}]
└── runtime_kind: windsurf

        ↓ (auto-derived on workstream start)

4 Projected Profiles          4 Role Charters           4 Sessions (auto-created)
windsurf/implementer         implementer (1 harness)   session:cascade:xyz-implementer
windsurf/orchestrator        orchestrator (1 harness)  session:cascade:xyz-orchestrator  
windsurf/reviewer            reviewer (1 harness)      session:cascade:xyz-reviewer
windsurf/verifier            verifier (1 harness)      session:cascade:xyz-verifier
```

## Configure Harness

```bash
cat > /tmp/update_harness.json << 'EOF'
{
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "name": "graph_update_node",
    "arguments": {
      "id": "harness:windsurf-native",
      "properties": {
        "agent": "cascade",
        "runtime_kind": "windsurf",
        "desired": {
          "supported_roles": ["implementer", "orchestrator", "reviewer", "verifier"],
          "skill_refs": ["session-hygiene", "graph-intelligence", "multi-agent-orchestration"],
          "projection_targets": [
            {"path": "WORKTREE_PATH", "kind": "worktree"},
            {"path": "AGENTS_MD_PATH", "kind": "file"}
          ]
        }
      }
    }
  },
  "id": 1
}
EOF
curl -s -X POST http://localhost:8901/mcp -H "Content-Type: application/json" -d @/tmp/update_harness.json
```

## Attach Harness Workflow (Outside Any Workstream)

Use this when you want to attach or refresh a harness before any workstream exists. This only updates the harness's desired/rendered/observed state; it does **not** create a seam, workstream, or session.

```bash
# Inspect current harness state
just phil harness status harness:windsurf-native

# Plan the desired attachment
just phil harness plan harness:windsurf-native --profile orchestrator

# Apply the desired attachment and render the local projection
just phil harness apply harness:windsurf-native --profile orchestrator

# Verify the local projection and refresh observed state
just phil harness verify harness:windsurf-native

# Optional: check for drift
just phil harness drift harness:windsurf-native
```

If you are attaching a named bundle instead of a profile, replace `--profile orchestrator` with `--bundle <bundle-name>`.

## Start Workstream Workflow

Use this workflow when you want to start a workstream with the windsurf-native multi-role harness:

### Step 1: Create Workstream + Seam

```bash
# Create seam first (seam is the stable structural boundary)
curl -s -X POST http://localhost:8901/mcp \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "tools/call",
    "params": {
      "name": "graph_create_node",
      "arguments": {
        "id": "seam:WORKSTREAM_NAME",
        "kind": "seam",
        "name": "WORKSTREAM_NAME",
        "properties": {
          "domain": "product-management-plane",
          "status": "active"
        }
      }
    },
    "id": 2
  }'

# Create workstream linked to seam
curl -s -X POST http://localhost:8901/mcp \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "tools/call",
    "params": {
      "name": "graph_create_node",
      "arguments": {
        "id": "workstream:WORKSTREAM_NAME",
        "kind": "workstream",
        "name": "WORKSTREAM_NAME",
        "properties": {
          "status": "active",
          "seam_id": "seam:WORKSTREAM_NAME",
          "harness_id": "harness:windsurf-native"
        }
      }
    },
    "id": 3
  }'
```

### Step 2: Create Sessions for All Roles

The harness's `supported_roles` array determines which sessions to create:

```bash
# For each role in harness desired.supported_roles:
id=1
for role in implementer orchestrator reviewer verifier; do
  curl -s -X POST http://localhost:8901/mcp \
    -H "Content-Type: application/json" \
    -d "{\n      \"jsonrpc\": \"2.0\",\n      \"method\": \"tools/call\",\n      \"params\": {\n        \"name\": \"session_start\",\n        \"arguments\": {\n          \"agent\": \"cascade\",\n          \"session_id\": \"session:cascade:WORKSTREAM_NAME-$role\",\n          \"workstream_id\": \"workstream:WORKSTREAM_NAME\",\n          \"seam_id\": \"seam:WORKSTREAM_NAME\",\n          \"harness_id\": \"harness:windsurf-native\",\n          \"canonical_profile\": \"$role\"\n        }\n      },\n      \"id\": $((id++))\n    }"
done
```

## What the UI Shows

After configuration:

### Projected Profiles
| Profile | Runtime | Canonical | Role | Harnesses |
|---------|---------|-----------|------|-----------|
| windsurf / implementer | windsurf | implementer | implementer | 1 |
| windsurf / orchestrator | windsurf | orchestrator | orchestrator | 1 |
| windsurf / reviewer | windsurf | reviewer | reviewer | 1 |
| windsurf / verifier | windsurf | verifier | verifier | 1 |

### Role Charters  
| Role | Runtimes | Harnesses |
|------|----------|-----------|
| implementer | windsurf | 1 |
| orchestrator | windsurf | 1 |
| reviewer | windsurf | 1 |
| verifier | windsurf | 1 |

### Live Sessions
- Shows 4 active sessions all linked to same workstream
- Coordination risk badge if they share worktree

## Compared to Multi-Harness Pattern

| Pattern | Harnesses | Use Case |
|---------|-----------|----------|
| **Multi-Role Single Harness** (this) | 1 | Same runtime, same repo, team of agents |
| **Multi-Harness** | 4 | Different runtimes (windsurf + codex + claude-code) |

## Verification

```bash
# Check harness config
curl -s "http://localhost:8900/api/nodes/harness:windsurf-native" | jq '.properties.desired.supported_roles'

# Check derived profile definitions
curl -s "http://localhost:8900/api/nodes?kind=profile_definition" | jq '.[] | select(.properties.runtime_kind == "windsurf") | .name'

# Check active sessions
curl -s http://localhost:8900/api/dashboard | jq '.active_sessions | map({agent, harness_id, canonical_profile})'
```

## API Reference

| Property | Type | Description |
|----------|------|-------------|
| `desired.supported_roles` | string[] | Roles this harness can spawn |
| `desired.projection_targets` | object[] | Where projections render |
| `desired.skill_refs` | string[] | Skills available to all roles |

| Tool | Purpose |
|------|---------|
| `graph_update_node` | Set harness `desired` properties |
| `graph_create_node` | Create seam, workstream |
| `session_start` | Spawn role-specific session |
