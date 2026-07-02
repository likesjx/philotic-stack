---
title: 'Philotic Web: The Mesh, Management Plane, and Distribution'
doc_type: proposal
domain: product-management-plane
status: proposed
disposition: accepted-current-slice
last_updated: 2026-03-31
tags:
- philotic-web
- aiua
- management-plane
- distribution
- homebrew
- security
- naming
- cli
- pki
- vpn
- nat-traversal
- mesh
related_docs:
- ARCHITECTURE.md
- ARCHITECTURE_STATUS.md
- CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md
- HOMEBREW_DISTRIBUTION_PROPOSAL.md
- RUNTIME_AUTHORITY_LEASES_PROPOSAL.md
- PERIMETER_EGRESS_CONTROL_PROPOSAL.md
- PHILOTE_MEMORY_CORE_PROPOSAL.md
- PORT_BLUEPRINT.md
task_refs:
- docs/task.md
proposal_id: philotic-web
implements:
- homebrew-distribution
- control-plane-admin-surface
supersedes:
- homebrew-distribution
active_seams:
- binary-rename-ansible-to-aiua
- philotic-web-crate
- management-plane-security
- distribution-pipeline
- repo-identity
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Philotic Web: The Mesh, Management Plane, and Distribution

## Goal

Define the product identity, naming architecture, management plane design, security model, and distribution strategy for Philotic Web — the operator-facing face of the Philotic Stack. Establish `aiua` as the canonical hotel daemon name, `philotic-web` as both **the mesh itself** and the CLI that governs it, and the Philotic Stack repo as the home for both. Define the distribution path across Homebrew, Cargo, Nix, and Docker. Set the security model for remote multi-node management as a first-class constraint.

## What philotic-web Is

**philotic-web is the mesh** — not a service running on top of the mesh, but the fabric of interconnected aiua nodes itself. The CLI (`philotic-web`) is the operator's interface into that fabric. A future HTTP dashboard would be another interface into the same thing. "Connecting to the philotic-web" means your aiua is mesh-joined.

This distinction matters for naming and mental model. You don't install a philotic-web server. You enroll nodes *into* the philotic-web. The web is the emergent thing; the nodes are the participants.

## Disposition

`proposed`

Track implementation in [docs/task.md](/docs/task.md).

## Why This Matters

The Philotic Stack is a distributed AI agent OS. Its power is the mesh — a collection of nodes that cooperate to materialize agents, route work, and share memory. But today there is no coherent operator surface for the mesh as a whole. Each hotel is managed by hand. Config is a JSON file. Distribution is `cargo run`.

This proposal defines what operators install, what they run, and how they govern the full philotic web — not just one hotel at a time.

Three problems are solved simultaneously:

1. **Naming collision.** The hotel daemon binary is named `ansible`, which conflicts with Red Hat Ansible in every package manager. This is not a future problem — it is a current blocker for distribution.
2. **No management plane.** Operators have no tool to inspect, configure, or act on remote aiua nodes. Every operation requires SSH access to the target machine. This does not scale.
3. **No distribution story.** There is no install path for operators who are not Rust developers. Homebrew, Nix, and Docker are all blocked on naming and release automation.

## Naming Architecture

The product hierarchy mirrors the Speaker for the Dead philotic web metaphor — not decoratively, but structurally:

| Name | Role | What It Is |
|------|------|------------|
| **Philotic Web** | The product | The distributed system as a whole — a mesh of connected aiua nodes |
| **aiua** | The node daemon | The hotel daemon running on each machine (replaces `ansible`) |
| **philote** | The agent | An individual AI agent materialized and supervised by an aiua |

In Xenocide and Children of the Mind, the **aiua** is the organizing soul of a philote-web — the principle that binds individual philotes into a coherent self. The hotel daemon is exactly this: the thing that takes individual agent-philotes and makes them a living, coordinated node.

The **philotic web** is the mesh of aiua nodes. `philotic-web` is the CLI tool that lets an operator govern that mesh.

### What Does Not Change

