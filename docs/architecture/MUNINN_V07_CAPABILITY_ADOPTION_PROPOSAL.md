---
title: Muninn v0.7 Capability Adoption Proposal
doc_type: proposal
domain: memory-context
status: accepted-current-slice
last_updated: 2026-06-27
tags:
  - muninn
  - memory
  - mcp
  - recall
  - api-keys
  - clustering
related_docs:
  - ARCHITECTURE_STATUS.md
  - MUNINN_MEMORY_PROTOCOL_PROPOSAL.md
  - MUNINN_CLUSTER_EVALUATION_CHECKLIST.md
  - ../reference/MCP_CREDENTIAL_LIFECYCLE.md
  - MEMORY_CULTIVATION_TRUE_UP_PROPOSAL.md
  - LIFE_GRAPH_OS_PROPOSAL.md
task_refs:
  - docs/task.md#muninn-v07-capability-adoption
proposal_id: muninn-v07-capability-adoption
implements:
  - muninn-memory-protocol
implemented_by:
  - muninn-v07-helper-and-uat-slice
active_seams:
  - muninn-scoped-client-keys
  - muninn-tagged-recall-lanes
  - muninn-concept-evolution-hygiene
  - muninn-hotel-cluster-authority
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# Muninn v0.7 Capability Adoption Proposal

## Goal

Adopt the useful Muninn v0.7 capabilities across Codex, Claude, Perplexity, and Philotic runtime surfaces without blurring memory authority, widening secrets, or turning every hotel into a replicated-memory cluster before we know which memory is supposed to be canonical.

This proposal covers four near-term capabilities:

1. scoped `observe` API keys for external clients
2. tag-filtered recall lanes for Codex, Claude, Perplexity, and Philotic roles
3. `muninn_evolve`-based memory label cleanup without breaking lineage
4. careful evaluation of Muninn cluster mode across the three hotels

## Disposition

`accepted for current slice`

Muninn v0.7.0 is installed and smoke-green across `local-bjork`, `mbp-jane`, and `vps-jane`, but these new capabilities are not yet adopted as Philotic policy.

