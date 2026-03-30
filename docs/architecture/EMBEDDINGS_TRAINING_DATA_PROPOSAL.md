---
title: "Embeddings Training Data from Graph Intelligence"
doc_type: proposal
domain: memory-context
status: proposed
last_updated: 2026-03-29
tags:
  - embeddings
  - rl-training
  - graph-intelligence
  - feedback-loop
  - model-router
related_docs:
  - EMBEDDINGS_IN_GRAPH_PROPOSAL.md
  - LOCAL_ONNX_INFERENCE_PROPOSAL.md
  - GRAPH_INTELLIGENCE_PROPOSAL.md
  - AGENT_RESOURCE_MODEL_PROPOSAL.md
proposal_id: embeddings-training-data
active_seams:
  - training-signal-capture
  - embedding-drift-detection
  - semantic-feedback-loop
---

# Embeddings Training Data from Graph Intelligence

## Goal

Capture high-quality, ongoing training signals from agent interaction with the graph intelligence system to fine-tune embedding models (specifically the 300M Gemma variant) for better semantic understanding of codebase-specific concepts.

The hypothesis: **RL-tuned embeddings on agent-graph interaction data will produce more meaningful relative shifts for our specific domain** (Philotic Stack architecture, Rust patterns, seam nomenclature) than generic pre-trained embeddings.

---

## Core Recommendation

### Training Signal Sources

#### 1. Explicit Feedback (Strong Signal)

**Manual relevance rating in UI:**
```typescript
// Agent marks search result as relevant/not
interface RelevanceFeedback {
  query_embedding: number[];      // The query that was sent
  result_node_id: string;         // Which node was shown
  result_embedding: number[];     // The node's embedding at that time
  relevance: 1 | -1 | 0;          // Positive, negative, neutral
  context: string;                // Why this was/wasn't relevant
  agent_session: string;          // Which agent turn
  timestamp: string;
}
```

**Usage pattern:** Agent searches "lease management", sees `acquire_lease` function — marks as **+1 relevant**. Sees `telegram_webhook` — marks as **-1 irrelevant**. This creates positive/negative pairs for contrastive learning.

#### 2. Implicit Feedback (Medium Signal)

**Click-through and dwell time:**
- Clicked result = weak positive
- Dwell time > 10s on node page = stronger positive  
- Immediate back-button = negative
- Navigated to related node via edge = path reward

**Search refinement chains:**
```
Query 1: "poll" → no click
Query 2: "poll lease ownership" → click `acquire_lease`
```
This sequence teaches: "poll" alone is ambiguous, "poll lease" narrows to specific semantic cluster.

#### 3. Verification Ladder Signals (Strong Signal)

When a seam advances from `code-complete` → `test-green` → `smoke-green`, we capture:
```typescript
interface VerificationSignal {
  seam_id: string;
  proposal_embedding: number[];   // What was proposed
  code_embeddings: number[][];    // Functions that implemented it
  verification_level: string;     // test-green, smoke-green
  commit_shas: string[];          // Which commits
  // Positive pair: proposal <-> implementing code
}
```

This creates **grounded semantic links** — proposal text should be close in embedding space to the code that correctly implements it.

#### 4. Worktree Navigation Patterns (Weak Signal)

When an agent browses:
```
Proposal → Seam → Task → Function A → Function B
```
This path suggests A and B are semantically related (co-browsed), even if no explicit edge exists.

---

## Training Data Schema

### Primary Table: `embedding_feedback`

```sql
CREATE TABLE embedding_feedback (
    id TEXT PRIMARY KEY,
    feedback_type TEXT NOT NULL,     -- explicit, implicit, verification, navigation
    query_embedding BLOB,              -- May be NULL for verification type
    result_node_id TEXT NOT NULL,
    result_embedding BLOB NOT NULL,  -- Snapshot at time of feedback
    result_embedding_hash TEXT,      -- To detect model drift
    relevance_score REAL,            -- -1.0 to +1.0
    agent_session TEXT,
    worktree TEXT,
    timestamp TEXT NOT NULL,
    context_json TEXT                -- Additional metadata
);
```

