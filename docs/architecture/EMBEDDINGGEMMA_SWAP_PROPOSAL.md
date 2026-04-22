---
domain: memory-context
status: proposed
disposition: accepted for current slice
last_updated: 2026-03-31
active_seams:
- embeddinggemma-swap-validation
---

# EmbeddingGemma Swap Proposal

## Goal

Move graph embeddings from the current MiniLM default to EmbeddingGemma and prove stable semantic search quality and operational reliability.

## Core Recommendation

Treat model swap as a bounded seam with explicit verification gates:
- sidecar health and model load prove green
- embedding generation path remains deterministic
- semantic search results are non-empty for known benchmark queries
- fallback path to previous model remains available by configuration

## Disposition

`accepted for current slice` - Scope approved for this session as a focused implementation and validation seam.

## Current Slice

1. Register a dedicated workstream for this session in graph intelligence.
2. Add a seam-specific task surface and verification checklist.
3. Perform model swap wiring and smoke validation in a follow-on code slice.

## Verification Ladder Target

- `test-green`: embedding API + serialization behavior remains stable
- `smoke-green`: end-to-end `graph_embed` / `graph_semantic_search` behavior with EmbeddingGemma
- `watched-live-green` (optional): sustained sidecar run with repeated semantic queries
