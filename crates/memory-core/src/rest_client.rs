use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, instrument};

use crate::engine::MemoryEngine;
use crate::types::{
    ActivationResult, AttentionalLens, Engram, EngramId, EngramRef, LinkKind, MemoryScope, VaultId,
};

// ──── Config ──────────────────────────────────────────────────────────────────

/// Connection configuration for a MuninnDB REST instance.
///
/// MuninnDB uses per-vault API keys. The hotel daemon stores these in
/// `SecretRecord` entries and loads them at boot. Each vault that requires
/// auth must have an entry in `vault_tokens`.
///
/// Vault naming convention: `[a-z0-9_-]` only, max 64 chars.
/// `_` is the scope prefix separator; agent/user IDs use `-` internally.
/// Examples: `self_philote-1`, `user_jared`, `session_01abc-def`.
/// A vault name is always `{scope}_{id}` where scope is the first `_`-delimited segment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MuninnConfig {
    /// Base URL, e.g. `http://127.0.0.1:8475`
    pub base_url: String,
    /// Per-vault bearer tokens. Key is the vault name; value is the API token.
    /// Loaded from `SecretRecord` entries in the Context Graph at hotel boot.
    pub vault_tokens: HashMap<String, String>,
    /// Fallback token for vaults not present in `vault_tokens` (e.g. open vaults).
    pub default_token: Option<String>,
    /// Default vault when MemoryScope does not resolve to a named vault.
    pub default_vault: String,
    /// Muninn-cluster single-writer routing: node id of the hotel that owns
    /// the cluster PRIMARY (Cortex). When set — and the resolved vault is
    /// fleet-shared (see [`is_fleet_shared_vault`]) — guests forward the
    /// write through the hotel mesh to that node instead of writing to the
    /// local replica, where it would strand: observer/lobe muninn daemons
    /// accept local writes but never forward them, and only Cortex writes
    /// replicate (scrypster/muninndb#631). `None` (the default, and the
    /// correct value on the Cortex hotel itself) preserves direct local
    /// writes. Rides inside `MemoryConfigPayload.config_json`, so adding it
    /// is wire-compatible with older guests.
    #[serde(default)]
    pub shared_write_route: Option<String>,
}

/// True for vaults whose contents are fleet-visible and therefore must be
/// written on the cluster PRIMARY: the shared `default` vault and `user_*`
/// vaults. Agent (`self_*`) and `session_*` vaults are per-host by design —
/// the muninn vault registry does not replicate — and always write locally.
pub fn is_fleet_shared_vault(vault: &str) -> bool {
    vault == "default" || vault.starts_with("user_")
}

impl MuninnConfig {
    pub fn local(default_vault: impl Into<String>) -> Self {
        Self {
            base_url: "http://127.0.0.1:8475".to_string(),
            vault_tokens: HashMap::new(),
            default_token: None,
            default_vault: default_vault.into(),
            shared_write_route: None,
        }
    }

    pub fn with_vault_token(mut self, vault: impl Into<String>, token: impl Into<String>) -> Self {
        self.vault_tokens.insert(vault.into(), token.into());
        self
    }
}

// ──── Token rejection (self-heal signal) ─────────────────────────────────────

/// MuninnDB actively rejected our bearer token (HTTP 401): the server is
/// reachable but the stored token is stale — the token↔key binding spans two
/// independent stores (MuninnDB's key store and the hotel Context Graph
/// secret record), and MuninnDB has lost or rotated its half. Distinct from
/// network-unreachable so callers can trigger a token re-mint
/// (`IpcRequest::HealMemoryToken`) instead of backing off.
#[derive(Debug, Clone)]
pub struct TokenRejected {
    pub vault: String,
}

impl std::fmt::Display for TokenRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "muninn rejected bearer token for vault [{}] (HTTP 401) — stored token is stale",
            self.vault
        )
    }
}

impl std::error::Error for TokenRejected {}

/// If `err` is (or wraps) a [`TokenRejected`], return the rejected vault name.
pub fn token_rejected_vault(err: &anyhow::Error) -> Option<&str> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<TokenRejected>())
        .map(|t| t.vault.as_str())
}

// ──── Vault Address Resolution ────────────────────────────────────────────────

/// Resolves a `MemoryScope` + agent context into a concrete MuninnDB vault name.
///
/// Vault naming conventions (`_` = scope prefix separator, `-` = id internal separator):
///   L0 Semantic         →  `user_{user_id}`       e.g. `user_jared`
///   L1 Autobiographical →  `self_{agent_id}`      e.g. `self_philote-1`
///   L2 Working          →  `session_{session_id}` e.g. `session_01abc-def`
///   CrossScope          →  fan-out (handled by the caller per scope)
#[derive(Debug, Clone)]
pub struct VaultResolver {
    pub agent_id: String,
    pub user_id: String,
}

/// Coerce a vault-name component into MuninnDB's allowed alphabet
/// (`[a-z0-9_-]`, max 64 chars total for the full name). Session-derived
/// fallback ids (`cron:ephemeral:agent-aria`, `telegram:123:agent-jane`)
/// contain `:` and uppercase, which MuninnDB rejects outright — before this,
/// every shared/session-scope write from such a session failed at the vault
/// layer. Mapping is deterministic so the same session always resolves to
/// the same vault.
fn sanitize_vault_component(raw: &str) -> String {
    let mut out: String = raw
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Leave room for the "{scope}_" prefix within MuninnDB's 64-char limit.
    out.truncate(56);
    out
}

impl VaultResolver {
    pub fn resolve(&self, scope: &MemoryScope) -> Vec<VaultId> {
        match scope {
            MemoryScope::SelfOnly => {
                vec![format!("self_{}", sanitize_vault_component(&self.agent_id))]
            }
            MemoryScope::SharedUser => {
                vec![format!("user_{}", sanitize_vault_component(&self.user_id))]
            }
            MemoryScope::Session(id) => {
                vec![format!("session_{}", sanitize_vault_component(id))]
            }
            MemoryScope::CrossScope(scopes) => {
                scopes.iter().flat_map(|s| self.resolve(s)).collect()
            }
        }
    }

    /// Returns the primary vault for write operations (first resolved vault).
    pub fn resolve_primary(&self, scope: &MemoryScope) -> VaultId {
        self.resolve(scope)
            .into_iter()
            .next()
            .unwrap_or_else(|| "default".to_string())
    }
}

// ──── Wire Types (MuninnDB REST API shapes) ───────────────────────────────────