- The repo remains `philotic-stack`. It contains the full stack: `philotic-web`, `aiua`, `agent-core`, `membrane`, `model-router`, `tool-runner`, `ansible-mesh-core`, `philotic-client`. The repo name is accurate and does not need to match the product or binary names.
- `ansible-mesh-core` retains its name. It is the shared mesh primitives library — "ansible" there refers to the quantum communication concept and does not collide with the Red Hat tool at the binary or package level.

### The Binary Rename

`crates/ansible/` becomes `crates/aiua/`. The package name in `Cargo.toml` changes from `ansible` to `aiua`. All other changes are downstream of this. The existing IPC socket, DB file, and string literals referencing `ansible` are updated as part of the rename slice.

| Before | After |
|--------|-------|
| `crates/ansible/` | `crates/aiua/` |
| binary: `ansible` | binary: `aiua` |
| `ansible_context.db` | `aiua_context.db` |
| `/tmp/philotic-ansible.sock` | `/tmp/philotic-aiua.sock` |
| `just start-ansible` | `just start-aiua` |

## The philotic-web CLI

`philotic-web` is a new crate in the workspace: `crates/philotic-web/`. It is the operator's single entry point for running, configuring, and governing the entire mesh.

### Why It Lives in This Repo

`philotic-web` is not a thin wrapper. It needs deep knowledge of the Context Graph schema, aiua IPC protocol, mesh event types, lease structures, vault operations, and action grant contracts. These are all defined in `ansible-mesh-core`. Keeping `philotic-web` in the same workspace means:

- Direct dependency on `ansible-mesh-core` — no cross-repo version dance
- Atomic refactors when protocol types change
- Single release pipeline tags and bottles everything together
- `cargo build --workspace` produces all binaries

### Operator Workflow

```bash
# Install
brew install jaredlikes/philotic/philotic-web

# Bootstrap a new aiua node
philotic-web init --hotel my-node --config mesh-config.json

# Start the local aiua daemon
philotic-web start

# Inspect the local node
philotic-web status
philotic-web guests list
philotic-web sessions list

# Inspect the full mesh (via Context Graph)
philotic-web mesh status
philotic-web mesh nodes
philotic-web mesh agents

# Manage a remote node
philotic-web --node vps-jane guests restart membrane
philotic-web --node vps-jane secrets rotate gemini

# Admin elevation
philotic-web admin elevate
philotic-web admin vault secret add --provider gemini
```

### Relationship to the Existing Admin Surface

`CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md` defined the admin surface as part of the hotel daemon itself. `philotic-web` is the realization of that proposal — the CLI/TUI that surfaces admin capabilities, mints action grants, and routes elevated operations to the target aiua. The boundary holds: `philotic-web` mints and presents grants; `aiua` validates, authorizes, persists, and executes. `philotic-web` never holds secret material.

### Workspace Layout

```
crates/
  aiua/              # hotel daemon (renamed from ansible)
  philotic-web/      # operator CLI (new)
  agent-core/        # cognitive loop guest
  membrane/          # Telegram/external gateway guest
  model-router/      # model provider routing guest
  tool-runner/       # tool execution guest
  ansible-mesh-core/ # shared mesh primitives (name unchanged)
  philotic-client/   # guest SDK
```

## CLI + Optional Web Server

`philotic-web` is a CLI-first tool. The operator surface is terminal-native. However, the CLI can optionally spawn a local web server for operators who want a browser-based dashboard:

```bash
philotic-web serve              # start local web UI on http://127.0.0.1:7700
philotic-web serve --port 7700 --open   # open browser automatically
```

The web server is strictly a local process serving a read-mostly dashboard — it speaks to the local aiua over UDS, the same as the CLI. It is **not** a public-facing service and **not** a management API. Remote node management always goes through the CLI's mTLS management port, never through the web server. The web server has no elevated permissions that the CLI does not already have.

This keeps the security surface clean: CLI → UDS for local, CLI → mTLS for remote, browser → localhost CLI process for dashboard. The browser never touches a remote aiua directly.

## Mesh Topology Management

philotic-web is the operator tool for mesh topology — enrolling nodes, migrating guests, merging or splitting context graphs.