### Training Export Format

```json
{
  "model_gen": "embeddinggemma-300m@a1b2c3d4",
  "exported_at": "2026-03-29T12:00:00Z",
  "pairs": [
    {
      "anchor": [0.12, -0.34, ...],    // 768 dims
      "positive": [0.11, -0.33, ...],  // Should be close
      "negative": [0.89, 0.21, ...],   // Should be far
      "weight": 1.0,                    // Confidence
      "source": "explicit_feedback"
    }
  ]
}
```

---

## RL Training Loop Architecture

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│  Graph Agent    │────▶│  Feedback Store  │────▶│  Training Data  │
│  (You, me, etc) │     │  (SQLite/Graph)  │     │  Export (JSONL) │
└─────────────────┘     └──────────────────┘     └─────────────────┘
         │                       │                       │
         ▼                       ▼                       ▼
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│  Semantic       │◄────│  Model Router    │◄────│  Fine-tuning    │
│  Search Results │     │  (onnx-runner)   │     │  Pipeline       │
└─────────────────┘     └──────────────────┘     └─────────────────┘
         │
         └──────────────────────────────────────────────┐
                                                          │
┌─────────────────────────────────────────────────────────┼──────────┐
│                    Model Drift Detection                │          │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────┐  │          │
│  │ Old Model   │  │ New Model   │  │ Embedding Hash  │◄─┘          │
│  │ emb(old)    │  │ emb(new)    │  │ Comparison      │             │
│  └─────────────┘  └─────────────┘  └─────────────────┘             │
└──────────────────────────────────────────────────────────────────────┘
```

---

## Positive/Negative Pair Construction

### Contrastive Learning Format

For each feedback event, construct triplets `(anchor, positive, negative)`:

**Explicit feedback:**
- Anchor: query embedding
- Positive: clicked/good result embedding  
- Negative: shown-but-ignored result embedding

**Verification ladder:**
- Anchor: proposal text embedding
- Positive: implementing code embedding
- Negative: code from unrelated proposal (random negative)

**Navigation path:**
- Anchor: starting node embedding
- Positive: next node in path
- Negative: random node not in path

---

## Deterministic Safeguards

| Risk | Mitigation |
|------|------------|
| Feedback spam / noise | Weight by agent trust score; require multiple confirmations |
| Embedding drift breaking search | Store `model_gen` with each embedding; detect drift via hash comparison |
| Training data contamination | Separate train/test by time; never train on future feedback |
| Overfitting to current patterns | Regular negative mining from unrelated domains |

---

## Implementation Phases

### Phase 1: Signal Capture ✅ PREREQ MET

- [x] Embedding storage in graph (DONE)
- [ ] Add `embedding_feedback` table to graph-intelligence
- [ ] Extend MCP tools to record feedback
- [ ] Add feedback UI to web interface

### Phase 2: Training Pipeline

- [ ] Export job: `graph_feedback_export → JSONL`
- [ ] Contrastive dataset builder
- [ ] Integration with HuggingFace `sentence-transformers` training
- [ ] Local fine-tuning script using `onnx-runner` as inference backend

### Phase 3: Model Swapping

- [ ] Hot-swap mechanism in `onnx-runner` (existing seam)
- [ ] A/B test: old vs new embeddings on held-out feedback
- [ ] Automatic rollback if search quality degrades

### Phase 4: Self-Improving Loop

- [ ] Background retraining trigger when feedback volume threshold hit
- [ ] Agent notification: "Embedding model updated based on your feedback"
- [ ] Explicit feedback on the embedding change itself (meta-learning)

---

## Disposition

`proposed` — awaiting Phase 1 completion (feedback capture infrastructure).

This is the **feedback loop that closes the learning cycle**: agents use the graph → graph learns from agent behavior → embeddings improve → agents find things faster.

Related: `LOCAL_ONNX_INFERENCE_PROPOSAL` (model router), `EMBEDDINGS_IN_GRAPH_PROPOSAL` (storage layer), `MUNINN_MEMORY_PROTOCOL` (agent context).

Last updated: 2026-03-29
