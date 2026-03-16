use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::engine::MemoryEngine;
use crate::types::{
    ActivationResult, AttentionalLens, Engram, EngramId, EngramRef, LinkKind, MemoryScope,
};

// ──── NullMemoryEngine ────────────────────────────────────────────────────────

/// Stub implementation that returns an error for every operation.
///
/// Used as the default when no MuninnDB backend is configured, and as a
/// baseline in tests that verify error-path behavior.
///
/// Phase 1 replaces this with the MuninnDB REST client (`MuninnRestEngine`).
pub struct NullMemoryEngine;

#[async_trait]
impl MemoryEngine for NullMemoryEngine {
    async fn remember(
        &self,
        _scope:   MemoryScope,
        _concept: &str,
        _content: &str,
        _tags:    Vec<String>,
    ) -> anyhow::Result<EngramRef> {
        anyhow::bail!("NullMemoryEngine: no memory backend configured")
    }

    async fn remember_batch(
        &self,
        _scope:   MemoryScope,
        _entries: Vec<(String, String, Vec<String>)>,
    ) -> anyhow::Result<Vec<EngramRef>> {
        anyhow::bail!("NullMemoryEngine: no memory backend configured")
    }

    async fn evolve(
        &self,
        _id:      &EngramId,
        _content: &str,
        _tags:    Option<Vec<String>>,
    ) -> anyhow::Result<EngramRef> {
        anyhow::bail!("NullMemoryEngine: no memory backend configured")
    }

    async fn forget(&self, _id: &EngramId) -> anyhow::Result<()> {
        anyhow::bail!("NullMemoryEngine: no memory backend configured")
    }

    async fn activate(
        &self,
        _context:     &str,
        _scope:       MemoryScope,
        _max_results: Option<usize>,
    ) -> anyhow::Result<ActivationResult> {
        anyhow::bail!("NullMemoryEngine: no memory backend configured")
    }

    async fn read(&self, _id: &EngramId) -> anyhow::Result<Option<Engram>> {
        anyhow::bail!("NullMemoryEngine: no memory backend configured")
    }

    async fn link(
        &self,
        _from_id: &EngramId,
        _to_id:   &EngramId,
        _kind:    LinkKind,
    ) -> anyhow::Result<()> {
        anyhow::bail!("NullMemoryEngine: no memory backend configured")
    }

    async fn traverse(
        &self,
        _from_id:   &EngramId,
        _max_depth: Option<usize>,
    ) -> anyhow::Result<Vec<Engram>> {
        anyhow::bail!("NullMemoryEngine: no memory backend configured")
    }

    async fn set_lens(&self, _lens: AttentionalLens) -> anyhow::Result<()> {
        anyhow::bail!("NullMemoryEngine: no memory backend configured")
    }

    async fn current_lens(&self) -> anyhow::Result<Option<AttentionalLens>> {
        anyhow::bail!("NullMemoryEngine: no memory backend configured")
    }

    async fn subscribe(
        &self,
        _context: &str,
        _scope:   MemoryScope,
    ) -> anyhow::Result<mpsc::Receiver<Engram>> {
        anyhow::bail!("NullMemoryEngine: subscribe not available until Phase 5 MBP transport")
    }
}
