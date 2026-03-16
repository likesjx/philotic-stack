---
title: "Philotic Web: Product Identity, Management Plane, and Distribution"
doc_type: proposal
domain: product-management-plane
status: proposed
last_updated: 2026-03-15
tags:
  - philotic-web
  - aiua
  - management-plane
  - distribution
  - homebrew
  - security
  - naming
  - cli
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

# Philotic Web: Product Identity, Management Plane, and Distribution

## Goal

Define the product identity, naming architecture, management plane design, security model, and distribution strategy for Philotic Web — the operator-facing face of the Philotic Stack. Establish `aiua` as the canonical hotel daemon name, `philotic-web` as the management CLI that governs a mesh of aiua nodes, and the Philotic Stack repo as the home for both. Define the distribution path across Homebrew, Cargo, Nix, and Docker. Set the security model for remote multi-node management as a first-class constraint.

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

## Security Model

Remote multi-node management is the highest-privilege surface in the system. A tool that can reach any aiua and issue management commands is equivalent to root access across the mesh. The security model must be designed before the protocol, not retrofitted after.

### Threat Model

The security model is designed against these threat scenarios:

1. **Compromised operator credential** — a stolen `philotic-web` credential should not grant mesh-wide root access
2. **Man-in-the-middle** — management traffic between `philotic-web` and a remote aiua must not be interceptable or replayable
3. **Compromised node** — a compromised aiua should not be able to issue management commands to peer nodes
4. **Escalation via agent** — an agent (philote) must never be able to trigger management-plane operations without explicit operator elevation
5. **Replay attack** — captured action grants must not be replayable

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

### Credential Bootstrap

The first-run problem: how does `philotic-web` get credentials to connect to a new aiua?

- **Local (same machine):** `philotic-web init` generates a keypair. The public key is registered with the local aiua as a trusted operator principal. No network credential needed.
- **Remote (new node):** Operator SSHes to the target node once to register the `philotic-web` public key. After that, all management is via `philotic-web --node <target>`.
- **Key rotation:** `philotic-web admin keys rotate` — generates a new keypair, registers the new public key with all known nodes, revokes the old key.

This is intentionally conservative for the first slice. Automated certificate provisioning (via mesh CA or external PKI) is deferred.

### Blast Radius Containment

- A compromised aiua node cannot issue management commands to peer nodes — management authority flows from `philotic-web` down to aiua, never laterally between aiua nodes.
- `philotic-web` never holds decrypted secret material — it presents grants; aiua executes against its own vault.
- Node isolation: `philotic-web mesh isolate <node>` revokes that node's mesh participation without affecting other nodes.

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

## Open Questions

1. **Management port number** — Needs assignment in `PORT_BLUEPRINT.md`. Must not collide with existing ports (8999 mesh UDP, 9001 blob HTTP, 1235 ansible plugin, 18789 gateway).
2. **Mesh CA vs. self-signed mTLS** — First slice uses self-signed keypairs with manual registration. A mesh CA (where aiua nodes auto-trust certificates signed by the mesh CA) is a cleaner long-term model but adds infrastructure. Decision deferred to Phase 4 design.
3. **philotic-web as a daemon?** — Should `philotic-web` run as a persistent background process that maintains mesh connections, or remain a stateless CLI that opens connections on demand? Stateless is simpler; persistent enables real-time mesh monitoring and push notifications from aiua nodes. Decision deferred.
4. **crates.io publish scope** — Which crates get published to crates.io? `philotic-web` and `philotic-client` are clear candidates. `ansible-mesh-core` enables third-party integrations but exposes internal protocol types. Decision needed before Phase 3.
5. **Windows support** — Not in scope for the first distribution slices. Deferred unless there is a concrete operator need.
