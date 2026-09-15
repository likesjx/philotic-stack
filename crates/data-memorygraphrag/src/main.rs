mod provider;

use anyhow::Result;
use data_memorygraphrag::hygiene;
use datasource::runtime::{DatasourceGuestConfig, run_datasource_controller};
use provider::LifeGraphProvider;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

fn guest_id() -> String {
    std::env::var("PHILOTIC_LIFE_GRAPH_RUNNER_ID")
        .or_else(|_| std::env::var("PHILOTIC_GRAPH_RUNNER_ID"))
        .unwrap_or_else(|_| "life-graph-runner".to_string())
}

/// Initial delay before the first hygiene sweep of a fresh runner process —
/// lets the runner finish registering/settling before it starts issuing
/// bulk Memgraph writes on its own initiative.
const HYGIENE_INITIAL_DELAY: Duration = Duration::from_secs(5 * 60);

/// Spawn the internal nightly hygiene-sweep timer, gated on
/// `PHILOTIC_LIFE_HYGIENE_ENABLED` (default OFF). Non-fatal by design: a
/// sweep error is logged and the loop keeps ticking — it must never crash
/// the runner or affect `life.observe`/`life.recall` availability.
fn spawn_hygiene_sweep_timer() {
    if !hygiene::hygiene_enabled_from_env() {
        info!(
            env = hygiene::HYGIENE_ENABLED_ENV,
            "life-graph hygiene sweep disabled (set to \"1\"/\"true\"/\"yes\" to enable)"
        );
        return;
    }
    let interval = Duration::from_secs(hygiene::interval_hours_from_env().saturating_mul(3600));
    tokio::spawn(async move {
        tokio::time::sleep(HYGIENE_INITIAL_DELAY).await;
        loop {
            let provider = LifeGraphProvider::from_env();
            match provider.hygiene_sweep().await {
                Ok(summary) => info!(
                    retired_stale = summary.retired_stale,
                    collapsed_duplicates = summary.collapsed_duplicates,
                    capped = summary.capped,
                    "life-graph hygiene sweep tick completed"
                ),
                Err(e) => warn!("life-graph hygiene sweep tick failed (non-fatal): {e:#}"),
            }
            tokio::time::sleep(interval).await;
        }
    });
}

/// Spawn the hotel-managed heartbeat: deterministic sensors on a short
/// interval that fire ONE agent turn only when a check finds real work
/// (operator-directed, 2026-08-27). Default ON (operator-facing reminders);
/// disable with `PHILOTIC_HEARTBEAT_ENABLED=0`. Non-fatal by design, like
/// the hygiene sweep. Because the runner materializes on one hotel, the
/// chief of staff stays the single deliverer structurally.
fn spawn_heartbeat_timer() {
    use data_memorygraphrag::heartbeat;
    if !heartbeat::enabled_from_env() {
        info!(
            env = heartbeat::HEARTBEAT_ENABLED_ENV,
            "philotic heartbeat disabled"
        );
        return;
    }
    let interval_secs = heartbeat::interval_secs_from_env();
    let target_role = heartbeat::target_role_from_env();
    let interval = Duration::from_secs(interval_secs);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        // Chat id is HOTEL config first (`config:heartbeat_chat_id` in the
        // context graph — durable, no deployment tooling), env override
        // second. Absent → sensor stays quiet but keeps checking, so setting
        // the config value later activates delivery without a restart.
        let mut chat_id: Option<String> = None;
        loop {
            if chat_id.is_none() {
                chat_id =
                    heartbeat::chat_id_from_env().or(fetch_hotel_config("heartbeat_chat_id").await);
                if chat_id.is_none() {
                    warn!(
                        config_key = "heartbeat_chat_id",
                        env = heartbeat::HEARTBEAT_CHAT_ID_ENV,
                        "philotic heartbeat: no chat id configured — sensing only"
                    );
                    tokio::time::sleep(interval).await;
                    continue;
                }
            }
            let provider = LifeGraphProvider::from_env();
            match provider.heartbeat_reminders_tick(interval_secs).await {
                Ok(None) => info!("heartbeat: check=reminders quiet"),
                Ok(Some(message)) => {
                    let chat = chat_id.as_deref().unwrap_or_default();
                    let emitted = emit_delivery_turn(chat, &target_role, &message).await;
                    info!(emit_ok = emitted, "heartbeat: check=reminders fired");
                }
                Err(e) => warn!("heartbeat tick failed (non-fatal): {e:#}"),
            }
            tokio::time::sleep(interval).await;
        }
    });
}

/// Read one hotel config value over IPC (`ConfigData.value_json`), tolerating
/// both raw and JSON-quoted storage.
async fn fetch_hotel_config(key: &str) -> Option<String> {
    use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: "philotic-heartbeat".into(),
        role: "membrane".into(),
        supported_tools: Vec::new(),
    })
    .await
    .ok()?;
    match client
        .send_request(IpcRequest::GetConfig {
            key: key.to_string(),
        })
        .await
    {
        Ok(IpcResponse::ConfigData { value_json, .. }) => value_json
            .map(|raw| {
                serde_json::from_str::<serde_json::Value>(&raw)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or(raw)
            })
            .map(|v| v.trim().trim_matches('"').to_string())
            .filter(|v| !v.is_empty()),
        _ => None,
    }
}

/// Hand a pre-formatted delivery message to the steward agent as ONE turn.
async fn emit_delivery_turn(chat_id: &str, target_role: &str, message: &str) -> bool {
    use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
    let task_json = match serde_json::to_string(&serde_json::json!({
        "chat_id": chat_id,
        "content": message,
        "source": "telegram",
    })) {
        Ok(json) => json,
        Err(e) => {
            warn!("heartbeat: task serialization failed: {e}");
            return false;
        }
    };
    let connect = PhiloticClient::connect(GuestIdentity {
        guest_id: "philotic-heartbeat".into(),
        role: "membrane".into(),
        supported_tools: Vec::new(),
    })
    .await;
    let mut client = match connect {
        Ok(client) => client,
        Err(e) => {
            warn!("heartbeat: hotel IPC unavailable: {e:#}");
            return false;
        }
    };
    let target_node =
        std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string());
    match client
        .send_request(IpcRequest::EmitTask {
            target_node,
            target_role: target_role.to_string(),
            target_guest_id: None,
            task_json,
        })
        .await
    {
        Ok(IpcResponse::Standard {
            ok: false, message, ..
        }) => {
            warn!(refusal = message.as_str(), "heartbeat: emit refused");
            false
        }
        Ok(_) => true,
        Err(e) => {
            warn!("heartbeat: emit failed: {e:#}");
            false
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let guest_id_static: &'static str = Box::leak(guest_id().into_boxed_str());

    info!(
        guest_id = guest_id_static,
        memgraph_uri = %std::env::var("PHILOTIC_MEMGRAPH_URI").unwrap_or_else(|_| "127.0.0.1:7687".to_string()),
        "life-graph-runner starting"
    );

    spawn_hygiene_sweep_timer();
    spawn_heartbeat_timer();

    run_datasource_controller(DatasourceGuestConfig {
        guest_id: guest_id_static,
        role: "life-graph-runner",
        providers: Box::new(|| vec![Arc::new(LifeGraphProvider::from_env())]),
    })
    .await
}
