---
title: Philote Memory Core Proposal
doc_type: proposal
domain: memory-context
status: proposed
last_updated: 2026-03-31
tags:
- memory
- muninn
- cognitive
- vaults
- propagation
- introspection
- consolidation
- embedding
- multi-tenant
related_docs:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
- MEMORY_ENGINE_ABSTRACTION_PROPOSAL.md
- MEMORY_DURABILITY_PROPOSAL.md
- MUNINN_MEMORY_PROTOCOL_PROPOSAL.md
- PLUGGABLE_CONTEXT_ENGINE_PROPOSAL.md
- PERSONALITY_AND_CONTEXT_PROPOSAL.md
- ROLE_ACTIVATION_AND_SUBAGENT_CONTRACTS_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: philote-memory-core
implements:
- memory-engine-abstraction
supersedes:
- memory-engine-abstraction
active_seams:
- memory-core-sdk
- memory-vault-topology
- role-lens-activation
- introspection-skill
- memory-propagation-mesh
- consolidation-subsystem
- embedding-integration
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Philote Memory Core Proposal

## Goal

Introduce a layered, vault-isolated memory system backed by MuninnDB as the cognitive substrate. Define the capability boundaries (MemoryEngine / CognitiveEngine), vault topology, propagation model, role-lens activation system, consolidation subsystem, multi-tenant access control, embedding integration points, and agent introspection capability as a single cohesive architecture.

The `sync_apartment` / `get_apartment` surface on the Context Graph retains its management-plane responsibilities (topology, config, coordination). Only the cognitive memory workload — the things an agent *knows*, *believes*, and *remembers* — migrates to MuninnDB vaults. The Context Graph continues to serve as the management plane: shared knowledge across hotels, the full blueprint of each hotel, and current running processes.

This proposal is the definitive memory design for the Philotic Stack. It supersedes the earlier `MEMORY_ENGINE_ABSTRACTION_PROPOSAL` (which identified the need for a pluggable boundary) and `MUNINN_MEMORY_PROTOCOL_PROPOSAL` (which established the initial MCP-based client protocol for SVE tooling). Both remain accurate in their observations — this proposal provides the concrete architecture that realizes them.

## Disposition

`proposed`

Track implementation in [docs/task.md](/docs/task.md).

## Why This Matters

The current cognitive memory system is a last-write-wins key-value store bolted onto the Context Graph. It has no concept of decay, salience, cognitive weight, or multi-agent isolation. Every agent on every hotel writes to the same flat namespace. This is the single largest gap between the current runtime and the system's architectural ambition.

MuninnDB already provides the cognitive primitives we need — ACT-R decay, Hebbian co-activation, predictive activation, contradiction detection, and confidence tracking. The work is integration architecture, not inventing new science.

The risk of deferring is that memory-adjacent systems (roles, personality, session management, inter-hotel routing) keep building workarounds for the absence of real memory, and those workarounds become load-bearing walls.

## Design Principles

1. **MuninnDB is the cognitive engine, not a sidecar.** All durable cognitive memory lives in MuninnDB vaults. The Context Graph stores topology, config, and management-plane state only.
2. **Three memory strata, no more.** User (L0), Agent Self (L1), Session (L2). Hotels are topographical boundaries, not memory strata. This mirrors the neuroscience distinction between semantic memory (durable knowledge — L0), autobiographical memory (self-model — L1), and working memory (active session context — L2).
3. **Roles are attentional lenses, not containers.** One agent has one vault. Roles shape activation through tag filters and biases — functioning like selective attention in cognitive psychology: the same underlying memory store, different retrieval biases depending on current focus.
4. **Write-local, propagate-async.** Every write hits local MuninnDB synchronously. Cross-hotel propagation happens through the existing mesh event bus. This mirrors the neuroscience model of encoding (immediate, local) followed by systems consolidation (gradual, distributed).
5. **The engram is the unit of propagation.** Content, tags, and relationships propagate. Cognitive state (decay, Hebbian weights) stays local — each node develops its own salience landscape, just as individual neurons develop independent synaptic strengths.
6. **Config-driven everything.** Vaults, lenses, autonomy policies, consolidation schedules, embedding backends, and propagation rules are all operator configuration for the Philotic runtime. This is not MuninnDB configuration — it is the Philotic Stack's own control surface for how memory-core orchestrates its relationship with MuninnDB.
7. **Subagents have no memory.** They are ephemeral workers. Only primary agents with durable identity have vaults.
8. **Consolidation is a designed subsystem, not an afterthought.** Memory maintenance — pruning, compression, promotion, deduplication — runs as a first-class administrative process with its own scheduling, policies, and observability.

## Conceptual Model: The Neurological Metaphor

The architecture draws deliberately from neuroscience and cognitive psychology. These are not decorative labels — they map real cognitive phenomena onto system behaviors:

| Neuroscience Concept | System Analog | Why It Matters |
|---------------------|---------------|----------------|
| **Engram** (the physical trace of a memory in neural tissue) | The atomic unit of memory in MuninnDB — concept, content, tags, relationships | Memory has structure, not just content. An engram is a first-class citizen with metadata, associations, and lifecycle. |
| **Spreading activation** (retrieving one memory primes related memories) | MuninnDB's ACTIVATE pipeline — Hebbian co-activation and graph traversal | Recall is not keyword search. Activating "morning routine" should prime "coffee preferences" and "schedule patterns." |
| **Synaptic consolidation** (strengthening connections between neurons during rest) | Post-session consolidation cycles — episode compression, pattern extraction, deduplication | Memories must be processed, not just accumulated. The quiet hours are when the system digests experience into knowledge. |
| **Systems consolidation** (gradual transfer from hippocampus to cortex) | Session-to-durable promotion — L2 findings migrating to L0/L1 | Temporary context becomes lasting knowledge through an active process, not passive persistence. |
| **Selective attention** (focusing on relevant stimuli while ignoring others) | Role lenses — tag filters and activation biases that shape retrieval | The same memory store, different retrieval profiles depending on what the agent is currently doing. |
| **ACT-R decay** (memories fade without reinforcement) | MuninnDB's base-level activation with configurable decay rates | Unused knowledge naturally recedes. Important memories are reinforced through access. |
| **Hebbian learning** ("neurons that fire together wire together") | Co-activation weight strengthening in MuninnDB | Memories that are frequently retrieved together become associated, building emergent knowledge graphs. |
| **Predictive coding** (the brain anticipates likely inputs) | MuninnDB's PAS (Predictive Activation System) | The system can pre-load likely-relevant context before it's explicitly requested. |
| **Metacognition** (thinking about one's own thinking) | The introspection skill — analyzing memory patterns to propose self-optimization | An agent that can observe its own cognitive patterns and suggest improvements. |
| **Autonomic vs. deliberate processing** | Tiered autonomy — auto (reflexive), notify (aware), propose (deliberate) | Not all self-modification carries the same risk. Lens tuning is autonomic; role restructuring requires deliberation. |

## Architecture Overview

### Memory Strata

| Stratum | Vault Pattern | Scope | Lifecycle | Propagation |
|---------|--------------|-------|-----------|-------------|
| **L0: Semantic** (User Knowledge) | `user:{user_id}` | All agents serving this user | Durable, persists indefinitely | Continuous async via mesh event bus |
| **L1: Autobiographical** (Agent Self) | `self:{agent_id}` | Owning agent only | Durable, follows agent across hotels | Sync-on-materialization, periodic consolidation |
| **L2: Working** (Session) | `session:{session_id}` | Session participants | Semi-durable, selective promotion at session end | On-handoff via HandoffBundle |

There is no hotel-scoped memory stratum. Hotels are infrastructure topology — an agent can be materialized on any hotel. Memory should not be shaped by where the agent happens to be running.

The naming draws from established memory science: L0 holds semantic knowledge (facts about users, durable across contexts), L1 holds autobiographical knowledge (the agent's sense of self, identity, learned patterns), and L2 holds working context (the active conversation, transient goals, in-flight reasoning).

### Roles as Attentional Lenses

A role is a focus, not a container. When an agent incarnates into a role, it applies an **attentional lens** — a set of tag filters, activation biases, and write-side conventions that shape how the role interacts with the agent's unified self vault. This mirrors selective attention in cognitive psychology: same underlying store, different retrieval profile.

```yaml
# Example: persona role lens (stored on RoleIncarnationRecord in Context Graph)
attentional_lens:
  activation:
    include_tags: [conversational, relational, user-model]
    exclude_tags: [internal-only, deprecated]
    recency_bias: 0.3          # weight toward recent memories
    frequency_bias: 0.7        # weight toward frequently-accessed memories
    max_results: 20
  write:
    auto_tags: [persona, conversational]
    default_scope: shared_user
  cognitive:
    active_rule_tags: [social, empathy, communication]
    active_belief_tags: [user-relationship, persona-identity]
```

Why not separate vaults per role? MuninnDB's activation model already handles relevance through salience scoring and Hebbian co-activation. If an agent learns something during code review that is relevant to a conversation, it should surface — because the content is relevant, not because it was filed in the right drawer. Separate vaults would create artificial amnesia between roles.

### Capability Boundaries: MemoryEngine and CognitiveEngine

The system defines two semantic capability boundaries. These are described as contracts — the interface an implementation must satisfy — not as prescribed code. Coding agents will make the implementation decisions.

The dichotomy is not "basic vs. advanced" — it is a clean functional split:

- **MemoryEngine** = **storage, retrieval, and association.** The operations that get engrams into vaults, get them back out, and maintain the graph of relationships between them. This is the substrate. Any system that persists and recalls structured memories needs these operations.
- **CognitiveEngine** = **reasoning about memory.** The operations that interpret, evaluate, and regulate the contents of memory — beliefs with confidence, rules with conditions, contradiction detection, pattern analysis, and self-observation. This is the interpretive layer that sits on top of the substrate.

The analogy: MemoryEngine is the hippocampus (encoding, storage, retrieval). CognitiveEngine is the prefrontal cortex (evaluation, planning, metacognition). You can have a functioning memory system with just the hippocampus. The prefrontal cortex adds the ability to reason about what you know.

**MemoryEngine** — storage, retrieval, and association:

| Capability | Semantic Contract | Notes |
|-----------|------------------|-------|
| **remember** | Store an engram in a scoped vault. Apply lens auto-tags. Return an identifier. | Single and batch variants. |
| **evolve** | Update an existing engram's content, tags, or metadata. | Immutable core identity; mutable content envelope. |
| **activate** | Given a context string and scope, retrieve the most salient engrams. Lens filters and biases apply. | This is MuninnDB's ACTIVATE pipeline: full-text + vector, fusion, Hebbian, predictive, graph traversal, ACT-R. |
| **read** | Direct retrieval by identifier. | Bypasses activation; for known-ID access. |
| **link** | Create a typed association between two engrams. | Feeds Hebbian co-activation. Aggressive auto-linking — never wait for confirmation. |
| **traverse** | Walk the association graph from a starting engram. | Supports spreading activation patterns. |
| **forget** | Mark an engram for removal or accelerated decay. | Soft delete; consolidation may finalize. |
| **subscribe** | Register for real-time activation pushes matching a context. | Maps to MuninnDB's ActivationPush subscription. |
| **set_lens / current_lens** | Apply or inspect the active attentional lens. | Lens determines retrieval bias and write conventions. |

**CognitiveEngine** — reasoning about memory (extends MemoryEngine):

| Capability | Semantic Contract | Notes |
|-----------|------------------|-------|
| **decide** | Record a decision as a first-class engram with rationale, alternatives considered, and confidence level. | Decisions are cognitive acts, not just storage — they require evaluating alternatives and assigning confidence. |
| **active_rules** | Retrieve the currently salient cognitive rules given a context. | Rules are engrams tagged `cognitive:rule` — behavioral patterns the agent has extracted from experience. |
| **active_beliefs** | Retrieve current beliefs with confidence levels. | Beliefs are engrams tagged `cognitive:belief`. Confidence updated via Bayesian inference through MuninnDB. |
| **store_rule / store_belief** | Persist a new rule or belief with appropriate cognitive tagging. | Extracted from patterns during consolidation or at runtime. |
| **contradict** | Given two engrams, evaluate whether they are in tension and produce a resolution recommendation. | Leverages MuninnDB's contradiction detection. Resolution may be: supersede, coexist-with-context, or flag-for-review. |
| **extract_patterns** | Analyze a set of engrams for recurring structures — co-occurring tags, temporal sequences, causal chains. | Powers quiet-hours consolidation and the introspection skill. |
| **introspect** | Analyze memory patterns — tag co-occurrence, activation clustering, decay curves, cross-role overlap — and produce a structured report. | The metacognitive operation. Powers the introspection skill on the orchestrator role. |

The practical consequence: Phase 1 ships MemoryEngine. An agent can remember, recall, associate, and forget. Phase 4 adds CognitiveEngine. The agent can now reason about what it knows — detect contradictions, extract patterns, make tracked decisions, and observe its own cognitive dynamics.

**MemoryScope** determines vault routing:

| Scope | Routes To | Use Case |
|-------|-----------|----------|
| SelfOnly | `self_{agent_id}` | Agent's private autobiographical memory |
| SharedUser | `user_{user_id}` | Shared knowledge about the user |
| Session(id) | `session_{session_id}` | Active working context |
| CrossScope(scopes) | Fan-out query across multiple vaults | "Search everything I know" |

**Vault naming convention:** MuninnDB vault names are restricted to `[a-z0-9_-]`, max
64 chars. The `_` character is reserved as the scope prefix separator. Agent IDs and
user IDs must use `-` as their internal separator (e.g. `philote-1`, `user-jared`)
so that vault names are unambiguous by construction: `self_philote-1`, `user-jared`,
`session_01abc-def`. A vault name is always parseable as `{scope}_{id}` where scope
is the first `_`-delimited segment.

### The Thin Barrier: memory-core SDK

`memory-core` is a new crate in the workspace — a peer to `philote`, not a module inside it. One instance materializes per hotel (singleton within a hotel, like how the hotel daemon is singular). It is a managed guest, supervised by GuestManager and declared in `materialized_guests`, with its own IPC surface for agents that need to read or write memory.

The crate is a typed client that sits between agent-core and MuninnDB. It is a translation layer, not a middleware. The name reflects that this is an abstraction over the memory substrate — MuninnDB is the primary (and likely sole) backend, but it is not a first-class name in the Philotic Stack's own interface surface.

**What it does:**
- Translates Philotic domain types to MuninnDB API calls (REST initially, MBP when validated)
- Resolves vault addressing from MemoryScope + agent context
- Applies attentional lens (tag conventions, activation biases) automatically
- Emits propagation events to the event ledger for cross-hotel distribution
- Handles connection pooling, retries, and backpressure

**What it does not do:**
- Decide where memory lives (Context Graph decides)
- Manage propagation topology (mesh dispatcher handles that)
- Re-implement cognitive primitives (MuninnDB owns decay, Hebbian, PAS)
- Hold state beyond connection context

### Propagation Topology

Every memory write goes to the local MuninnDB instance synchronously. The write is immediately available for local reads. Propagation to other hotels happens asynchronously through the existing mesh event bus (`EventLedger`, `mesh_dispatcher`, `execution_transport`).

| Stratum | Propagation Strategy | Consistency |
|---------|---------------------|-------------|
| **L0: Semantic** | Continuous async via `EventEnvelope(kind=MemoryOp)` | Eventual (1-2s). All hotels converge. |
| **L1: Autobiographical** | Sync-on-materialization + periodic consolidation | Consistent at boot. Eventually consistent at runtime. |
| **L2: Working** | On-handoff via HandoffBundle | Consistent at handoff. Local-only otherwise. |

The unit of propagation is the **engram** — content, tags, relationships, and metadata. MuninnDB internal cognitive state (decay scores, Hebbian weights, activation counts) is **not** propagated. The receiving instance stores the engram as a new write and its own cognitive machinery starts from that point independently. This means salience scores will diverge across hotels — that is correct. It reflects local usage patterns, like how the same memory has different emotional weight depending on context.

### Vault Authority and Multi-Server Scenarios

In a multi-hotel deployment, each vault has a single **authoritative instance** at any given time. The Context Graph maintains a vault authority map:

| Scenario | Authority Model | Transfer Protocol |
|----------|----------------|-------------------|
| Single hotel | Trivial — local MuninnDB is authoritative | N/A |
| Agent materializes on new hotel | Authority transfers to new host | Checkpoint → transfer → verify → update authority map |
| Multiple agents share a user vault (L0) | One designated authority; others are replicas | Writes fan-in to authority; reads are local |
| Authority host goes down | Orchestrator promotes a replica based on recency | Fencing token prevents split-brain writes |

**Materialization sync:** When an agent materializes on a new hotel, the Context Graph knows which MuninnDB instance holds the authoritative copy of the agent's self vault. The sequence: Context Graph lookup → verify or pull vault snapshot → agent boots with local copy → Context Graph updated with new authority location. The exact checkpoint wire format is an implementation decision for the coding agents.

### Memory Formation Pipeline

Not every interaction becomes a durable memory. The pipeline models the neuroscience of encoding — sensory input passes through attentional filters before reaching long-term storage.

#### What is and is not memory-eligible

The reasoning path to get to a response — intermediate steps, hypotheses considered and rejected, chain-of-thought — is not memory-eligible. It is high-volume, low-signal, and the outcome supersedes it. What the agent *thought about* dies with the turn. What the agent *concluded, resolved, or observed about itself* is a memory candidate.

Specifically, these things that arise *inside* a turn have durable cognitive value:
- A **resolved contradiction** — the resolution is worth storing as a belief update; the deliberation path is not
- A **metacognitive observation** — "I struggled with this class of question" is a `cognitive:meta` engram candidate
- A **working hypothesis that solidified** — a belief formed mid-turn (e.g., about user preferences) is L1-eligible at turn-end
- A **rejected approach with a reason** — has durable value for future turns

This means the Attend step is not a separate LLM call over the full reasoning trace. It is a structured extraction from the turn's *outcomes*: what changed, what was decided, what was observed. That is a much smaller and cheaper operation.

#### Relation lifetimes and working turn

Turn-local relational structure — provisional associations, contradictions-in-flight, co-occurrence signals that arise during a single reasoning step — lives in `philote`'s `WorkingTurn` state, not in memory-core. It does not touch MuninnDB. At turn-end, anything that changed the agent's knowledge or beliefs is extracted by the Attend step and becomes an L2 write. The rest is discarded with the turn.

This gives clean lifetime semantics:
- **turn-local** → `WorkingTurn` in agent-core, never enters memory-core
- **session-local** → L2 session vault
- **candidate-durable** → L2 with selective promotion at session end
- **durable** → L0 or L1

#### Pipeline

1. **Perceive** — agent-core turn loop captures raw turn content (sensory input)
2. **Attend** — at turn-end, extract cognitive outcomes from `WorkingTurn`: resolved contradictions, solidified beliefs, metacognitive observations, rejected approaches with reasons. Not the reasoning path — only what changed.
3. **Encode** — determine scope (L0/L1/L2), apply tags from lens conventions (encoding with context)
4. **Consolidate** — memory-core checks for existing engrams with similar content (MuninnDB contradiction detection), resolves conflicts
5. **Store** — `remember()` with vault routing + lens auto-tags (long-term potentiation)
6. **Propagate** — emit `EventEnvelope(MemoryOp)` for L0 engrams (systems consolidation)
7. **Associate** — create links to related engrams (MuninnDB-internal Hebbian auto-linking)

**Session-end promotion (synaptic → systems consolidation):** When a session ends, durable findings promote to L0 (user facts) or L1 (agent self-learnings). Task-specific context stays in the session vault and decays aggressively. This mirrors how the brain's hippocampus hands off consolidated memories to the cortex during rest.

### Consolidation Subsystem

Consolidation is not a background afterthought — it is a designed subsystem with explicit scheduling, policies, and observability. In neuroscience, memory consolidation during sleep is when the brain reorganizes, strengthens, and prunes memories. The Philotic Stack models this directly.

**Consolidation Modes:**

| Mode | Trigger | Purpose | Scope |
|------|---------|---------|-------|
| **Post-session** | Session end event | Promote durable findings from L2 → L0/L1. Decay transient context. | Per-session |
| **Quiet hours** | Scheduled (configurable cron) | Deep reorganization: episode compression, pattern extraction, deduplication, Hebbian weight normalization | Per-vault |
| **Periodic pruning** | Timer-based interval | Remove engrams that have decayed below threshold. Archive rather than delete. | Per-vault |
| **On-demand** | Admin trigger or operator API | Force consolidation for maintenance, migration prep, or debugging | Targeted vault(s) |

**All consolidation is admin-managed.** The operator configures schedules, thresholds, and policies. The agent does not self-initiate consolidation — it is a platform concern, not an agent concern.

**Consolidation Pipeline:**

1. **Scan** — Identify candidates: decayed engrams below threshold, duplicate clusters, episodic sequences eligible for compression
2. **Classify** — Categorize each candidate: prune, merge, compress, promote, or preserve
3. **Execute** — Apply the classified action. Merged engrams create new composite engrams with provenance links to originals.
4. **Observe** — Emit consolidation metrics: engrams scanned, pruned, merged, promoted, errors. Feed into platform observability.

```yaml
# Consolidation configuration (operator config, NOT MuninnDB config)
consolidation:
  post_session:
    enabled: true
    promotion_policy: selective   # selective | aggressive | manual
    decay_acceleration: 3.0      # multiply decay rate for unpromoted L2 engrams
  quiet_hours:
    schedule: "0 3 * * *"        # 3 AM daily
    episode_compression: true
    pattern_extraction: true
    deduplication: true
    hebbian_normalization: true
  periodic_pruning:
    interval_seconds: 3600
    decay_threshold: 0.05        # prune engrams below this activation level
    archive_before_delete: true
  on_demand:
    enabled: true
    require_admin_auth: true
```

### Multi-Tenant Access Control

When multiple users or teams share an agent (e.g., a shared assistant), engrams carry access-control tags that determine visibility:

| Tag | Semantics | Example |
|-----|-----------|---------|
| `access:private` | Visible only to the originating user | Personal preferences, private notes |
| `access:shared` | Visible to all users of this agent | General knowledge, shared decisions |
| `access:team:{team_id}` | Visible to members of the specified team | Team-specific context, project knowledge |

**Runtime decision point:** The tag assignment happens at step 3 (Encode) of the Memory Formation Pipeline. The attentional lens's `default_scope` provides the default tag, but the encoding logic may override based on:
- Content classification (personal information → `access:private`)
- Session context (team channel → `access:team:{id}`)
- Explicit user instruction ("remember this for everyone" → `access:shared`)

The specific runtime logic for making this decision is an implementation concern. The architecture requires that the decision point exists and that the chosen tags are applied before the engram is stored. The MemoryEngine's `activate` operation filters results by the requesting user's access permissions.

```yaml
# Multi-tenant configuration
multi_tenant:
  enabled: false                 # opt-in per agent
  default_access: private        # private | shared
  team_resolution: context_graph # how to resolve team membership at runtime
  access_enforcement: read_filter # read_filter | vault_partition
```

### Embedding Integration

MuninnDB provides built-in local ONNX embedding for vector similarity, with configurable backends (Ollama, OpenAI, etc.). The Philotic Stack's embedding strategy is configuration-driven — operators point the runtime at the desired embedding backend.

**Current state:** MuninnDB's ACTIVATE pipeline already uses embeddings for the vector component of its hybrid retrieval (full-text + vector fusion). The default is MuninnDB's built-in ONNX embedder.

**Philotic Stack integration:** The `embeddinggemma` model will be deployed as a proper hotel-component within the Philotic Stack topology. The memory-core configuration points to it:

```yaml
# Embedding configuration (operator config)
embedding:
  backend: hotel_component       # builtin | ollama | openai | hotel_component
  hotel_component:
    component_id: embeddinggemma
    # How memory-core routes to the hotel-component endpoint
    # is a runtime wiring decision — the component exposes an
    # embedding API, memory-core consumes it.
  fallback: builtin              # use MuninnDB's ONNX embedder if component unavailable
```

The exact mapping between the memory-core SDK and the embeddinggemma hotel-component is an open design question. The component will expose an embedding API; memory-core will consume it. The wiring details depend on how hotel-component service discovery and routing evolve.

### Introspection Skill (Metacognition)

Introspection is an **embedded skill on the conversational/orchestration role**, not a background daemon. The orchestrator has natural visibility into all other roles' activity, making it the right home for self-optimization. In cognitive psychology, this is metacognition — the capacity to monitor and regulate one's own cognitive processes.

The skill uses `CognitiveEngine::introspect()` to query memory patterns — tag co-occurrence matrices, temporal activation patterns, Hebbian weight distributions, decay curves, and cross-role activation overlap.

| Signal | Evidence | Proposal |
|--------|----------|----------|
| **Cognitive fragmentation** | Activation clusters with near-zero tag overlap within one role | "My code-review role has two distinct clusters: frontend UI and infrastructure. Consider splitting." |
| **Redundant attention** | Two roles activate nearly identical engrams | "My research and writing roles share the same memory. These should probably merge." |
| **Emergent specialization** | New tag cluster forming that doesn't match any existing role's lens | "I keep handling data-analysis tasks in my persona role. There may be an unnamed specialization emerging." |
| **Cognitive atrophy** | A role hasn't been activated in an extended period | "My data-analysis role hasn't been active in 3 months. Should we retire it?" |
| **Attentional drift** | Activation results consistently miss or over-include engrams | Auto-adjust bias weights, tag filters, activation thresholds (within auto tier) |

### Tiered Autonomy Model (Autonomic vs. Deliberate)

Not all self-modifications carry the same risk. This mirrors the distinction between autonomic processes (breathing, reflexes) and deliberate cognition (planning, decision-making):

| Tier | Processing Mode | Examples | Approval |
|------|----------------|----------|----------|
| **Auto** (Autonomic) | Reflexive, self-regulating | Adjust bias weights, tune tag filters, adjust activation thresholds | Self-applied. Logged as `cognitive:meta` engram. |
| **Notify** (Aware) | Conscious but self-directed | New rules, belief updates, behavior pattern detection | Self-applied, user notified. User can review/override. |
| **Propose** (Deliberate) | Requires external validation | Split/combine/create/retire roles, toolset changes | Requires explicit user approval with evidence. |

```yaml
# Per-agent autonomy policy (operator config in Context Graph)
autonomy_policy:
  auto_tier:
    - lens_bias_adjustment
    - tag_filter_tuning
    - activation_threshold_adjustment
  notify_tier:
    - rule_extraction
    - belief_update
    - behavior_pattern_detection
  propose_tier:
    - role_split
    - role_combine
    - role_create
    - role_retire
    - toolset_change
  override: propose_all   # safety valve: require approval for everything
```

### Context Graph Integration

The Context Graph is the **management plane** for memory topology. It does not store cognitive content. It is to MuninnDB as Kubernetes etcd is to the application containers — one stores what exists, where it runs, and how it's configured; the other stores what is known, remembered, and believed.

#### Blueprint and Materialization

Memory-related components are hotel-level infrastructure. They must be declared in the hotel blueprint and managed through the existing `GuestManager` materialization and supervision lifecycle — the same way membrane, agent-core, and model-router are managed today.

| Component | Guest Type | Materialization | Health Check | Supervision |
|-----------|-----------|-----------------|--------------|-------------|
| **MuninnDB instance** | Infrastructure guest | Spawned by GuestManager on hotel boot. Config from `materialized_guests` table. | Port liveness on configured endpoint (REST 8475 or MBP 8474). | 5s supervisor loop; restart on crash. |
| **embeddinggemma** | Hotel-component guest | Spawned by GuestManager when embedding config specifies `backend: hotel_component`. | Embedding API health endpoint. | Standard guest supervision. |
| **Consolidation scheduler** | In-process service | Not a separate guest — runs as a service within the hotel daemon, similar to BeaconDaemon or BlobService. | Internal health flag. | Part of hotel daemon lifecycle. |

The Context Graph stores the blueprint for these components:

- **materialized_guests** table: rows for MuninnDB and embeddinggemma with `is_active`, PID, and config JSON
- **node_config**: memory-core configuration (vault topology, endpoints, consolidation schedules)
- **vault_registry**: which vaults are hosted on this instance, their authority status, and replica locations

When a hotel boots, the sequence extends the existing boot flow:

1. GuestManager reads `materialized_guests` → spawns MuninnDB instance (if configured as local guest)
2. GuestManager spawns embeddinggemma (if configured)
3. Hotel daemon starts consolidation scheduler service
4. memory-core SDK initializes: connects to MuninnDB, loads vault registry from Context Graph, applies any pending materialization sync for agent vaults
5. Agent guests materialize and receive their vault handles through the existing IPC handshake

For remote MuninnDB instances (the multi-hotel case where MuninnDB runs on a different host), the `materialized_guests` entry is replaced by a `muninn_endpoint` config pointing to the remote address. The Context Graph still tracks the connection and health status, but materialization and supervision are the responsibility of the hosting hotel.

#### Management-Plane State

| What | Where | Purpose |
|------|-------|---------|
| MuninnDB endpoints | Config: `muninn_endpoint` | Where the local instance lives |
| MuninnDB guest record | `materialized_guests` table | PID, is_active, config JSON for GuestManager supervision |
| embeddinggemma guest record | `materialized_guests` table | PID, is_active, config JSON for GuestManager supervision |
| Vault registry | Config: `vault_registry` | Which vaults exist on which instances |
| Auth tokens | `SecretRecord` (encrypted) | Per-vault bearer tokens |
| Attentional lens configs | `RoleIncarnationRecord` | Per-role activation and write conventions |
| Autonomy policies | Agent config (`bundle_json`) | What the agent can self-modify vs. propose |
| Propagation rules | Config: `memory_propagation` | Per-stratum propagation strategy |
| Vault authority map | Config: `vault_authority` | Authoritative instance per vault (multi-server) |
| Consolidation schedules | Config: `consolidation` | Per-vault consolidation policy |
| Embedding configuration | Config: `embedding` | Backend selection and routing |

**sync_apartment / get_apartment:** These methods on `GraphStorage` retain their management-plane responsibilities. Cognitive memory moves to MuninnDB vaults, but the Context Graph still uses these methods for what they were always meant for — managing the hotel's own state: topology records, configuration, running processes, and coordination data.

### Per-Vault MuninnDB Cognitive Tuning

Vaults are independently configurable. Cognitive tuning differs by stratum, reflecting how different memory systems in the brain have different consolidation and retrieval characteristics:

| Parameter | Semantic Vault (L0) | Autobiographical Vault (L1) | Working Vault (L2) |
|-----------|-----------|------------|---------------|
| ACT-R decay rate | Very slow (0.1) — facts persist | Moderate (0.3); identity engrams: 0.05 (near-permanent) | Aggressive (0.8) — transient by design |
| Hebbian learning | Cross-agent co-activation | Role-context co-occurrence | Minimal |
| Confidence tracking | High priority | Medium priority | Low priority |
| PAS (predictive) | User behavior prediction | Role-switching prediction | Disabled |
| Consolidation | Entity dedup, pattern extraction | Episode compression, rule extraction | Promote durable findings to L0/L1 |

### Configuration Schema

The entire memory topology is representable as operator configuration for the Philotic runtime, extending the existing `openclaw.json` pattern. This is the Philotic Stack's own configuration surface — it tells memory-core how to orchestrate its relationship with MuninnDB, not how to configure MuninnDB itself.

```yaml
memory:
  # Connection to the cognitive substrate
  muninn:
    endpoint: "127.0.0.1:8474"
    protocol: rest               # rest | mbp
    auth_secret_ref: muninn_api_key

  # Vault topology and per-stratum tuning
  vaults:
    user:
      pattern: "user:{user_id}"
      decay_rate: 0.1
      propagation: continuous
    self:
      pattern: "self:{agent_id}"
      decay_rate: 0.3
      identity_decay_rate: 0.05
      propagation: materialization_sync
    session:
      pattern: "session:{session_id}"
      decay_rate: 0.8
      propagation: on_handoff
      promotion_policy: selective

  # Consolidation subsystem
  consolidation:
    post_session:
      enabled: true
      promotion_policy: selective
      decay_acceleration: 3.0
    quiet_hours:
      schedule: "0 3 * * *"
      episode_compression: true
      pattern_extraction: true
      deduplication: true
    periodic_pruning:
      interval_seconds: 3600
      decay_threshold: 0.05
      archive_before_delete: true

  # Embedding backend selection
  embedding:
    backend: hotel_component
    hotel_component:
      component_id: embeddinggemma
    fallback: builtin

  # Multi-tenant access control (opt-in)
  multi_tenant:
    enabled: false
    default_access: private
    access_enforcement: read_filter

  # Per-role attentional lenses
  roles:
    persona:
      attentional_lens:
        activation:
          include_tags: [conversational, relational, user-model]
          recency_bias: 0.3
          frequency_bias: 0.7
        write:
          auto_tags: [persona, conversational]
          default_scope: shared_user
        cognitive:
          active_rule_tags: [social, empathy]
          active_belief_tags: [user-relationship]
    code_review:
      attentional_lens:
        activation:
          include_tags: [technical, architecture, review]
          recency_bias: 0.5
          frequency_bias: 0.5
        write:
          auto_tags: [code-review, technical]
          default_scope: self_only

  # Autonomy policy
  autonomy_policy:
    auto_tier: [lens_bias_adjustment, tag_filter_tuning]
    notify_tier: [rule_extraction, belief_update]
    propose_tier: [role_split, role_combine, role_create, role_retire]
```

## Migration Path

### What Changes

- Cognitive memory workload (what the agent knows, believes, remembers) moves from `sync_apartment` / `get_apartment` to MuninnDB vaults
- `MemoryApartment` and `MemoryEntry` types in `graph.rs` are retired for cognitive use
- LWW memory storage in `aiua_context.db` is replaced by vault-backed engrams

### What Gets Added

- `memory-core` module with MemoryEngine + CognitiveEngine capability contracts
- MuninnDB client (REST initially, MBP when validated)
- `MemoryOp` variant on `EventEnvelope` for mesh propagation
- Attentional lens fields on `RoleIncarnationRecord`
- Autonomy policy fields on agent config
- Vault topology, authority map, and consolidation config keys in Context Graph
- Consolidation subsystem with scheduling and observability
- Embedding backend configuration and hotel-component integration points

### What Stays

- **sync_apartment / get_apartment** retain management-plane responsibilities (topology, config, coordination, running processes)
- Context Graph remains the management plane (shared knowledge across hotels, hotel blueprints)
- Mesh event bus (`EventLedger`, `mesh_dispatcher`, `execution_transport`) carries memory ops
- IPC transport between agent-core and ansible (UDS + JSON framing) unchanged
- Hotel/server topology unchanged — hotels are still topographical boundaries

## Relationship to Prior Proposals

**MEMORY_ENGINE_ABSTRACTION_PROPOSAL** — This proposal implements and supersedes it. The abstraction boundary it called for (`search` / `store` / `explain`) is realized here as the MemoryEngine contract with richer semantics (remember, activate, link, traverse, subscribe, lens). The core insight — that memory should not collapse into one provider — is preserved through the capability boundary, even though MuninnDB is the intended first (and likely primary) backend.

**MUNINN_MEMORY_PROTOCOL_PROPOSAL** — This proposal does not supersede the SVE tooling or the MCP-based development habit. Those remain active and independent — they are how coding agents and operators interact with Muninn during the SVE development cycle, and that work has already surpassed expectations. This proposal defines the production Philotic-native integration path: a typed memory-core SDK, MuninnDB running as a hotel guest, and the full vault/stratum/lens/consolidation architecture as a first-class runtime concern. The two tracks coexist. The observations from SVE usage — retrieval quality, atomic memory granularity, the value of write-local recall — directly informed the design here and will continue to inform Phase 1 implementation.

**PLUGGABLE_CONTEXT_ENGINE_PROPOSAL** — The Context Graph remains pluggable behind `Arc<dyn GraphStorage>`. This proposal does not change that. It separates cognitive memory out of the Context Graph entirely, clarifying the boundary between management-plane state and cognitive content.

**PERSONALITY_AND_CONTEXT_PROPOSAL** — Soul text, voice patterns, and agent identity are stored as durable engrams in the `self:{agent_id}` vault with `identity_decay_rate: 0.05` (near-permanent). This proposal provides the concrete storage model for personality.

## Active Seams

- **memory-core-sdk** — The `memory-core` module does not exist yet. It is the critical-path deliverable.
- **memory-vault-topology** — Vault naming conventions, per-vault config, and vault authority maps need Context Graph schema changes.
- **role-lens-activation** — Attentional lens struct and `RoleIncarnationRecord` integration. Depends on role incarnation being stable.
- **introspection-skill** — The CognitiveEngine `introspect()` capability and the orchestrator skill that consumes it. Can be built after the base contract is working.
- **memory-propagation-mesh** — `EventEnvelope(MemoryOp)` variant and mesh dispatcher handling. Depends on mesh event bus being stable for memory payloads.
- **consolidation-subsystem** — Scheduling, pipeline execution, observability. Can be built incrementally after basic store/recall works.
- **embedding-integration** — Wiring memory-core to the embeddinggemma hotel-component. Depends on hotel-component service discovery patterns.

## Implementation Roadmap

### Phase 1: Foundation (Encoding)

Stand up the basic memory pipeline with MuninnDB as the cognitive substrate.

- Create `memory-core` module with MemoryEngine capability contract
- Implement REST-based MuninnDB client
- Vault addressing: `user:{user_id}` and `self:{agent_id}`
- Basic memory formation: perceive, attend, encode, store from post-turn hook
- Clarify sync_apartment boundary: cognitive memory moves out, management-plane stays
- Context Graph: `muninn_endpoint` + `vault_registry` config keys

### Phase 2: Attention and Propagation

Add role-aware memory retrieval and cross-hotel distribution.

- Attentional lens configuration on `RoleIncarnationRecord`
- Lens-aware activation (tag filters, bias weights)
- Write-side auto-tagging from lens conventions
- `EventEnvelope(MemoryOp)` emission for L0 engrams
- Mesh dispatcher propagation to peer hotels
- Session vault lifecycle: create, use, promote, decay
- Handoff-portable session memory

### Phase 3: Consolidation and Access Control

Build the consolidation subsystem and multi-tenant foundations.

- Post-session consolidation: promote, decay, archive
- Quiet-hours consolidation: episode compression, deduplication, pattern extraction
- Periodic pruning with configurable thresholds
- Consolidation observability: metrics, logs, admin triggers
- Multi-tenant access tags: private / shared / team:{id}
- Access tag assignment at encoding step
- Read-path filtering by requester permissions

### Phase 4: Metacognition

Add cognitive overlays and the introspection skill.

- CognitiveEngine capability implementation
- Decision tracking: `decide()` with rationale, alternatives, and confidence
- Contradiction detection and resolution: `contradict()`
- Pattern extraction from engram sets: `extract_patterns()`
- Rule extraction from patterns (stored as `cognitive:rule` engrams)
- Belief tracking with confidence (Bayesian updates via MuninnDB)
- Introspection skill: tag co-occurrence, activation clustering, decay analysis
- Tiered autonomy: auto/notify/propose pipeline
- Meta-memory: `cognitive:meta` engrams for self-observations

### Phase 5: Advanced Topology

MBP transport, materialization sync, and embedding integration.

- MBP client implementation in memory-core
- Vault authority map in Context Graph
- Materialization sync: checkpoint and restore agent self vaults
- Authority transfer protocol with fencing tokens
- Cross-scope queries (fan-out across user + self vaults)
- Embedding backend integration: embeddinggemma hotel-component wiring
- MuninnDB subscription (ActivationPush) for real-time memory triggers

## Open Questions

1. **embeddinggemma hotel-component mapping** — The embeddinggemma model will be a proper hotel-component. The exact service discovery and routing pattern between memory-core and the component needs design as hotel-component patterns mature.
2. **Vault migration wire format** — When an agent's authoritative vault moves between MuninnDB instances, what is the checkpoint format? Implementation decision for coding agents.
3. **Multi-user vault topology** — If an agent serves multiple users simultaneously, the access-tag approach (private/shared/team) is the starting point. Whether this eventually requires physical vault partitioning (separate `user:{user_id}` vaults with cross-vault fan-out) vs. tag-based filtering within a shared vault is a runtime scaling decision.
4. **Consolidation observability surface** — What metrics does the consolidation subsystem expose? Engrams scanned/pruned/merged/promoted is the minimum. Whether this feeds into the platform's existing observability stack or needs its own dashboard is TBD.
5. **Encoding decision logic** — The Attend step extracts cognitive outcomes from `WorkingTurn` state at turn-end: resolved contradictions, solidified beliefs, metacognitive observations, rejected approaches. It operates on outcomes, not on the reasoning path. This is a structured extraction from a bounded state object, not a full-context LLM pass. The exact extraction logic — heuristic rules, lightweight model call, or a hybrid — is an implementation decision for Phase 1, but the input surface is well-defined.
