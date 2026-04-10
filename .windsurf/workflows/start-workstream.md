---
description: Start a new workstream with multi-role harness support. Auto-creates seam, workstream, and sessions for all roles declared in harness desired.supported_roles.
---

# Start Workstream (Multi-Role)

Use this workflow when you want to start a workstream with the windsurf-native multi-role harness. It:

1. Creates a seam (stable structural boundary)
2. Creates a workstream linked to the seam  
3. Reads harness `desired.supported_roles`
4. Auto-creates sessions for each role

## Prerequisites

- `harness:windsurf-native` must exist with `desired.supported_roles` array
- Graph server running on localhost:8900/8901

## Usage

Say: **"start a workstream for [name]"**

Or manually:
```bash
just start-workstream WORKSTREAM_NAME
```

## What It Does

### 1. Create Seam
```bash
curl -s -X POST http://localhost:8901/mcp \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "tools/call",
    "params": {
      "name": "graph_create_node",
      "arguments": {
        "id": "seam:{{WORKSTREAM_NAME}}",
        "kind": "seam",
        "name": "{{WORKSTREAM_NAME}}",
        "properties": {"domain": "product-management-plane", "status": "active"}
      }
    },
    "id": 1
  }'
```

### 2. Create Workstream
```bash  
curl -s -X POST http://localhost:8901/mcp \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "tools/call",
    "params": {
      "name": "graph_create_node",
      "arguments": {
        "id": "workstream:{{WORKSTREAM_NAME}}",
        "kind": "workstream", 
        "name": "{{WORKSTREAM_NAME}}",
        "properties": {
          "status": "active",
          "seam_id": "seam:{{WORKSTREAM_NAME}}",
          "harness_id": "harness:windsurf-native"
        }
      }
    },
    "id": 2
  }'
```

### 3. Get Harness Config
```bash
SUPPORTED_ROLES=$(curl -s "http://localhost:8900/api/nodes/harness:windsurf-native" \
  | jq -c '.properties.desired.supported_roles // ([.properties.desired.role_charter] | map(select(. != null))) // ["implementer"]')
```

### 4. Create Sessions for Each Role
```bash
for role in $(echo "$SUPPORTED_ROLES" | jq -r '.[]'); do
  curl -s -X POST http://localhost:8901/mcp \
    -H "Content-Type: application/json" \
    -d "{
      \"jsonrpc\": \"2.0\",
      \"method\": \"tools/call\",
      \"params\": {
        \"name\": \"session_start\",
        \"arguments\": {
          \"agent\": \"cascade\",
          \"session_id\": \"session:cascade:{{WORKSTREAM_NAME}}-$role\",
          \"workstream_id\": \"workstream:{{WORKSTREAM_NAME}}\",
          \"seam_id\": \"seam:{{WORKSTREAM_NAME}}\",
          \"harness_id\": \"harness:windsurf-native\",
          \"canonical_profile\": \"$role\"
        }
      },
      \"id\": 1
    }"
done
```

## Justfile Recipe

This alias should exist in `justfile`:

```make
# Start a new workstream with multi-role harness support
start-workstream NAME:
    @echo "Creating workstream: {{NAME}}"
    # Create seam
    @curl -s -X POST http://localhost:8901/mcp \
        -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"graph_create_node","arguments":{"id":"seam:{{NAME}}","kind":"seam","name":"{{NAME}}","properties":{"domain":"product-management-plane","status":"active"}},"id":1}' > /dev/null
    @echo "✓ Seam created: seam:{{NAME}}"
    
    # Create workstream
    @curl -s -X POST http://localhost:8901/mcp \
        -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"graph_create_node","arguments":{"id":"workstream:{{NAME}}","kind":"workstream","name":"{{NAME}}","properties":{"status":"active","seam_id":"seam:{{NAME}}","harness_id":"harness:windsurf-native"}},"id":2}' > /dev/null
    @echo "✓ Workstream created: workstream:{{NAME}}"
    
    # Get supported roles from harness
    @SUPPORTED_ROLES=$(curl -s "http://localhost:8900/api/nodes/harness:windsurf-native" | jq -r '.properties.desired.supported_roles // [.properties.desired.role_charter] // ["implementer"]' | jq -r '.[]'); \
    for role in $$SUPPORTED_ROLES; do \
        curl -s -X POST http://localhost:8901/mcp \
            -H "Content-Type: application/json" \
            -d "{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"session_start\",\"arguments\":{\"agent\":\"cascade\",\"session_id\":\"session:cascade:{{NAME}}-$$role\",\"workstream_id\":\"workstream:{{NAME}}\",\"seam_id\":\"seam:{{NAME}}\",\"harness_id\":\"harness:windsurf-native\",\"canonical_profile\":\"$$role\"}},\"id\":3}" > /dev/null; \
        echo "✓ Session created: $$role"; \
    done
    
    @echo ""
    @echo "Workstream ready. View at: http://localhost:8900"
```

## Verification

After running:

```bash
# Check dashboard
curl -s http://localhost:8900/api/dashboard | jq '{
  workstream: .active_sessions[0].workstream_id,
  seam: .active_sessions[0].seam_id,
  roles: [.active_sessions[].canonical_profile] | unique
}'

# Expected output:
# {
#   "workstream": "workstream:your-name",
#   "seam": "seam:your-name", 
#   "roles": ["implementer", "orchestrator", "reviewer", "verifier"]
# }
```

## Related

- **Skill**: `@skills/windsurf-harness-setup` — Configure harness multi-role support
- **Skill**: `@skills/multi-agent-orchestration` — Coordinate multiple agents
