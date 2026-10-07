//! Memory: Muninn token self-heal, endpoint probe loop, memory delta digest.
//!
//! Moved verbatim from `ipc/mod.rs` (IPC_DISPATCH_SPLIT); only item
//! visibility was widened so the parent module can reach it.

use super::*;

impl IpcServer {
    /// Hotel-side handler for `IpcRequest::HealMemoryToken`: a guest hit a
    /// token-401 (MuninnDB reachable but rejecting the stored bearer). The
    /// hotel — which owns the vault master key, the Context Graph, and the
    /// MuninnDB admin credential — re-mints the token and rotates the stored
    /// secret in place, then returns the refreshed memory config so the guest
    /// can retry once. Guardrails: per-vault mint budget (inside the window
    /// no second mint happens, but the live config — which already carries
    /// any just-rotated token — is served so other guests of a shared vault
    /// are not stranded); vault must already be registered; no admin
    /// credential → throttled operator escalation via the heal queue instead
    /// of a mint. Raw tokens never appear in logs or heal entries.
    pub(super) async fn handle_heal_memory_token(
        graph: &GraphDomain,
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
        attempts: &Mutex<HashMap<String, std::time::Instant>>,
        vault: &str,
    ) -> IpcResponse {
        {
            let mut map = attempts.lock().await;
            if let Some(last) = map.get(vault) {
                if last.elapsed() < Self::MUNINN_HEAL_MIN_INTERVAL {
                    // No second mint inside the window — but the FIRST heal
                    // (the one that consumed the budget) rotated the secret in
                    // the Context Graph, so serving the LIVE config still
                    // heals this caller. Vaults are shared across guests
                    // (`user_*`, fleet vaults): after a key-store wipe every
                    // guest of the vault 401s and requests a heal near-
                    // simultaneously; only the first may mint, the rest must
                    // not be stranded with a bare error until the window
                    // expires.
                    info!(
                        vault = %vault,
                        "HealMemoryToken: mint budget consumed {:?} ago — serving live config without minting",
                        last.elapsed()
                    );
                    let config_json = crate::memory::load_muninn_config(graph)
                        .ok()
                        .flatten()
                        .and_then(|cfg| serde_json::to_string(&cfg).ok());
                    if config_json.is_some() {
                        return IpcResponse::MemoryConfig(MemoryConfigPayload { config_json });
                    }
                    return IpcResponse::error(
                        "memory",
                        "HEAL_BUDGET_EXHAUSTED",
                        format!(
                            "token heal for vault [{vault}] attempted {:?} ago — next attempt allowed after {:?}",
                            last.elapsed(),
                            Self::MUNINN_HEAL_MIN_INTERVAL
                        ),
                    );
                }
            }
            map.insert(vault.to_string(), std::time::Instant::now());
        }

        let endpoint = graph
            .get_muninn_endpoint()
            .ok()
            .flatten()
            .unwrap_or_else(|| "http://127.0.0.1:8475".to_string());

        let cred = match crate::muninn_provision::resolve_admin_credential(graph) {
            Ok(Some(cred)) => cred,
            Ok(None) => {
                warn!(
                    vault = %vault,
                    "HealMemoryToken: MuninnDB rejects the stored token but no admin credential is available — manual resync required"
                );
                if let Some(hq) = heal_queue {
                    let _ = hq.push_error(
                        "hotel",
                        &format!(
                            "muninn token rejected for vault [{vault}] but no admin credential available to re-mint — manual token resync required (see MEMORY_TOKEN_SELF_HEAL_PROPOSAL)"
                        ),
                    );
                }
                return IpcResponse::error(
                    "memory",
                    "NO_ADMIN_CREDENTIAL",
                    format!("cannot heal token for vault [{vault}]: no MuninnDB admin credential"),
                );
            }
            Err(err) => {
                return IpcResponse::error(
                    "memory",
                    "HEAL_FAILED",
                    format!("admin credential resolution failed: {err}"),
                );
            }
        };

        match crate::muninn_provision::remint_vault_token(
            graph,
            &endpoint,
            &cred.username,
            &cred.password,
            vault,
        )
        .await
        {
            Ok(()) => {
                info!(vault = %vault, "HealMemoryToken: token re-minted and rotated — returning refreshed config");
                let config_json = crate::memory::load_muninn_config(graph)
                    .ok()
                    .flatten()
                    .and_then(|cfg| serde_json::to_string(&cfg).ok());
                IpcResponse::MemoryConfig(MemoryConfigPayload { config_json })
            }
            Err(err) => {
                warn!(vault = %vault, error = %err, "HealMemoryToken: re-mint failed");
                if let Some(hq) = heal_queue {
                    let _ = hq.push_error(
                        "hotel",
                        &format!("muninn token heal failed for vault [{vault}]: {err}"),
                    );
                }
                IpcResponse::error(
                    "memory",
                    "HEAL_FAILED",
                    format!("token heal for vault [{vault}] failed: {err}"),
                )
            }
        }
    }

