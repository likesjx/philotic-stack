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

- [ ] Review [MODEL_CONTROLLER_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack-model-controller-abstraction/docs/architecture/MODEL_CONTROLLER_PROPOSAL.md).
- [x] Land the first `voice.synthesize` request envelope with `display_text`, `spoken_text`, `voice`, `model`, and `provider_options`.
- [x] Add an upstream producer example that emits the richer `voice.synthesize` envelope through the hotel.
- [ ] Define the canonical capability envelope for:
  - `text.generate`
  - `voice.synthesize`
  - `voice.dialogue`
  - `sound.generate`
  - `music.generate`
  - `response.generate`
- [x] Propose the first structured model request envelope split:
  - `response_contract`
  - `context`
  - `affordances`
  - `routing_hints`
  - `provider_options`
- [x] Implement the first compatibility-first structured model envelope seam in `model-router`.
- [x] Propose the first structured model response envelope split:
  - `result`
  - `artifacts`
  - `trace`
  - `provider_output`
- [x] Define optimization-oriented response channels:
  - `display_text`
  - `spoken_text`
  - `working_memory_delta`
  - `follow_up_questions`
  - `intent_summary`
  - `state_updates`
  - `delivery_hints`
- [x] Define canonical context layers for model requests:
  - `instructions`
  - `identity`
  - `memory`
  - `dialogue_window`
  - `active_turn`
  - `attachments`
- [x] Decide that tools and skills should project separately from semantic context under `affordances`.
- [x] Implement the first compatibility-first structured model response seam in `model-router`.
- [x] Preserve a first-class minimal prompt-response path with the structured envelope fields remaining mostly optional.
- [x] Add initial `response_contract.channels` handling for text-generation requests.
- [x] Add first structured response fields for:
  - `display_text`
  - `spoken_text`
  - `working_memory_delta`
  - `follow_up_questions`
- [ ] Add `spoken_text` / expressive speech projection alongside user-visible text.
- [ ] Define ElevenLabs default-voice pinning plus upstream voice override behavior.
- [ ] Add Eleven v3 model selection and expressive-tag support without pretending it is the same as the low-latency conversational path.
- [ ] Define how native-audio multimodal models emit text plus audio without being forced through TTS.
- [ ] Define Gemini auth modes:
  - hotel-managed OAuth
  - API key fallback
  - possible ADC path
- [x] Add the first Gemini guest auth abstraction that prefers OAuth bearer config over API key fallback.
- [x] Add `ansible --test` startup harness support for:
  - `text-roundtrip`
  - `gemini-oauth-roundtrip`
  - `telegram-roundtrip`
  - `voice-sample`
- [x] Add a startup-driven model-controller smoke script for the text round-trip path.
- [x] Add a startup-driven Gemini OAuth smoke through the materialized model-controller guest.
- [x] Add a hotel-startup Telegram controller smoke via `ansible --test telegram-roundtrip` using a local fake Telegram API.
- [x] Extend the startup Telegram smoke so it simulates text, photo, and voice-note ingress and exercises fake-Gemini multimodal requests on top of blob-backed media transport.
- [x] Prove watched-live Telegram text/photo/voice/document delivery through hegemon -> agent-core -> Gemini and normalize markdown-ish document MIME for Gemini media analysis.
- [x] Make materialized Telegram/agent guests configurable enough for separate hotel/persona stacks (for example Jane vs Aria) instead of hardcoding one Jane-shaped membrane.
- [ ] Seed `hotels.aria-architect-hotel.context_graph.telegram_bot_token` in `mesh-config.json` and run the first watched-live Aria hotel Telegram poller on its own bot token.
- [ ] Make agent-level media routing policy configurable so text/media/voice decisions are owned by the agent/session profile instead of one hardcoded runtime branch.
- [ ] Investigate splitting voice-note transcription/understanding toward ElevenLabs or another speech-specialized provider while keeping richer text reasoning in the agent/model loop.
- [ ] Define hotel CLI OAuth UX:
  - browser launch
  - temporary localhost callback listener
  - token exchange
  - token storage/refresh
  - guest handoff
- [x] Add a transitional `ansible auth google start --provider gemini` flow with browser launch, localhost callback, token exchange, and access-token persistence.
- [x] Add a hotel-side Gemini OAuth validation command that performs a real model call with the stored auth path.
- [x] Refresh model-controller provider config per task so updated Gemini auth takes effect without a guest restart.
- [ ] Run a Keychain-backed Gemini OAuth smoke with `PHILOTIC_VAULT_MASTER_KEY` unset.
- [x] Run a full guest-path Gemini OAuth smoke through the materialized model-controller, not just hotel-side validation.
- [ ] Wire refresh-token persistence and refresh lifecycle behind the hotel vault.
- [ ] Deliver an honest ElevenLabs end-to-end voice path beyond inline-audio/testing mode.

## New Project: Key Vault

- [ ] Review [KEY_VAULT_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack-model-controller-abstraction/docs/architecture/KEY_VAULT_PROPOSAL.md).
- [x] Define the first vault record schema and context-graph secret references.
- [x] Begin removing new OAuth access-token storage from plain `node_config` by storing secret refs instead.
- [x] Define and implement the first hotel-local secret fetch API for guests.
- [x] Define and implement the first envelope-encryption and root-key strategy:
  - OS keychain / TPM / Secure Enclave preferred
  - cloud KMS/HSM for hosted hotels
  - operator master key only as fallback
- [ ] Replace the current local root-key bootstrap with stronger platform-native backing beyond basic macOS Keychain item storage.
- [ ] Define secret lifecycle:
  - create
  - stage
  - rotate
  - revoke
  - rollback
- [ ] Move Gemini OAuth refresh tokens behind vault references.
- [x] Move Gemini OAuth access tokens behind vault references for model-controller consumption.
- [ ] Define Telegram-safe secret onboarding:
  - control-plane command in chat
  - Mini App or secure browser handoff
  - no plaintext secret entry in normal chat messages
- [ ] Define Telegram-safe rotation UX and operator approvals.

## Next Project: Tool Assembly and Routed Execution

- [ ] Review [TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_ASSEMBLY_EXECUTION_PROPOSAL.md).
- [ ] Review [TOOL_MANAGEMENT_PLANE_PROPOSAL.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/TOOL_MANAGEMENT_PLANE_PROPOSAL.md).
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
- [ ] Telegram slash-command elevation: raise deterministic `/commands` into `hegemon` before the normal agent loop so Telegram-side testing and operational control become faster and cleaner.
- [ ] Telegram approval card UX: include request IDs, tool/action names, args summaries, and resolution messages in a more native Telegram approval experience.
- [ ] Telegram streaming and media UX: define partial delivery, edits vs follow-up messages, and interruption behavior for Telegram replies.
- [ ] Voice machine design: define STT, TTS, speech-to-speech, transcript generation, and media artifact/session handling.
- [ ] Nostr communication-plane investigation: evaluate Nostr as a decentralized/event-native transport, with security and privacy-first scrutiny before any implementation.
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
