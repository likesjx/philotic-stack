---
title: Cross-Agent Knowledge Architecture Proposal
doc_type: proposal
domain: memory-context
status: accepted-current-slice
last_updated: 2026-06-28
tags:
  - muninn
  - lifegraph
  - mcp
  - memory
  - client-access
related_docs:
  - LIFE_GRAPH_OS_PROPOSAL.md
  - MUNINN_V07_CAPABILITY_ADOPTION_PROPOSAL.md
  - MUNINN_CLUSTER_EVALUATION_CHECKLIST.md
  - MUNINN_MEMORY_PROTOCOL_PROPOSAL.md
  - ../reference/MUNINN_DIRECT_CLIENT_ACCESS.md
task_refs:
  - docs/task.md#cross-agent-knowledge-architecture
proposal_id: cross-agent-knowledge-architecture
implements:
  - muninn-memory-protocol
  - life-graph-os
active_seams:
  - muninn-native-client-access
  - lifegraph-muninn-promotion
  - cross-agent-context-packet
source_of_truth_targets:
  - ARCHITECTURE_STATUS.md
---

# Cross-Agent Knowledge Architecture Proposal

## Goal

Make Codex, Claude, Perplexity, Beacon, and future harnesses knowledgeable about Jared's life, decisions, and active work without flattening every memory surface into one unsafe global write path.

## Core Recommendation

Use a layered knowledge model:

1. **Muninn:** continuity memory, preference recall, decisions, reality gaps, and compact session handoff.
2. **LifeGraph:** structured life truth, evidence, commitments, goals, open loops, relationships, and governed updates.
3. **Intel Graph / repo docs:** project structure, code facts, proposals, seams, verification, and implementation truth.
4. **Philotic MCP frontdoor:** public HTTPS coordination surface with deliberately projected tools and bearer scopes.
5. **Native Muninn MCP:** private trusted-client surface over loopback, private overlay, or SSH tunnel only.

The architecture should be local-vault-first for Muninn. A hotel's native Muninn vault is its local continuity substrate. Cross-hotel synthesis should move through LifeGraph, Intel Graph, or explicit governed exports until a Muninn cluster decision proves authority, replication, and secret handling.

## Current Decision

Do not publicly expose native Muninn MCP on `vps-jane`.

The live-safe posture is:

- `vps-jane` native Muninn MCP stays on `127.0.0.1:8750`
- trusted agents use local loopback or an SSH tunnel to the remote loopback listener
- Perplexity continues through `https://mcp.jaredlikes.com/mcp` and `context.capture`
- LifeGraph access remains a separate governed MCP surface
- cluster mode remains lab-only until the authority decision is recorded

## Surface Ownership

| Surface | Owner | Writes | Reads | Public shape |
| --- | --- | --- | --- | --- |
| `context.capture` | Philotic MCP frontdoor | Muninn continuity memory | no broad recall by default | HTTPS bearer, Perplexity-safe |
| native `muninn_*` MCP | Muninn | continuity memories, decisions, labels | Muninn recall/read/status | loopback, private tunnel, or trusted local stdio proxy |
| `life.observe` | LifeGraph runner | proposed evidence signals | n/a | governed, may require approval |
| `life.commit` / `life.resolve` | LifeGraph runner | confirmed graph truth | n/a | unavailable or approval-gated externally |
| `life.recall` | LifeGraph runner | recall feedback only if invoked | governed context packets | HTTPS bearer, scoped endpoint |
| graph-intelligence MCP | Intel Graph | project decisions and verification | repo/project structure | local/dev or operator-scoped |

## Muninn To LifeGraph Flow

Muninn can surface candidate truth, but it does not confirm LifeGraph truth by itself.

Recommended promotion path:

1. Agent recalls Muninn continuity.
2. Agent identifies a life-relevant durable claim, preference, commitment, or contradiction.
3. Agent calls or proposes `life.observe` with provenance back to the Muninn memory ID.
4. LifeGraph stores this as evidence or a signal, not confirmed truth.
5. Operator or governed policy promotes it through `life.commit` / `life.resolve` when appropriate.
6. Muninn stores the compact decision or reality-gap delta, not a duplicate full graph record.

## LifeGraph To Muninn Flow

LifeGraph should feed context, not become a second transcript.

Recommended projection path:

1. Agent calls `life.recall` for a task-specific context packet.
2. The context packet separates confirmed graph facts, evidence, inferred intent, and missing context.
3. Agent uses that packet during the turn.
4. If the turn produces a durable learning, the agent writes a short Muninn memory that points to the LifeGraph node or packet handle.

Good Muninn write:

```text
Decision: Use LifeGraph node life:commitment:... as the canonical source for the active family travel commitment; Muninn should recall it only as a continuity pointer.
```

Bad Muninn write:

```text
Full copy of every LifeGraph fact, evidence snippet, and retrieval packet.
```

## Cross-Agent Context Packet

Every knowledgeable agent/harness should assemble context in this order:

1. **Identity:** who is the agent, what role is it playing, and what authority does it have?
2. **Operator continuity:** Muninn recall for Jared preferences, decisions, and active seams.
3. **Life truth:** LifeGraph recall for goals, commitments, open loops, relationships, and evidence-backed facts.
4. **Project truth:** repo docs, Intel Graph, and runtime observations for implementation facts.
5. **Policy:** projected tools and write permissions for the current endpoint.

This keeps "knowing Jared" from becoming a permission mistake. A client can be emotionally and operationally helpful while still lacking authority to mutate canonical truth.

Implemented slice:

- `data-memorygraphrag::ContextPacket` is the cross-agent envelope for Muninn, LifeGraph, Intel Graph, repo, and runtime references.
- `ContextRef.authority` labels every reference as `muninn_continuity`, `life_graph_truth`, `life_graph_evidence`, `intel_graph_project_truth`, `runtime_observation`, or `agent_inference`.
- `life.recall` now returns both the existing LifeGraph `context_packet` and a `cross_agent_context_packet` projection.
- contract validation rejects a Muninn engram ref that claims `life_graph_truth` authority.

## Agent Defaults

| Client | Default access | Notes |
| --- | --- | --- |
| Codex | local native Muninn MCP plus repo/Intel Graph | `.mcp.json` uses `muninn mcp` against loopback |
| Claude Code/Desktop | local native Muninn MCP when trusted | use the same stdio proxy shape as Codex |
| Perplexity | Philotic MCP frontdoor | `context.capture` remains Muninn-only; retrieval needs separate scoped grant |
| Beacon / philotes | hotel-mediated tools | native projected tools inside Philotic policy |
| Future harnesses | start read-only | earn write access through explicit bearer/key scope |

## Remote Trusted Native Access

Current decision: standardize on SSH tunnel access for remote trusted native Muninn.

Use `scripts/muninn-private-access.sh` or `just muninn-private-smoke` to prove the path:

1. local Muninn MCP is healthy
2. `vps-jane` native Muninn MCP is not public-bound
3. local port `18750` forwards to `vps-jane:127.0.0.1:8750`
4. the shared MCP helper succeeds against `http://127.0.0.1:18750/mcp`

Do not add a private HTTPS native Muninn ingress until API key/bearer scope, rotation, and revocation are documented. Do not treat Tailscale reachability as public exposure, but also do not make it the only required mechanism until the client configuration story is explicit.

## Cluster Decision

Muninn cluster mode should not become production continuity authority until the decision answers:

- is there one canonical replicated Muninn continuity substrate, or hotel-local Muninn vaults?
- which vaults may replicate?
- who can write replicated memory?
- how are API keys and MCP tokens excluded from replication mistakes?
- how do LifeGraph and Intel Graph remain canonical for their own truth domains?

Until then, prefer local vaults plus governed synthesis through LifeGraph and explicit exports.

## Verification

Minimum evidence before calling this slice live-green:

- local Codex MCP config contains `muninn-local`
- local Muninn helper reports MCP health
- `vps-jane` native Muninn listener is loopback-only
- `vps-jane` firewall still blocks unintended public raw MCP ports
- Perplexity `context.capture` remains on the Philotic MCP frontdoor
- `cargo test -p data-memorygraphrag` passes with ContextPacket authority-boundary tests
- `just muninn-private-smoke` proves the SSH tunnel path and remote private binding

## Open Seams

- Decide whether LifeGraph conflict handoff should call Muninn `muninn_evolve`, `muninn_decide`, or a dedicated true-up tool.
- Teach other recall producers, including Intel Graph and Muninn helper output, to project into `ContextPacket` when they are used together in a model turn.
- Revisit Tailscale-only or private HTTPS native Muninn access only after credential lifecycle and client config are explicit.
