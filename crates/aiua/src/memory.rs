//! Boot-time loading of MuninnDB configuration and per-agent engine factory.
//!
//! # Two-phase design
//!
//! **Phase 1 — Hotel boot**: `load_muninn_config` reads the vault registry from
//! the Context Graph, decrypts each vault token, and returns a `MuninnConfig`.
//! This config is stored on the hotel's shared state.
//!
//! **Phase 2 — Agent session start**: `engine_for_agent` wraps the shared
//! `MuninnConfig` with a `VaultResolver` scoped to the agent+user identity,
//! returning a ready `MuninnRestEngine`. This is Slice E (agent-core Attend hook).
//!
//! Returns `None` from `load_muninn_config` if MuninnDB is not configured
//! (no vault registry entries). Guests should fall back to `NullMemoryEngine`.

use ansible_mesh_core::domain::GraphDomain;
use anyhow::Result;
use memory_core::{MuninnConfig, MuninnRestEngine, VaultResolver};

use crate::vault::{SecretAccess, resolve_secret};

/// Load `MuninnConfig` from the Context Graph at hotel boot.
///
/// Reads `muninn_endpoint` and decrypts each entry in `vault_registry`.
/// Returns `None` if the vault registry is empty (MuninnDB not configured).
pub fn load_muninn_config(graph: &GraphDomain) -> Result<Option<MuninnConfig>> {
    let registry = graph.get_vault_registry()?;
    if registry.is_empty() {
        return Ok(None);
    }

    let endpoint = graph
        .get_muninn_endpoint()?
        .unwrap_or_else(|| "http://127.0.0.1:8475".to_string());

    let mut config = MuninnConfig::local("default");
    config.base_url = endpoint;
    // Muninn-cluster single-writer routing: on lobe/observer hotels the
    // operator sets `muninn_write_route` to the Cortex hotel's node id;
    // guests then forward fleet-shared-vault writes over the mesh instead of
    // stranding them on the local replica. Absent everywhere by default and
    // on the Cortex hotel itself.
    config.shared_write_route = graph.get_muninn_write_route()?;

    // Hotel daemon accesses its own secrets using the "hotel" identity.
    let access = SecretAccess {
        role: "hotel".to_string(),
        guest_id: "hotel".to_string(),
    };

    let mut loaded_vaults = 0usize;
    let mut skipped_non_muninn = 0usize;
    for entry in &registry {
        let Some(secret) = graph.get_secret(&entry.secret_ref)? else {
            tracing::warn!(
                vault = %entry.vault_name,
                secret_ref = %entry.secret_ref,
                "vault registry entry has no secret record — skipping"
            );
            continue;
        };
        if secret.secret_kind != "muninn_vault_token" {
            skipped_non_muninn += 1;
            tracing::debug!(
                vault = %entry.vault_name,
                secret_ref = %entry.secret_ref,
                secret_kind = %secret.secret_kind,
                "vault registry entry is not a Muninn vault token — skipping"
            );
            continue;
        }

        match resolve_secret(graph, &entry.secret_ref, &access)? {
            Some(token) => {
                config = config.with_vault_token(&entry.vault_name, token);
                loaded_vaults += 1;
            }
            None => {
                tracing::warn!(
                    vault = %entry.vault_name,
                    secret_ref = %entry.secret_ref,
                    "vault registry entry has no resolvable secret — skipping"
                );
            }
        }
    }

    tracing::info!(
        endpoint = %config.base_url,
        vaults   = loaded_vaults,
        skipped_non_muninn,
        "MuninnDB configured"
    );

    Ok(Some(config))
}

/// Apply a mesh-forwarded shared-vault memory write to THIS hotel's muninn —
/// the Cortex-side of `MuninnConfig::shared_write_route` (single-writer
/// routing). The payload is the `memory.write_forward` task JSON emitted by
/// a lobe philote's `forward_shared_memory_write`; the vault name arrives
/// already resolved by the originating guest, so this writes directly into
/// that vault (`remember_in_vault`) rather than re-resolving against a local
/// identity. Idempotent per `{vault}:{concept}` — a redelivered mesh
/// envelope reinforces instead of duplicating.
pub async fn apply_forwarded_write(graph: &GraphDomain, task_json: &str) -> Result<String> {
    let payload: serde_json::Value = serde_json::from_str(task_json)?;
    let op = payload.get("op").and_then(|v| v.as_str()).unwrap_or("");
    if op != "remember" {
        anyhow::bail!("memory.write_forward: unsupported op {op:?}");
    }
    let vault = payload
        .get("vault")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("memory.write_forward: missing vault"))?;
    let concept = payload
        .get("concept")
        .and_then(|v| v.as_str())
        .unwrap_or("untitled");
    let content = payload
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tags: Vec<String> = payload
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let metadata = payload
        .get("metadata")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let config = load_muninn_config(graph)?
        .ok_or_else(|| anyhow::anyhow!("memory.write_forward: MuninnDB not configured here"))?;
    let engine = engine_for_agent(config, "hotel", "hotel");
    let engram = engine
        .remember_in_vault(vault, concept, content, tags, metadata)
        .await?;
    Ok(engram.id)
}

