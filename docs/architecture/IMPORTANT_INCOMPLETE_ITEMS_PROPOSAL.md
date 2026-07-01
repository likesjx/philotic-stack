---
domain: workflow-docs
status: proposed
disposition: proposed
last_updated: 2026-06-30
tags:
- roadmap
- audit
- remaining-work
- proposal-rollup
related_docs:
- PROPOSAL_ORGANIZATION_PROPOSAL.md
- VERIFICATION_LADDER_PROPOSAL.md
---

# Important Incomplete Items — Consolidated Remaining-Work Roadmap

## Provenance

This is a **derived rollup**, not a new feature proposal. It was produced on 2026-06-30 by a
forensic audit of all 110 proposals in the intel-graph: each proposal's self-declared
disposition was cross-referenced against the live codebase, git history, and operator memory
by ten domain-batch verifier agents. Every line below is backed by a commit SHA or `file:line`
in the per-proposal verdict records.

The audit's headline finding: the graph **understates** how much has shipped (21 proposals are
fully implemented vs. 9 self-labeled "implemented") **and** scatters the genuinely-remaining work
across dozens of "accepted-current-slice" docs with no single view. This document is that single
view — the work that is actually still open and worth doing.

Scope note: this rollup deliberately **excludes** the 21 implemented and 3 obsolete proposals.
It ranks the remainder into three tiers by how unbuilt and how load-bearing each item is.

---

## Tier 1 — Deferred but wanted (≈zero implementation)

These were accepted in principle but have essentially no code. They are the cleanest "important
incomplete" items: a decision was made that they matter, and nothing has been built.

### runner-artifact-build-distribution `[deployment-distribution]`
Zero implementation, consistent with its `deferred` label. The entire artifact plane is open:
builder/distributor/materializer roles, `build_job`/`artifact`/`runner_release` records, and the
sandbox/verification/provenance/approval trust model. Currently every host runs `cargo build`
in place (see `rh-ansible-vps-deployment` below — same gap from the deploy side).

### memory-durability `[memory-context]`
No `MemoryOp`, `MemoryEvent`, or `VaultAdvertisement` types exist anywhere in `crates/`. Open:
L0 stability pinning in the Attend step; L1 `MemoryEvent` write-ahead replication over
`mesh_events` + cursor replay; `VaultAdvertisement` bootstrap broadcast + receiving-hotel handler.
Blocked-behind: depends on `philote-memory-core`'s mesh-propagation seam (also unbuilt).

### memory-enrichment-rust-port `[cognitive-plane]`
Not ported. Only a no-op `retry_enrich()` placeholder exists (`memory-core/engine.rs:73`). Open:
`EnrichmentExtractor` trait in memory-core, `enrich_with_content()` on `MemoryEngine`, and a
`GeminiEnrichmentExtractor` calling model-router Gemini Flash structured output.

### embeddinggemma-swap `[memory-context]` — **also a correctness flag**
An EmbeddingGemma ONNX backend was wired (onnx-runner defaults to `embeddinggemma-300m-ONNX`),
**but** the canonical embed model the Life Graph actually shipped is `all-mpnet-base-v2`
(V003, commit d8ed735), and `EMBEDDINGS_IN_GRAPH` records EmbeddingGemma ONNX **failing init**.
Open: reconcile which model is actually canonical, then sustained validation of semantic-search
quality on it. (Audit recommends demoting this from `accepted-current-slice` → `deferred`.)

---

## Tier 2 — Genuinely unbuilt (still `proposed`, high confidence)

Verified absent in code. Grouped by the cluster they belong to. The **operator trust/control
cluster** is called out first because batch05 found the entire grant/posture/ceremony layer is
unbuilt while only Tier-1 typed confirmation exists — a coherent, load-bearing gap.

**Operator trust & control plane**
- `operator-identity-and-dangerous-action-ceremonies` — ActionGrant / break-glass / dangerous-action
  ceremonies; only typed confirmation exists today.
- `role-posture-and-admin` — `admin_elevated` posture model + session elevation.
- `control-plane-admin-surface` — CLI/TUI admin surface + action-grant contract.
- `local-admin-fallback-model` — ONNX admin fallback path when cloud is unreachable.

**Membrane & external reach**
- `external-agent-event-membranes` — A2A / Nostr membrane contracts (doc-only, no code).
- `mcp-coordination-endpoint` — substrate exists (membrane-mcp live), but the coordination
  tool-catalog + philote chat-dispatch mapping is unbuilt.