### Node Lifecycle

```bash
philotic-web mesh join <node>        # enroll an aiua into the philotic-web
philotic-web mesh leave <node>       # detach cleanly; guests migrated or stopped
philotic-web mesh status             # topology view across all enrolled nodes
philotic-web mesh isolate <node>     # revoke mesh participation without stopping node
```

### Guest Migration

Guest migration is a first-class primitive — stop a guest on one node, export its apartment state from the source context-graph, import to the target, rematerialize:

```bash
philotic-web guest migrate <guest-id> --from <node-a> --to <node-b>
```

The mesh updates routing automatically after migration completes.

### Merge and Split

```bash
philotic-web mesh merge <node-a> <node-b>   # pull node-b context-graph into node-a
philotic-web mesh split <node> --guests <g1,g2> --target <new-node>
```

Merge uses the existing LWW conflict resolution model. The `EventStorage` ledger is the audit trail for conflict resolution. Split is cleaner — a context-graph subset export followed by guest migration.

## Security Model

Remote multi-node management is the highest-privilege surface in the system. A tool that can reach any aiua and issue management commands is equivalent to root access across the mesh. The security model must be designed before the protocol, not retrofitted after.

### Threat Model

The security model is designed against these threat scenarios:

1. **Compromised operator credential** — a stolen `philotic-web` credential should not grant mesh-wide root access
2. **Man-in-the-middle** — management traffic between `philotic-web` and a remote aiua must not be interceptable or replayable
3. **Compromised node** — a compromised aiua should not be able to issue management commands to peer nodes
4. **Escalation via agent** — an agent (philote) must never be able to trigger management-plane operations without explicit operator elevation
5. **Replay attack** — captured action grants must not be replayable

### Auth Stack: Layered, Not Exclusive

Three auth paths converge on the same session token the hotel issues after verification:

| Path | Use Case | Trust Basis |
|------|----------|-------------|
| **PKI keypair** | Programmatic, automated, serious admin | Operator holds private key; hardware key supported |
| **Username/password** | Human operators who don't want to manage keys | Argon2-hashed in `node_config` |
| **OAuth (optional)** | Convenience wrapper — offloads password reset, MFA | External IdP vends token; hotel treats it as a password-equivalent |

OAuth is not required. It is a convenience layer for operators who want to offload credential management to a provider they already trust (GitHub, Google). The hotel does not depend on any external IdP.

### PKI Architecture

**Node identity — Ed25519 keypair per aiua:**
- Generated at first boot; private key never leaves the node
- Stored encrypted in `node_config` under a protected key slot
- Public key is the node's identity fingerprint — printed on first run, pinned by the CLI on enrollment
- The node's cert is its **mesh membership credential** — not just a TLS detail

**Operator identity — CLI keypair:**
- `philotic-web init` generates an operator Ed25519 keypair
- Stored in `~/.philotic/identity/` — optionally backed by OS keychain or YubiKey
- Operator public key registered in the hotel's `node_config` on enrollment

**Mesh CA:**
- `philotic-web ca init` — creates a local CA keypair that lives only on the operator machine, never on a node
- `philotic-web node enroll <node>` — node generates a CSR, CA signs it, cert installed back on node
- Mesh peers validate against the CA cert — simple, auditable chain of trust
- A compromised node cannot forge a CA-signed cert for a peer

### Auth Model: mTLS + Action Grants

Every management connection between `philotic-web` and a remote aiua uses **mutual TLS (mTLS)**. Both sides present certificates. The aiua refuses connections from unknown clients.

On top of the transport layer, operations use the **Action Grant** pattern already defined in `CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL.md`:

```json
{
  "grant_id": "grant_01...",
  "principal_id": "operator:likesjx",
  "hotel_id": "vps-jane",
  "action_class": "vault.secret.rotate",
  "action_target": "provider:gemini",
  "status": "active",
  "issued_at": 1741782000,
  "expires_at": 1741782300,
  "nonce": "random-one-time",
  "one_time_use": true
}
```

