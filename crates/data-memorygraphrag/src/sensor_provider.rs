//! `SensorProvider` — runs a scripted hotel sensor's Rhai check in-process.
//!
//! Dispatch is an ordinary `sensor.run` `DatasourceTask`, delivered by an
//! ordinary `CronJob { target_role: "life-graph-runner", .. }` — no sentinel
//! intercept in `aiua`'s `CronTicker` is needed; this provider is just
//! another `DatasourceProvider` in the same registry as `LifeGraphProvider`.
//! `life_call` binds straight to this guest's own in-process
//! `LifeGraphProvider::invoke`, zero IPC hop. See
//! `data_memorygraphrag::sensor_scripts` for the engine and
//! `docs/architecture/SCRIPTED_HOTEL_SENSORS_PROPOSAL.md` for the design.

use crate::provider::LifeGraphProvider;
use anyhow::{Context, Result};
use async_trait::async_trait;
use data_memorygraphrag::sensor_scripts::{self, SENSOR_TASK_KIND, SensorScript, SensorVerdict};
use datasource::controller::{DatasourceProvider, DatasourceTask, ProviderOutput};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::{Value, json};
use std::sync::Arc;
use tracing::warn;

pub struct SensorProvider {
    life: Arc<LifeGraphProvider>,
    guest_id: String,
    role: String,
}

impl SensorProvider {
    pub fn new(life: Arc<LifeGraphProvider>, guest_id: String, role: String) -> Self {
        Self {
            life,
            guest_id,
            role,
        }
    }

    fn identity(&self) -> GuestIdentity {
        GuestIdentity {
            guest_id: self.guest_id.clone(),
            role: self.role.clone(),
            supported_tools: Vec::new(),
        }
    }
}

#[async_trait]
impl DatasourceProvider for SensorProvider {
    fn id(&self) -> &str {
        "sensor"
    }

    fn supports(&self, task: &DatasourceTask) -> bool {
        task.kind.as_str() == SENSOR_TASK_KIND
    }

    async fn invoke(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let sensor_id = task
            .parameters
            .get("sensor_id")
            .and_then(Value::as_str)
            .context("contract_error: sensor.run requires parameters.sensor_id")?
            .to_string();

        let Some(mut script) = load_script(&self.identity(), &sensor_id).await? else {
            return Ok(quiet(&sensor_id, "no script registered locally"));
        };
        if !script.enabled {
            return Ok(quiet(&sensor_id, "disabled"));
        }
        if !script.operator_approved {
            warn!(
                sensor_id,
                "sensor.run: script not operator_approved — refusing to run"
            );
            return Ok(quiet(&sensor_id, "not operator_approved"));
        }

        let identity_for_config = self.identity();
        let life = Arc::clone(&self.life);
        let source = script.source.clone();
        let handle = tokio::runtime::Handle::current();

        // Rhai's Engine and its Rc-backed verdict cell are not Send, so
        // run_script must fully resolve before this async fn does anything
        // else with the result. Its config_value/life_call closures do real
        // async IPC/Memgraph work underneath — block_in_place lets them
        // drive that work to completion on this worker thread without
        // starving the runtime (multi-threaded #[tokio::main], required).
        let outcome: Result<SensorVerdict, String> = tokio::task::block_in_place(|| {
            let config_handle = handle.clone();
            let life_handle = handle.clone();
            sensor_scripts::run_script(
                &source,
                move |key: &str| {
                    config_handle
                        .block_on(fetch_config(&identity_for_config, key))
                        .unwrap_or_default()
                },
                move |tool: &str, params: Value| -> Result<Value, String> {
                    let sub_task = DatasourceTask::from_value(&json!({
                        "kind": tool,
                        "parameters": params,
                    }))
                    .map_err(|e| e.to_string())?;
                    life_handle
                        .block_on(life.invoke(&sub_task))
                        .map(provider_output_to_json)
                        .map_err(|e| format!("{e:#}"))
                },
            )
            .map_err(|e| e.to_string())
        });

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        script.last_run_at = Some(now_ms);
        script.last_result = Some(match &outcome {
            Ok(SensorVerdict::Quiet) => "quiet".to_string(),
            Ok(SensorVerdict::Deliver { .. }) => "delivered".to_string(),
            Ok(SensorVerdict::Investigate { .. }) => "investigate".to_string(),
            Err(e) => format!("error: {e}"),
        });
        if let Err(e) = save_script(&self.identity(), &script).await {
            warn!(
                sensor_id,
                "sensor.run: failed to persist last_result: {e:#}"
            );
        }

        match outcome {
            Ok(SensorVerdict::Quiet) => Ok(quiet(&sensor_id, "check ran, nothing due")),
            Ok(SensorVerdict::Deliver {
                target_role,
                message,
            }) => {
                let delivered = deliver(&self.identity(), &target_role, &message).await;
                Ok(ProviderOutput::ResultSet(json!({
                    "status": if delivered { "delivered" } else { "delivery_failed" },
                    "sensor_id": sensor_id,
                    "target_role": target_role,
                })))
            }
            Ok(SensorVerdict::Investigate { target_role, brief }) => {
                warn!(
                    sensor_id,
                    target_role, brief, "sensor.run: investigate verdict — dispatch not yet wired"
                );
                Ok(ProviderOutput::ResultSet(json!({
                    "status": "investigate_not_wired",
                    "sensor_id": sensor_id,
                })))
            }
            Err(e) => {
                warn!(sensor_id, "sensor.run: script error (non-fatal): {e}");
                Ok(ProviderOutput::ResultSet(json!({
                    "status": "error",
                    "sensor_id": sensor_id,
                    "message": e,
                })))
            }
        }
    }
}

