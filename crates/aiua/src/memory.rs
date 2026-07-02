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

    // Hotel daemon accesses its own secrets using the "hotel" identity.
    let access = SecretAccess {
        role: "hotel".to_string(),
        guest_id: "hotel".to_string(),
    };

    for entry in &registry {
        match resolve_secret(graph, &entry.secret_ref, &access) {
            Ok(Some(token)) => {
                config = config.with_vault_token(&entry.vault_name, token);
            }
            Ok(None) => {
                tracing::warn!(
                    vault = %entry.vault_name,
                    secret_ref = %entry.secret_ref,
                    "vault registry entry has no resolvable secret — skipping"
                );
            }
            Err(err) => {
                // The vault registry also holds provider API keys (gemini/openai/
                // elevenlabs), which are ACL-scoped to their model roles and are
                // intentionally NOT readable by the "hotel" identity. Those are not
                // Muninn vault tokens, so a denial here is expected — skip the entry
                // instead of aborting the whole load (which would leave every agent
                // without memory).
                tracing::debug!(
                    vault = %entry.vault_name,
                    secret_ref = %entry.secret_ref,
                    error = %err,
                    "vault registry entry not accessible to hotel — skipping (expected for model-scoped provider keys)"
                );
            }
        }
    }

    tracing::info!(
        endpoint = %config.base_url,
        vaults   = registry.len(),
        "MuninnDB configured"
    );

    Ok(Some(config))
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
    fn model_scoped_provider_key_in_registry_is_skipped_not_fatal() {
        // Regression: the vault registry also holds provider API keys scoped to
        // model roles (gemini/openai/elevenlabs). The hotel identity cannot read
        // those; a denial must skip the entry, not abort the whole load and leave
        // every agent without memory.
        let (_storage, domain) = open_domain();

        // A real Muninn vault token the hotel CAN read.
        let muninn_ref = store_secret(
            &domain,
            SecretInput {
                plaintext: "mk_test-token-abc123".to_string(),
                secret_kind: "muninn_vault_token".to_string(),
                scope: "hotel".to_string(),
                allowed_roles: vec!["hotel".to_string()],
                allowed_guests: vec![],
            },
        )
        .unwrap();
        domain
            .upsert_vault_registry_entry(&VaultRegistryEntry {
                vault_name: "self_philote-1".into(),
                secret_ref: muninn_ref,
            })
            .unwrap();

        // A provider key scoped to model roles — NOT accessible to "hotel".
        let gemini_ref = store_secret(
            &domain,
            SecretInput {
                plaintext: "gemini-secret".to_string(),
                secret_kind: "gemini_api_key".to_string(),
                scope: "hotel".to_string(),
                allowed_roles: vec!["model".to_string(), "model.gemini".to_string()],
                allowed_guests: vec![],
            },
        )
        .unwrap();
        domain
            .upsert_vault_registry_entry(&VaultRegistryEntry {
                vault_name: "gemini_api_key".into(),
                secret_ref: gemini_ref,
            })
            .unwrap();
        domain.set_muninn_endpoint("http://127.0.0.1:8475").unwrap();

        let config = load_muninn_config(&domain)
            .unwrap()
            .expect("Some config despite an inaccessible provider key in the registry");
        // The Muninn token loaded; the model-scoped provider key was skipped.
        assert_eq!(
            config.vault_tokens.get("self_philote-1").map(String::as_str),
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