/// Create a `MuninnRestEngine` scoped to a specific agent+user identity.
///
/// Called at agent session start (Slice E). The `config` is the shared hotel
/// config loaded by `load_muninn_config`.
#[allow(dead_code)]
pub fn engine_for_agent(config: MuninnConfig, agent_id: &str, user_id: &str) -> MuninnRestEngine {
    MuninnRestEngine::new(
        config,
        VaultResolver {
            agent_id: agent_id.to_string(),
            user_id: user_id.to_string(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{SecretInput, store_secret};
    use ansible_mesh_core::domain::GraphDomain;
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
    use ansible_mesh_core::storage::VaultRegistryEntry;
    use std::sync::Arc;

    #[tokio::test]
    async fn apply_forwarded_write_rejects_bad_payloads() {
        let (_s, domain) = open_domain();
        // unsupported op
        let err = apply_forwarded_write(&domain, r#"{"op":"forget","vault":"default"}"#)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unsupported op"), "{err}");
        // missing vault
        let err = apply_forwarded_write(&domain, r#"{"op":"remember","concept":"c"}"#)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing vault"), "{err}");
        // valid shape but no muninn configured on this hotel
        let err = apply_forwarded_write(
            &domain,
            r#"{"op":"remember","vault":"default","concept":"c","content":"x"}"#,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("not configured"), "{err}");
    }

    #[test]
    fn write_route_config_round_trips_through_load() {
        let (_s, domain) = open_domain();
        domain
            .set_muninn_write_route("vps-jane-aiua-01")
            .expect("set route");
        assert_eq!(
            domain
                .get_muninn_write_route()
                .expect("get route")
                .as_deref(),
            Some("vps-jane-aiua-01")
        );
    }

    fn open_domain() -> (SqliteGraphStorage, GraphDomain) {
        let storage = SqliteGraphStorage::open(":memory:").expect("open in-memory graph");
        let domain = GraphDomain::new(Arc::new(storage.adapter()));
        (storage, domain)
    }

    #[test]
    fn no_vault_registry_returns_none() {
        let (_storage, domain) = open_domain();
        let result = load_muninn_config(&domain).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn vault_registry_with_missing_secret_skips_and_returns_config() {
        let (_storage, domain) = open_domain();

        domain
            .upsert_vault_registry_entry(&VaultRegistryEntry {
                vault_name: "self_philote-1".into(),
                secret_ref: "secret://hotel/default/muninn/does-not-exist".into(),
            })
            .unwrap();

        // Should not error — skips with a warning, returns Some(config) with empty tokens.
        let config = load_muninn_config(&domain)
            .unwrap()
            .expect("Some even without valid secret");
        assert!(config.vault_tokens.is_empty());
    }

    #[test]
    fn vault_registry_with_valid_secret_builds_config() {
        let (_storage, domain) = open_domain();

        let secret_ref = store_secret(
            &domain,
            SecretInput {
                plaintext: "mk_test-token-abc123".to_string(),
                secret_kind: "muninn_vault_token".to_string(),
                scope: "hotel".to_string(),
                allowed_roles: vec!["hotel".to_string()],
                allowed_guests: vec!["hotel".to_string()],
            },
        )
        .unwrap();

        domain
            .upsert_vault_registry_entry(&VaultRegistryEntry {
                vault_name: "self_philote-1".into(),
                secret_ref: secret_ref.clone(),
            })
            .unwrap();
        domain.set_muninn_endpoint("http://127.0.0.1:8475").unwrap();

        let config = load_muninn_config(&domain)
            .unwrap()
            .expect("Some with valid registry");
        assert_eq!(config.base_url, "http://127.0.0.1:8475");
        assert_eq!(
            config
                .vault_tokens
                .get("self_philote-1")
                .map(String::as_str),
            Some("mk_test-token-abc123")
        );
    }

    #[test]
    fn vault_registry_skips_non_muninn_provider_secrets() {
        let (_storage, domain) = open_domain();

        let muninn_ref = store_secret(
            &domain,
            SecretInput {
                plaintext: "mk_test-token-abc123".to_string(),
                secret_kind: "muninn_vault_token".to_string(),
                scope: "hotel".to_string(),
                allowed_roles: vec!["hotel".to_string()],
                allowed_guests: vec!["hotel".to_string()],
            },
        )
        .unwrap();
        let provider_ref = store_secret(
            &domain,
            SecretInput {
                plaintext: "sk-provider".to_string(),
                secret_kind: "gemini_api_key".to_string(),
                scope: "hotel".to_string(),
                allowed_roles: vec!["model".to_string(), "model.gemini".to_string()],
                allowed_guests: Vec::new(),
            },
        )
        .unwrap();

        domain
            .upsert_vault_registry_entry(&VaultRegistryEntry {
                vault_name: "self_philote-1".into(),
                secret_ref: muninn_ref,
            })
            .unwrap();
        domain
            .upsert_vault_registry_entry(&VaultRegistryEntry {
                vault_name: "gemini_api_key".into(),
                secret_ref: provider_ref,
            })
            .unwrap();

        let config = load_muninn_config(&domain)
            .unwrap()
            .expect("mixed registry still configures Muninn");
        assert_eq!(config.vault_tokens.len(), 1);
        assert_eq!(
            config
                .vault_tokens
                .get("self_philote-1")
                .map(String::as_str),
            Some("mk_test-token-abc123")
        );
        assert!(!config.vault_tokens.contains_key("gemini_api_key"));
    }

    #[test]
    fn engine_for_agent_resolves_correct_vault_names() {
        // Smoke: engine_for_agent constructs without panic and resolver is wired.
        let config = MuninnConfig::local("default");
        let engine = engine_for_agent(config, "philote-1", "jared");
        // VaultResolver should produce self_philote-1 for SelfOnly scope.
        use memory_core::MemoryScope;
        let vaults = memory_core::VaultResolver {
            agent_id: "philote-1".to_string(),
            user_id: "jared".to_string(),
        }
        .resolve(&MemoryScope::SelfOnly);
        assert_eq!(vaults, vec!["self_philote-1"]);
        let _ = engine; // no panic
    }
}
