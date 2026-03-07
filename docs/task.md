# Ansible Mesh Architecture Separation Task List

- [x] Inspect the `openclaw-plugin-ansible` repository and identify the rust port branch.
- [x] Understand the current architecture of the ansible mesh.
- [x] Design the separation of the stack into independently addressable units:
  - [x] chat/communication (including telegram)
  - [x] agent loop + context + session
  - [x] memory
  - [x] models
  - [x] mcp/tools
  - [x] context graph
- [x] Review code for communication and split points.
- [x] Create an implementation plan for the new services architecture.

## Current Work Item Split

### WI 1: Session Management

- [x] Decide that session state has one canonical home in the Context Graph; apartment checkpoints are derived recovery projections, not a second source of truth.
- [x] Generalize session as a cross-component coordination envelope rather than an agent-only transcript.
- [x] Add graph-modeled session entities for session lifecycle, participants, and turns.
- [x] Bind transport identities in `hegemon` to stable `session_id` values.
- [ ] Add session leases / ownership semantics for active work.
- [x] Persist session timeline/progress events while keeping the IPC plane general.
- [x] Support recovery flows at the session layer.
- [x] Support approval flows at the session layer.

### WI 2: Agent Logic

- [ ] Implement the bounded ZeroClaw-style loop in `agent-core`.
- [ ] Build context from session snapshot plus memory apartments.
- [ ] Execute tools with approval-aware flow control.
- [x] Keep local working turn state in the agent during execution.
- [x] Use `SyncApartment` as periodic derived snapshot/checkpoint sync back to the Context Graph, not as canonical session ownership.
- [ ] Add compaction/checkpoint policy so apartment sync stays structured and reasonably small.
- [x] Add slash-command short-circuiting for deterministic agent/system commands before the normal model loop.
- [x] Add approval interrupts with explicit history and a pre-approval runtime path.

## New Project: Philotic Agent Loop

- [ ] Write a dedicated proposal for the Philotic loop architecture using Pi as the core turn-engine reference.
- [ ] Write an implementation spec for loop state, events, checkpoints, tools, and approval interrupts.
- [ ] Define the provider boundary (`transformContext`, `convertToLlm`, tool/result records, structured outputs).
- [ ] Define the bounded execution loop and checkpoint boundaries.
- [ ] Define approval interrupt/resume semantics.
- [ ] Define loop event streaming and tracing payloads.

## Next Project: Tool Assembly and Routed Execution

- [ ] Review [TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md).
- [ ] Review [TOOL_MANAGEMENT_PLANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_MANAGEMENT_PLANE_PROPOSAL.md).
- [ ] Review [RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md).
- [ ] Introduce a first-class `ToolAssembly` model with model-facing tool definitions and runtime-facing execution routes.
- [ ] Formalize the system tool management plane in the Context Graph:
  - known tool runners
  - abstract tools
  - discovered hotel environments
  - agent default toolsets
  - session narrowing/hiding rules
- [ ] Move real tool execution out of `agent-core` and behind routed tool runners/toolset components.
- [ ] Add runner readiness/materialization checks during tool assembly.
- [ ] Add environment-aware runner routing and materialization policy so tools can target non-IPC execution environments when needed.
- [ ] Keep local config/session mutation commands in `agent-core`, but externalize real tool execution.
- [ ] Return to skill design after tool assembly and routed execution boundaries are in place.

## Deferred Design Threads

- [ ] Agent workflow formalization: adopt a standing Codex process for context gathering, slice sizing, verification ladders, watched live runs, and decision capture.
- [ ] Command Center / architect continuity: define how architecture-impact work should be surfaced to Aria once the new home is ready.
- [ ] Fresh onboarding flow: design repo/bootstrap onboarding from scratch for a new operator or agent entering Philotic.
- [ ] `openclaw.json` ingestion: define a migration/import path that can consume legacy agent manifests and materialize Philotic agents.
- [ ] Context graph deployment model: decide local-first vs cloud-backed vs hybrid graph ownership, sync, and operational model.
- [ ] Context graph decentralization: decide how much of the graph can be replicated/federated across hotels versus kept locally authoritative.
- [ ] Approval UX evolution: add `/preapprove`, `/approval status`, `/approval reset`, and richer session policy editing for constrained transports like Telegram.
- [ ] Tool runner lifecycle policy: define idle retention, sleep/teardown timing, wake-up thresholds, and environment-specific materialization rules for routed tools.
- [ ] Runner artifact plane: define builder trust, sandboxing, testing, signing, release, and distribution policy for executable tool runners.
- [ ] Memory consolidation / dreaming: define how short-term session state becomes long-term memory, including sleep/dream cycles, compaction, and candidate memory backends such as `scryper/miniminddb`.

## MVP 1: Single-Node Mesh & Basic Tools

- [x] Scaffold the `ansible-mesh-core` crate.
- [x] Define the UDP `BeaconMessage` transport envelope.
- [x] Implement the beacon daemon listener.
- [x] Implement the `AgentBundle` struct and a basic `AgentRuntime`.
- [x] Build a simple `ToolInvoker` with local mock tools.
- [x] Set up a hard-coded `node_capabilities.json` for testing.