fn quiet(sensor_id: &str, reason: &str) -> ProviderOutput {
    ProviderOutput::ResultSet(json!({
        "status": "quiet",
        "sensor_id": sensor_id,
        "reason": reason,
    }))
}

fn provider_output_to_json(out: ProviderOutput) -> Value {
    match out {
        ProviderOutput::ResultSet(v) => v,
        ProviderOutput::PartitionCreated { graph_id } => json!({ "graph_id": graph_id }),
        ProviderOutput::Acknowledge => json!({ "ok": true }),
    }
}

/// Read one hotel config value over IPC, tolerating both raw and
/// JSON-quoted storage. Connects under the guest's real identity — the
/// thing this port retires is the *fake* `"philotic-heartbeat"/"membrane"`
/// identity `main.rs::fetch_hotel_config` used before this move, not the
/// per-call connect itself (`DatasourceProvider::invoke` has no access to
/// the runtime's own already-open connection).
async fn fetch_config(identity: &GuestIdentity, key: &str) -> Option<String> {
    let mut client = PhiloticClient::connect(identity.clone()).await.ok()?;
    match client
        .send_request(IpcRequest::GetConfig {
            key: key.to_string(),
        })
        .await
    {
        Ok(IpcResponse::ConfigData { value_json, .. }) => value_json
            .map(|raw| {
                serde_json::from_str::<Value>(&raw)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or(raw)
            })
            .map(|v| v.trim().trim_matches('"').to_string())
            .filter(|v| !v.is_empty()),
        _ => None,
    }
}

async fn load_script(identity: &GuestIdentity, sensor_id: &str) -> Result<Option<SensorScript>> {
    let key = sensor_scripts::config_key(sensor_id);
    match fetch_config(identity, &key).await {
        None => Ok(None),
        Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
    }
}

async fn save_script(identity: &GuestIdentity, script: &SensorScript) -> Result<()> {
    let key = sensor_scripts::config_key(&script.id);
    let value_json = serde_json::to_string(script)?;
    let mut client = PhiloticClient::connect(identity.clone()).await?;
    client
        .send_request(IpcRequest::SetConfig { key, value_json })
        .await?;
    Ok(())
}

