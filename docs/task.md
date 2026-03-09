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

## New Project: Model Controller

- [x] Define the controller seam so mesh routing (`model.manager.*`) and provider invocation have separate owners.
- [x] Treat `model-router` as shared SDK/runtime infrastructure instead of the single materialized model guest.
- [x] Add a provider abstraction in `crates/model-router` and move Gemini invocation behind it.
- [x] Add separate materialized controller guests for Gemini (`model.gemini`) and ElevenLabs (`model.elevenlabs`).
- [x] Point the current text-generation path at the Gemini-specific controller role so multiple model guests do not all receive the same task.
- [x] Add a hotel-startup self-test path for routed ElevenLabs voice synthesis via `ansible --test voice-sample`.
- [x] Add a hotel-startup self-test path for text model-controller round-trips via `ansible --test text-roundtrip`.
- [x] Add a hotel-startup Telegram controller smoke via `ansible --test telegram-roundtrip` using a local fake Telegram API.
- [x] Extend the startup Telegram smoke so it simulates text, photo, and voice-note ingress and exercises fake-Gemini multimodal requests on top of blob-backed media transport.
- [ ] Define the canonical model task envelope for text, voice, structured output, and future multimodal requests.
- [ ] Add first-class audio artifact delivery from model controller through agent/hegemon or the future voice machine.
- [ ] Define the agent/model outbound rich-text contract so `agent-core` is not forced to emit transport-specific Markdown quirks for Telegram, WhatsApp, or future hegemon transports.
- [ ] Decide whether ElevenLabs stays in the model controller long-term or moves wholly behind the dedicated voice machine.

## Next Project: Tool Assembly and Routed Execution

- [ ] Review [TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md).
- [ ] Review [TOOL_MANAGEMENT_PLANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_MANAGEMENT_PLANE_PROPOSAL.md).
- [ ] Review [TASK_RUNNER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TASK_RUNNER_PROPOSAL.md).
- [ ] Review [RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RUNNER_ARTIFACT_BUILD_DISTRIBUTION_PROPOSAL.md).
- [x] Introduce a first-class `ToolAssembly` model with model-facing tool definitions and runtime-facing execution routes.
- [ ] Formalize the system tool management plane in the Context Graph:
  - known tool runners
  - runner incarnations
  - abstract tools
  - discovered hotel environments
  - agent default toolsets
  - session narrowing/hiding rules
- [x] Move real tool execution out of `agent-core` and behind routed tool runners/toolset components.
- [ ] Add runner readiness/materialization checks during tool assembly.
- [ ] Add environment-aware runner routing and materialization policy so tools can target non-IPC execution environments when needed.
- [ ] Define task-runner specialization for environment-bound execution:
  - workspace runners
  - shell runners
  - runner base config + agent/session overlays
  - unreachable-incarnation handling in routing/materialization policy rather than inside the runner
- [x] Keep local config/session mutation commands in `agent-core`, but externalize real tool execution.
- [x] Let session-scoped allowed runner incarnations derive visible tools and preassembled execution routes.
- [x] Add execution taxonomy for routed tools:
  - `local_agent`
  - `capability`
  - `pinned`
- [x] Land the first basic tool-family split:
  - `session.status` as `local_agent`
  - `echo` as `capability`
  - `workspace.list` and `workspace.read` as `pinned`
- [ ] Return to skill design after tool assembly and routed execution boundaries are in place.
- [x] Add agent-specific runner routing:
  - preferred environment
  - preferred hotel/node
  - preferred runner
  - route selection reason in `tool_assembly`
- [ ] Add runner fallback policy and smarter reroute behavior when the preferred route cannot materialize or should be bypassed.

## New Work Item: Telegram Controller

- [x] Research current Telegram Bot API capabilities and identify the transport boundary opportunity.
- [x] Review webhook ingress with a security-first lens before treating it as a default inbound mode.
- [x] Accept [TELEGRAM_INTEGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_INTEGRATION_PROPOSAL.md) for the current slice:
  - polling remains the default ingress
  - webhook support is deferred behind an explicit security contract
