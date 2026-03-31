---
domain: memory-context
status: in-progress
disposition: accepted for current slice
last_updated: 2026-03-31
active_seams:
- onnx-runner-embed-surface
- model-router-embed-kind
- graph-intel-embed-mcp
---

# Embeddings in Graph Proposal

## Goal

Add semantic vector embeddings to the graph intelligence system for similarity search, semantic clustering, and intelligent code/code+doc matching.

## Core Recommendation

Embeddings should be **model-agnostic and swappable** via a sidecar pattern, not hardcoded to a single model. The system supports:
- **ONNX local models** (EmbeddingGemma, MiniLM) via `onnx-runner` sidecar
- **MLX Apple Silicon optimized** models via `mlx-runner` sidecar (future)
- **Remote API models** (OpenAI, etc.) via model-router delegation

Model selection is runtime-configurable via `PHILOTIC_ONNX_EMBED_REPO` env var or MCP `graph_embed_batch` parameters.

## Disposition

`accepted for current slice` - Schema implemented, sidecar operational with MiniLM. EmbeddingGemma integration in progress. Model controller abstraction being hardened.

## Current Slice

1. **Fix Web UI** for embeddings (complete) - Timeline, semantic search, proposal detail
2. **Trace workstream** in graph (in progress) - This session
3. **Swap to EmbeddingGemma** (next) - Currently MiniLM loaded
4. **Model controller sidecar** architecture (in progress) - Make embeddings swappable runtime

---

## Why This Matters

Current search is lexical (text matching). With embeddings:

- **Semantic search**: Find "lease management" when querying "poll ownership"
- **Similar code detection**: "This function looks like that one" across worktrees
- **Cross-domain discovery**: "Sessions" in runtime vs "sessions" in membrane - are they related?
- **Documentation drift detection**: Doc says X, code implements Y - embedding distance reveals mismatch
- **Proposal-to-code linking**: Auto-suggest which functions implement a proposal based on semantic similarity

---

## Schema Design

### Vector Storage

SQLite doesn't have native vector types. Options considered:

| Option | Pros | Cons |
|--------|------|------|
| BLOB of f32s | Native SQLite, fast retrieval | No native similarity ops |
| JSON array | Human-readable | Parsing overhead, 2x storage |
| Separate vectors table | Clean schema | JOIN overhead |
| **sqlite-vec extension** | Native vector ops, ANN | Extra dependency, C extension |

**Decision**: Store as BLOB in `nodes.embedding` with helper functions for cosine similarity. Add optional sqlite-vec support later for ANN.

### New Columns

```sql
-- In nodes table
embedding BLOB,           -- Serialized f32 array (4 bytes * dimensions)
embedding_model TEXT,     -- Model used: "text-embedding-3-small", "nomic-embed", etc.
embedding_dims INTEGER,   -- Dimensionality: 512, 768, 1536, etc.
embedding_updated TEXT,   -- ISO timestamp of last embedding generation
embedding_hash TEXT       -- Hash of source text to detect stale embeddings
```

### Vector Index

```sql
-- For similarity search acceleration (optional, sqlite-vec)
CREATE VIRTUAL TABLE IF NOT EXISTS node_vectors USING vec0(
    node_id TEXT PRIMARY KEY,
    embedding FLOAT[768]
);
```

---

## Embedding Sources

### What Gets Embedded

| Node Kind | Source Text | Example |
|-----------|-------------|---------|
| `Proposal` | Title + summary + goals | "Session Leases: managing concurrent session access..." |
| `Seam` | ID + description | "telegram-poll-lease: Telegram poll ownership..." |
| `Function` | Signature + doc + body (first 500 chars) | `pub fn acquire_lease(...) -> Result<...>` |
| `Type` | Definition + doc + impl summary | `pub struct Lease { ... }` |
| `Module` | Path + doc + public items summary | "membrane::lease - Poll lease management..." |
| `Snippet` | Full body | Complete function/type implementation |
| `Task` | Title + description | "Implement graceful poll release..." |

### Text Preprocessing

