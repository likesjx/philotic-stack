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
use tracing::{info, warn};

use crate::vault::{SecretInput, rotate_secret, store_secret};

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

// ──── Admin session + mint helpers ───────────────────────────────────────────

/// Establish an authenticated MuninnDB admin session. Admin login lives on
/// the web UI port (API port + 1); the session rides a cookie store on the
/// returned client.
async fn admin_login(endpoint: &str, username: &str, password: &str) -> Result<Client> {
    let client = Client::builder()
        .cookie_store(true)
        .build()
        .context("Failed to build HTTP client")?;
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
    Ok(client)
}

/// Mint a full-access token for `vault` on an authenticated admin session.
/// The raw token is returned exactly once — callers must encrypt and store
/// it immediately and never log it.
async fn mint_token(client: &Client, endpoint: &str, vault: &str) -> Result<String> {
    let keys_url = format!("{}/api/admin/keys", endpoint.trim_end_matches('/'));
    let label = format!("aiua-{}", now_secs());
    let resp = client
        .post(&keys_url)
        .json(&CreateKeyRequest {
            vault,
            label: &label,
            mode: "full",
        })
        .send()
        .await
        .with_context(|| format!("Failed to mint token for vault {vault}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!(
            "Failed to mint token for vault {vault} ({}): {}",
            status,
            body
        );
    }
    let key_resp: CreateKeyResponse = resp
        .json()
        .await
        .with_context(|| format!("Failed to parse key response for vault {vault}"))?;
    Ok(key_resp.token)
}

/// Outcome of probing a stored vault token against MuninnDB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenProbe {
    /// MuninnDB accepted the token (any non-401 API answer to an
    /// authenticated request).
    Valid,
    /// MuninnDB actively rejected the token (HTTP 401) — stale binding.
    Rejected,
    /// Could not determine (network error) — do NOT mint on uncertainty.
    Indeterminate,
}

/// Cheap authenticated probe: GET a sentinel engram id in `vault`. A valid
/// token yields 404 (engram absent) or any other non-401 status; a stale
/// token yields 401.
pub async fn probe_token_validity(
    http: &Client,
    endpoint: &str,
    vault: &str,
    token: &str,
) -> TokenProbe {
    let url = format!(
        "{}/api/engrams/00TOKENPROBE?vault={}",
        endpoint.trim_end_matches('/'),
        vault
    );
    match http.get(&url).bearer_auth(token).send().await {
        Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => TokenProbe::Rejected,
        Ok(_) => TokenProbe::Valid,
        Err(_) => TokenProbe::Indeterminate,
    }
}

/// Re-mint the token for an ALREADY-REGISTERED vault and rotate the stored
/// secret in place (`secret_ref` preserved, so the `vault_registry` entry
/// stays valid). This is the hotel-side heal for a token-401
/// (`IpcRequest::HealMemoryToken`): re-derive the disposable MuninnDB half
/// of the token↔key binding from the durable Context-Graph truth.
///
/// Refuses vaults not present in the registry — new-vault creation stays in
/// [`provision_muninn_vaults`].
pub async fn remint_vault_token(
    graph: &GraphDomain,
    endpoint: &str,
    username: &str,
    password: &str,
    vault: &str,
) -> Result<()> {
    let registry = graph.get_vault_registry()?;
    let entry = registry
        .iter()
        .find(|e| e.vault_name == vault)
        .ok_or_else(|| {
            anyhow::anyhow!("vault {vault} is not registered — refusing to mint a token for it")
        })?;
    let client = admin_login(endpoint, username, password).await?;
    let token = mint_token(&client, endpoint, vault).await?;
    rotate_secret(graph, &entry.secret_ref, &token)
        .with_context(|| format!("Failed to rotate stored token for vault {vault}"))?;
    info!(
        vault = %vault,
        secret_ref = %entry.secret_ref,
        "MuninnDB vault token re-minted and rotated in place"
    );
    Ok(())
}

// ──── Admin credential resolution ────────────────────────────────────────────