- [x] Review [HEGEMON_COMPONENT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HEGEMON_COMPONENT_PROPOSAL.md).
- [x] Define `hegemon` as a component type with transport-specific implementations rather than a single Telegram-named implementation.
- [x] Mark the generic `final_reply_role = "hegemon"` routing path as transitional and define the first replacement step:
  - optional guest-specific local delivery via `target_guest_id`
  - optional turn-level `final_reply_guest_id` preserved through agent/model/hegemon flow
- [x] Define how session bindings identify the owning hegemon component/incarnation for outbound delivery.
- [x] Define the normalized Telegram ingress envelope and prove it on the current text polling path.
- [x] Expand `hegemon` polling ingestion beyond `message.text` while keeping one canonical transport-normalization path.
- [x] Add an initial Telegram media transport step on top of normalized attachment metadata:
  - resolve Telegram `file_id` values via `getFile`
  - download attachment bytes
  - upload them into the hotel blob service and attach blob refs to the envelope
- [ ] Extend Telegram media transport:
  - watched live validation against real Telegram media
  - specialized downstream transcription/vision routing on top of the initial blob-backed media-analysis path
- [x] Add an initial downstream media-analysis path on top of blob-backed Telegram attachments:
  - route supported blob-backed attachments through `agent-core` as `media.analyze`
  - let the Gemini model-controller consume blob-backed media bytes for first-pass analysis
  - keep specialized voice transcription and richer vision workflows as follow-on work
- [x] Add an initial Telegram outbound formatting projector in `hegemon`:
  - translate a supported Markdown subset into Telegram-safe HTML `parse_mode`
  - apply it to outbound `sendMessage` replies
- [ ] Extend Telegram outbound formatting projection:
  - move from HTML-only projection toward explicit `entities` where it improves reliability
  - account for Telegram text and caption length limits with chunking or fallback behavior
- [ ] Elevate deterministic Telegram slash commands into `hegemon` before the normal agent loop.
- [ ] Add Telegram delivery primitives for typing state, partial streaming, and final message commit.
- [ ] Specify webhook config shape and verification behavior:
  - Telegram secret-token enforcement
  - request size/time limits
  - update dedupe/idempotency
  - tunnel/proxy deployment guidance
- [ ] Add targeted tests for Telegram normalization and webhook security gates.
- [ ] Run a Telegram smoke pass for command routing and partial/final delivery before broadening the transport story.

## Next Project: Personality and Context

- [ ] Review [PERSONALITY_AND_CONTEXT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/PERSONALITY_AND_CONTEXT_PROPOSAL.md).
- [ ] Review [ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/ZEROCLAW_TO_PHILOTIC_BRIDGE_PROPOSAL.md).
- [ ] Review [HEURISTIC_MIND_AND_CONTEXT_PAPER.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/HEURISTIC_MIND_AND_CONTEXT_PAPER.md).
- [x] Refactor `agent-core` prompt assembly into turn-time projection functions:
  - `project_agent_self`
  - `project_user`
  - `project_knowledge`
- [ ] Make skill and tool exposure goal-scoped turn projections instead of full inventory dumps when the goal is clear.
- [ ] Compose those projections with explicit context layers:
  - soul
  - identity
  - user context
  - memory context
  - session context
- [x] Add initial agent-level personality fields for:
  - `soul_text`
  - `identity_text`
  - `user_context_text`
  - `memory_summary`
- [ ] Define the import path from `openclaw.json`.
- [ ] Import legacy workspace bootstrap files when present:
  - `SOUL.md`
  - `IDENTITY.md`
  - `USER.md`
  - `MEMORY.md`
- [ ] Define how user continuity should follow an identified person across sessions.
- [ ] Define memory retrieval layers for:
  - context graph
  - knowledge graph
  - hippocampal / episodic memory
  - heuristic memory backends
  - tool-runner local indexes
- [ ] Define projection profiles for:
  - conversational agents
  - workers
  - subagents
