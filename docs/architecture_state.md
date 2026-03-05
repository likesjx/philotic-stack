# ZeroClaw Mesh Architecture Handoff & State

**Target Spec**: Evolving ZeroClaw into an Ansible Mesh SOA where nodes expose local capabilities (Tools, Models, Ansible Infra) via UDP.
_(Note: "Ansible" refers doubly to the Speaker for the Dead quantum meshops framework for runtime communication, AND Red Hat Ansible for the physical infrastructure provisioning of the mesh nodes)._

**Core Paradigm**: The network acts as a "Philotic Web". Local devices run a single ZeroClaw Beacon ("The Hotel") which accepts dynamic capability checks-ins ("Guests") at runtime through a **Hybrid Extensibility** model: standalone OS Processes via Local UDP IPC, and embedded WebAssembly `.wasm` plugins.

## Current State of the Codebase

All mesh components are currently housed in a new workspace crate: `crates/ansible-mesh-core/`.

### MVP 1 (Complete): Base UDP Transport & Capabilities

- **Envelopes**: `BeaconMessage` defines the UDP envelope (with routing, sequence, and payload).
- **Daemon (`beacon.rs`)**: `BeaconDaemon` listens on a UDP port (e.g., `8999`). It accepts a `node_capabilities.json` configuration defining its local `ToolRef`, `ModelRef`, and `NodeRole`s.
- **CLI Integration (`src/mesh.rs`, `src/main.rs`)**: The daemon is wired into the `zeroclaw` CLI under `zeroclaw mesh run`. We successfully executed and bound the daemon locally.
- **Agent Types (`agent.rs`, `runtime.rs`)**: Scaffolded `AgentBundle`, `AgentIdentity`, and trait stubs for `AgentRuntime` and `ToolInvoker`.

### MVP 2 (Complete): Routing & Discovery

- **Heartbeats (`heartbeat.rs`, `registry.rs`)**: Built the `NodeRegistry` which tracks living mesh nodes and their advertised capabilities. `BeaconDaemon` handles incoming `MsgType::Heartbeat` parsing to populate the registry.
- **Model Manager (`model_manager.rs`)**: Implemented `ModelManagerInvoker` handling `model.manager.list@1` and `model.manager.route@1`. Queries the local `NodeRegistry` to route inferences to nodes offering requested models.
- **Ansible Meshops Controller (`meshops.rs`)**: Created `MeshopsNodeInvoker` to catch instantaneous quantum-framework `ansible.mesh.broadcast@1` mesh requests.
- **Mesh Client Adapter (`adapter.rs`)**: Implemented `MeshAdapter` which allows the `zeroclaw` chat components to bind an ephemeral UDP socket and send `MsgType::ToolCall` envelopes over the mesh to a remote beacon.

## Next Steps: MVP 3 (Context Graph & iOS Profiling)

The immediate next milestones for any agent resuming this work are:

1.  **Context Graph DB**: Scaffold the memory primitives (`MemoryApartment` struct and queries) inside `crates/ansible-mesh-core/src/graph.rs` or a specialized crate.
2.  **Context Graph Tools**: Expose `memory.read@1`, `memory.write@1`, `memory.summarize@1` over the ToolInvoker interface.
3.  **iOS Apple Silicon Mock Profile**: Define `ios_capabilities.json` outlining HealthKit, iOS Contacts, and a localized on-device Swift LLM `ModelRef`.
4.  **Local Memory Integration**: Refactor the existing ZeroClaw `src/memory/mod.rs` to query the `MeshAdapter` for episodic persistence instead of treating SQLite as the sole monolith store.