/// MuninnDB admin credential resolved from the Context Graph.
pub struct MuninnAdminCredential {
    pub username: String,
    pub password: String,
}

/// Resolve the MuninnDB admin credential from the Context Graph.
///
/// Preferred source: a `SecretRecord` (kind `muninn_admin_credential`,
/// JSON `{"username":..,"password":..}`) pointed at by the
/// `muninn_admin_secret_ref` config key — the interface the
/// `muninn-vps-reharden` baseline provisions. Fallback: the `muninn` config
/// object injected at `--load-config` (`admin_username`/`admin_password`).
/// Returns `Ok(None)` when neither source yields a non-empty credential.
pub fn resolve_admin_credential(graph: &GraphDomain) -> Result<Option<MuninnAdminCredential>> {
    // Preferred: encrypted secret record.
    if let Some(raw_ref) = graph.get_config_value("muninn_admin_secret_ref")? {
        let secret_ref = json_string_or_raw(&raw_ref);
        if let Some(plaintext) = crate::vault::export_secret_plaintext(graph, &secret_ref)? {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&plaintext) {
                let username = value.get("username").and_then(|v| v.as_str()).unwrap_or("");
                let password = value.get("password").and_then(|v| v.as_str()).unwrap_or("");
                if !username.is_empty() && !password.is_empty() {
                    return Ok(Some(MuninnAdminCredential {
                        username: username.to_string(),
                        password: password.to_string(),
                    }));
                }
            }
            warn!(
                secret_ref = %secret_ref,
                "muninn_admin_secret_ref resolves but is not a {{username,password}} JSON object — falling back to config"
            );
        }
    }

    // Fallback: the raw `muninn` config object from --load-config.
    if let Some(raw) = graph.get_config_value("muninn")? {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
            let username = value
                .get("admin_username")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let password = value
                .get("admin_password")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !username.is_empty() && !password.is_empty() {
                return Ok(Some(MuninnAdminCredential {
                    username: username.to_string(),
                    password: password.to_string(),
                }));
            }
        }
    }

    Ok(None)
}

