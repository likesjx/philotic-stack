use ansible_mesh_core::heal_queue::HealQueueRow;
use anyhow::Result;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient, is_ipc_disconnect};
use std::time::Duration;
use tracing::{error, info, warn};

const ROLE: &str = "heal-dispatcher";
const GUEST_ID: &str = "heal-dispatcher-01";

// How often to poll heal_queue for pending entries.
const POLL_INTERVAL_SECS: u64 = 30;
// Max entries fetched per cycle.
const BATCH_LIMIT: usize = 20;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("heal-dispatcher starting");

    loop {
        match run().await {
            Ok(()) => {
                info!("heal-dispatcher exiting cleanly");
                break;
            }
            Err(e) if is_ipc_disconnect(&e) => {
                warn!("heal-dispatcher IPC disconnected, reconnecting in 5s…");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Err(e) => {
                error!("heal-dispatcher fatal error: {e:#}");
                return Err(e);
            }
        }
    }
    Ok(())
}

async fn run() -> Result<()> {
    let identity = GuestIdentity {
        guest_id: GUEST_ID.to_string(),
        role: ROLE.to_string(),
        supported_tools: Vec::new(),
    };
    let mut ipc = PhiloticClient::connect(identity).await?;
    info!("heal-dispatcher connected");

    // Optional: Ollama base URL for Gemma-powered classification.
    let ollama_url = std::env::var("PHILOTIC_OLLAMA_URL")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());
    let ollama_model =
        std::env::var("PHILOTIC_HEAL_MODEL").unwrap_or_else(|_| "gemma3:4b".to_string());
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let mut interval = tokio::time::interval(Duration::from_secs(POLL_INTERVAL_SECS));
    loop {
        interval.tick().await;
        if let Err(e) = dispatch_cycle(&mut ipc, &http, &ollama_url, &ollama_model).await {
            if is_ipc_disconnect(&e) {
                return Err(e);
            }
            warn!("heal-dispatcher cycle error: {e:#}");
        }
    }
}

async fn dispatch_cycle(
    ipc: &mut PhiloticClient,
    http: &reqwest::Client,
    ollama_url: &str,
    ollama_model: &str,
) -> Result<()> {
    let resp = ipc
        .send_request(IpcRequest::GetHealQueuePending { limit: BATCH_LIMIT })
        .await?;
    let IpcResponse::HealQueuePending { rows } = resp else {
        return Ok(());
    };

    if rows.is_empty() {
        return Ok(());
    }

    info!(
        count = rows.len(),
        "heal-dispatcher: processing pending entries"
    );

    for row in rows {
        process_row(ipc, http, ollama_url, ollama_model, &row).await;
    }
    Ok(())
}

async fn process_row(
    ipc: &mut PhiloticClient,
    http: &reqwest::Client,
    ollama_url: &str,
    ollama_model: &str,
    row: &HealQueueRow,
) {
    let (severity, pattern_tag, heal_action) =
        classify(http, ollama_url, ollama_model, &row.guest_id, &row.raw_text).await;

    // Write triage back.
    if let Err(e) = ipc
        .send_request(IpcRequest::TriageHealEntry {
            id: row.id.clone(),
            severity: severity.clone(),
            pattern_tag: pattern_tag.clone(),
            heal_action: heal_action.clone(),
        })
        .await
    {
        warn!(id = %row.id, "triage write failed: {e}");
        return;
    }

    // Execute the action and record outcome.
    let outcome = execute_action(ipc, &row.guest_id, &heal_action).await;
    if let Err(e) = ipc
        .send_request(IpcRequest::ResolveHealEntry {
            id: row.id.clone(),
            outcome: outcome.clone(),
        })
        .await
    {
        warn!(id = %row.id, "resolve write failed: {e}");
    }

    info!(
        id = %row.id,
        guest_id = %row.guest_id,
        pattern = %pattern_tag,
        action = %heal_action,
        outcome = %outcome,
        "heal-dispatcher: entry resolved"
    );
}

// ── Classifier ────────────────────────────────────────────────────────────────

// Returns (severity, pattern_tag, heal_action).
async fn classify(
    http: &reqwest::Client,
    ollama_url: &str,
    ollama_model: &str,
    guest_id: &str,
    raw_text: &str,
) -> (String, String, String) {
    // Rule-based fast path — always runs, no model dependency.
    if let Some(result) = rule_classify(raw_text) {
        return result;
    }

    // FunctionGemma: call Ollama for novel/unclassified patterns.
    match gemma_classify(http, ollama_url, ollama_model, guest_id, raw_text).await {
        Ok(result) => result,
        Err(e) => {
            warn!("gemma classify failed ({e}), falling back to noop");
            ("unknown".into(), "unclassified".into(), "noop".into())
        }
    }
}