Action grants are:
- **Short-lived** — default 5 minutes, configurable per action class
- **One-time-use** — nonce prevents replay
- **Scoped** — bound to a specific hotel, action class, and action target
- **Principal-bound** — tied to the operator identity, not the session

### Management Port

The existing IPC socket (`/tmp/philotic-aiua.sock`) is for intra-hotel guest-to-hotel communication. Remote management uses a **separate management port** — not the guest IPC protocol and not the mesh UDP port. This prevents management traffic from mixing with guest traffic.

| Port | Protocol | Purpose |
|------|----------|---------|
| UDS `/tmp/philotic-aiua.sock` | JSON IPC | Intra-hotel guest ↔ hotel |
| 8999 (UDP) | Beacon | Inter-hotel mesh gossip |
| Management port (TBD) | mTLS + JSON | `philotic-web` ↔ remote aiua |

The management port number is assigned in the Port Blueprint. The protocol is mTLS-wrapped newline-framed JSON, consistent with the existing IPC framing convention.

### Authorization Tiers

| Operation Class | Who Can Issue | Elevation Required |
|----------------|--------------|-------------------|
| `status.inspect` | Any authenticated operator | No |
| `guests.list` | Any authenticated operator | No |
| `guests.restart` | Operator with node access | Session elevation |
| `secrets.rotate` | Admin-elevated operator | Admin elevation + action grant |
| `secrets.add` | Admin-elevated operator | Admin elevation + action grant |
| `mesh.topology.mutate` | Admin-elevated operator | Admin elevation + action grant |
| `node.shutdown` | Admin-elevated operator | Admin elevation + action grant |
| `break-glass` | Local console only | Physical access |

### Audit Trail

Every management operation — read or write — is appended to the aiua's event ledger as a `ManagementEvent`. The event is:
- Immutable once written
- Attributed to a principal and grant ID
- Timestamped with monotonic clock
- Replicated to the mesh event bus for cross-hotel visibility

Agents (philotes) have no access to the management event ledger. It is a control-plane concern, not a cognitive one.

### Local IPC Auth — Challenge/Response over UDS

For local operations (same machine), PKI replaces password even for the UDS path:

- Hotel issues a nonce on each new connection
- CLI signs `nonce + timestamp + operation` with operator key
- Hotel verifies signature against registered operators before processing any privileged IPC
- Even local root access cannot forge requests without the operator key

### Secrets — Use What Exists

`philotic-web` never invents a new secret store. All secret material lives in aiua's existing encrypted `node_config` slots and MuninnDB vault registry. The CLI provides ergonomic access:

```bash
philotic-web secret set gemini_api_key
philotic-web secret list
philotic-web vault ls
philotic-web vault rotate <vault-id>
```

Envelope encryption: `philotic-web secret set` encrypts the value with the node's public key before sending over the wire. The hotel decrypts with its private key. Plaintext never travels in the clear and is never stored in CLI history or config files.

### Credential Bootstrap

The first-run problem: how does `philotic-web` get credentials to connect to a new aiua?

```bash
# On the node — first boot prints node fingerprint:
aiua init → node fingerprint: ed25519:abc123...

# On operator machine:
philotic-web init                                          # generate operator keypair
philotic-web enroll jane-vps --fingerprint ed25519:abc123  # trust anchor ceremony
```

After that one enrollment ceremony, all subsequent IPC is cryptographically grounded. No passwords, no shared secrets. The operator key can be on hardware.

- **Key rotation:** `philotic-web admin keys rotate` — generates a new keypair, registers the new public key with all known nodes, revokes the old key.

This is intentionally conservative for the first slice. Automated certificate provisioning (via mesh CA or external PKI) is deferred.

## VPN and NAT Traversal

The philotic-web mesh must work across network boundaries — nodes behind NAT, on different cloud providers, on mobile networks. The VPN/NAT traversal layer is what makes the mesh a real distributed system rather than a LAN-only toy.

### Architecture

The traversal stack mirrors WireGuard + STUN/TURN concepts, adapted for the philotic-web mesh:

- **Each aiua node runs a lightweight WireGuard peer** — the philotic-web mesh overlay network
- **Node-to-node communication uses the overlay** — mesh gossip, management traffic, and guest routing all flow over the VPN tunnel once established
- **NAT traversal via STUN/hole-punching** — nodes behind NAT negotiate a direct P2P path; fall back to relay if direct path fails
- **Relay nodes** — well-connected nodes (e.g., a VPS like jane-vps) can optionally serve as relay for nodes that cannot establish direct paths

### Integration with PKI

The WireGuard public key *is* derived from the node's Ed25519 identity key — same keypair, different use. This means:
- Mesh enrollment (`philotic-web node enroll`) simultaneously registers the node's management cert **and** its VPN peer key
- No separate VPN credential management
- Revoking a node's mesh cert revokes its VPN access

### Operator Commands

```bash
philotic-web mesh vpn status              # show overlay topology and tunnel health
philotic-web mesh vpn add-relay <node>    # designate a node as relay
philotic-web mesh vpn diagnose <node>     # NAT traversal diagnostic for a peer
```

### Relationship to Existing Mesh UDP

The current BeaconMessage UDP gossip on port 8999 is LAN/trusted-network only. The VPN overlay replaces or wraps it for cross-network deployments. Nodes on the same LAN can continue to use direct UDP; nodes across the internet use the overlay tunnel.

### Blast Radius Containment

- A compromised aiua node cannot issue management commands to peer nodes — management authority flows from `philotic-web` down to aiua, never laterally between aiua nodes.
- `philotic-web` never holds decrypted secret material — it presents grants; aiua executes against its own vault.
- Node isolation: `philotic-web mesh isolate <node>` revokes that node's mesh participation without affecting other nodes.

## Local Model Infrastructure

The philotic-web vision includes on-device inference as a first-class capability — reducing API dependency, enabling offline operation, lowering cost, and keeping sensitive data local. Three local model subsystems are planned:

### WhisperKit — On-Device ASR