```rust
pub fn embeddable_text(node: &Node) -> String {
    match node.kind {
        NodeKind::Function => format!(
            "{sig}\n{doc}\n{body}",
            sig = snippet.signature,
            doc = snippet.doc_comment.unwrap_or(""),
            body = &snippet.body.unwrap_or_default()[..500.min(body.len())]
        ),
        NodeKind::Proposal => {
            let p = &node.properties;
            format!("{}\n{}\n{}", 
                p["title"].as_str().unwrap_or(""),
                p["summary"].as_str().unwrap_or(""),
                p["goals"].as_str().unwrap_or("")
            )
        }
        // ... etc
    }
}
```

---

## Embedding Generation

### Integration Points

**Option A: External Service**
- Call OpenAI, Azure, or local embedding model
- Pros: High quality, no GPU requirements
- Cons: API cost, latency, external dependency

**Option B: Local Model**
- Run `sentence-transformers`, `nomic-embed`, `bge-small` locally
- Pros: Deterministic, free, offline-capable
- Cons: Memory/GPU requirements, model management

**Option C: Hybrid**
- Default to local fast model for basic similarity
- Optional upgrade to high-quality model for critical matches
- Store both: `embedding_fast`, `embedding_quality`

### Model Compatibility Findings (2026-03-29)

| Model | Variant | Status | Notes |
|-------|---------|--------|-------|
| **MiniLM-L6-v2** | ONNX fp32 | ✅ **Working** | 384-dim, ~23MB, ~50ms on M1 |
| **EmbeddingGemma 300M** | ONNX q4 (~197MB) | ❌ Failed | "unknown exception in Initialize()" |
| **EmbeddingGemma 300M** | ONNX fp32 (~1.2GB) | ❌ Failed | Same init error |
| **EmbeddingGemma 300M** | ONNX quantized | ❌ Failed | Same init error |

**Root Cause:** ONNX Runtime incompatibility with Gemma embedding architecture. Per community research, requires specific `optimum-onnx` fork (not merged upstream).

**Decision:** Use MiniLM for production. Defer Gemma until upstream ONNX export fixed or MLX-native path viable.

---

### Generation Trigger

```rust
// After scan completes
fn generate_missing_embeddings(engine: &GraphEngine) -> Result<()> {
    let nodes: Vec<Node> = engine.query_nodes()
        .filter(|n| n.embedding.is_none())
        .filter(|n| should_embed(n.kind))  // Only specific kinds
        .collect();
    
    for batch in nodes.chunks(100) {
        let texts: Vec<String> = batch.iter()
            .map(embeddable_text)
            .collect();
        
        let embeddings = embedding_model.embed_batch(&texts)?;
        
        for (node, vec) in batch.iter().zip(embeddings.iter()) {
            engine.update_embedding(&node.id, vec)?;
        }
    }
}
```

---

## Similarity Search API

### SQL Implementation

```rust
/// Cosine similarity between two vectors
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (norm_a * norm_b)
}

/// Find similar nodes
pub fn find_similar(
    &self, 
    query_vec: &[f32], 
    kind: Option<NodeKind>,
    threshold: f32,
    limit: usize
) -> Result<Vec<(Node, f32)>> {
    let mut results = vec![];
    
    let mut stmt = self.conn.prepare(
        "SELECT id, kind, name, properties, embedding 
         FROM nodes 
         WHERE embedding IS NOT NULL
         AND (?1 IS NULL OR kind = ?1)"
    )?;
    
    let rows = stmt.query_map(params![kind.map(|k| k.as_str())], |row| {
        Ok((
            Node { /* ... */ },
            row.get::<_, Vec<u8>>(4)?,  // embedding blob
        ))
    })?;
    
    for (node, blob) in rows {
        let vec: Vec<f32> = deserialize_f32_vec(&blob);
        let sim = cosine_similarity(query_vec, &vec);
        if sim >= threshold {
            results.push((node, sim));
        }
    }
    
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    results.truncate(limit);
    Ok(results)
}
```

---

## MCP Tools

```
graph_semantic_search       → Search by semantic similarity
graph_find_similar          → Find nodes similar to given node
graph_embedding_status      → Check which nodes have embeddings
graph_generate_embeddings   → Trigger embedding generation
graph_semantic_cluster      → Cluster nodes by embedding similarity
```