fn rule_classify(text: &str) -> Option<(String, String, String)> {
    let t = text.to_lowercase();
    // MuninnDB outage — hotel pushes this exact phrase; handle before generic connection_refused.
    if t.contains("muninndb unreachable") {
        return Some((
            "high".into(),
            "muninn_unreachable".into(),
            "refresh_memory_config".into(),
        ));
    }
    if t.contains("connection refused") || t.contains("econnrefused") {
        return Some((
            "high".into(),
            "connection_refused".into(),
            "restart_guest".into(),
        ));
    }
    if t.contains("401 unauthorized") || t.contains("unauthorized") {
        return Some(("high".into(), "auth_failure".into(), "escalate".into()));
    }
    if t.contains("api key expired") || t.contains("key expired") {
        return Some(("high".into(), "api_key_expired".into(), "escalate".into()));
    }
    if t.contains("no_provider") || t.contains("no provider registered") {
        return Some((
            "high".into(),
            "provider_unavailable".into(),
            "escalate".into(),
        ));
    }
    if t.contains("panicked") || t.contains("thread 'main' panicked") {
        return Some(("critical".into(), "panic".into(), "restart_guest".into()));
    }
    if t.contains("out of memory") || t.contains("oom") {
        return Some(("critical".into(), "oom".into(), "restart_guest".into()));
    }
    if t.contains("timed out") || t.contains("timeout") {
        return Some(("medium".into(), "timeout".into(), "noop".into()));
    }
    if t.contains("failed to open agent graph sqlite database")
        || t.contains("unable to open database file")
        || t.contains("unable to open the database file")
    {
        return Some((
            "high".into(),
            "database_open_failed".into(),
            "escalate".into(),
        ));
    }
    if t.contains("media-codec") || t.contains("failed to normalize audio") {
        return Some((
            "medium".into(),
            "media_codec_failed".into(),
            "escalate".into(),
        ));
    }
    if t.contains("no such file") || t.contains("enoent") {
        return Some(("high".into(), "missing_file".into(), "escalate".into()));
    }
    None
}

async fn gemma_classify(
    http: &reqwest::Client,
    ollama_url: &str,
    model: &str,
    guest_id: &str,
    raw_text: &str,
) -> Result<(String, String, String)> {
    let prompt = format!(
        r#"You are a system reliability classifier. Given a guest process error log line, output ONLY valid JSON with three fields:
- "severity": one of "critical", "high", "medium", "low", "unknown"
- "pattern_tag": a short snake_case label (e.g. "connection_refused", "auth_failure", "disk_full", "panic", "oom", "timeout", "config_error", "unclassified")
- "heal_action": one of "restart_guest", "escalate", "noop"

Guest: {guest_id}
Error: {raw_text}

Output only the JSON object, no other text."#
    );

    let body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "stream": false,
        "format": "json"
    });

    let resp = http
        .post(format!("{ollama_url}/api/generate"))
        .json(&body)
        .send()
        .await?;

    let json: serde_json::Value = resp.json().await?;
    let text = json["response"].as_str().unwrap_or("{}");
    let parsed: serde_json::Value = serde_json::from_str(text)?;

    let severity = parsed["severity"].as_str().unwrap_or("unknown").to_string();
    let pattern_tag = parsed["pattern_tag"]
        .as_str()
        .unwrap_or("unclassified")
        .to_string();
    let heal_action = parsed["heal_action"].as_str().unwrap_or("noop").to_string();

    Ok((severity, pattern_tag, heal_action))
}

// ── Action executor ───────────────────────────────────────────────────────────

async fn execute_action(ipc: &mut PhiloticClient, guest_id: &str, heal_action: &str) -> String {
    match heal_action {
        "restart_guest" => {
            info!(guest_id, "heal-dispatcher: requesting guest restart");
            // Emit a restart request to the hotel's materialization surface.
            // The hotel will reclaim and respawn the guest via GuestManager.
            match ipc
                .send_request(IpcRequest::RestartComponent {
                    guest_id: guest_id.to_string(),
                })
                .await
            {
                Ok(IpcResponse::Standard { ok: true, .. }) => "restarted".into(),
                Ok(resp) => {
                    warn!(guest_id, ?resp, "restart response unexpected");
                    "restart_failed".into()
                }
                Err(e) => {
                    warn!(guest_id, "restart request error: {e}");
                    "restart_error".into()
                }
            }
        }
        "refresh_memory_config" => {
            info!(
                guest_id,
                "heal-dispatcher: triggering immediate MuninnDB probe"
            );
            match ipc.send_request(IpcRequest::RefreshMemoryConfig).await {
                Ok(IpcResponse::MuninnStatus {
                    available,
                    endpoint,
                }) => {
                    if available {
                        info!(guest_id, endpoint = %endpoint, "MuninnDB probe succeeded — memory restored");
                        "memory_restored".into()
                    } else {
                        warn!(guest_id, endpoint = %endpoint, "MuninnDB still unreachable after probe");
                        "still_unreachable".into()
                    }
                }
                Ok(resp) => {
                    warn!(
                        guest_id,
                        ?resp,
                        "refresh_memory_config got unexpected response"
                    );
                    "probe_failed".into()
                }
                Err(e) => {
                    warn!(guest_id, "refresh_memory_config IPC error: {e}");
                    "probe_error".into()
                }
            }
        }
        "escalate" => {
            warn!(
                guest_id,
                "heal-dispatcher: escalating — operator attention required"
            );
            "escalated".into()
        }
        _ => "noop".into(),
    }
}