/// Hand a pre-formatted delivery message to the target philote as ONE turn.
/// Chat routing reuses `heartbeat_chat_id` (same hotel config key the
/// retired `data-memorygraphrag::heartbeat` prototype proved live) — a
/// script names *who* (target_role); the hotel config still names *where*.
async fn deliver(identity: &GuestIdentity, target_role: &str, message: &str) -> bool {
    let Some(chat_id) = fetch_config(identity, "heartbeat_chat_id").await else {
        warn!("sensor.run: deliver skipped — no heartbeat_chat_id configured");
        return false;
    };
    let task_json = match serde_json::to_string(&json!({
        "chat_id": chat_id,
        "content": message,
        "source": "telegram",
    })) {
        Ok(j) => j,
        Err(e) => {
            warn!("sensor.run: task serialization failed: {e}");
            return false;
        }
    };
    let mut client = match PhiloticClient::connect(identity.clone()).await {
        Ok(c) => c,
        Err(e) => {
            warn!("sensor.run: hotel IPC unavailable: {e:#}");
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
            warn!(refusal = message.as_str(), "sensor.run: emit refused");
            false
        }
        Ok(_) => true,
        Err(e) => {
            warn!("sensor.run: emit failed: {e:#}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datasource::controller::TaskKind;

    fn sensor_run_task(sensor_id: &str) -> DatasourceTask {
        DatasourceTask {
            kind: TaskKind::Custom(SENSOR_TASK_KIND.to_string()),
            provider: None,
            db: None,
            graph_id: None,
            query: None,
            parameters: json!({ "sensor_id": sensor_id }),
            identity: json!({}),
        }
    }

    /// No hotel IPC socket reachable (no aiua process in a unit test) must
    /// degrade to a quiet result, not hang or error — exercises
    /// `SensorProvider::invoke`'s real code path end to end (identity,
    /// `load_script`'s `fetch_config` over a socket that isn't there,
    /// `PhiloticClient::connect` failing gracefully) for the first time;
    /// `sensor_scripts::run_script` itself is covered separately with
    /// hand-built closures. Deliberately does not exercise the
    /// `block_in_place`/`life_call` bridge — that requires a script to
    /// actually be loaded, which needs a live hotel config store.
    #[tokio::test]
    async fn invoke_is_quiet_when_hotel_ipc_is_unavailable() {
        let life = Arc::new(LifeGraphProvider::from_env());
        let provider = SensorProvider::new(
            life,
            "test-sensor-guest".to_string(),
            "life-graph-runner".to_string(),
        );
        let task = sensor_run_task("reminders");

        assert!(provider.supports(&task));

        let output =
            tokio::time::timeout(std::time::Duration::from_secs(5), provider.invoke(&task))
                .await
                .expect("invoke must not hang when the hotel socket is unreachable")
                .expect("invoke must degrade gracefully, not error");

        let ProviderOutput::ResultSet(v) = output else {
            panic!("expected ResultSet");
        };
        assert_eq!(v["status"], "quiet");
        assert_eq!(v["sensor_id"], "reminders");
    }

    #[tokio::test]
    async fn invoke_rejects_task_missing_sensor_id() {
        let life = Arc::new(LifeGraphProvider::from_env());
        let provider = SensorProvider::new(
            life,
            "test-sensor-guest".to_string(),
            "life-graph-runner".to_string(),
        );
        let task = DatasourceTask {
            kind: TaskKind::Custom(SENSOR_TASK_KIND.to_string()),
            provider: None,
            db: None,
            graph_id: None,
            query: None,
            parameters: json!({}),
            identity: json!({}),
        };

        let err = provider.invoke(&task).await.unwrap_err();
        assert!(format!("{err:#}").contains("sensor_id"));
    }
}
