//! MuninnDB vault provisioning — run once during `--load-config`.
//!
//! # What this does
//!
//! For each derived vault name (e.g. `self_agent-jane-01`, `user_jared`):
//!
//! 1. **Context Graph check** — if a `vault_registry` entry already exists for
//!    this vault name, the token is already stored and we skip it entirely.
//! 2. **MuninnDB vault check** — if the vault does not exist in MuninnDB yet,
//!    create it (`PUT /api/admin/vaults/config`).
//! 3. **Mint token** — `POST /api/admin/keys` with `mode: "full"`. The token
//!    is shown exactly once by MuninnDB; we encrypt and store it immediately.
//! 4. **Store** — `store_secret` encrypts the token into the Context Graph;
//!    `upsert_vault_registry_entry` registers the vault name → secret_ref mapping.
//!
//! Running `--load-config` more than once is safe: already-registered vaults
//! are skipped. New vaults (e.g. a second agent added later) are provisioned
//! on the next run.

use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::storage::VaultRegistryEntry;
use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::vault::{SecretInput, store_secret};

// ──── Wire types ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct LoginRequest<'a> {
    username: &'a str,
    password: &'a str,
}

#[derive(Debug, Serialize)]
struct CreateVaultRequest<'a> {
    name: &'a str,
    public: bool,
}

#[derive(Debug, Serialize)]
struct CreateKeyRequest<'a> {
    vault: &'a str,
    label: &'a str,
    mode: &'a str,
}

#[derive(Debug, Deserialize)]
struct CreateKeyResponse {
    token: String,
}

/// Plasticity config sent to `PUT /api/admin/vault/{name}/plasticity`.
/// Only LTP and inline enrichment fields are set — all other fields are
/// left nil (use preset defaults).
#[derive(Debug, Serialize)]
struct PlasticityPatch {
    version: u32,
    preset: &'static str,
    ltp_threshold: u32,
    ltp_weight_floor: f32,
    /// "caller_preferred" — Rust Attend-phase enrichment takes priority;
    /// background LLM pipeline fills any stages we haven't provided.
    inline_enrichment: &'static str,
}

impl PlasticityPatch {
    /// Per-agent vault: memories are personal and high-value.
    /// LTP potentiates after 5 co-activations; floor at 0.3.
    fn for_agent_vault() -> Self {
        Self {
            version: 1,
            preset: "default",
            ltp_threshold: 5,
            ltp_weight_floor: 0.3,
            inline_enrichment: "caller_preferred",
        }
    }

    /// Shared user vault: memories span multiple agents, slightly higher bar.
    /// LTP potentiates after 8 co-activations; floor at 0.2.
    fn for_user_vault() -> Self {
        Self {
            version: 1,
            preset: "default",
            ltp_threshold: 8,
            ltp_weight_floor: 0.2,
            inline_enrichment: "caller_preferred",
        }
    }
}

// ──── Public entry point ──────────────────────────────────────────────────────

