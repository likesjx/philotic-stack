# `model-router` — LLM Provider Routing Guest

`model-router` is a materialized guest process responsible for routing language
model inference requests to available model providers on the mesh.

## Responsibilities

- Register with the hotel (identity: `model-router-gemini-01`)
- Expose `model.manager.list@1` — enumerate available models across the mesh
- Expose `model.manager.route@1` — select best node for a given model + constraints
- Execute model inference calls against chosen provider
- Return results as `TaskResult` events to the requesting agent

## Routing Logic (MVP)

1. Receive `ModelRouteRequest { task, constraints, preferred_models }`
2. Query `NodeRegistry` for active nodes with matching `ModelRef`
3. Return first match as `ModelRouteResponse { model_ref, endpoint_node, invocation_params }`

Routing constraints supported: `latency_ms`, `privacy`, `cost_tier`.

## Running

Spawned automatically by `GuestManager`. For development:

```bash
cargo run -p model-router -- --ansible-port 9000
```
