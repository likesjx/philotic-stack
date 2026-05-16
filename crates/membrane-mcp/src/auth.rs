//! Per-route bearer token authentication.
//!
//! # Security model
//!
//! - Tokens are never stored in plaintext anywhere in the system.
//! - The philote provisioned a token, stored `BLAKE3(raw_token)` in the vault
//!   under a `vault_ref`, and put the `vault_ref` in the `McpTokenGrant`.
//! - On each request the membrane: fetches the hash from vault (with short TTL
//!   cache to avoid hammering IPC on every call), hashes the presented token
//!   with BLAKE3, and compares in constant time via `blake3::Hash::eq`.
//! - Allotment is tracked in-memory per `(tool_name, token_id)` using a
//!   sliding window. It resets on membrane restart (intentional: simple and
//!   safe — a restart resets the budget, not a security bypass).

use ansible_mesh_core::mcp_route::{McpAuthScheme, McpCallAllotment, McpTokenGrant};
use anyhow::{Result, bail};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::warn;

// ── Caller identity (post-auth) ───────────────────────────────────────────────

/// The verified identity of an authenticated caller for one tool call.
#[derive(Debug, Clone)]
pub struct CallerIdentity {
    /// The `token_id` label from the `McpTokenGrant`.
    pub token_id: String,
    /// Scopes granted by the matching token grant.
    pub scopes: Vec<String>,
}

// ── Vault hash cache ──────────────────────────────────────────────────────────

const VAULT_CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Debug)]
struct CachedHash {
    /// Raw bytes of the BLAKE3 hash stored in vault (hex-decoded).
    hash_bytes: [u8; 32],
    cached_at: Instant,
}

impl CachedHash {
    fn is_fresh(&self) -> bool {
        self.cached_at.elapsed() < VAULT_CACHE_TTL
    }
}

/// Thread-safe cache of vault_ref → BLAKE3 hash bytes.
///
/// The membrane fetches each hash once per TTL window; after that it serves
/// from this cache to avoid per-request IPC round-trips.
#[derive(Debug, Default)]
pub struct VaultHashCache {
    inner: Mutex<HashMap<String, CachedHash>>,
}

impl VaultHashCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return cached hash bytes if still fresh.
    pub fn get(&self, vault_ref: &str) -> Option<[u8; 32]> {
        let guard = self.inner.lock().unwrap();
        guard
            .get(vault_ref)
            .filter(|c| c.is_fresh())
            .map(|c| c.hash_bytes)
    }

    /// Store fetched hash bytes for the given vault_ref.
    pub fn insert(&self, vault_ref: &str, hash_bytes: [u8; 32]) {
        let mut guard = self.inner.lock().unwrap();
        guard.insert(
            vault_ref.to_string(),
            CachedHash {
                hash_bytes,
                cached_at: Instant::now(),
            },
        );
    }

    /// Invalidate a cached entry (e.g. after a vault update).
    pub fn invalidate(&self, vault_ref: &str) {
        let mut guard = self.inner.lock().unwrap();
        guard.remove(vault_ref);
    }
}

// ── Allotment tracker ─────────────────────────────────────────────────────────

#[derive(Debug)]
struct AllotmentWindow {
    count: u32,
    window_start: Instant,
}

/// In-memory sliding-window call budget tracker.
///
/// Keyed by `(tool_name, token_id)`. Checked and incremented atomically
/// under the internal mutex.
#[derive(Debug, Default)]
pub struct AllotmentTracker {
    windows: Mutex<HashMap<(String, String), AllotmentWindow>>,
}