/// Config values are stored JSON-serialized (strings arrive quoted); accept
/// both a JSON string and a raw value.
fn json_string_or_raw(raw: &str) -> String {
    serde_json::from_str::<String>(raw).unwrap_or_else(|_| raw.to_string())
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

    // ── Admin login ───────────────────────────────────────────────────────────
    // MuninnDB admin login lives on the web UI port (8476), not the API port (8475).
    let client = admin_login(endpoint, username, password).await?;
    info!("MuninnDB admin session established");

    // Cookie-free client for token-validity probes: cookies are host-scoped
    // (not port-scoped), so probing with the admin-session client could let
    // the session cookie authenticate the request and mask a stale bearer.
    let probe_client = Client::new();

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
        // Step 1: Context Graph check — an already-registered vault is only
        // skipped if its stored token is still ACCEPTED by MuninnDB. A bare
        // presence check here is what left stale tokens in place after
        // MuninnDB key-store wipes (2026-07-20/21): the registry entry
        // blocked re-minting forever. On a 401 we re-mint and rotate the
        // secret in place instead of skipping.
        let registry = graph.get_vault_registry()?;
        if let Some(entry) = registry.iter().find(|e| &e.vault_name == vault_name) {
            let stored =
                crate::vault::export_secret_plaintext(graph, &entry.secret_ref).unwrap_or_default();
            match stored {
                Some(token) => {
                    match probe_token_validity(&probe_client, endpoint, vault_name, &token).await {
                        TokenProbe::Valid => {
                            info!(vault = %vault_name, "Vault registered and token valid — skipping");
                            continue;
                        }
                        TokenProbe::Indeterminate => {
                            warn!(
                                vault = %vault_name,
                                "Vault registered but token validity indeterminate (MuninnDB unreachable?) — skipping without minting"
                            );
                            continue;
                        }
                        TokenProbe::Rejected => {
                            warn!(
                                vault = %vault_name,
                                secret_ref = %entry.secret_ref,
                                "Vault registered but MuninnDB rejects the stored token — re-minting"
                            );
                            let token = mint_token(&client, endpoint, vault_name).await?;
                            rotate_secret(graph, &entry.secret_ref, &token).with_context(|| {
                                format!("Failed to rotate stored token for vault {vault_name}")
                            })?;
                            info!(vault = %vault_name, "Stale vault token re-minted and rotated in place");
                            continue;
                        }
                    }
                }
                None => {
                    warn!(
                        vault = %vault_name,
                        secret_ref = %entry.secret_ref,
                        "Vault registered but stored secret is missing/undecryptable — re-provisioning"
                    );
                    // Fall through to full provisioning; the registry upsert
                    // below replaces the dangling entry.
                }
            }
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
        let token = mint_token(&client, endpoint, vault_name).await?;

        // Step 4: Encrypt and store token; add vault_registry entry.
        let secret_ref = store_secret(
            graph,
            SecretInput {
                plaintext: token,
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

    // ──── Admin credential resolution ────────────────────────────────────

    fn open_domain() -> (
        ansible_mesh_core::sqlite_storage::SqliteGraphStorage,
        GraphDomain,
    ) {
        let storage =
            ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open(":memory:").expect("open");
        let domain = GraphDomain::new(std::sync::Arc::new(storage.adapter()));
        (storage, domain)
    }

    #[test]
    fn admin_credential_absent_when_unconfigured() {
        let (_s, domain) = open_domain();
        assert!(resolve_admin_credential(&domain).unwrap().is_none());
    }

    #[test]
    fn admin_credential_falls_back_to_muninn_config_object() {
        let (_s, domain) = open_domain();
        domain
            .set_config_value(
                "muninn",
                r#"{"endpoint":"http://127.0.0.1:8475","admin_username":"root","admin_password":"pw"}"#,
            )
            .unwrap();
        let cred = resolve_admin_credential(&domain).unwrap().expect("Some");
        assert_eq!(cred.username, "root");
        assert_eq!(cred.password, "pw");
    }

    #[test]
    fn admin_credential_rejects_empty_password() {
        let (_s, domain) = open_domain();
        domain
            .set_config_value("muninn", r#"{"admin_username":"root","admin_password":""}"#)
            .unwrap();
        assert!(resolve_admin_credential(&domain).unwrap().is_none());
    }

    #[test]
    fn admin_credential_prefers_secret_record() {
        let (_s, domain) = open_domain();
        // Fallback source present…
        domain
            .set_config_value(
                "muninn",
                r#"{"admin_username":"fallback","admin_password":"fallback-pw"}"#,
            )
            .unwrap();
        // …but the reharden-provisioned secret record wins.
        let secret_ref = store_secret(
            &domain,
            SecretInput {
                plaintext: r#"{"username":"vault-admin","password":"vault-pw"}"#.to_string(),
                secret_kind: "muninn_admin_credential".to_string(),
                scope: "hotel".to_string(),
                allowed_roles: vec!["hotel".to_string()],
                allowed_guests: vec!["hotel".to_string()],
            },
        )
        .unwrap();
        domain
            .set_config_value(
                "muninn_admin_secret_ref",
                &serde_json::to_string(&secret_ref).unwrap(),
            )
            .unwrap();
        let cred = resolve_admin_credential(&domain).unwrap().expect("Some");
        assert_eq!(cred.username, "vault-admin");
        assert_eq!(cred.password, "vault-pw");
    }

    // ──── Token validity probe ───────────────────────────────────────────

    fn spawn_canned_server(status: u16) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                use std::io::{Read, Write};
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn probe_classifies_401_as_rejected_and_404_as_valid() {
        let http = Client::new();
        let rejected = spawn_canned_server(401);
        assert_eq!(
            probe_token_validity(&http, &rejected, "self_x", "stale").await,
            TokenProbe::Rejected
        );
        let valid = spawn_canned_server(404);
        assert_eq!(
            probe_token_validity(&http, &valid, "self_x", "good").await,
            TokenProbe::Valid
        );
    }

    #[tokio::test]
    async fn probe_unreachable_is_indeterminate() {
        let http = Client::new();
        // Bind then drop a listener to get a port that refuses connections.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        assert_eq!(
            probe_token_validity(&http, &format!("http://127.0.0.1:{port}"), "self_x", "t").await,
            TokenProbe::Indeterminate
        );
    }

    // ──── Provisioning re-mint on stale token (end-to-end) ───────────────

    /// Route requests by "METHOD /path" prefix on a std-thread listener.
    fn serve_routes(listener: std::net::TcpListener, route: fn(&str) -> (u16, &'static str)) {
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                use std::io::{Read, Write};
                let mut buf = Vec::new();
                let mut chunk = [0u8; 2048];
                let (mut header_end, mut content_len) = (None, 0usize);
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if header_end.is_none() {
                                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
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
                            }
                            if let Some(end) = header_end {
                                if buf.len() >= end + content_len {
                                    break;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                let request_line = String::from_utf8_lossy(&buf)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let (status, body) = route(&request_line);
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
    }

    /// Bind an adjacent (api, api+1) port pair — provisioning derives the
    /// admin UI port as API port + 1.
    fn bind_port_pair() -> (std::net::TcpListener, std::net::TcpListener) {
        for _ in 0..32 {
            let api = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = api.local_addr().unwrap().port();
            if port == u16::MAX {
                continue;
            }
            if let Ok(ui) = std::net::TcpListener::bind(("127.0.0.1", port + 1)) {
                return (api, ui);
            }
        }
        panic!("could not bind an adjacent port pair");
    }

    #[tokio::test]
    async fn provisioning_remints_stale_token_in_place() {
        // The 2026-07-20/21 recurrence: vault registered, MuninnDB key store
        // wiped → stored token 401s. Provisioning must re-mint and rotate the
        // secret IN PLACE (same secret_ref) instead of skipping on registry
        // presence.
        let (_s, domain) = open_domain();
        let secret_ref = store_secret(
            &domain,
            SecretInput {
                plaintext: "mk_stale-token".to_string(),
                secret_kind: "muninn_vault_token".to_string(),
                scope: "hotel".to_string(),
                allowed_roles: vec!["hotel".to_string()],
                allowed_guests: vec!["hotel".to_string()],
            },
        )
        .unwrap();
        domain
            .upsert_vault_registry_entry(&VaultRegistryEntry {
                vault_name: "self_stale".to_string(),
                secret_ref: secret_ref.clone(),
            })
            .unwrap();

        let (api, ui) = bind_port_pair();
        let endpoint = format!("http://{}", api.local_addr().unwrap());
        serve_routes(api, |req| {
            if req.starts_with("GET /api/vaults") {
                (200, r#"["self_stale"]"#)
            } else if req.starts_with("GET /api/engrams/") {
                (401, "{}") // stale-token probe
            } else if req.starts_with("POST /api/admin/keys") {
                (200, r#"{"token":"mk_fresh-token"}"#)
            } else {
                (200, "{}") // plasticity etc.
            }
        });
        serve_routes(ui, |req| {
            if req.starts_with("POST /api/auth/login") {
                (200, "{}")
            } else {
                (404, "{}")
            }
        });

        provision_muninn_vaults(
            &domain,
            &endpoint,
            "root",
            "pw",
            vec!["self_stale".to_string()],
        )
        .await
        .expect("provisioning succeeds");

        // Same secret_ref, fresh plaintext.
        let registry = domain.get_vault_registry().unwrap();
        assert_eq!(registry.len(), 1);
        assert_eq!(registry[0].secret_ref, secret_ref);
        let plaintext = crate::vault::export_secret_plaintext(&domain, &secret_ref)
            .unwrap()
            .expect("secret still present");
        assert_eq!(plaintext, "mk_fresh-token");
    }
}