    /// Returns `true` if the MuninnDB REST endpoint answers any HTTP request.
    /// A connection error (refused, timeout) returns `false`; any HTTP response returns `true`.
    pub(super) async fn probe_muninn_endpoint(http: &reqwest::Client, endpoint: &str) -> bool {
        match http.get(endpoint).send().await {
            Ok(_) => true,
            Err(e) if e.is_connect() || e.is_timeout() => false,
            Err(_) => true, // redirect, TLS, etc. — server exists
        }
    }

    /// Periodic MuninnDB probe loop. Spawned at hotel boot when MuninnDB is configured.
    /// Checks every 60 seconds; on state flip broadcasts `MuninnStatus` to all guests and
    /// pushes a heal-queue entry when the endpoint goes down.
    pub(super) async fn run_muninn_probe_loop(
        config: Arc<memory_core::MuninnConfig>,
        reachable: Arc<std::sync::atomic::AtomicBool>,
        broadcast_tx: tokio::sync::broadcast::Sender<IpcResponse>,
        heal_queue: Option<Arc<dyn ansible_mesh_core::heal_queue::HealQueueStorage>>,
    ) {
        let endpoint = config.base_url.clone();
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let available = Self::probe_muninn_endpoint(&http, &endpoint).await;
            let was = reachable.swap(available, std::sync::atomic::Ordering::Relaxed);
            if available != was {
                info!(available, endpoint = %endpoint, "MuninnDB reachability changed");
                let _ = broadcast_tx.send(IpcResponse::MuninnStatus {
                    available,
                    endpoint: endpoint.clone(),
                });
                if !available {
                    if let Some(hq) = heal_queue.as_deref() {
                        let msg = format!("MuninnDB unreachable: connection refused at {endpoint}");
                        if let Err(e) = hq.push_error("hotel", &msg) {
                            warn!(error = %e, "Failed to push MuninnDB outage to heal queue");
                        }
                    }
                }
            }
        }
    }

    /// Handle `GetConfig("__memory_delta_digest__")` /
    /// `GetConfig("__memory_delta_digest__:{hours}")` — the Memory
    /// Transparency Slice M3 delta digest the `memory.delta_digest` philote
    /// tool calls. Reconstructs a [`MuninnConfig`](memory_core::MuninnConfig)
    /// on demand via [`crate::memory::load_muninn_config`] rather than
    /// threading one through `process_request`'s already-long parameter
    /// list — the same config `run_scheduled_sweep` uses, built from the same
    /// graph-backed vault registry. Returns `ConfigData` with `value_json`
    /// `null` when Muninn is not configured on this hotel (an honest "not
    /// wired up" rather than an empty-but-successful digest).
    pub(super) async fn handle_memory_delta_digest(
        graph: &GraphDomain,
        local_node_id: &str,
        window_hours: u64,
    ) -> IpcResponse {
        let key = format!("__memory_delta_digest__:{window_hours}");

        let hotel_name = match Self::local_hotel_name(graph, local_node_id) {
            Some(name) => name,
            None => {
                warn!("memory.delta_digest: local hotel record missing — cannot collect digest");
                return IpcResponse::ConfigData {
                    key,
                    value_json: None,
                };
            }
        };

        let muninn_config = match crate::memory::load_muninn_config(graph) {
            Ok(Some(config)) => config,
            Ok(None) => {
                debug!("memory.delta_digest: Muninn not configured on this hotel — skipping");
                return IpcResponse::ConfigData {
                    key,
                    value_json: None,
                };
            }
            Err(e) => {
                warn!("memory.delta_digest: failed to load Muninn config: {e:#}");
                return IpcResponse::ConfigData {
                    key,
                    value_json: None,
                };
            }
        };

        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                warn!("memory.delta_digest: failed to build HTTP client — {e}");
                return IpcResponse::ConfigData {
                    key,
                    value_json: None,
                };
            }
        };

        let digest = crate::memory_delta_digest::collect(
            &client,
            &muninn_config,
            graph,
            &hotel_name,
            window_hours,
            chrono::Utc::now(),
        )
        .await;

        let value_json = serde_json::to_string(&serde_json::json!({
            "rendered": digest.render(),
            "digest": digest,
        }))
        .ok();
        IpcResponse::ConfigData { key, value_json }
    }
}