impl AllotmentTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check the allotment and increment the counter if within budget.
    ///
    /// Returns `Ok(())` if the call is allowed, `Err` with a human-readable
    /// message if the budget is exhausted.
    pub fn check_and_increment(
        &self,
        tool_name: &str,
        token_id: &str,
        allotment: &McpCallAllotment,
    ) -> Result<()> {
        let key = (tool_name.to_string(), token_id.to_string());
        let window_duration = Duration::from_secs(allotment.window_secs);

        let mut guard = self.windows.lock().unwrap();
        let entry = guard.entry(key).or_insert_with(|| AllotmentWindow {
            count: 0,
            window_start: Instant::now(),
        });

        if entry.window_start.elapsed() >= window_duration {
            entry.count = 0;
            entry.window_start = Instant::now();
        }

        if entry.count >= allotment.max_per_window {
            bail!(
                "call allotment exhausted for token '{}' on '{}': {}/{} per {}s window",
                token_id,
                tool_name,
                entry.count,
                allotment.max_per_window,
                allotment.window_secs,
            );
        }

        entry.count += 1;
        Ok(())
    }
}

// ── Auth checker ──────────────────────────────────────────────────────────────

/// Resolves a vault_ref to BLAKE3 hash bytes.
///
/// In Slice 1 this is a sync interface backed by a cache. Slice 2 wires this
/// to `IpcRequest::GetSecret` so the vault lookup goes through the hotel.
pub trait VaultResolver: Send + Sync {
    fn resolve(&self, vault_ref: &str) -> Result<[u8; 32]>;
}

/// Check a presented bearer token against a route's auth scheme.
///
/// Returns the `CallerIdentity` on success, or an error describing the failure.
/// The error message is safe to include in a JSON-RPC error response (no secrets).
pub fn check_bearer_token(
    tool_name: &str,
    presented_token: &str,
    grants: &[McpTokenGrant],
    vault_cache: &VaultHashCache,
    vault: &dyn VaultResolver,
    allotment: &AllotmentTracker,
) -> Result<CallerIdentity> {
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // BLAKE3 hash of the presented token.
    let presented_hash = blake3::hash(presented_token.as_bytes());

    for grant in grants {
        // Check expiry before touching vault.
        if let Some(exp) = grant.expires_at {
            if now_epoch > exp {
                warn!(token_id = %grant.token_id, "token expired, skipping");
                continue;
            }
        }

        // Fetch hash from cache or vault.
        let stored_bytes = if let Some(bytes) = vault_cache.get(&grant.vault_ref) {
            bytes
        } else {
            match vault.resolve(&grant.vault_ref) {
                Ok(bytes) => {
                    vault_cache.insert(&grant.vault_ref, bytes);
                    bytes
                }
                Err(e) => {
                    warn!(vault_ref = %grant.vault_ref, err = %e, "vault lookup failed");
                    continue;
                }
            }
        };

        let stored_hash = blake3::Hash::from_bytes(stored_bytes);

        // Constant-time comparison via blake3::Hash::eq.
        if presented_hash != stored_hash {
            continue;
        }

        // Token matched — check allotment.
        if let Some(a) = &grant.allotment {
            allotment.check_and_increment(tool_name, &grant.token_id, a)?;
        }

        return Ok(CallerIdentity {
            token_id: grant.token_id.clone(),
            scopes: grant.scopes.clone(),
        });
    }

    bail!(
        "no matching token grant for presented credential on tool '{}'",
        tool_name
    )
}

/// Extract the raw bearer token from an `Authorization: Bearer <token>` header.
pub fn extract_bearer(authorization_header: Option<&str>) -> Option<&str> {
    let header = authorization_header?;
    header.strip_prefix("Bearer ").map(str::trim)
}

/// Check auth for a route whose scheme is `McpAuthScheme::None`.
///
/// Only permitted when the request comes from a loopback address.
pub fn check_none_auth(is_loopback: bool) -> Result<CallerIdentity> {
    if !is_loopback {
        bail!("route has no auth scheme but request is not from loopback");
    }
    Ok(CallerIdentity {
        token_id: "loopback".into(),
        scopes: vec!["*".into()],
    })
}