Track execution in [docs/task.md](/Users/jaredlikes/code/philotic-stack/docs/task.md#muninn-v07-capability-adoption).

## Proven Current State

As of 2026-06-27:

- `local-bjork`, `mbp-jane`, and `vps-jane` report Muninn `v0.7.0`.
- all three Muninn MCP surfaces return 39 tools, including `muninn_recall` and `muninn_remember`.
- all three Muninn listeners are loopback-only.
- `auth_secret` checksums matched pre-upgrade backups on all three hotels.
- `mbp-jane` has a preserved `mcp.token`; no-token MCP access returns `401`.
- no Muninn API keys are currently configured on any hotel via `muninn api-key list`.
- the Perplexity-facing `context.capture` MCP route writes to Muninn continuity memory only; LifeGraph access remains split under its governed endpoint.

## Core Recommendation

Adopt Muninn v0.7 in three layers:

1. **Client access layer:** use scoped API keys and endpoint-specific MCP tokens so external clients get only the authority they need.
2. **Recall quality layer:** standardize a small tag lane vocabulary and use server-side tag filtering before adding more retrieval machinery.
3. **Memory hygiene layer:** use `muninn_evolve` for label and concept cleanup while preserving memory ULIDs and access history.
4. **Cluster evaluation layer:** evaluate cluster mode as an explicit authority decision, not as a default upgrade step.

The useful irony is that Muninn now has real distributed-systems features, which means we should become more conservative about enabling them. Replication makes memory more available; it also makes ownership mistakes more durable.

For current production posture, native Muninn MCP is a trusted private surface, not a public ingress. Codex and Claude may use `muninn mcp` against local loopback or a private tunnel. Perplexity and other external clients stay on the Philotic MCP frontdoor unless a separate scoped grant is approved.

## Capability 1: Scoped API Keys

### Recommendation

Create limited API keys for external clients that need Muninn access outside the local MCP token path.

Initial key classes:

| Client class | Mode | Expiry | Intended use |
| --- | --- | --- | --- |
| `external-readonly` | `observe` | 90 days | retrieval-only clients, dashboards, audit tools |
| `capture-writer` | `full` | 30-90 days | tightly scoped capture paths when MCP bearer routing is not enough |
| `operator-admin` | `full` | short/manual | operator maintenance, not routine agent use |

Use `observe` keys wherever possible. If a client only needs recall, it should not receive write authority. A memory system that hands every caller a pen is not collaborative; it is a shared whiteboard in a wind tunnel.

### First Slice

- create one `observe` key on `local-bjork` for client-side retrieval testing
- create one `observe` key on `mbp-jane` only if a remote client truly needs that hotel's memory surface
- do not create public API keys on `vps-jane` until ingress and revocation workflow are documented
- document labels, expiry, and intended client in an operator-only note, never in architecture prose with raw tokens

### Guardrails

- never commit raw API keys
- list keys by ID/label only
- prefer short expiries and rotation over permanent client keys
- revoke keys as part of client offboarding
- keep Perplexity `context.capture` on the existing MCP bearer route unless it specifically needs direct Muninn API access

## Capability 2: Tag-Filtered Recall Lanes

### Recommendation

Use Muninn v0.7 server-side recall filters to create explicit lanes for memory retrieval.

Initial lane vocabulary:

| Lane | Tags | Purpose |
| --- | --- | --- |
| `continuity` | `continuity`, `operator-preference`, `decision` | cross-client orientation and durable preferences |
| `philotic-runtime` | `philotic-stack`, `runtime`, `validation`, `reality-gap` | repo/runtime work and live ops |
| `mcp-boundary` | `mcp`, `perplexity`, `lifegraph`, `muninn-only` | external MCP boundary hygiene |
| `lifegraph-adjacent` | `lifegraph`, `evidence`, `recall-feedback` | LifeGraph context without treating Muninn as LifeGraph truth |
| `client-session` | `codex`, `claude`, `perplexity` | client-specific working continuity |

The tags are retrieval hints, not ontology. LifeGraph remains the structured operator graph; Muninn remains continuity memory.

### First Slice

- add helper support for `tags_all`, `tags_any`, and `tag_filter` in [scripts/muninn_mcp.py](/Users/jaredlikes/code/philotic-stack/scripts/muninn_mcp.py)
- update the repo-local Muninn memory habit and MCP surface hygiene guidance with the lane vocabulary
- smoke recall on recent memories from:
  - Perplexity capture memory `01KW3AZQXRTA4XAKSF2YFEKKQT`
  - Muninn upgrade memory `01KW3BW0X3G90S56RG2N9H1PF8`
- compare filtered recall against unfiltered recall before making the filter default

### Guardrails

- do not over-tag memories
- avoid tag names that imply confirmed LifeGraph truth
- prefer `tags_any` for broad orientation and `tags_all` for precise workflow recall
- keep tag lane changes in docs and skills so future clients do not invent parallel dialects

## Capability 3: Concept Evolution Hygiene

### Recommendation

Use `muninn_evolve` to improve memory labels and small concept metadata without deleting/re-creating memories.

This should become the default cleanup tool for:

- duplicate or vague concepts
- misspelled labels
- stale labels whose content is still valid
- old memories that need a better retrieval handle

It should not be used to rewrite history, launder mistakes, or make an old memory pretend it always meant the new thing.

### First Slice

- pick 10-20 recent memory records with weak labels
- evolve labels only, preserving ULIDs
- record before/after examples in a short operator note
- run recall before and after to see whether label cleanup improves retrieval

### Guardrails

- use `muninn_evolve` for label/concept hygiene, not content replacement
- if the content is wrong, write a correcting memory or use a contradiction/trust workflow
- keep the original ULID lineage visible in audit notes
- do not batch-evolve high-value memories without a reviewed candidate list

## Capability 4: Three-Hotel Cluster Evaluation

### Recommendation

Evaluate Muninn cluster mode across `local-bjork`, `mbp-jane`, and `vps-jane`, but do not enable it as the default continuity architecture until authority and failure semantics are explicit.

Cluster mode may be useful for:

- higher availability for cross-client continuity
- hotel-local recall when one machine is offline
- future memory failover for long-running agents

It may be harmful if:

- each hotel is meant to preserve local perspective rather than share one canonical memory
- external capture paths replicate low-quality or client-specific noise everywhere
- split-brain or stale recall creates false confidence during live ops
- secrets and API keys are copied as incidental cluster baggage

### First Slice

Run cluster mode as a lab evaluation, not production memory authority:

1. create three disposable test vaults, one per hotel
2. enable cluster mode only for those test vaults or an isolated data directory if supported
3. seed non-secret test memories
4. validate replication, failover, returning-primary deference, and recovery
5. prove that API keys and MCP tokens are not casually replicated into the wrong trust zone
6. write a decision record before enabling cluster mode for real continuity vaults

Use [MUNINN_CLUSTER_EVALUATION_CHECKLIST.md](/Users/jaredlikes/code/philotic-stack/docs/architecture/MUNINN_CLUSTER_EVALUATION_CHECKLIST.md) as the lab checklist before touching real continuity vaults.

### Decision Question

The key decision is not "can Muninn cluster?" It is:

Should Philotic have one canonical replicated Muninn continuity substrate, or should each hotel keep local Muninn memory and use LifeGraph / intel-graph / explicit exports for cross-hotel synthesis?

Until that is answered, clustering is promising infrastructure, not current architecture truth.

## Security Model

### Access Boundaries

- local MCP remains loopback-only by default
- external MCP endpoints stay behind explicit HTTPS ingress and bearer grants
- Muninn API keys are scoped, labeled, expiring credentials
- LifeGraph tools remain separate from Muninn capture tools
- cluster transport, if enabled, must have authenticated node identity and documented peer inventory

### Secret Handling

- raw tokens are never stored in docs
- key creation outputs are captured only into local operator secret stores
- docs may record key IDs, labels, modes, and expiry
- revocation is part of the lifecycle, not an emergency-only operation
- external bearer provisioning scripts must never echo the raw bearer token back to the terminal

Credential lifecycle and UAT rules live in [MCP_CREDENTIAL_LIFECYCLE.md](/Users/jaredlikes/code/philotic-stack/docs/reference/MCP_CREDENTIAL_LIFECYCLE.md).

## Verification Ladder

### Scoped API Keys

- `muninn api-key list` shows expected labels and expiry
- observe-mode key can recall/list but cannot write
- revoked key fails immediately

### Tag-Filtered Recall

- unfiltered recall baseline recorded
- `tags_any` returns lane-relevant memories
- `tags_all` narrows correctly
- filtered recall finds known Perplexity and upgrade memories

### Concept Evolution

- evolved memory keeps the same ULID
- new concept label appears in read/recall
- access history is preserved
- recall quality improves or the change is reverted

### Cluster Evaluation

- three nodes report expected roles
- failover elects one leader
- returning primary defers without split-brain
- test memories replicate once and remain readable
- cluster can be disabled without losing the original standalone data

## Current Slice

1. Land this proposal and task surface.
2. Add helper support for tag-filtered recall.
3. Create one short-lived observe API key for retrieval testing.
4. Run a small `muninn_evolve` cleanup trial on low-risk memories.
5. Draft the cluster evaluation checklist before enabling cluster mode anywhere.

Progress in this slice:

- [x] landed the proposal and task surface
- [x] added helper support for `tags_all`, `tags_any`, and `tag_filter`
- [x] created a short-lived local observe API key for retrieval UAT
- [x] verified observe-mode read/list succeeds and write is denied
- [x] drafted the cluster evaluation checklist
- [x] ran the low-risk `muninn_evolve` cleanup trial
- [x] added the MCP credential lifecycle runbook and `just mcp-client-uat` safe/local UAT gate
- [x] extended `just mcp-client-uat` with token-backed `context.capture` and `life.recall` positive-path calls
- [x] tightened `just mcp-client-uat live` so live mode now fails loudly when required bearer tokens are absent; `all` remains the safe/opportunistic mode
- [x] removed raw bearer echoing from the Perplexity `context.capture` provisioner
- [x] verified `just mcp-client-uat remote-native` against `vps-jane`: native Muninn stayed loopback-only and SSH-tunneled MCP health passed
- [x] added `just muninn-cluster-preflight` as a non-mutating cluster lab readiness gate before any cluster enablement
- [x] verified `RUN_REMOTE=1 just muninn-cluster-preflight all`: local, `mbp-jane`, and `vps-jane` have cluster CLI support, healthy standalone daemons, and no public remote MCP binding
- [x] proved disposable same-host Muninn daemon isolation with alternate REST/UI/MCP/MBP/gRPC bindings and `/tmp` data
- [x] recorded the current cluster enablement blocker: the CLI reaches the admin endpoint but does not attach an admin session cookie, so unauthenticated enablement fails with HTTP 401

Observed during the cleanup trial:

- evolving `01KW3CH26H5RD1W4R728T5J4YY` produced active successor `01KW3FVB9WHG646EWBR0CAQ34Z`
- the original memory became `soft_deleted`
- recall finds the successor with the updated concept
- the `muninn_evolve` response envelope still echoed an empty `concept` field even though `muninn_read` showed the correct evolved concept

## Deferred

- production Muninn cluster mode for real continuity vaults
- public Muninn API ingress
- automatic cross-client recall injection based only on tags
- LifeGraph writes through Muninn capture paths

## Open Questions

- Should `local-bjork` remain the primary continuity memory, or should `vps-jane` become the always-on continuity node?
- Should Perplexity get retrieval access, or remain write-only `context.capture` for now?
- Should tag lanes be global across all clients, or should client-specific lanes be projected by each adapter?
- Should cluster mode replicate all vaults or only explicit continuity vaults?
- What is the revocation ceremony for external memory clients?