/// Provision MuninnDB vaults for all agents and users derived from the mesh
/// config. Idempotent: already-registered vaults are skipped.
///
/// `vault_names` should be pre-derived by the caller (e.g. `self_agent-jane-01`,
/// `user_jared`). The admin session is established once and reused for all vaults.
pub async fn provision_muninn_vaults(
    graph: &GraphDomain,
    endpoint: &str,
    username: &str,
    password: &str,
    vault_names: Vec<String>,
) -> Result<()> {
    if vault_names.is_empty() {
        info!("No vaults to provision.");
        return Ok(());
    }

    let client = Client::builder()
        .cookie_store(true)
        .build()
        .context("Failed to build HTTP client")?;

    // ── Admin login ───────────────────────────────────────────────────────────
    // MuninnDB admin login lives on the web UI port (8476), not the API port (8475).
    // Derive it: replace the last port segment with 8476.
    let ui_base = derive_ui_base(endpoint);
    let login_url = format!("{}/api/auth/login", ui_base);
    let resp = client
        .post(&login_url)
        .json(&LoginRequest { username, password })
        .send()
        .await
        .context("MuninnDB admin login request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("MuninnDB admin login failed ({}): {}", status, body);
    }
    info!("MuninnDB admin session established");

    // ── Fetch existing MuninnDB vaults ────────────────────────────────────────
    let vaults_url = format!("{}/api/vaults", endpoint.trim_end_matches('/'));
    let existing_muninn: Vec<String> = client
        .get(&vaults_url)
        .send()
        .await
        .context("Failed to list MuninnDB vaults")?
        .json()
        .await
        .context("Failed to parse MuninnDB vault list")?;

    // ── Provision each vault ──────────────────────────────────────────────────
    for vault_name in &vault_names {
        // Step 1: Context Graph check — skip if already registered.
        let registry = graph.get_vault_registry()?;
        if registry.iter().any(|e| &e.vault_name == vault_name) {
            info!(vault = %vault_name, "Vault already registered — skipping");
            continue;
        }

        // Step 2: Create vault in MuninnDB if it doesn't exist there yet.
        if !existing_muninn.contains(vault_name) {
            let create_url = format!("{}/api/admin/vaults/config", endpoint.trim_end_matches('/'));
            let resp = client
                .put(&create_url)
                .json(&CreateVaultRequest {
                    name: vault_name,
                    public: false,
                })
                .send()
                .await
                .with_context(|| format!("Failed to create vault {vault_name}"))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                bail!("Failed to create vault {vault_name} ({}): {}", status, body);
            }
            info!(vault = %vault_name, "MuninnDB vault created");
        } else {
            info!(vault = %vault_name, "MuninnDB vault exists — minting new token");
        }

        // Step 3: Mint a full-access token for the vault.
        let keys_url = format!("{}/api/admin/keys", endpoint.trim_end_matches('/'));
        let label = format!("aiua-{}", now_secs());
        let resp = client
            .post(&keys_url)
            .json(&CreateKeyRequest {
                vault: vault_name,
                label: &label,
                mode: "full",
            })
            .send()
            .await
            .with_context(|| format!("Failed to mint token for vault {vault_name}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "Failed to mint token for vault {vault_name} ({}): {}",
                status,
                body
            );
        }

        let key_resp: CreateKeyResponse = resp
            .json()
            .await
            .with_context(|| format!("Failed to parse key response for vault {vault_name}"))?;

        // Step 4: Encrypt and store token; add vault_registry entry.
        let secret_ref = store_secret(
            graph,
            SecretInput {
                plaintext: key_resp.token,
                secret_kind: "muninn_vault_token".to_string(),
                scope: "hotel".to_string(),
                allowed_roles: vec!["hotel".to_string()],
                allowed_guests: vec!["hotel".to_string()],
            },
        )
        .with_context(|| format!("Failed to store vault token for {vault_name}"))?;

        graph
            .upsert_vault_registry_entry(&VaultRegistryEntry {
                vault_name: vault_name.clone(),
                secret_ref,
            })
            .with_context(|| format!("Failed to register vault {vault_name}"))?;

        // Step 5: Configure LTP and inline enrichment plasticity.
        // self_* vaults get aggressive LTP (threshold 5); user_* vaults get
        // a slightly higher bar (threshold 8). Both prefer Rust-generated
        // enrichment over the background LLM pipeline.
        let patch = if vault_name.starts_with("self_") {
            PlasticityPatch::for_agent_vault()
        } else {
            PlasticityPatch::for_user_vault()
        };
        let plasticity_url = format!(
            "{}/api/admin/vault/{}/plasticity",
            endpoint.trim_end_matches('/'),
            vault_name,
        );
        let resp = client
            .put(&plasticity_url)
            .json(&patch)
            .send()
            .await
            .with_context(|| format!("Failed to set plasticity for {vault_name}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            // Non-fatal: plasticity defaults are safe. Log and continue.
            tracing::warn!(
                vault = %vault_name,
                %status,
                body = %body,
                "Failed to configure vault plasticity — using defaults"
            );
        } else {
            info!(
                vault = %vault_name,
                ltp_threshold = patch.ltp_threshold,
                ltp_weight_floor = patch.ltp_weight_floor,
                "Vault plasticity configured"
            );
        }

        info!(vault = %vault_name, "Vault provisioned and registered");
    }

    info!(
        count = vault_names.len(),
        "MuninnDB vault provisioning complete"
    );
    Ok(())
}