**Capability & fleet composition**
- `unified-capability-stream` — only `CapabilityRequest`/`Event` envelope types landed; the
  `CapabilityProvider` trait unification (the actual proposal) is unstarted.
- `capability-pool-and-purpose-composition` — zero code.
- `multi-agent-coding-fleet` — process-only; no handoff-packet/pilot. `fleet.rs` is unrelated.

**Graph / model context**
- `graph-datasource-philote` — no `GraphIntelligenceProvider` anywhere.
- `embeddings-training-data` — no `embedding_feedback` table, no training-collector.
- `model-graph-context-1` — naming collision: `Context1Advisory` is a planning approval type,
  not the proposed model-selection graph.
- `model-graph-catalog` — docs-only (commit 5711d59); schema/seed/projection unbuilt.

**Mesh & distribution**
- `mesh-visibility-state-placement` — mesh-visible state contract unbuilt.
- `multi-hotel-component-distribution` — precursor transport exists; the materialization-intent /
  capacity-relief ceremony is unbuilt.
- `openclaw-parity-migration` — parity matrix + migration-readiness gates not started.

**Workflow & docs hygiene**
- `worktree-reintegration-tracking` — no reintegration graph state; only hot-file overlap scripts.
- `document-reorganization` — `generated/` holds unrelated PlantUML, not the proposed migration.
- `guest-identity-component-type-proposal` — rename genuinely not started.

Lower-confidence / exploratory (kept here for completeness, flagged medium):
`memory-layering-and-work-product-split`, `memory-relation-lifecycle-whitepaper`
(partially-superseded), `role-context-shift-subagents`, `streaming-tts-and-music-analysis`.

---

## Tier 3 — Shipped a slice, heaviest backlog remains (`accepted-current-slice`)

These have a verified first slice in production but the largest open backlogs. Ordered by gap count.

| Proposal | Verif. | The load-bearing gaps |
|---|---|---|
| `model-graph-flywheel` | watched-live | model-graph schema + trust model; Routing Oracle query layer; vision/image pipeline |
| `mesh-pki-hotel-identity` | test-green | per-peer beacon HMAC enforcement; MeshMemberRecord lifecycle + list/revoke CLI; revocation propagation |
| `desktop-membrane` | smoke-green | full remote mgmt-plane transport (mutual auth, replay resistance, audit); action-grant ceremonies; drop bearer-token fallbacks |
| `model-controller` | smoke-green | OpenAI realtime websocket; voice.dialogue / sound.generate / music.generate stubs; multimodal audio routing |
| `philote-memory-core` | test-green | mesh propagation (EventEnvelope(MemoryOp) + dispatcher); consolidation subsystem; multi-tenant access tags |
| `telegram-integration` | smoke-green | webhook ingress + secret-token contract (polling only today); edit-based streaming; chunking/entities |
| `skill-lifecycle-delegation-contract` | smoke-green | mesh-capability validation; skill-creator meta-skill; **skill.register authz gate (currently open to any agent)** |
| `singular-mesh-membership` | smoke-green | global membership convergence (still partly pairwise); revocation propagation; audit lineage |
| `rh-ansible-vps-deployment` | watched-live | pre-built artifact fetch (no on-host build); secret-handling hardening; Tailscale auth automation |
| `routed-operator-chat` | smoke-green | provider-native incremental generation; session continuity; watched-live remote-hotel proof |
| `dev-engine-optimization` | smoke-green | `just memory-consolidate` missing; VPS Muninn truth-cache deploy; engram schema |
| `agent-workstream-tracking` | smoke-green | WebSocket live board; session metrics; fix per-session workstream de-dup |

Security-relevant gaps to prioritize: the **`skill.register` authorization gate** (open to any
agent today) and the **Telegram webhook secret-token contract** stand out as the highest-risk
items in this tier.

---

## How to use this document

- Per-proposal evidence (commit SHAs, file:line, MEMORY refs) lives in the audit's verdict records.
- This rollup is a snapshot of 2026-06-30. Re-derive after the next batch of merges rather than
  hand-editing — it is downstream of the proposals, not a source of truth for them.
- Companion deliverable of the same audit: a set of **disposition corrections** (35 proposals whose
  graph label disagreed with verified reality — including demotions like `memory-cultivation-true-up`
  `implemented`→`accepted-current-slice`), applied separately at the proposal frontmatter source.
