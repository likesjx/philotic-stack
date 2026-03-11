# Zeroclaw Mesh Beacon Architecture & SOA Implementation Plan

The goal is to evolve the ZeroClaw monolith into a Service-Oriented Architecture (SOA) where different capabilities operate as independently addressable nodes/units on a WireGuard-based mesh VPN (Tailscale). Zeroclaw serves as the primary brain/router, while other nodes act as remote capabilities (MCP/hands, models, context).

## Proposed Architecture (Addressable Roles/Units)

The system is designed around a **Local Hotel** model (Actor System with a Distributed Event Bus), heavily inspired by the Speaker for the Dead "Ansible/Philotic Web" framework.

- **The Ansible (The Hotel Manager)**: Each physical device (Mac, VPS, iPhone) runs exactly one lightweight Rust daemon natively binding to a local configuration database and the WireGuard IP. This daemon acts as the **Ansible**.
- **The Empty Boot & Universal Materialization**: The Ansible boots completely empty. It reads the local Context Graph database for its assigned capabilities. It then **literally spawns a new OS child process** (or WASM sandbox) for every single capability it is assigned to run.
- **The Guests (Dynamic Capabilities)**: Every capability—_Telegram Pollers, Agent Personas, Gemini Routers, MCP Wrappers_—is a dynamically materialized "Guest". They wake up, connect to the local Ansible via IPC, pull their specific state/keys from the Context Graph, and begin listening to the Philotic Web.

This prevents the monolith from needing to compile in every provider. It supports a **Hybrid Extensibility** model:

1. **Multi-Process Extensibility (IPC)**: External scripts (Python MCPs, Node.js tools, standalone Rust binaries like the Telegram poller) execute as separate OS processes and check in by firing UDP/UDS packets to the local Ansible.
2. **WebAssembly Extensibility (WASM)**: Extremely fast, sandboxed hooks and data parsers compile to `.wasm` files. The local Ansible's embedded WASM engine dynamically loads them into its own memory space on the fly.

### 1. Context Graph (Agent + Memory + Configuration Substrate)

- **Role:** The canonical, always-on store for Agent identity, Memory, and global Mesh configuration.
- **Location:** The database engine (e.g. SQLite `rusqlite`) runs natively and exclusively inside the **Ansible Daemon** process. This ensures the configuration is available 24/7 in the background without requiring the CLI to be open.
- **Function:**
  - **System Configuration:** Holds the global state of the mesh (authorized nodes, roles, secrets pointers). Because this lives in the always-on Ansible, the daemon can independently materialize Guests (like the Telegram listener) on boot without user intervention.
  - **Identities:** Stores first-class agent personas (`soul.md`, `identity.md`, WASM hooks).
  - **Memory Apartments:** Allocates a namespace (apartment) for each agent's short-term, long-term, episodic, and semantic memory. Accessible via mesh tools (`memory.read`, `memory.write`).
- **Component File:** Functioning Local/Disk-backed KV or Graph store module (`src/memory/graph.rs` mounted inside `crates/ansible`).

### 2. Chat / Communication Gateway (Materializing Membrane)

- **Role:** Edge connectors to external chat networks (Telegram, Discord, Slack, Matrix).
- **Function:** If an Ansible is assigned the "Membrane" role in the Config Graph, it spawns a Telegram listener child process. This process pulls the `bot_key` and agent-mapping from the Graph, listens to webhooks, and translates user texts into tasks dropped onto the Philotic Web.

### 3. Agent Personas + Context + Session (Materializing Guests)

- **Role:** Portable intelligence bundles materialized by an Ansible.
- **Function:** The mesh supports _many_ agent personas (e.g., "Jane", "Ender", "Alai"), not just one. When an Ansible receives a `MATERIALIZE_AGENT` command from the Philotic Web, it **spawns a new OS child process** (or WASM sandbox) out of thin air to physically "materialize" that persona on its host machine.
- **Materialization Loop:** The new process wakes up, uses the IPC capability to pull its `identity.md` and specific session memory straight from the Context Graph, and then checks itself into the local Ansible as a verified Guest to execute its assigned task.

### 4. Model Context & Routing (Materializing the Mind)

- **Role:** Centralized inference routing and provider wrappers.
- **Function:** If an Ansible is assigned a model provider role (e.g., Gemini), it spawns a provider child process. This process pulls the API Keys (or OAuth tokens) from the Context Graph/Secrets Authority. It listens on the Philotic Web for `model.manager.route` requests.
- **OAuth Considerations:** Because the Gemini provider is now an isolated OS process, the ZeroClaw CLI's `auth login` command must update the central Context Graph database with the refreshed OAuth tokens, which the Gemini child process subscribes to (or pulls on every inference).