// ──── Config helpers ──────────────────────────────────────────────────────────

/// Derive vault names from a mesh-config JSON value.
///
/// - Each `agent_id` in any hotel's agents → `self_{agent_id}`
/// - Each username in any agent's `telegram.allowed_users` → `user_{username}`
pub fn derive_vault_names(config_json: &serde_json::Value) -> Vec<String> {
    let mut vaults = std::collections::BTreeSet::new();

    let Some(hotels) = config_json.get("hotels").and_then(|v| v.as_object()) else {
        return vec![];
    };

    for (_hotel_name, hotel) in hotels {
        let Some(agents) = hotel.get("agents").and_then(|v| v.as_object()) else {
            continue;
        };
        for (_agent_key, agent) in agents {
            if let Some(agent_id) = agent.get("agent_id").and_then(|v| v.as_str()) {
                vaults.insert(format!("self_{agent_id}"));
            }
            if let Some(users) = agent
                .get("telegram")
                .and_then(|t| t.get("allowed_users"))
                .and_then(|v| v.as_array())
            {
                for user in users {
                    if let Some(username) = user.as_str() {
                        vaults.insert(format!("user_{username}"));
                    }
                }
            }
        }
    }

    vaults.into_iter().collect()
}

/// Derive the MuninnDB web UI base URL from the API endpoint.
/// The web UI port is always API port + 1 (8475 → 8476).
fn derive_ui_base(endpoint: &str) -> String {
    // Parse the port and increment it.
    if let Some(colon_pos) = endpoint.rfind(':') {
        let (base, port_str) = endpoint.split_at(colon_pos);
        // port_str starts with ':', may have a path suffix
        let port_part = &port_str[1..];
        let (port_num_str, suffix) = port_part
            .find('/')
            .map(|i| port_part.split_at(i))
            .unwrap_or((port_part, ""));
        if let Ok(port) = port_num_str.parse::<u16>() {
            return format!("{}:{}{}", base, port + 1, suffix);
        }
    }
    // Fallback: assume standard ports
    endpoint.replace(":8475", ":8476")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ──── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> serde_json::Value {
        serde_json::json!({
            "hotels": {
                "default": {
                    "agents": {
                        "jane": {
                            "agent_id": "agent-jane-01",
                            "telegram": { "allowed_users": ["jared", "bob"] }
                        },
                        "aria": {
                            "agent_id": "agent-aria-01",
                            "telegram": { "allowed_users": ["jared"] }
                        }
                    }
                },
                "second-hotel": {
                    "agents": {
                        "rex": {
                            "agent_id": "agent-rex-01",
                            "telegram": { "allowed_users": ["alice"] }
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn derive_vault_names_collects_agents_and_users() {
        let mut names = derive_vault_names(&config());
        names.sort();
        assert_eq!(
            names,
            vec![
                "self_agent-aria-01",
                "self_agent-jane-01",
                "self_agent-rex-01",
                "user_alice",
                "user_bob",
                "user_jared",
            ]
        );
    }

    #[test]
    fn derive_vault_names_deduplicates_shared_users() {
        // jared appears in both jane and aria — should appear once.
        let names = derive_vault_names(&config());
        let jared_count = names.iter().filter(|n| n.as_str() == "user_jared").count();
        assert_eq!(jared_count, 1);
    }

    #[test]
    fn derive_vault_names_empty_on_no_hotels() {
        let names = derive_vault_names(&serde_json::json!({}));
        assert!(names.is_empty());
    }
}