## MVP 2: Multi-Node Mesh + Model Manager + Ansible

- [x] Implement `node_capabilities` sync and simple health/heartbeat.
- [x] Build the initial Component Model Manager node exposing `model.manager.list` and `model.manager.route`.
- [x] Integrate the Rust Ansible port as `ansible.mesh.*` tools on a designated node.
- [x] Update the Zeroclaw orchestrator adapter to call remote tools (e.g., `mesh.tool_call("ansible.mesh.broadcast")`).

## MVP 3: Context Graph & Edge Client

- [x] Scaffold the memory primitives (`MemoryApartment` struct and queries) inside `crates/ansible-mesh-core/src/graph.rs`.
- [x] Expose `memory.read@1`, `memory.write@1` over the ToolInvoker interface.
- [x] Define `ios_capabilities.json` outlining HealthKit, iOS Contacts, and a localized on-device Swift LLM `ModelRef`.
- [x] Refactor the existing ZeroClaw `src/memory/mod.rs` to query the `MeshAdapter` for episodic persistence.

## MVP 4: Infrastructure Provisioning (Red Hat Ansible)

- [x] Initialize `ansible/` directory structure (inventory, playbooks, roles).
- [x] Create `roles/mesh_node/` to install dependencies (Rust, systemd).
- [x] Create `deploy_mesh_node.yml` playbook to compile and run ZeroClaw `mesh run` as a service.

## Phase 1: Making it Ours (The Hegemon Workspace)

- [x] Initialize new Rust crates to build the Philotic architecture alongside the legacy code.
- [x] Formalize the Cargo Workspace (Monorepo) structure:
  - `crates/ansible` (The Hotel Manager / local UDP/IPC Event Bus).
  - `crates/hegemon` (The primary CLI and routing logic).
  - `crates/philotic-ipc` (The IPC client library for child processes).
- [x] Port the essential `ansible-mesh-core` MVP 3 logic into the new `crates/ansible` bin.
- [x] Leave the legacy `src/` monolith untouched for reference and gradual migration.

## Phase 2: Universal Materialization (The Hotel, Guests, & Context Graph)

- [x] Implement a concrete, disk-backed `ContextGraph` store (e.g. SQLite/RocksDB/Sled) to hold the entire system configuration, identities, and memory apartments.
- [x] Scaffold the `PhiloticClient` (IPC) to allow external Guests (MCP Wrappers, Agent Personas) to connect to the local Ansible.
- [x] Connect the `Hegemon` Telegram poller to the Philotic Web via the new IPC trait.
- [x] Close the UDP Request/Response loop (Ansible Echoes back `MsgType::Result` to Hegemon).
- [x] Write a sample Python and Rust MCP wrapper that registers tools dynamically with the local Ansible.
- [x] Implement Agent Materialization: Refactor the runtime to spawn child OS processes dynamically from graph data.

## Phase 3: The End-to-End Philotic Stack (Telegram -> Agent -> Model)

We will materialize the core ZeroClaw pipeline as three completely independent binaries that communicate exclusively over the Ansible's UDP IPC:

### 1. The Gateway (Telegram Hegemon)

- [x] Port the legacy `TelegramChannel` struct from `src/channels/telegram.rs` into the `crates/hegemon` binary.
- [x] Connect the `Hegemon` Telegram poller to the Philotic Web via the `UdpPhiloticClient`.
- [x] Ensure inbound messages over Telegram are translated to `IpcRequest::EmitTask` and routed to the Agent persona.
- [x] Refactor the long-polling loop to read the `bot_token` via an IPC config pull rather than the static `config.toml`.

### 2. The Persona (Agent Core)

- [x] Create a new `crates/agent-core` binary in the workspace.
- [x] Implement the core agent loop (receiving a prompt, building context) checking in as a Guest.
- [x] When the agent receives a task from Telegram, it queries the Model capabilities via an IPC `EmitTask`.

### 3. The Mind (Model Router)

- [x] Create a new `crates/model-router` binary in the workspace.
- [x] Implement the Gemini API payload constructor for text generation.
- [x] Subscribe to Model invocation tasks over IPC, trigger inference, and pass the text back to Hegemon.uter receives an inference task from the Agent, it calls the Gemini API and routes the text response back via IPC.

## Phase 4: The Philotic Split & Metaphor Visualization

Now that the End-to-End Philotic architecture is complete, we need to separate it from the legacy monolith and create visual documentation.

### 1. Repository Separation

- [x] Create a new repository for the Philotic architecture.
- [x] Migrate the `ansible`, `hegemon`, `agent-core`, `model-router`, `philotic-ipc`, and `ansible-mesh-core` crates to the new repository.
- [x] Ensure the legacy ZeroClaw/OpenClaw code remains accessible in the original repository as a reference for migrating future capabilities (tools, MCPs).

### 2. Veo3 Metaphor Video

- [x] Brainstorm the visual concepts for the Veo3 video explaining the system metaphors in motion.
- [x] Draft a storyboard artifact documenting the scenes (The Universal Materialization, The Hotel, The Ansible, The Guests).
- [x] Refine prompts for Veo3 video generation based on the storyboard.