[WhisperKit](https://github.com/argmaxinc/WhisperKit) runs OpenAI Whisper models on Apple Silicon via Core ML. It replaces the ElevenLabs transcription path for macOS/iOS nodes.

**Integration point:** `model-router` gains a `whisperkit` provider that handles `TaskKind::AudioTranscribe`. When a node has WhisperKit available, the model-router preference order puts it before the remote ElevenLabs path.

**philotic-web CLI:**
```bash
philotic-web models whisperkit install    # download and install a WhisperKit model
philotic-web models whisperkit status     # show installed model + benchmark
philotic-web models whisperkit set-default <model>
```

**Configuration in mesh-config.json:**
```json
{
  "local_models": {
    "whisperkit": {
      "enabled": true,
      "model": "openai_whisper-large-v3-turbo",
      "compute_units": "cpuAndNeuralEngine"
    }
  }
}
```

### EmbeddingsG — On-Device Embeddings

Local embedding generation for MuninnDB semantic search — removes the dependency on an external embeddings API for every attend-hook write and recall query.

**Model target:** A small, fast embedding model runnable on Apple Silicon (e.g., `nomic-embed-text`, `all-MiniLM-L6-v2` via Core ML or GGML). Latency matters more than accuracy here — the attend hook is fire-and-forget but embedding latency directly impacts recall responsiveness.

**Integration point:** MuninnDB's REST client in `memory-core` gains an optional local embeddings path. When `EmbeddingsG` is available, the `memory-core` engine generates embeddings locally before writing to MuninnDB, bypassing MuninnDB's own embedding step.

**philotic-web CLI:**
```bash
philotic-web models embeddings install <model>
philotic-web models embeddings benchmark
philotic-web models embeddings set-default <model>
```

### FunctionG — On-Device Function Calling

A local model capable of structured output and tool/function calling — enabling agents to use tools without sending every tool-selection decision to a remote API. Target use cases: low-latency tool routing, privacy-sensitive tool calls, offline fallback.

**Model target:** A quantized function-calling model (e.g., `functionary`, `Llama-3.2` with tool-call fine-tuning, or `Gorilla`) running via `llama.cpp` or `mlx` on Apple Silicon.

**Integration point:** `model-router` gains a `local` provider. The agent profile can specify `prefer_local_for_tools: true` — tool-selection turns route to FunctionG; content generation turns route to the configured remote provider.

**philotic-web CLI:**
```bash
philotic-web models function install <model>
philotic-web models function status
philotic-web models function benchmark --tool-suite default
```

### Local Model Registry in Context Graph

All local model configurations are stored in `node_config` as a `local_model_registry` record — same pattern as the vault registry. `philotic-web models ls` queries it. Model availability is advertised in the capability registry so agents can see what local inference is available on their node.

```
capability: model.local.transcribe    → WhisperKit
capability: model.local.embed         → EmbeddingsG
capability: model.local.function      → FunctionG
```

Agents with `prefer_local: true` in their profile will route to these capabilities before falling back to remote providers.

## Distribution Strategy

### Principles

1. **GitHub Releases is the foundation.** Tagged, checksummed release artifacts are the source of truth. Every other distribution mechanism is a layer on top.
2. **Reflexive release automation.** Tag a release → CI builds → CI bottles → CI opens a PR to the tap. Merge to ship. No manual steps.
3. **macOS leads.** Darwin arm64 and x86_64 are the first bottled targets. Linux follows.

### Distribution Matrix

| Channel | Install Command | Audience | Priority |
|---------|----------------|----------|----------|
| **Homebrew tap** | `brew install jaredlikes/philotic/philotic-web` | macOS operators | P0 |
| **cargo install** | `cargo install philotic-web` | Rust developers | P0 |
| **GitHub Releases** | Direct binary download | All | P0 (prerequisite) |
| **Nix** | `nix profile install github:likesjx/philotic-stack` | Infra/self-hosted operators | P1 |
| **Docker / OCI** | `docker pull ghcr.io/likesjx/aiua` | VPS / containerized | P1 |
| **apt / .deb** | `apt install philotic-web` | Debian/Ubuntu servers | P2 |
| **Homebrew core** | `brew install philotic-web` | General | Deferred |

### Homebrew Tap Structure

Repo: `jaredlikes/homebrew-philotic`

```
Formula/
  philotic-web.rb   # the operator CLI — primary install target
  aiua.rb           # hotel daemon — for operators who need direct access
```

Initial install UX:

```bash
brew tap jaredlikes/philotic
brew install philotic-web
```

`philotic-web.rb` declares `aiua` as a dependency so both binaries are present after a single install.

### Release Automation Pipeline

Triggered by a semver tag on `philotic-stack`:

1. **Build** — GitHub Actions matrix: `darwin-arm64`, `darwin-x86_64`, `linux-x86_64`
2. **Test** — smoke test each binary on its target platform
3. **Upload** — binaries and SHA256 checksums attached to GitHub Release
4. **Bottle** — `brew bottle` run for each formula/platform combination; bottles uploaded as release artifacts
5. **Tap PR** — automated PR opened on `jaredlikes/homebrew-philotic` with updated URLs and bottle hashes
6. **Merge** — human or auto-merge (with tests passing); tap users get the new version on next `brew upgrade`

This pipeline is fully reflexive once wired. The only human action is tagging a release.

### Nix Flake

A `flake.nix` in the repo root enables:

```bash
nix profile install github:likesjx/philotic-stack
```

And for declarative NixOS/nix-darwin configurations:

```nix
inputs.philotic-stack.url = "github:likesjx/philotic-stack";
```

Nix's reproducible build model is a natural fit for the target operator profile — technical, self-hosted, opinionated about reproducibility.

### Docker / OCI

Two images:

| Image | Contents | Use Case |
|-------|---------|---------|
| `ghcr.io/likesjx/aiua` | `aiua` daemon only | VPS containerized deployment (matches jane-vps pattern) |
| `ghcr.io/likesjx/philotic-web` | `philotic-web` CLI | Remote management from a container |

The `aiua` image is the natural evolution of the current Docker deployment on jane-vps (`~/apps/jane/docker-compose.yml`).

## Implementation Roadmap

### Phase 1: The Rename (ansible → aiua)

Prerequisite for everything. Low risk, high impact on distribution.

- Rename `crates/ansible/` → `crates/aiua/`
- Update `Cargo.toml` package name: `ansible` → `aiua`
- Update workspace `Cargo.toml`: member path and dependency reference
- Update `justfile`: recipe names, `-p ansible` flags, binary paths in `pkill`
- Update socket path: `philotic-ansible.sock` → `philotic-aiua.sock`
- Update DB name: `ansible_context.db` → `aiua_context.db`
- Update string literals in Rust source (startup test roles, node_id format, etc.)
- Update documentation references (preserve `RH_ANSIBLE_VPS_DEPLOYMENT_PROPOSAL.md` — that `ansible` is intentional)
- Update `CLAUDE.md` and `AGENTS.md` hot file references

Worktree: `codex/ansible-to-aiua`

### Phase 2: Stub the philotic-web Crate

Stand up the new crate with a functional CLI skeleton.

- Create `crates/philotic-web/` with `Cargo.toml` and `src/main.rs`
- Add to workspace
- Implement `philotic-web status` (local aiua health check via UDS)
- Implement `philotic-web start` (spawn local aiua, manage lifecycle)
- Implement `philotic-web guests list` (query Context Graph via local IPC)
- Clap CLI structure that anticipates remote `--node` flag without implementing it yet

### Phase 3: GitHub Releases + Homebrew Tap

First distribution slice.

- Create `jaredlikes/homebrew-philotic` repo
- Write `philotic-web.rb` formula (source build from tagged release)
- Write `aiua.rb` formula
- Tag first release (`v0.1.0`) and verify manual install works
- Wire GitHub Actions release workflow (build, upload binaries + checksums)
- Wire automated tap PR workflow

### Phase 4: Management Port + mTLS

Remote management foundation. This is where security becomes load-bearing.

- Assign management port in Port Blueprint
- Generate and manage mTLS keypairs: `philotic-web admin keys init`
- Register operator public key with local aiua on first run
- Implement remote key registration flow (SSH-assisted bootstrap)
- Implement `philotic-web --node <target>` routing over mTLS management port
- Action grant minting and validation in aiua management handler
- Management event audit log in aiua event ledger

### Phase 5: Full Mesh Management

The complete operator surface for a multi-node philotic web.

- `philotic-web mesh status` — query all known nodes via Context Graph topology
- `philotic-web mesh nodes` — list all aiua nodes and health
- `philotic-web mesh isolate <node>` — revoke node mesh participation
- Cross-node secret rotation: `philotic-web secrets rotate --scope mesh`
- Consolidated mesh audit log: `philotic-web audit log`
- Admin elevation flow: `philotic-web admin elevate`

### Phase 6: Nix + Docker

Extend distribution to additional channels.

- `flake.nix` in repo root for Nix flake support
- `Dockerfile` for `aiua` image, targeting the jane-vps deployment pattern
- GitHub Actions CI for OCI image builds on tag
- Push to `ghcr.io/likesjx/aiua` on release

## Relationship to Prior Proposals

**HOMEBREW_DISTRIBUTION_PROPOSAL** — This proposal supersedes it. The naming conflict it identified (`ansible` collision) is resolved by the `aiua` rename. The tap-first recommendation and phase structure are carried forward.

**CONTROL_PLANE_ADMIN_SURFACE_PROPOSAL** — This proposal implements it. `philotic-web` is the CLI/TUI that the admin surface proposal called for. The action grant model, elevation tiers, and membrane/CLI boundary are preserved exactly.

**RUNTIME_AUTHORITY_LEASES_PROPOSAL** — Management operations issued via `philotic-web` respect the lease model. A `philotic-web guests restart` on a leased guest triggers the appropriate lease revocation and re-acquisition flow rather than forcibly killing the process.

**PERIMETER_EGRESS_CONTROL_PROPOSAL** — Management traffic from `philotic-web` to remote aiua nodes is control-plane egress and is exempt from the perimeter model. Outbound requests triggered by management operations (e.g., credential rotation calling an external API) are subject to perimeter policy.

**PHILOTE_MEMORY_CORE_PROPOSAL** — MuninnDB instances running on aiua nodes are manageable through `philotic-web`: `philotic-web memory status`, `philotic-web memory consolidate`. This is deferred to Phase 5 but the management port must be designed with this in mind.

## Active Seams

- **binary-rename-ansible-to-aiua** — The rename itself. Prerequisite for all distribution work.
- **philotic-web-crate** — The new crate. Critical path for the operator story.
- **management-plane-security** — mTLS, action grants, audit trail. Must be designed before Phase 4 implementation begins.
- **distribution-pipeline** — GitHub Actions release automation + tap PR workflow.
- **repo-identity** — `philotic-stack` repo stays; product is Philotic Web; binaries are `philotic-web` and `aiua`. This distinction should be reflected in README.md.

## Implementation Roadmap (Extended)

### Phase 4b: PKI + Mesh CA

- `philotic-web ca init` — local CA keypair on operator machine
- `philotic-web node enroll` — CSR flow, cert installation, operator pubkey registration
- Challenge/response auth over UDS (nonce + Ed25519 signature)
- Envelope encryption for `philotic-web secret set`
- Username/password auth path (argon2, stored in `node_config`)
- OAuth token acceptance (optional, offloads to external IdP)
- Management event audit log attributed to operator key fingerprint

### Phase 4c: VPN / NAT Traversal

- WireGuard peer per aiua node, keypair derived from Ed25519 node identity
- STUN-based hole-punching for nodes behind NAT
- Relay designation: `philotic-web mesh vpn add-relay`
- Mesh overlay replaces/wraps BeaconMessage UDP for cross-network nodes
- `philotic-web mesh vpn diagnose` for traversal debugging

### Phase 5b: Mesh Topology Operations

- `philotic-web mesh join/leave/isolate`
- `philotic-web guest migrate`
- `philotic-web mesh merge/split` with LWW conflict resolution
- `philotic-web serve` — optional local web dashboard

### Phase 7: Local Model Infrastructure

- WhisperKit integration in `model-router` (`model.local.transcribe` capability)
- EmbeddingsG integration in `memory-core` (local embedding before MuninnDB write)
- FunctionG integration in `model-router` (`model.local.function` capability, `prefer_local_for_tools`)
- `local_model_registry` in `node_config`, advertised in capability registry
- `philotic-web models` subcommand (install, status, benchmark, set-default)

## Open Questions

1. **Management port number** — Needs assignment in `PORT_BLUEPRINT.md`. Must not collide with existing ports (8999 mesh UDP, 9001 blob HTTP, 1235 ansible plugin, 18789 gateway).
2. **WireGuard keypair derivation** — Ed25519 and Curve25519 (WireGuard's native format) require conversion. Standard practice is to derive the WireGuard key from the Ed25519 key via a deterministic hash. Needs validation against WireGuard's security assumptions.
3. **Relay node incentive/selection** — In a multi-node mesh, which node(s) serve as relay? Auto-election vs. explicit designation. VPS nodes are natural candidates but this needs a policy.
4. **FunctionG model selection** — The local function-calling model space is moving fast. `functionary`, Llama-3.2, Gorilla — benchmark needed before committing to a default. Tie to Apple MLX or llama.cpp?
5. **EmbeddingsG and MuninnDB embedding contract** — If local embeddings are generated before the MuninnDB write, MuninnDB must accept pre-computed vectors. Confirm MuninnDB REST API supports this.
6. **philotic-web as a daemon?** — Stateless CLI is simpler; persistent daemon enables real-time mesh monitoring and push notifications. Decision deferred but Phase 4c VPN work may force the answer.
7. **crates.io publish scope** — `philotic-web` and `philotic-client` are clear candidates. `ansible-mesh-core` exposes internal protocol types. Decision needed before Phase 3.
8. **Windows support** — Not in scope for first distribution slices. Deferred unless there is a concrete operator need.