### Tool: graph_semantic_search

```json
{
  "name": "graph_semantic_search",
  "inputSchema": {
    "query": "lease management ownership",
    "kind": "function",
    "threshold": 0.75,
    "limit": 10
  }
}
```

Response:
```json
{
  "results": [
    {"node_id": "fn:acquire_lease", "similarity": 0.89, "snippet": "..."},
    {"node_id": "fn:release_poll", "similarity": 0.82, "snippet": "..."},
  ]
}
```

---

## Use Cases Enabled

### 1. Proposal-to-Code Discovery

Query: "What code implements the telegram poll lease proposal?"

```rust
let proposal_embedding = embed(proposal_text);
let functions = find_similar(proposal_embedding, Some(Function), 0.70, 20);
// Suggests functions that semantically match the proposal
```

### 2. Cross-Worktree Similarity

"Show me functions similar to `acquire_lease` in other branches"

### 3. Documentation Drift Detection

Compare doc embedding vs implementation embedding. High distance = potential drift.

### 4. Semantic Clustering

Group modules/functions by concern using clustering on embeddings.

### 5. Intelligent Autocomplete

Given partial query "poll...", suggest completions from semantic neighborhood.

---

## Implementation Phases

### Phase 1: Schema + Storage ✅ COMPLETE

- [x] Add `embedding BLOB` column to `nodes` table
- [x] Add metadata columns: `embedding_model`, `embedding_dims`, `embedding_updated`, `embedding_hash`
- [x] Add embedding fields to `Node` struct
- [x] Add `serialize_embedding` / `deserialize_embedding` helpers
- [x] Add `cosine_similarity` function

### Phase 2: Generation ✅ COMPLETE

- [x] Create `embeddings.rs` HTTP client for ONNX sidecar
- [x] Integrate with `onnx-runner` sidecar at `127.0.0.1:11435`
- [x] Add `graph_embed` MCP tool with hash-based staleness detection
- [x] Add `graph_semantic_search` MCP tool

**ONNX Sidecar Integration:**

| Component | Location | API |
|-----------|----------|-----|
| Embeddings Client | `graph-intelligence/src/embeddings.rs` | HTTP client |
| ONNX Sidecar | `model-router` binary | `POST /api/embeddings` |
| Default Model | `onnx-community/embeddinggemma-300m-ONNX` | 768 dims |

### Phase 3: Advanced (Future)

- [ ] sqlite-vec extension for ANN
- [ ] Cross-modal embeddings (code + docs)
- [ ] Background embedding generation queue

---

## Deterministic Defaults

To maximize determinism:

| Decision | Rationale |
|----------|-----------|
| Local model default | No API variance, no network, reproducible |
| Fixed 768 dimensions | Consistent storage, predictable performance |
| Text truncation at 512 tokens | Deterministic input size |
| Hash-based staleness check | Re-embed only when source changes |
| Cosine similarity (not dot) | Normalized, threshold comparable across queries |

---

## Dogfooding Readiness Checklist

Before agents (you, me) actively use the intel graph for daily work:

| Requirement | Status | Notes |
|-------------|--------|-------|
| Embeddings generation | ✅ | `graph_embed` MCP tool ready |
| Semantic search | ✅ | `graph_semantic_search` MCP tool ready |
| ONNX sidecar running | ⚠️ | Need `model-controller-onnx --sidecar-only` |
| Proposals embedded | ⬜ | Seam 1: Batch embed all existing proposals |
| Auto-embed on scan | ⬜ | Seam 3: Background embedding generation |
| Web UI search panel | ⬜ | Seam: Add semantic search to index.html |
| Training data capture | ⬜ | Phase 1 of training proposal |

**Ready for dogfooding when:** ONNX sidecar is running + proposals are embedded.

---

## Disposition

`implemented` — Phase 1 (schema + storage) and Phase 2 (ONNX integration) complete.
Build verified. Ready for Phase 3 (advanced features).

Related: `GRAPH_INTELLIGENCE_PROPOSAL`, `DOCUMENT_REORGANIZATION_PROPOSAL`, `EMBEDDINGS_TRAINING_DATA_PROPOSAL`

Last updated: 2026-03-29