/// Dispatch-time auth check — called per `tools/call` before dispatch.
pub fn authorize_call(
    tool_name: &str,
    auth_scheme: &McpAuthScheme,
    authorization_header: Option<&str>,
    is_loopback: bool,
    vault_cache: &VaultHashCache,
    vault: &dyn VaultResolver,
    allotment: &AllotmentTracker,
) -> Result<CallerIdentity> {
    match auth_scheme {
        McpAuthScheme::BearerToken { grants } => {
            let token = extract_bearer(authorization_header)
                .ok_or_else(|| anyhow::anyhow!("missing Authorization: Bearer header"))?;
            check_bearer_token(tool_name, token, grants, vault_cache, vault, allotment)
        }
        McpAuthScheme::HmacSha256 { .. } => {
            // Slice 3: implement HMAC body signing.
            bail!("HMAC-SHA256 auth not yet implemented")
        }
        McpAuthScheme::None => check_none_auth(is_loopback),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticVault(HashMap<String, [u8; 32]>);

    impl VaultResolver for StaticVault {
        fn resolve(&self, vault_ref: &str) -> Result<[u8; 32]> {
            self.0
                .get(vault_ref)
                .copied()
                .ok_or_else(|| anyhow::anyhow!("not found: {}", vault_ref))
        }
    }

    fn make_grant(token: &str) -> (McpTokenGrant, [u8; 32]) {
        let hash = blake3::hash(token.as_bytes());
        let grant = McpTokenGrant {
            token_id: "test-caller".into(),
            vault_ref: "vault/test".into(),
            scopes: vec!["read".into()],
            expires_at: None,
            allotment: None,
        };
        (grant, *hash.as_bytes())
    }

    #[test]
    fn correct_token_accepted() {
        let raw = "supersecret";
        let (grant, hash_bytes) = make_grant(raw);
        let mut vault_map = HashMap::new();
        vault_map.insert("vault/test".to_string(), hash_bytes);
        let vault = StaticVault(vault_map);
        let cache = VaultHashCache::new();
        let allotment = AllotmentTracker::new();

        let result = check_bearer_token("my_tool", raw, &[grant], &cache, &vault, &allotment);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().token_id, "test-caller");
    }

    #[test]
    fn wrong_token_rejected() {
        let raw = "supersecret";
        let (grant, hash_bytes) = make_grant(raw);
        let mut vault_map = HashMap::new();
        vault_map.insert("vault/test".to_string(), hash_bytes);
        let vault = StaticVault(vault_map);
        let cache = VaultHashCache::new();
        let allotment = AllotmentTracker::new();

        let result = check_bearer_token(
            "my_tool",
            "wrongtoken",
            &[grant],
            &cache,
            &vault,
            &allotment,
        );
        assert!(result.is_err());
    }

    #[test]
    fn allotment_exhausted() {
        let raw = "tok";
        let quota = McpCallAllotment {
            max_per_window: 2,
            window_secs: 60,
        };
        let (mut grant, hash_bytes) = make_grant(raw);
        grant.allotment = Some(quota);
        let mut vault_map = HashMap::new();
        vault_map.insert("vault/test".to_string(), hash_bytes);
        let vault = StaticVault(vault_map);
        let cache = VaultHashCache::new();
        let allotment = AllotmentTracker::new();

        let grants = vec![grant];
        let ok1 = check_bearer_token("t", raw, &grants, &cache, &vault, &allotment);
        let ok2 = check_bearer_token("t", raw, &grants, &cache, &vault, &allotment);
        let denied = check_bearer_token("t", raw, &grants, &cache, &vault, &allotment);
        assert!(ok1.is_ok());
        assert!(ok2.is_ok());
        assert!(denied.is_err());
    }

    #[test]
    fn extract_bearer_parses_header() {
        assert_eq!(
            extract_bearer(Some("Bearer mytoken123")),
            Some("mytoken123")
        );
        assert_eq!(extract_bearer(Some("Basic xyz")), None);
        assert_eq!(extract_bearer(None), None);
    }
}
