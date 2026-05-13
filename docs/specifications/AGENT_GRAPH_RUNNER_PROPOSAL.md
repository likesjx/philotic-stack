# Agent Graph Runner — Per-Philote Spawn & Routing

## Status
Draft — ready for intel-graph scan

## Problem

`agent.graph.*` tools (`agent.graph.read`, `agent.graph.write`, `agent.graph.declare`,
`agent.graph.recall`, `agent.graph.sync`) are defined in the tool catalog and have a
complete implementation in `crates/agent-graph-runner/`, but are **unreachable** by any
philote today due to two missing pieces:

1. **No spawn path**: `agent-graph-runner` is a per-agent guest (requires `PHILOTIC_AGENT_ID`,
   stores data at `~/.philotic/agent-graph-{id}.db`). It is NOT in `seed_default_guests()` in
   `aiua/src/main.rs`. No hotel currently materialises it.

2. **Wrong routing**: In `philote/src/session/mod.rs`, `default_tool_assembly_for_bindings()`
   routes `agent.graph.*` tools via the generic `"capability"` path, yielding
   `target_role = "tool.agent.graph.read"`. The runner registers as `role = "agent-graph"`.
   The hotel has no guest with the capability-route target role → all calls hang.

## Design

### Spawn (aiua)

Add a `spawn_agent_graph_runner(agent_id, hotel_name, socket_path)` call from the philote
materialisation path in `aiua/src/service/ipc.rs`. The guest record:

```
guest_id = "{hotel_name}:agent-graph-{agent_id}"
role     = "agent-graph"
command  = "agent-graph-runner"
env:
  PHILOTIC_AGENT_ID        = agent_id
  PHILOTIC_GRAPH_RUNNER_ID = "{hotel_name}:agent-graph-{agent_id}"
  PHILOTIC_IPC_SOCKET      = socket_path
```

The guest lifecycle must track the owning agent — when the philote guest is deactivated,
its agent-graph-runner should be deactivated too.

### Routing (philote)

Add `fn is_agent_graph_tool(tool_name: &str) -> bool` in `philote/src/session/mod.rs`:

```rust
fn is_agent_graph_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "agent.graph.read"
            | "agent.graph.write"
            | "agent.graph.declare"
            | "agent.graph.recall"
            | "agent.graph.sync"
    )
}
```

Wire into the execution mode chain before the `else` fallback, with
`execution_mode = "agent_graph"` and `target_role = "agent-graph"`.

### Profiles

Once spawned and routed, expose to agents by adding to profile `allowed_tools`:
- `orchestrator`: `agent.graph.read`, `agent.graph.write`, `agent.graph.recall`
- `admin`: `agent.graph.read`, `agent.graph.write`
- `architect`: `agent.graph.read`

## Dead Tool Audit (from catalog review 2026-05-12)

| Tool | Status | Recommendation |
|---|---|---|
| `agent.graph.read/write/declare/recall/sync` | in catalog, NOT in profiles, runner not spawned | this proposal |
| `approval.request_standing` | always-on (injected regardless of profile) | no change needed |
| `routing.policy.propose` | in catalog, implied by `routing.refinement` skill | deprecate or implement handler |
| `rule.propose` | in catalog, no dispatch | deprecate |
| `mcp.provision/revoke` | in catalog, no runner | deprecate or implement |
| `desktop.observe` | pinned, no desktop guest materialised | remove from `is_pinned_tool` or implement |

## Slices

1. **Spawn path**: Add agent-graph-runner materialisation in aiua philote spawn path
2. **Routing fix**: Add `is_agent_graph_tool` in philote session routing
3. **Profile exposure**: Add tools to orchestrator/admin/architect `allowed_tools`
4. **Binary distribution**: Add `agent-graph-runner` to `AIUA_BINS` in justfile
5. **Smoke test**: Extend startup round-trip test to include agent.graph.read call
