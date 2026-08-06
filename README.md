# The Philotic Stack

A distributed AI agent operating system built in Rust — designed around a clear, intuitive **Hotel & Guest** metaphor.

At the core of the stack is **The Hotel** (an autonomous, Rust-powered node) protected by an **Ingress Fence** security perimeter. The Hotel acts as a secure supervisor and message switchboard. It brings **Guests** (specialized AI agent processes like your personal assistant or system architect) to life, manages their memory within a local **Context Graph**, and connects them to the outside world through secure **Membranes**. 

Multiple Hotels can link together to form a resilient, cryptographically secure mesh network—the **Philotic Web**—using hybrid UDP/TCP gossiping, WebRTC signaling for direct execution channels, and the **Whisper Protocol** for paracrine agent communication.

[![Philotic Web Teaser](https://img.youtube.com/vi/SF2C9rbz330/maxresdefault.jpg)](https://youtu.be/SF2C9rbz330)

## Installation

### Homebrew (macOS)

```bash
brew tap likesjx/philotic
brew install philotic-web    # installs the operator CLI + all core binaries
brew install muninn          # installs the cognitive memory store
```

This installs the `phil` CLI (symlinked from `philotic-web`), the `aiua` hotel daemon, and all guest binaries (philote, membrane-telegram, membrane-discord, membrane-mcp, membrane-mcp-client, model-router, the model-controller-* family, tool-runner, graph-datasource, and the rest of the release set below).

### From Source

```bash
git clone https://github.com/likesjx/philotic-stack.git
cd philotic-stack
cargo build --release

# Put the operator CLI on your PATH as `phil`. Homebrew creates this symlink
# for you; a source build does not, and every command below is named `phil`.
ln -sf "$(pwd)/target/release/philotic-web" /usr/local/bin/phil
```

> `just phil-install` does the same thing against `target/debug/` — use it when
> you are iterating with `cargo build`, not after a `--release` build.

#### System dependencies

The workspace links against a few things Cargo cannot install for you. `just
preflight` checks all of them and tells you how to fix what is missing; this
table is what it checks and why.

| Requirement | Needed by | Notes |
|---|---|---|
| C compiler (`cc`) | `libsqlite3-sys` (`bundled`) | Compiles SQLite from source. `xcode-select --install` / `build-essential`. |
| `pkg-config` | `audiopus_sys` | Used to locate a system Opus. |
| **`opus`** | `membrane-discord` → `opus` | **`brew install opus`.** Without it `audiopus_sys` builds Opus from source, and its bundled `CMakeLists.txt` declares `cmake_minimum_required(VERSION <3.5)`, which **CMake 4 rejects**. This is not hypothetical — it broke the first CI run, and it was invisible for months because the dev Macs already had Opus via Homebrew. |
| Network (first build) | `onnx-runner` → `ort` | `ort` uses `download-binaries`, so the first build fetches ONNX Runtime. Set `ORT_LIB_LOCATION` to use a local copy. |
| `npm` (conditional) | `philotic-web/build.rs` | Only when `PHILOTIC_DESKTOP_DIR` or `PHILOTIC_REFRESH_DESKTOP_UI` is set — otherwise the committed `ui-dist` is reused. build.rs **panics** if npm is missing. |
| `rustup` (recommended) | `rust-toolchain.toml` | The 1.94.0 pin is only honoured by rustup. With a Homebrew toolchain the file is inert and local builds drift from CI. |

>Binaries are built to `target/release/`. A full release build emits ~24 binaries (the deployed set is pinned in `AIUA_BINS` in the [justfile](justfile) and mirrored by [.github/workflows/build-linux.yml](.github/workflows/build-linux.yml)). The primary ones:

| Binary | Purpose |
|---|---|
| `aiua` | Hotel daemon — materializes and supervises all guests |
| `philotic-web` | Operator CLI (`phil init`, `phil start`, `phil status`) + desktop membrane |
| `philote` / `philote-worker` | Agent core — cognitive loop, sessions, roles (+ delegated worker) |
| `membrane-telegram` | Telegram gateway (runs on the MembraneRuntime SDK) |
| `membrane-discord` | Discord gateway |
| `membrane-mcp` | MCP gateway (serves hotel tools to external MCP clients) |
| `membrane-mcp-client` | MCP client guest (consumes upstream MCP servers, projects their tools to philotes) |
| `membrane` | MembraneRuntime SDK library (no binary; consumed by the gateway guests) |
| `model-router` | LLM inference routing |
| `model-controller-*` | Per-provider model controllers (gemini, elevenlabs, openai, openrouter, anthropic, mlx, ollama, onnx, parakeet, vision) |
| `tool-runner` | Sandboxed tool execution |
| `graph-datasource` / `table-datasource` / `agent-datasource` | Graph and table datasource guests |
| `life-graph-runner` | LifeGraph / MemGraphRAG runner (from `data-memorygraphrag`) |
| `router-listener` | Router training tap |
| `heal-dispatcher` | FunctionGemma self-healing dispatcher |

## Quick Start

```bash
# After installation:
phil init                    # generate identity keypair + mesh-config.json
phil start                   # start the hotel daemon (materializes all agents)
phil status                  # check running agents
phil agents                  # list configured agents
```

Or manually:
```bash
cp mesh-config.example.json mesh-config.json
# Edit mesh-config.json with your API keys and node identity

# Apply the config to the Context Graph DB. Run this once on first setup, and
# again whenever the config changes.
aiua load --file mesh-config.json --hotel default

# Normal startup then runs purely from the DB — it does not re-read the file.
aiua --hotel default
```

## Architecture Diagrams

### Architecture Overview

![Philotic Stack Architecture](docs/architecture.svg)

### Target Architecture

![Target Architecture](docs/target_architecture.svg)

### Implementation Status

![Implementation Status](docs/implementation_status.svg)

> **Legend** — 🟢 Implemented · 🟡 In progress / flag-gated · 🟠 Scaffolded / blocked · ⚫ Planned / docs only

## Crates

31 crates live under [`crates/`](crates/): 29 workspace members, `philotic-primitives-mesh` (not an explicit workspace member but pulled into the build as a path dependency of `ansible-mesh-core` — 30 buildable packages total; the other five primitives stubs were empty scaffolds and were folded back and deleted 2026-07-06, alongside the retired `graph-runner` crate), and `agent-graph-runner` (dead — no `Cargo.toml`, not part of the build; superseded by `agent-datasource`, directory deletion pending).

### Binaries

| Crate | Role |
|---|---|
| [`aiua`](crates/aiua/) | Hotel daemon — guest materialization, IPC server, mesh routing, perimeter security |
| [`philote`](crates/philote/) | Agent core — cognitive loop, session management, role incarnation |
| [`membrane`](crates/membrane/) | MembraneRuntime SDK library (lib-only; consumed by the gateway guests) |
| [`membrane-telegram`](crates/membrane-telegram/) | Telegram / external protocol gateway (MembraneRuntime SDK + LeaseDriver) |
| [`membrane-discord`](crates/membrane-discord/) | Discord gateway |
| [`membrane-mcp`](crates/membrane-mcp/) | MCP gateway |
| [`membrane-mcp-client`](crates/membrane-mcp-client/) | MCP client guest — upstream server consumption (`mcp-client-fabric` Phase 1) |
| [`philotic-web`](crates/philotic-web/) | Operator CLI + desktop membrane (REST API, WebSocket, operator chat) |
| [`model-router`](crates/model-router/) | Shared LLM inference routing SDK |
| [`tool-runner`](crates/tool-runner/) | Sandboxed tool execution (Landlock + seccomp via philotic-sandbox) |
| [`agent-datasource`](crates/agent-datasource/) | Per-agent cognitive graph partition datasource (`agent.graph.*` tool surface) |
| [`graph-datasource`](crates/graph-datasource/) | Autonomous graph partition management tool surface |
| [`graph-intelligence`](crates/graph-intelligence/) | Project intelligence graph + MCP server |
| [`heal-dispatcher`](crates/heal-dispatcher/) | FunctionGemma self-healing dispatcher |
| [`parakeet-runner`](crates/parakeet-runner/) | NVIDIA Parakeet ASR model controller |

### Libraries

| Crate | Role |
|---|---|
| [`ansible-mesh-core`](crates/ansible-mesh-core/) | Shared core library — storage traits, `GraphDomain`, mesh types (only mesh primitives were extracted; the wider primitives split was folded back 2026-07-06) |
| [`philotic-primitives-mesh`](crates/philotic-primitives-mesh/) | Mesh primitives (EventEnvelope, BeaconMessage, etc.) — the only primitives crate; the six-crate split was folded back (2026-07-06) |
| [`philotic-client`](crates/philotic-client/) | Guest SDK — IPC client for hotel communication |
| [`philotic-edge-protocol`](crates/philotic-edge-protocol/) | Wire types for the edge-mesh client-server protocol (edge clients, e.g. the Apple app, <-> hotel termination) |
| [`memory-core`](crates/memory-core/) | MemoryEngine trait, CognitiveEngine, Muninn integration |
| [`philotic-graph`](crates/philotic-graph/) | Core graph intelligence and SVE tooling |
| [`datasource`](crates/datasource/) | SQLite partition and datasource management |
| [`data-memorygraphrag`](crates/data-memorygraphrag/) | MemGraphRAG / LifeGraph runner toolset layer |
| [`router-listener`](crates/router-listener/) | Router training tap |
| [`table-datasource`](crates/table-datasource/) | Multi-DB datasource support + full CRUD task kinds |
| [`media-codec`](crates/media-codec/) | Audio normalization and voice transcoding |
| [`perimeter-core`](crates/perimeter-core/) | Security perimeter boundary, IngressFence |
| [`onnx-runner`](crates/onnx-runner/) | Local ONNX inference (embeddings, transcription) |
| [`mlx-runner`](crates/mlx-runner/) | Local MLX inference (Apple Silicon) |
| [`philotic-sandbox`](crates/philotic-sandbox/) | Secure execution sandbox (Landlock + seccomp policies) |
| [`media-prep`](crates/media-prep/) | Media processing and preparation |

## The Agent Fleet

The default Philotic stack materializes a fleet of specialized agents, each with a distinct persona and mission:

- **Jane (The Assistant)**: Warm, capable, and direct. Your primary point of contact for daily help.
- **Aria (The Architect)**: Technical lead and development specialist. Manages the stack itself.
- **Beacon (The Chief of Staff)**: Keeper of goals, projects, and commitments. Ensures clarity and focus.
- **Hermes (The Communicator)**: Specialized in routing, summaries, and correspondence.
- **Astrid (The Librarian)**: Archivist of knowledge and organization. Manages documentation and vault systems.

## Key Design Principles

- **Hotel = source of truth.** The Context Graph SQLite DB owns all state.
- **Security first, especially at the perimeter.** External communication surfaces default to minimal trust and explicit policy.
- **IPC for intra-hotel.** All local communication uses Unix Domain Sockets (`/tmp/philotic-aiua.sock`).
- **Mesh for inter-hotel.** Cross-machine coordination uses UDP Gossip/Beacons (Control Plane) and TCP Exec-Transport (Data Plane), secured by a WireGuard-inspired Ed25519 PKI identity and ephemeral X25519 session keys.
- **GraphDomain is the access layer.** Entity-typed graph methods enforce naming conventions and reduce raw SQL surface area.
- **Guests are crash-safe.** The supervisor loop auto-respawns dead guests every 5s.
- **Memory is eventually consistent.** Guests write optimistically; the hotel resolves conflicts via Last-Writer-Wins.

## Documentation

- **[Full Architecture Reference](docs/architecture/ARCHITECTURE.md)** — system design, data flows, component reference.
- **[Architecture Status](docs/architecture/ARCHITECTURE_STATUS.md)** — live status of implemented vs. transitional features.
- **[Domain Map](docs/architecture/DOMAIN_MAP.md)** — architectural domains and their governing proposals.
- **[Seam Registry](docs/architecture/SEAM_REGISTRY.md)** — all registered implementation seams.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the build commands, the branch model,
and the verification ladder this project grades claims against
(*test-green* / *smoke-green* / *watched-live-green*).

## Security

See [SECURITY.md](SECURITY.md) to report a vulnerability, and read the threat
model there before deploying. The stack assumes a single operator on a private
network; it is not currently hardened for multi-tenancy or untrusted networks.

## License

MIT — see [LICENSE](LICENSE).