### 5. Ansible Meshops Node (First-Class Network Controller)

- **Role:** Dedicated mesh network controller node (`roles: ["ansible-node", "meshops"]`).
- **Function:** Exposes instantaneous Ansible Mesh communication capabilities as first-class mesh tools (`ansible.mesh.broadcast`, `ansible.mesh.locate_agent`, `ansible.mesh.telemetry`) via the beacon's tool invoker. Wraps the Rust port of the `openclaw-plugin-ansible` meshops framework (inspired by Speaker for the Dead).
- **Component Files:** `src/meshops.rs` and the broader `openclaw-plugin-ansible/rust-core` port.

### 5b. Infrastructure Deployment (Red Hat Ansible)

_Note on terminology: While "Ansible Meshops" refers to the Speaker for the Dead instantaneous communication network during runtime, we will utilize **Red Hat Ansible** (the IT automation tool) as the primary deployment vehicle to bootstrap and provision the mesh nodes themselves (installing beacons, WireGuard, Secrets)._

### 6. MCP Server Wrappers ("Hands")

- **Role:** Environment-specific capability execution.
- **Function:** Legacy Model Context Protocol (MCP) servers (e.g., shell, browser automation, GitHub integrations) are wrapped and checked into the Hotel as Guests. The local beacon daemon listens for `TOOL_CALL` messages on the Philotic Web and routes them to the local MCP wrapper process (via IPC) to execute the work.
- **Component Files:** `src/tools/*.rs`, `src/hardware/*.rs`, and specific iOS MCP implementations.

### 7. Secrets Authority Node

- **Role:** Centralized secret management.
- **Function:** Stores secrets by role, node, and tool. Nodes send a `SECRET_PULL` request on boot to obtain necessary keys for their assigned roles/tools.

## Phased Implementation (MVP Stages)

### MVP 1: Single-Node Mesh & Basic Tools

- **Focus:** Foundation and base protocol.
- **Tasks:**
  - Scaffold the Rust Beacon daemon.
  - Define the UDP `BeaconMessage` transport envelope and basic ACK/retry logic.
  - Implement the `AgentBundle` struct and a basic `AgentRuntime` that echoes input.
  - Build a simple `ToolInvoker` with 1-2 local mock tools.
  - Use a hard-coded `node_capabilities.json`.
- **Goal:** Prove end-to-end flow: Zeroclaw -> beacon -> tool -> back.

### MVP 2: Multi-Node Mesh + Model Manager + Ansible

- **Focus:** Coordination and Routing.
- **Tasks:**
  - Deploy beacons on 2-3 machines over Tailscale/WireGuard.
  - Implement dynamic `node_capabilities` sync and simple health/heartbeat.
  - Build the initial Model Manager node exposing `model.manager.list` and `model.manager.route`.
  - Integrate the Rust Ansible port as `ansible.mesh.*` tools on a designated node.
  - Update the Zeroclaw orchestrator adapter to call remote tools (e.g., `mesh.tool_call("ansible.mesh.broadcast")`).
- **Goal:** Real mesh operations controlled by agents, with model routing mediated by the Model Manager.

### MVP 3: Context Graph + iOS Beacon + Whisper

- **Focus:** Memory, Identity, and Edge Devices.
- **Tasks:**
  - Build the Context Graph service: Agent nodes and Memory Apartments (`memory.read/write/summarize`).
  - Prototype the iOS beacon: VPN client, Contacts MCP, Speech MCP (SpeechAnalyzer), Foundation Models MCP.
  - Deploy a custom fine-tuned Whisper model (`model.whisper-custom-small@1`) on a home-lab node.
- **Goal:** A cohesive system where a portable agent on iOS can process speech, access local health context (securely), recall long-term memories via the Context Graph, and offload heavy reasoning to the mesh.

### Phase 4: Repository Separation (The Philotic Stack)

- **Focus:** Clean break from legacy monolithic architecture into a specialized mesh routing ecosystem.
- **Strategy:**
  1. Initialize a completely new Git repository named `philotic-stack` (or similar).
  2. Extract the newly built `crates/` directory (`ansible`, `membrane`, `agent-core`, `model-router`, `philotic-ipc`, `ansible-mesh-core`) into the new workspace.
  3. Ensure the core UDP/UDS communication protocols build successfully devoid of legacy dependencies.
  4. Leave the current `zeroclaw`/`openclaw` monorepo perfectly intact. Coding agents can run parallel views to use the legacy code inside `src/` (e.g., Slack channels, Notion integrations, original MCP Hands) as an exact reference when migrating those capabilities to the new Philotic stack as discrete Guests.
- **Goal:** Maintain an unpolluted baseline for the Philotic architecture while retaining the vast legacy monolithic code as a migration guidebook.