#[derive(Debug, Serialize)]
struct WriteRequest {
    vault: String,
    concept: String,
    content: String,
    tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    confidence: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<serde_json::Value>,
    /// Caller-provided idempotency key. MuninnDB returns the existing engram
    /// if a live engram with this key already exists in the vault, rather than
    /// creating a duplicate. Use for identity/system writes that should
    /// reinforce rather than accumulate.
    #[serde(skip_serializing_if = "Option::is_none")]
    idempotent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WriteResponse {
    id: String,
    /// Set to "duplicate_content" by MuninnDB when content-hash dedup fired
    /// (i.e. identical content already existed and was reinforced instead of
    /// creating a new engram). Useful for observability / dedup metrics.
    #[serde(default)]
    hint: Option<String>,
}

#[derive(Debug, Serialize)]
struct BatchWriteRequest {
    vault: String,
    engrams: Vec<BatchItem>,
}

#[derive(Debug, Serialize)]
struct BatchItem {
    concept: String,
    content: String,
    tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    idempotent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BatchWriteResponse {
    results: Vec<BatchResult>,
}

#[derive(Debug, Deserialize)]
struct BatchResult {
    id: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ActivateRequest {
    vault: Option<String>,
    context: Vec<String>,
    max_results: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ActivateResponse {
    total_found: usize,
    activations: Vec<ActivationItem>,
}

#[derive(Debug, Deserialize)]
struct ActivationItem {
    id: String,
    concept: String,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    confidence: f32,
    /// Server-side activation relevance for the query (semantic + graph
    /// blend). Defaults to 0.0 against servers that predate the field, in
    /// which case cross-scope ranking degrades to recency + confidence.
    #[serde(default)]
    score: f64,
    created_at: i64,
    updated_at: Option<i64>,
    #[serde(default)]
    metadata: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct ReadResponse {
    id: String,
    concept: String,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    confidence: f32,
    created_at: i64,
    updated_at: Option<i64>,
    #[serde(default)]
    metadata: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct LinkRequest {
    vault: String,
    source_id: String,
    target_id: String,
    relation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    weight: Option<f32>,
}

// ──── Type Conversions ────────────────────────────────────────────────────────

impl From<ActivationItem> for Engram {
    fn from(item: ActivationItem) -> Self {
        Engram {
            id: item.id.clone(),
            vault_id: String::new(), // not returned by activate; filled by context
            concept: item.concept,
            content: item.content,
            tags: item.tags,
            confidence: item.confidence,
            created_at: item.created_at as u64,
            updated_at: item.updated_at.unwrap_or(item.created_at) as u64,
            metadata: item.metadata,
        }
    }
}

impl From<ReadResponse> for Engram {
    fn from(r: ReadResponse) -> Self {
        Engram {
            id: r.id.clone(),
            vault_id: String::new(), // filled by caller after vault discovery
            concept: r.concept,
            content: r.content,
            tags: r.tags,
            confidence: r.confidence,
            created_at: r.created_at as u64,
            updated_at: r.updated_at.unwrap_or(r.created_at) as u64,
            metadata: r.metadata,
        }
    }
}

/// Cross-scope merge ranking. Each vault's activations arrive already ranked
/// by the server's query relevance; merging on raw `confidence` (the old
/// behavior) let a high-confidence but irrelevant memory outrank an exact
/// match from another vault. Relevance dominates here; recency (exponential
/// decay, ~14-day scale, mirroring the LifeGraph ranking model) and
/// confidence act as bounded tiebreakers.
fn cross_scope_rank_score(
    server_score: f64,
    confidence: f32,
    updated_at_secs: u64,
    now_secs: u64,
) -> f64 {
    const RECENCY_DECAY_DAYS: f64 = 14.0;
    const RECENCY_WEIGHT: f64 = 0.25;
    const CONFIDENCE_WEIGHT: f64 = 0.15;
    let age_days = now_secs.saturating_sub(updated_at_secs) as f64 / 86_400.0;
    let recency = (-age_days / RECENCY_DECAY_DAYS).exp();
    server_score + RECENCY_WEIGHT * recency + CONFIDENCE_WEIGHT * f64::from(confidence)
}

fn link_kind_to_relation(kind: &LinkKind) -> &'static str {
    match kind {
        LinkKind::Related => "relates_to",
        LinkKind::Contradicts => "contradicts",
        LinkKind::Supersedes => "supersedes",
        LinkKind::Supports => "supports",
        LinkKind::DerivedFrom => "depends_on",
        LinkKind::Custom(_) => "user_defined",
    }
}

// ──── Recall Cache ─────────────────────────────────────────────────────────────

/// Env var overriding the recall cache TTL (seconds). `0` disables caching entirely.
const RECALL_CACHE_TTL_ENV: &str = "MUNINN_RECALL_CACHE_TTL_SECS";
const RECALL_CACHE_DEFAULT_TTL_SECS: u64 = 45;
const RECALL_CACHE_CAPACITY: usize = 32;

/// Normalizes a recall context string into a cache key component:
/// lowercased alphanumeric tokens joined by a single space. This makes
/// whitespace/punctuation variation between otherwise-identical turns
/// collapse onto the same cache entry.
fn normalize_recall_context(context: &str) -> String {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in context.chars() {
        if ch.is_alphanumeric() {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens.join(" ")
}

struct RecallCacheEntry {
    key: String,
    value: ActivationResult,
    inserted_at: Instant,
}

/// Short-TTL cache for `activate()` results, keyed on `(normalized context,
/// scope-derived vault list, effective max_results)`.
///
/// `activate()` runs in the philote turn path before context composition —
/// every turn pays the per-vault HTTP round-trip(s) even when consecutive
/// turns carry near-identical recall context. A short cache window (default
/// 45s, overridable via `MUNINN_RECALL_CACHE_TTL_SECS`, `0` disables caching)
/// amortizes that cost.
///
/// Staleness up to the TTL is acceptable for reads EXCEPT immediately after
/// this engine performs a write of its own — read-your-own-write matters for
/// UX (the agent should see a memory it just wrote reflected in the very
/// next recall). So every successful write path (`remember`,
/// `remember_batch`, `forget`, `link`, `evolve`) clears the cache outright
/// rather than waiting out the TTL.
///
/// Bounded to `RECALL_CACHE_CAPACITY` entries, LRU-evicted (oldest insert
/// first) via a `VecDeque` — no new dependency needed for 32 entries.
struct RecallCache {
    ttl: Duration,
    capacity: usize,
    entries: Mutex<VecDeque<RecallCacheEntry>>,
}

impl RecallCache {
    fn from_env() -> Self {
        let ttl_secs = std::env::var(RECALL_CACHE_TTL_ENV)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(RECALL_CACHE_DEFAULT_TTL_SECS);
        Self::new(Duration::from_secs(ttl_secs), RECALL_CACHE_CAPACITY)
    }

    fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            ttl,
            capacity,
            entries: Mutex::new(VecDeque::new()),
        }
    }

    fn make_key(context: &str, vaults: &[VaultId], max_results: Option<usize>) -> String {
        format!(
            "{}|{}|{:?}",
            normalize_recall_context(context),
            vaults.join(","),
            max_results
        )
    }

    /// Returns a clone of the cached value if present and still fresh.
    /// A stale entry is evicted on lookup. The clone happens under the lock;
    /// HTTP never happens under the lock.
    fn get(&self, key: &str) -> Option<ActivationResult> {
        if self.ttl.is_zero() {
            return None;
        }
        let mut entries = self.entries.lock().unwrap();
        let pos = entries.iter().position(|e| e.key == key)?;
        if entries[pos].inserted_at.elapsed() < self.ttl {
            Some(entries[pos].value.clone())
        } else {
            entries.remove(pos);
            None
        }
    }

    /// Insert or refresh an entry, evicting the oldest entry once over
    /// capacity.
    fn insert(&self, key: String, value: ActivationResult) {
        if self.ttl.is_zero() || self.capacity == 0 {
            return;
        }
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|e| e.key != key);
        if entries.len() >= self.capacity {
            entries.pop_front();
        }
        entries.push_back(RecallCacheEntry {
            key,
            value,
            inserted_at: Instant::now(),
        });
    }

    /// Invalidates all cached recall results. Called after every successful
    /// write so the next `activate()` reflects it immediately.
    fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

// ──── MuninnRestEngine ────────────────────────────────────────────────────────

/// MemoryEngine implementation backed by the MuninnDB REST API (port 8475).
///
/// One instance per agent session. Holds a vault resolver (agent + user context)
/// and a shared reqwest client (connection-pooled).
///
/// ## Vault routing
///
/// MuninnDB requires a `vault` param (query string or body) on all operations,
/// and tokens are scoped per-vault. Write operations (`remember`, `remember_batch`)
/// always know the vault from `MemoryScope` resolution. Read-side operations
/// (`read`, `forget`, `link`, `traverse`) use a write-populated `id_vault_cache`
/// for the fast path (O(1) lookup, direct auth). On cache miss — cold start or
/// an id from an external source — they fall back to trying each known
/// `(vault, token)` pair in turn and populate the cache on success.
///
/// Phase 5 will provide an MBP alternative for lower latency.
pub struct MuninnRestEngine {
    client: reqwest::Client,
    config: MuninnConfig,
    resolver: VaultResolver,
    /// Active attentional lens. Applied to activate() and remember() calls.
    lens: tokio::sync::RwLock<Option<AttentionalLens>>,
    /// id → vault_id populated by every write. Eliminates vault-discovery
    /// overhead on the read side for the common case.
    id_vault_cache: tokio::sync::RwLock<HashMap<EngramId, VaultId>>,
    /// Short-TTL cache of recent activate() results. See `RecallCache` docs.
    recall_cache: RecallCache,
}

impl MuninnRestEngine {
    pub fn new(config: MuninnConfig, resolver: VaultResolver) -> Self {
        Self {
            client: reqwest::Client::new(),
            config,
            resolver,
            lens: tokio::sync::RwLock::new(None),
            id_vault_cache: tokio::sync::RwLock::new(HashMap::new()),
            recall_cache: RecallCache::from_env(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.config.base_url, path)
    }

    /// Returns the reqwest RequestBuilder with the correct Authorization header
    /// for the given vault, if a token exists.
    fn with_auth(&self, builder: reqwest::RequestBuilder, vault: &str) -> reqwest::RequestBuilder {
        let token = self
            .config
            .vault_tokens
            .get(vault)
            .or(self.config.default_token.as_ref());
        match token {
            Some(t) => builder.bearer_auth(t),
            None => builder,
        }
    }

    fn has_auth_for_vault(&self, vault: &str) -> bool {
        self.config.vault_tokens.contains_key(vault) || self.config.default_token.is_some()
    }

    /// Convert an HTTP 401 into the typed [`TokenRejected`] error so callers
    /// can distinguish a stale token from network-unreachable and trigger
    /// token self-heal. Any other status passes through for the caller's
    /// normal `error_for_status` handling.
    fn auth_checked(resp: reqwest::Response, vault: &str) -> anyhow::Result<reqwest::Response> {
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(anyhow::Error::new(TokenRejected {
                vault: vault.to_string(),
            }));
        }
        Ok(resp)
    }

    /// Merge lens auto-tags into the caller-provided tags.
    async fn apply_lens_tags(&self, mut tags: Vec<String>) -> Vec<String> {
        if let Some(lens) = self.lens.read().await.as_ref() {
            for tag in &lens.auto_tags {
                if !tags.contains(tag) {
                    tags.push(tag.clone());
                }
            }
        }
        tags
    }

    /// Apply lens max_results override when caller did not specify.
    async fn effective_max_results(&self, caller: Option<usize>) -> Option<usize> {
        if caller.is_some() {
            return caller;
        }
        self.lens.read().await.as_ref().and_then(|l| l.max_results)
    }

    /// Look up the vault for an engram id from the write-side cache.
    async fn cached_vault(&self, id: &EngramId) -> Option<VaultId> {
        self.id_vault_cache.read().await.get(id).cloned()
    }

    /// Record id → vault in the cache. Called after every write or vault discovery.
    async fn cache_vault(&self, id: &EngramId, vault: &VaultId) {
        self.id_vault_cache
            .write()
            .await
            .insert(id.clone(), vault.clone());
    }

    /// Returns all known `(vault_name, token)` pairs, default-token vault first.
    /// Used only for cache-miss fallback on vault-agnostic operations.
    fn vault_token_pairs(&self) -> Vec<(&str, &str)> {
        let mut pairs: Vec<(&str, &str)> = self
            .config
            .vault_tokens
            .iter()
            .map(|(v, t)| (v.as_str(), t.as_str()))
            .collect();
        if let Some(def) = self.config.default_token.as_deref() {
            pairs.sort_by_key(|(_, t)| if *t == def { 0usize } else { 1usize });
        }
        pairs
    }

    /// Try each `(vault, token)` pair until a 2xx is received.
    /// Returns `Ok(Some((vault, response)))` on success, `Ok(None)` if all pairs
    /// returned 404 (engram genuinely absent everywhere), or `Err` on auth failure
    /// or unexpected status codes.
    ///
    /// The winning vault is returned so callers can populate `id_vault_cache`.
    async fn discover_vault(
        &self,
        build: impl Fn(&str, &str) -> reqwest::RequestBuilder,
    ) -> anyhow::Result<Option<(VaultId, reqwest::Response)>> {
        let pairs = self.vault_token_pairs();
        if pairs.is_empty() {
            let resp = build(&self.config.default_vault, "").send().await?;
            return Ok(Some((self.config.default_vault.clone(), resp)));
        }
        let mut all_404 = true;
        let mut first_rejected_vault: Option<String> = None;
        for (vault, token) in &pairs {
            let resp = build(vault, token).send().await?;
            if resp.status().is_success() {
                return Ok(Some((vault.to_string(), resp)));
            }
            match resp.status() {
                reqwest::StatusCode::UNAUTHORIZED => {
                    all_404 = false;
                    // wrong vault/token — try next
                    if first_rejected_vault.is_none() {
                        first_rejected_vault = Some(vault.to_string());
                    }
                }
                reqwest::StatusCode::NOT_FOUND => {
                    // engram not in this vault — try next
                }
                _ => {
                    all_404 = false;
                    resp.error_for_status()?;
                }
            }
        }
        if all_404 {
            Ok(None) // genuinely absent from all known vaults
        } else if let Some(vault) = first_rejected_vault {
            Err(anyhow::Error::new(TokenRejected { vault })
                .context("unauthorized on all vault token pairs"))
        } else {
            anyhow::bail!("unauthorized on all vault token pairs")
        }
    }

    /// Write an engram directly into a NAMED vault, bypassing the
    /// `MemoryScope` → `VaultResolver` mapping. This is the apply side of
    /// mesh-forwarded shared writes (`memory.write_forward`): the Cortex
    /// hotel receives the originating guest's already-resolved vault name in
    /// the forwarded payload and must write to exactly that vault, not one
    /// re-resolved against its own agent/user identity.
    ///
    /// Same idempotency contract as `remember_with_metadata`
    /// (`idempotent_id = "{vault}:{concept}"`), so a mesh envelope that is
    /// redelivered reinforces the existing engram instead of duplicating it.
    pub async fn remember_in_vault(
        &self,
        vault: &str,
        concept: &str,
        content: &str,
        tags: Vec<String>,
        metadata: serde_json::Value,
    ) -> anyhow::Result<EngramRef> {
        let tags = self.apply_lens_tags(tags).await;
        let metadata = match metadata {
            serde_json::Value::Null => None,
            other => Some(other),
        };
        let body = WriteRequest {
            vault: vault.to_string(),
            concept: concept.to_string(),
            content: content.to_string(),
            tags,
            confidence: None,
            metadata,
            idempotent_id: Some(format!("{}:{}", vault, concept)),
        };
        let resp = self
            .with_auth(self.client.post(self.url("/api/engrams")), vault)
            .json(&body)
            .send()
            .await?;
        let resp: WriteResponse = Self::auth_checked(resp, vault)?
            .error_for_status()?
            .json()
            .await?;
        self.cache_vault(&resp.id, &vault.to_string()).await;
        self.recall_cache.clear();
        Ok(EngramRef {
            id: resp.id,
            vault_id: vault.to_string(),
        })
    }
}

#[async_trait]
impl MemoryEngine for MuninnRestEngine {
    #[instrument(skip(self, content, tags), fields(scope = ?scope, concept))]
    async fn remember(
        &self,
        scope: MemoryScope,
        concept: &str,
        content: &str,
        tags: Vec<String>,
    ) -> anyhow::Result<EngramRef> {
        self.remember_with_metadata(scope, concept, content, tags, serde_json::Value::Null)
            .await
    }

    async fn remember_with_metadata(
        &self,
        scope: MemoryScope,
        concept: &str,
        content: &str,
        tags: Vec<String>,
        metadata: serde_json::Value,
    ) -> anyhow::Result<EngramRef> {
        let vault = self.resolver.resolve_primary(&scope);
        let tags = self.apply_lens_tags(tags).await;
        let metadata = match metadata {
            serde_json::Value::Null => None,
            other => Some(other),
        };

        let body = WriteRequest {
            vault: vault.clone(),
            concept: concept.to_string(),
            content: content.to_string(),
            tags,
            confidence: None,
            metadata,
            idempotent_id: Some(format!("{}:{}", vault, concept)),
        };

        let resp = self
            .with_auth(self.client.post(self.url("/api/engrams")), &vault)
            .json(&body)
            .send()
            .await?;
        let resp: WriteResponse = Self::auth_checked(resp, &vault)?
            .error_for_status()?
            .json()
            .await?;

        if resp.hint.as_deref() == Some("duplicate_content") {
            tracing::trace!(
                concept = %concept,
                vault = %vault,
                id = %resp.id,
                "memory write deduplicated — existing engram reinforced"
            );
        }

        self.cache_vault(&resp.id, &vault).await;
        // Read-your-own-write: a fresh recall must reflect what was just
        // written rather than serving a pre-write cache entry for its TTL.
        self.recall_cache.clear();
        Ok(EngramRef {
            id: resp.id,
            vault_id: vault,
        })
    }

    async fn remember_batch(
        &self,
        scope: MemoryScope,
        entries: Vec<(String, String, Vec<String>)>,
    ) -> anyhow::Result<Vec<EngramRef>> {
        let vault = self.resolver.resolve_primary(&scope);

        let mut items = Vec::with_capacity(entries.len());
        for (concept, content, tags) in entries {
            let tags = self.apply_lens_tags(tags).await;
            let idempotent_id = Some(format!("{}:{}", vault, concept));
            items.push(BatchItem {
                concept,
                content,
                tags,
                idempotent_id,
            });
        }

        let body = BatchWriteRequest {
            vault: vault.clone(),
            engrams: items,
        };
        let resp = self
            .with_auth(self.client.post(self.url("/api/engrams/batch")), &vault)
            .json(&body)
            .send()
            .await?;
        let resp: BatchWriteResponse = Self::auth_checked(resp, &vault)?
            .error_for_status()?
            .json()
            .await?;

        // Read-your-own-write: clear the recall cache now that the batch
        // write round-trip has completed, regardless of individual item
        // outcomes below (a successful round-trip means at least the server
        // processed the batch; per-item failures are surfaced via the
        // returned Result but don't change the invalidation need).
        self.recall_cache.clear();

        resp.results
            .into_iter()
            .map(|r| match r.id {
                Some(id) => {
                    // Cache inline; async block not needed since we take ownership of id.
                    // Cache will be populated by the caller via the returned refs if needed,
                    // but we eagerly populate here. Use a blocking insert via try_write —
                    // if contended, skip; the cache is best-effort for batch.
                    if let Ok(mut cache) = self.id_vault_cache.try_write() {
                        cache.insert(id.clone(), vault.clone());
                    }
                    Ok(EngramRef {
                        id,
                        vault_id: vault.clone(),
                    })
                }
                None => anyhow::bail!("batch write item failed: {}", r.error.unwrap_or_default()),
            })
            .collect()
    }

    async fn evolve(
        &self,
        id: &EngramId,
        content: &str,
        tags: Option<Vec<String>>,
    ) -> anyhow::Result<EngramRef> {
        // MuninnDB REST does not yet expose a direct PATCH endpoint;
        // evolve is available via MCP (muninn_evolve). For now, write the
        // updated content as a new engram and link it as supersedes.
        // Phase 5 (MBP) will replace this with a native evolve call.
        let existing = self
            .read(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("evolve: engram not found: {id}"))?;

        let effective_tags = tags.unwrap_or_else(|| existing.tags.clone());
        let scope = MemoryScope::SelfOnly; // evolve preserves vault via link
        let new_ref = self
            .remember(scope, &existing.concept, content, effective_tags)
            .await?;

        self.link(id, &new_ref.id, LinkKind::Supersedes).await?;

        Ok(new_ref)
    }

    async fn forget(&self, id: &EngramId) -> anyhow::Result<()> {
        let base_url = self.url(&format!("/api/engrams/{id}"));
        let client = &self.client;

        if let Some(vault) = self.cached_vault(id).await {
            // Fast path: vault known from write-side cache.
            let url = format!("{}?vault={}", base_url, vault);
            let resp = self.with_auth(client.delete(&url), &vault).send().await?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(()); // already gone — idempotent
            }
            Self::auth_checked(resp, &vault)?.error_for_status()?;
            self.recall_cache.clear();
            return Ok(());
        }

        // Slow path: vault unknown, try all pairs. None = not found anywhere (idempotent).
        if let Some((_, resp)) = self
            .discover_vault(|vault, token| {
                let url = format!("{}?vault={}", base_url, vault);
                client.delete(&url).bearer_auth(token)
            })
            .await?
        {
            resp.error_for_status()?;
            self.recall_cache.clear();
        }
        Ok(())
    }

    async fn retry_enrich(&self, id: &EngramId) -> anyhow::Result<()> {
        let vault = match self.cached_vault(id).await {
            Some(v) => v,
            None => {
                tracing::debug!(engram_id = %id, "retry_enrich: vault unknown, skipping");
                return Ok(());
            }
        };
        // Use the agent-managed enrichment REST endpoint (promoted from MCP-only).
        // GET candidates for this engram, then POST enrichment back if stages are missing.
        // Falls back gracefully — enrichment is always best-effort in the Attend phase.
        let candidates_url = self.url(&format!("/api/enrichment/candidates?vault={vault}&limit=1"));
        #[derive(serde::Deserialize)]
        struct Candidate {
            id: String,
            updated_at: String,
            missing_stages: Vec<String>,
        }
        #[derive(serde::Deserialize)]
        struct CandidatesResp {
            items: Vec<Candidate>,
        }
        let resp = self
            .with_auth(self.client.get(&candidates_url), &vault)
            .send()
            .await?;
        if !resp.status().is_success() {
            tracing::debug!(engram_id = %id, vault = %vault, "retry_enrich: candidates fetch failed, skipping");
            return Ok(());
        }
        let body: CandidatesResp = resp.json().await?;
        // Find this specific engram in candidates (it may not need enrichment).
        let candidate = body.items.into_iter().find(|c| &c.id == id);
        let Some(c) = candidate else {
            tracing::debug!(engram_id = %id, vault = %vault, "retry_enrich: engram fully enriched or not found");
            return Ok(());
        };
        if c.missing_stages.is_empty() {
            return Ok(());
        }
        // POST enrichment with stages_completed signal — the server side will mark
        // the missing stages as complete. Actual content enrichment (summaries, entities)
        // will be wired when model-router is the enrichment source.
        let enrich_url = self.url(&format!("/api/engrams/{id}/enrich?vault={vault}"));
        let enrich_body = serde_json::json!({
            "expected_updated_at": c.updated_at,
            "stages_completed": c.missing_stages,
            "source": "philote-attend"
        });
        let enrich_resp = self
            .with_auth(self.client.post(&enrich_url), &vault)
            .json(&enrich_body)
            .send()
            .await?;
        match enrich_resp.status() {
            s if s.is_success() => {
                tracing::debug!(engram_id = %id, vault = %vault, stages = ?c.missing_stages, "enrichment applied");
            }
            reqwest::StatusCode::CONFLICT => {
                // OCC conflict — engram was updated between get and apply. Non-fatal.
                tracing::debug!(engram_id = %id, "retry_enrich: OCC conflict, skipping");
            }
            other => {
                tracing::debug!(engram_id = %id, status = %other, "retry_enrich: apply failed (non-fatal)");
            }
        }
        Ok(())
    }

    #[instrument(skip(self), fields(scope = ?scope))]
    async fn activate(
        &self,
        context: &str,
        scope: MemoryScope,
        max_results: Option<usize>,
    ) -> anyhow::Result<ActivationResult> {
        let vaults = self.resolver.resolve(&scope);
        let max = self.effective_max_results(max_results).await;
        let is_cross_scope = matches!(scope, MemoryScope::CrossScope(_));

        let cache_key = RecallCache::make_key(context, &vaults, max);
        if let Some(cached) = self.recall_cache.get(&cache_key) {
            debug!("recall cache hit");
            return Ok(cached);
        }

        let mut all_engrams = Vec::new();
        let mut total = 0usize;
        let mut had_vault_error = false;
        // First TokenRejected seen across the fan-out. Cross-scope recall
        // degrades to partial results on per-vault errors, but when EVERY
        // vault fails and at least one was an active 401, the degraded-empty
        // result must surface as TokenRejected so the self-heal path fires —
        // this was exactly the 2026-07-20/21 stale-token failure mode.
        let mut first_token_rejected: Option<anyhow::Error> = None;

        // Cross-scope recall runs in the turn path before context composition:
        // the per-vault activations are independent, so fire them concurrently
        // instead of paying up to three serial round-trips per turn.
        let fetches = vaults
            .iter()
            .filter(|vault| {
                if is_cross_scope && !self.has_auth_for_vault(vault) {
                    debug!(
                        vault = %vault,
                        "Skipping cross-scope activation for vault without token"
                    );
                    return false;
                }
                true
            })
            .map(|vault| {
                let body = ActivateRequest {
                    vault: Some(vault.clone()),
                    context: vec![context.to_string()],
                    max_results: max,
                };
                async move {
                    let resp: anyhow::Result<ActivateResponse> = async {
                        let resp = self
                            .with_auth(self.client.post(self.url("/api/activate")), vault)
                            .json(&body)
                            .send()
                            .await?;
                        Ok(Self::auth_checked(resp, vault)?
                            .error_for_status()?
                            .json()
                            .await?)
                    }
                    .await;
                    (vault, resp)
                }
            });

        for (vault, resp) in futures::future::join_all(fetches).await {
            let resp = match resp {
                Ok(resp) => resp,
                // Cross-scope recall is advisory: one failing vault degrades
                // to partial results instead of killing recall for the rest.
                Err(err) if is_cross_scope => {
                    tracing::warn!(
                        vault = %vault,
                        error = %err,
                        "Cross-scope activation failed for vault; continuing with others"
                    );
                    had_vault_error = true;
                    if first_token_rejected.is_none() && token_rejected_vault(&err).is_some() {
                        first_token_rejected = Some(err);
                    }
                    continue;
                }
                Err(err) => return Err(err),
            };

            total += resp.total_found;
            // Populate cache from activation results so subsequent ops are fast.
            for item in &resp.activations {
                self.cache_vault(&item.id, vault).await;
            }
            all_engrams.extend(resp.activations.into_iter().map(|item| {
                let server_score = item.score;
                let mut engram: Engram = item.into();
                engram.vault_id = vault.clone();
                (server_score, engram)
            }));
        }

        // Cross-scope: merge on combined relevance/recency/confidence and
        // truncate. Within a single vault the server's ordering stands.
        if is_cross_scope {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            all_engrams.sort_by(|(score_a, a), (score_b, b)| {
                cross_scope_rank_score(*score_b, b.confidence, b.updated_at, now_secs).total_cmp(
                    &cross_scope_rank_score(*score_a, a.confidence, a.updated_at, now_secs),
                )
            });
            if let Some(m) = max {
                all_engrams.truncate(m);
            }
        }

        let result = ActivationResult {
            engrams: all_engrams.into_iter().map(|(_, engram)| engram).collect(),
            total,
        };

        // Cache the result unless it's a cross-scope call that degraded to
        // nothing because every vault errored — that's not a genuine "no
        // memories" answer, just the absence of a good one. A partial
        // cross-scope result with at least some data, or a clean empty
        // result from vaults that all responded successfully, is the best
        // known answer and is worth caching.
        let is_empty_degraded_result =
            is_cross_scope && had_vault_error && result.engrams.is_empty() && result.total == 0;
        if is_empty_degraded_result && let Some(err) = first_token_rejected {
            return Err(err);
        }
        if !is_empty_degraded_result {
            self.recall_cache.insert(cache_key, result.clone());
        }

        Ok(result)
    }

    async fn read(&self, id: &EngramId) -> anyhow::Result<Option<Engram>> {
        let base_url = self.url(&format!("/api/engrams/{id}"));
        let client = &self.client;

        if let Some(vault) = self.cached_vault(id).await {
            // Fast path: vault known.
            let url = format!("{}?vault={}", base_url, vault);
            let resp = self.with_auth(client.get(&url), &vault).send().await?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            let mut engram: Engram = Self::auth_checked(resp, &vault)?
                .error_for_status()?
                .json::<ReadResponse>()
                .await
                .map(Into::into)?;
            engram.vault_id = vault;
            return Ok(Some(engram));
        }

        // Slow path: discover vault, cache the result.
        match self
            .discover_vault(|vault, token| {
                let url = format!("{}?vault={}", base_url, vault);
                client.get(&url).bearer_auth(token)
            })
            .await?
        {
            None => Ok(None),
            Some((vault, resp)) => {
                let mut engram: Engram = resp
                    .error_for_status()?
                    .json::<ReadResponse>()
                    .await
                    .map(Into::into)?;
                engram.vault_id = vault.clone();
                self.cache_vault(id, &vault).await;
                Ok(Some(engram))
            }
        }
    }

    async fn link(
        &self,
        from_id: &EngramId,
        to_id: &EngramId,
        kind: LinkKind,
    ) -> anyhow::Result<()> {
        let relation = link_kind_to_relation(&kind).to_string();
        let url = self.url("/api/link");
        let client = &self.client;
        let from_id = from_id.clone();
        let to_id = to_id.clone();

        // Use the source engram's vault. Cross-vault links use source vault for auth.
        let vault = if let Some(v) = self.cached_vault(&from_id).await {
            v
        } else {
            // Vault unknown — discover via a dummy GET on the source engram.
            let base_url = self.url(&format!("/api/engrams/{from_id}"));
            match self
                .discover_vault(|vault, token| {
                    let u = format!("{}?vault={}", base_url, vault);
                    client.get(&u).bearer_auth(token)
                })
                .await?
            {
                Some((v, _)) => {
                    self.cache_vault(&from_id, &v).await;
                    v
                }
                None => anyhow::bail!("link: source engram not found: {from_id}"),
            }
        };

        let body = LinkRequest {
            vault: vault.clone(),
            source_id: from_id.clone(),
            target_id: to_id.clone(),
            relation,
            weight: None,
        };
        let resp = self
            .with_auth(client.post(&url), &vault)
            .json(&body)
            .send()
            .await?;
        Self::auth_checked(resp, &vault)?.error_for_status()?;
        self.recall_cache.clear();
        Ok(())
    }

    async fn traverse(
        &self,
        from_id: &EngramId,
        _max_depth: Option<usize>,
    ) -> anyhow::Result<Vec<Engram>> {
        #[derive(Deserialize)]
        struct LinkItem {
            target_id: String,
        }
        #[derive(Deserialize)]
        struct LinksResponse {
            links: Vec<LinkItem>,
        }

        let base_url = self.url(&format!("/api/engrams/{from_id}/links"));
        let client = &self.client;

        let (vault, raw) = if let Some(vault) = self.cached_vault(from_id).await {
            // Fast path.
            let url = format!("{}?vault={}", base_url, vault);
            let resp = self.with_auth(client.get(&url), &vault).send().await?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(vec![]);
            }
            (vault, resp)
        } else {
            // Slow path.
            match self
                .discover_vault(|vault, token| {
                    let url = format!("{}?vault={}", base_url, vault);
                    client.get(&url).bearer_auth(token)
                })
                .await?
            {
                None => return Ok(vec![]),
                Some((vault, resp)) => {
                    self.cache_vault(from_id, &vault).await;
                    (vault, resp)
                }
            }
        };
        let resp: LinksResponse = Self::auth_checked(raw, &vault)?
            .error_for_status()?
            .json()
            .await?;

        let mut engrams = Vec::new();
        for link in resp.links {
            if let Some(engram) = self.read(&link.target_id).await? {
                engrams.push(engram);
            }
        }
        Ok(engrams)
    }

    async fn set_lens(&self, lens: AttentionalLens) -> anyhow::Result<()> {
        *self.lens.write().await = Some(lens);
        Ok(())
    }

    async fn current_lens(&self) -> anyhow::Result<Option<AttentionalLens>> {
        Ok(self.lens.read().await.clone())
    }

    async fn subscribe(
        &self,
        _context: &str,
        _scope: MemoryScope,
    ) -> anyhow::Result<mpsc::Receiver<Engram>> {
        anyhow::bail!("MuninnRestEngine: subscribe not available until Phase 5 MBP transport")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ──── TokenRejected (401 → typed self-heal signal) ──────────────────

    /// Minimal canned-response HTTP server on a std thread (memory-core's
    /// tokio has no `net` feature). Serves every connection the same status
    /// + body and closes.
    fn spawn_canned_server(status: u16, body: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                // Drain the request: headers, then content-length body bytes.
                use std::io::{Read, Write};
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                let (mut header_end, mut content_len) = (None, 0usize);
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if header_end.is_none()
                                && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
                            {
                                header_end = Some(pos + 4);
                                let headers = String::from_utf8_lossy(&buf[..pos]);
                                content_len = headers
                                    .lines()
                                    .find_map(|l| {
                                        let (k, v) = l.split_once(':')?;
                                        k.eq_ignore_ascii_case("content-length")
                                            .then(|| v.trim().parse().ok())?
                                    })
                                    .unwrap_or(0);
                            }
                            if let Some(end) = header_end
                                && buf.len() >= end + content_len
                            {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let reason = if status == 401 {
                    "Unauthorized"
                } else {
                    "Error"
                };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    fn engine_against(base_url: String) -> MuninnRestEngine {
        let mut config = MuninnConfig::local("default")
            .with_vault_token("self_agent-x", "stale-token")
            .with_vault_token("user_jared", "stale-token-2");
        config.base_url = base_url;
        MuninnRestEngine::new(
            config,
            VaultResolver {
                agent_id: "agent-x".into(),
                user_id: "jared".into(),
            },
        )
    }

    #[tokio::test]
    async fn remember_on_401_surfaces_token_rejected_with_vault() {
        let engine = engine_against(spawn_canned_server(401, "{\"error\":\"unauthorized\"}"));
        let err = engine
            .remember(MemoryScope::SelfOnly, "c", "content", vec![])
            .await
            .expect_err("401 must error");
        assert_eq!(token_rejected_vault(&err), Some("self_agent-x"));
    }

    #[tokio::test]
    async fn remember_on_500_is_not_token_rejected() {
        let engine = engine_against(spawn_canned_server(500, "boom"));
        let err = engine
            .remember(MemoryScope::SelfOnly, "c", "content", vec![])
            .await
            .expect_err("500 must error");
        assert_eq!(token_rejected_vault(&err), None);
    }

    #[tokio::test]
    async fn cross_scope_activate_with_all_vaults_401_surfaces_token_rejected() {
        // The 2026-07-20/21 failure mode: every stored token stale. The
        // degraded-empty cross-scope result must be a TokenRejected error,
        // not a silent empty recall.
        let engine = engine_against(spawn_canned_server(401, "{}"));
        let scope = MemoryScope::CrossScope(vec![MemoryScope::SelfOnly, MemoryScope::SharedUser]);
        let err = engine
            .activate("ctx", scope, Some(3))
            .await
            .expect_err("all-401 cross-scope must error");
        assert!(token_rejected_vault(&err).is_some());
    }

    #[tokio::test]
    async fn vault_discovery_all_401_surfaces_token_rejected() {
        // read() on an uncached id walks all (vault, token) pairs; when every
        // pair 401s, the old opaque "unauthorized on all vault token pairs"
        // string must now carry the typed marker.
        let engine = engine_against(spawn_canned_server(401, "{}"));
        let err = engine
            .read(&"01UNKNOWN".to_string())
            .await
            .expect_err("all-401 discovery must error");
        assert!(token_rejected_vault(&err).is_some());
    }

    #[test]
    fn cross_scope_resolves_unprovisioned_session_vault_without_auth() {
        let config = MuninnConfig::local("default")
            .with_vault_token("self_agent-aria", "self-token")
            .with_vault_token("user_likesjx", "user-token");
        let engine = MuninnRestEngine::new(
            config,
            VaultResolver {
                agent_id: "agent-aria".into(),
                user_id: "likesjx".into(),
            },
        );

        assert!(engine.has_auth_for_vault("self_agent-aria"));
        assert!(engine.has_auth_for_vault("user_likesjx"));
        assert!(!engine.has_auth_for_vault("session_telegram:7898847424:agent-aria"));
    }

    const DAY: u64 = 86_400;

    #[test]
    fn cross_scope_rank_relevance_beats_stale_confidence() {
        let now = 1_800_000_000;
        // Old behavior: a fully-confident but irrelevant memory (score 0.05)
        // outranked a strong match (score 0.7, confidence 0.6). Relevance
        // must dominate the merge.
        let irrelevant_confident = cross_scope_rank_score(0.05, 1.0, now - 30 * DAY, now);
        let relevant_match = cross_scope_rank_score(0.7, 0.6, now - 30 * DAY, now);
        assert!(relevant_match > irrelevant_confident);
    }

    #[test]
    fn cross_scope_rank_recency_tiebreaks_equal_relevance() {
        let now = 1_800_000_000;
        let fresh = cross_scope_rank_score(0.5, 0.8, now - DAY, now);
        let stale = cross_scope_rank_score(0.5, 0.8, now - 60 * DAY, now);
        assert!(fresh > stale);
    }

    #[test]
    fn cross_scope_rank_degrades_without_server_score() {
        // Servers that predate the score field default every item to 0.0 —
        // ranking must still order by recency + confidence, not collapse.
        let now = 1_800_000_000;
        let fresh_confident = cross_scope_rank_score(0.0, 0.9, now - DAY, now);
        let stale_unconfident = cross_scope_rank_score(0.0, 0.3, now - 90 * DAY, now);
        assert!(fresh_confident > stale_unconfident);
        // Clock skew (updated_at in the future) must not panic or NaN.
        let skewed = cross_scope_rank_score(0.2, 0.5, now + DAY, now);
        assert!(skewed.is_finite());
    }

    // ──── RecallCache ───────────────────────────────────────────────────

    fn dummy_activation_result(engram_id: &str) -> ActivationResult {
        ActivationResult {
            engrams: vec![Engram {
                id: engram_id.to_string(),
                vault_id: "self_test".to_string(),
                concept: "concept".to_string(),
                content: "content".to_string(),
                tags: vec![],
                confidence: 0.5,
                created_at: 0,
                updated_at: 0,
                metadata: serde_json::Value::Null,
            }],
            total: 1,
        }
    }

    #[test]
    fn normalize_recall_context_collapses_whitespace_case_and_punctuation() {
        let a = normalize_recall_context("  Hello,   World!! ");
        let b = normalize_recall_context("hello world");
        assert_eq!(a, b);
        assert_eq!(a, "hello world");
    }

    #[test]
    fn recall_cache_hit_returns_clone_within_ttl() {
        let cache = RecallCache::new(Duration::from_millis(200), 8);
        let key = RecallCache::make_key("ctx", &["self_a".to_string()], Some(5));
        cache.insert(key.clone(), dummy_activation_result("e1"));
        let hit = cache.get(&key);
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().engrams[0].id, "e1");
    }

    #[test]
    fn recall_cache_entry_expires_after_ttl() {
        let cache = RecallCache::new(Duration::from_millis(20), 8);
        let key = RecallCache::make_key("ctx", &["self_a".to_string()], None);
        cache.insert(key.clone(), dummy_activation_result("e1"));
        assert!(cache.get(&key).is_some());
        std::thread::sleep(Duration::from_millis(60));
        assert!(cache.get(&key).is_none());
        // Stale entry is evicted on lookup, not left dangling.
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn recall_cache_evicts_oldest_over_capacity() {
        let cache = RecallCache::new(Duration::from_secs(60), 2);
        cache.insert("k1".to_string(), dummy_activation_result("e1"));
        cache.insert("k2".to_string(), dummy_activation_result("e2"));
        cache.insert("k3".to_string(), dummy_activation_result("e3"));
        assert_eq!(cache.len(), 2);
        assert!(cache.get("k1").is_none()); // oldest evicted
        assert!(cache.get("k2").is_some());
        assert!(cache.get("k3").is_some());
    }

    #[test]
    fn recall_cache_clear_invalidates_all_entries() {
        let cache = RecallCache::new(Duration::from_secs(60), 8);
        cache.insert("k1".to_string(), dummy_activation_result("e1"));
        cache.insert("k2".to_string(), dummy_activation_result("e2"));
        assert_eq!(cache.len(), 2);
        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.get("k1").is_none());
    }

    #[test]
    fn recall_cache_ttl_zero_env_disables_caching() {
        // SAFETY: test-local env mutation; this var is not read by any other
        // test's assertions (only by RecallCache::from_env() at construction).
        unsafe {
            std::env::set_var(RECALL_CACHE_TTL_ENV, "0");
        }
        let cache = RecallCache::from_env();
        unsafe {
            std::env::remove_var(RECALL_CACHE_TTL_ENV);
        }
        assert!(cache.ttl.is_zero());
        cache.insert("k1".to_string(), dummy_activation_result("e1"));
        // insert() is a no-op when ttl is zero — nothing to hit, ever.
        assert!(cache.get("k1").is_none());
        assert_eq!(cache.len(), 0);
    }
}

#[cfg(test)]
mod shared_write_route_tests {
    use super::*;

    #[test]
    fn vault_components_are_sanitized_to_muninn_alphabet() {
        let r = VaultResolver {
            agent_id: "agent-aria".into(),
            user_id: "cron:ephemeral:agent-aria".into(),
        };
        assert_eq!(
            r.resolve_primary(&MemoryScope::SharedUser),
            "user_cron-ephemeral-agent-aria",
            "colons must be mapped, not passed through to MuninnDB"
        );
        assert_eq!(
            r.resolve_primary(&MemoryScope::Session("telegram:123:agent-jane".into())),
            "session_telegram-123-agent-jane"
        );
        // Clean ids pass through unchanged.
        let r = VaultResolver {
            agent_id: "agent-aria".into(),
            user_id: "likesjx".into(),
        };
        assert_eq!(r.resolve_primary(&MemoryScope::SharedUser), "user_likesjx");
        assert_eq!(r.resolve_primary(&MemoryScope::SelfOnly), "self_agent-aria");
    }

    #[test]
    fn fleet_shared_vault_predicate() {
        assert!(is_fleet_shared_vault("default"));
        assert!(is_fleet_shared_vault("user_likesjx"));
        assert!(!is_fleet_shared_vault("self_agent-aria"));
        assert!(!is_fleet_shared_vault("session_01abc"));
        assert!(!is_fleet_shared_vault("user")); // no underscore suffix — not a user vault
    }

    /// Wire compat: configs serialized before `shared_write_route` existed
    /// (e.g. an older hotel's MemoryConfigPayload.config_json) must still
    /// deserialize, defaulting to no routing.
    #[test]
    fn config_without_route_field_deserializes_to_none() {
        let legacy = r#"{"base_url":"http://127.0.0.1:8475","vault_tokens":{},"default_token":null,"default_vault":"default"}"#;
        let cfg: MuninnConfig = serde_json::from_str(legacy).expect("legacy config parses");
        assert_eq!(cfg.shared_write_route, None);
    }

    #[test]
    fn config_route_round_trips() {
        let mut cfg = MuninnConfig::local("default");
        cfg.shared_write_route = Some("vps-jane-aiua-01".into());
        let json = serde_json::to_string(&cfg).unwrap();
        let back: MuninnConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.shared_write_route.as_deref(), Some("vps-jane-aiua-01"));
    }
}