- [ ] Keep the first implementation slice personality-first; do not try to solve the full memory backend story in the same change.
- [ ] Build the first ZeroClaw/OpenClaw bridge slice:
  - [ ] import one agent from `openclaw.json`
  - [x] ingest `SOUL.md`, `IDENTITY.md`, `USER.md`, and `MEMORY.md`
  - [x] store them as Philotic compatibility inputs
  - [x] expose the imported Jane profile through the canonical session snapshot
  - [ ] materialize the imported agent in the Philotic web
  - [ ] verify recognizable identity continuity

## Next Work Item: Muninn Heuristic Memory Experiment

- [ ] Review [MUNINN_MEMORY_PROTOCOL_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MUNINN_MEMORY_PROTOCOL_PROPOSAL.md).
- [ ] Review [MUNINN_CLIENT_MEMORY_PROTOCOL.md](/Users/jaredlikes/code/philotic-stack/docs/reference/MUNINN_CLIENT_MEMORY_PROTOCOL.md).
- [x] Validate the local Muninn MCP handshake and core tool calls.
- [x] Establish a default Muninn retrieval/write-back habit for Codex.
- [x] Create a shared helper script for Muninn MCP transport and tool invocation.
- [x] Create a shareable client skill/instruction package for adopting the helper-backed Muninn protocol.
- [ ] Wire the helper into at least one additional cognitive client beyond Codex.
- [ ] Measure whether Muninn materially improves continuity, personalization, and decision recall over repeated sessions.
- [ ] Decide whether Muninn remains an external heuristic memory service or should inform a future Philotic-native memory layer.

## Deferred Design Threads

- [ ] Agent workflow formalization: adopt a standing Codex process for context gathering, slice sizing, verification ladders, watched live runs, proposal disposition updates, per-slice commit/push discipline, and assumption-vs-reality capture.
- [ ] Proposal lifecycle hygiene: add concise `Disposition` sections and task/work-item links to active architecture proposals.
- [ ] Skill/rules optimization loop: define a lightweight end-of-slice review for instruction gaps, reusable skills, and recurring reality-gap lessons.
- [x] Add executable workflow commands for the trusted vertical slice and operator checklist.
- [ ] Command Center / architect continuity: define how architecture-impact work should be surfaced to Aria once the new home is ready.
- [ ] Fresh onboarding flow: design repo/bootstrap onboarding from scratch for a new operator or agent entering Philotic.
- [ ] `openclaw.json` ingestion: define a migration/import path that can consume legacy agent manifests and materialize Philotic agents.
- [ ] Context graph deployment model: decide local-first vs cloud-backed vs hybrid graph ownership, sync, and operational model.
- [ ] Context graph decentralization: decide how much of the graph can be replicated/federated across hotels versus kept locally authoritative.
- [ ] Approval UX evolution: add `/preapprove`, `/approval status`, `/approval reset`, and richer session policy editing for constrained transports like Telegram.
- [ ] Review [TELEGRAM_INTEGRATION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TELEGRAM_INTEGRATION_PROPOSAL.md).
- [ ] Review [VOICE_MACHINE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/VOICE_MACHINE_PROPOSAL.md).
- [ ] Telegram approval card UX: include request IDs, tool/action names, args summaries, and resolution messages in a more native Telegram approval experience.
- [ ] Voice machine design: define STT, TTS, speech-to-speech, transcript generation, and media artifact/session handling.
- [ ] Nostr communication-plane investigation: evaluate Nostr as a decentralized/event-native transport, with security and privacy-first scrutiny before any implementation.
- [ ] Loop incarnations and delegation model: explore whether different agent loops should materialize as specialized agent incarnations, and define how delegation, subagents, and loop-specialized workers relate to the primary conversational agent.
- [ ] Tool runner lifecycle policy: define idle retention, sleep/teardown timing, wake-up thresholds, and environment-specific materialization rules for routed tools.
- [ ] Runner artifact plane: define builder trust, sandboxing, testing, signing, release, and distribution policy for executable tool runners.
- [ ] Review [RUST_FORGE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/RUST_FORGE_PROPOSAL.md) and decide whether a sandboxed `forge` runner family should eventually scaffold, build, publish, and propose mesh integration for Rust-based components.
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
