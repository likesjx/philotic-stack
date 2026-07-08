mod recurrence;

use ansible_mesh_core::heal_queue::HealQueueRow;
use anyhow::Result;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient, is_ipc_disconnect};
use recurrence::{Breach, RecurrenceTracker};
use std::time::Duration;
use tracing::{debug, error, info, warn};

const ROLE: &str = "heal-dispatcher";
const GUEST_ID: &str = "heal-dispatcher-01";

// How often to poll heal_queue for pending entries.
const POLL_INTERVAL_SECS: u64 = 30;
// Max entries fetched per cycle.
const BATCH_LIMIT: usize = 20;

// Prefix of the operator-visibility heal-queue entries the hotel writes when a
// work item is filed. Never track these for recurrence — they are our own echo.
const WORK_ITEM_FILED_PREFIX: &str = "work_item_filed:";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("heal-dispatcher starting");

    // Recurrence state is in-memory: a dispatcher restart resets every window
    // (acceptable — a recurring pattern re-accumulates within one window, and
    // the hotel dedups any double filing into a count bump). Owned by main so
    // IPC reconnects do NOT reset it.
    let mut tracker = RecurrenceTracker::from_env(|key| std::env::var(key).ok());
    let intel_graph_url = std::env::var("PHILOTIC_INTEL_GRAPH_URL")
        .ok()
        .map(|url| url.trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty());

    loop {
        match run(&mut tracker, intel_graph_url.as_deref()).await {
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

async fn run(tracker: &mut RecurrenceTracker, intel_graph_url: Option<&str>) -> Result<()> {
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
        if let Err(e) = dispatch_cycle(
            &mut ipc,
            &http,
            &ollama_url,
            &ollama_model,
            tracker,
            intel_graph_url,
        )
        .await
        {
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
    tracker: &mut RecurrenceTracker,
    intel_graph_url: Option<&str>,
) -> Result<()> {
    // Proactively repair session turns stuck in "running" for more than 5 minutes.
    match ipc
        .send_request(IpcRequest::RepairStaleSessionTurns { min_age_secs: 300 })
        .await
    {
        Ok(IpcResponse::Standard {
            data: Some(data), ..
        }) => {
            if let Some(n) = data.get("repaired").and_then(|v| v.as_u64()) {
                if n > 0 {
                    info!(
                        repaired = n,
                        "heal-dispatcher: zombie turn scan repaired stale turns"
                    );
                }
            }
        }
        Ok(_) => {}
        Err(e) => warn!("heal-dispatcher: RepairStaleSessionTurns failed: {e}"),
    }

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
        process_row(
            ipc,
            http,
            ollama_url,
            ollama_model,
            tracker,
            intel_graph_url,
            &row,
        )
        .await;
    }
    Ok(())
}

async fn process_row(
    ipc: &mut PhiloticClient,
    http: &reqwest::Client,
    ollama_url: &str,
    ollama_model: &str,
    tracker: &mut RecurrenceTracker,
    intel_graph_url: Option<&str>,
    row: &HealQueueRow,
) {
    // Rows from the hotel's turn-failure intake (FailTask classification,
    // philote PushHealEvent) arrive pre-triaged: pattern_tag + severity were
    // assigned at insert. Honour them — the tag keys A3 recurrence
    // aggregation, and re-classifying could split the same pattern across
    // two tags. Only the heal action is derived here.
    let (severity, pattern_tag, heal_action) =
        match row.pattern_tag.as_deref().filter(|tag| !tag.is_empty()) {
            Some(tag) => (
                if row.severity.is_empty() || row.severity == "unknown" {
                    "medium".to_string()
                } else {
                    row.severity.clone()
                },
                tag.to_string(),
                ansible_mesh_core::heal_queue::heal_action_for_pattern_tag(tag).to_string(),
            ),
            None => classify(http, ollama_url, ollama_model, &row.guest_id, &row.raw_text).await,
        };

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

    // Recurrence tracking (Autopoiesis Slice A3): a (pattern_tag, guest_id)
    // pair breaching the sliding-window threshold files a heal work item on
    // the hotel through the fleet.heal_slices autonomy lane.
    if !row.raw_text.starts_with(WORK_ITEM_FILED_PREFIX) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Some(breach) = tracker.record(&pattern_tag, &row.guest_id, now, &row.raw_text) {
            file_heal_work_item(
                ipc,
                http,
                intel_graph_url,
                &pattern_tag,
                &row.guest_id,
                breach,
                tracker.window_secs(),
            )
            .await;
        }
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

// ── Work-item filing (Autopoiesis Slice A3) ───────────────────────────────────

/// File a heal work item on the hotel via `IpcRequest::FileHealWorkItem`.
///
/// The hotel owns the autonomy decision (kill switch, grant freeze, daily
/// budget) and the durable record (autonomy_audit + heal_work_item nodes in
/// the hotel context graph). This side only reports the breach — and, when
/// the hotel confirms a fresh filing, mirrors it to the intel graph
/// best-effort.
async fn file_heal_work_item(
    ipc: &mut PhiloticClient,
    http: &reqwest::Client,
    intel_graph_url: Option<&str>,
    pattern_tag: &str,
    guest_id: &str,
    breach: Breach,
    window_secs: u64,
) {
    let response = ipc
        .send_request(IpcRequest::FileHealWorkItem {
            pattern_tag: pattern_tag.to_string(),
            guest_id: guest_id.to_string(),
            occurrence_count: breach.count,
            window_secs,
            evidence_lines: breach.evidence,
        })
        .await;

    let data = match response {
        Ok(IpcResponse::Standard {
            ok: true,
            data: Some(data),
            ..
        }) => data,
        Ok(other) => {
            warn!(
                pattern_tag,
                guest_id,
                ?other,
                "heal work item filing got unexpected response"
            );
            return;
        }
        Err(e) => {
            warn!(
                pattern_tag,
                guest_id, "heal work item filing IPC error: {e}"
            );
            return;
        }
    };

    let filed = data.get("filed").and_then(|v| v.as_bool()).unwrap_or(false);
    let deduped = data
        .get("deduped")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let work_item_id = data
        .get("work_item_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if filed {
        info!(
            pattern_tag,
            guest_id,
            work_item_id = %work_item_id,
            count = breach.count,
            window_secs,
            "heal work item filed"
        );
        // Best-effort intel-graph mirror — never blocks or fails the filing.
        if let Some(url) = intel_graph_url {
            push_intel_graph_record(
                http,
                url,
                pattern_tag,
                guest_id,
                breach.count,
                window_secs,
                &work_item_id,
            )
            .await;
        }
    } else if deduped {
        info!(
            pattern_tag,
            guest_id,
            work_item_id = %work_item_id,
            "heal work item already open — hotel bumped count/last_seen"
        );
    } else {
        let reason = data
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        debug!(
            pattern_tag,
            guest_id, reason, "heal work item filing refused by autonomy grant"
        );
    }
}

/// Mirror a fresh filing into the intel graph via `POST /api/decide` — the
/// graph server's decision-recording endpoint (there is no proposal-create
/// REST route; the decision node + mutation is the reviewable breadcrumb that
/// links the filing to the Autopoiesis proposal). Best-effort with a short
/// timeout: the intel-graph server lives on the dev machine and is often
/// down; the hotel-graph work item is the durable record.
async fn push_intel_graph_record(
    http: &reqwest::Client,
    base_url: &str,
    pattern_tag: &str,
    guest_id: &str,
    count: u32,
    window_secs: u64,
    work_item_id: &str,
) {
    let body = serde_json::json!({
        "target_node": "doc:AUTOPOIESIS_PROPOSAL",
        "action": "heal_work_item_filed",
        "to_value": work_item_id,
        "reason": format!(
            "recurring heal pattern '{pattern_tag}' on {guest_id} ({count}x in {window_secs}s) \
             filed as hotel-graph heal_work_item {work_item_id}"
        ),
        "agent": "heal-dispatcher",
    });
    let result = http
        .post(format!("{base_url}/api/decide"))
        .timeout(Duration::from_secs(5))
        .json(&body)
        .send()
        .await;
    match result {
        Ok(resp) if resp.status().is_success() => {
            info!(work_item_id, "heal work item mirrored to intel graph");
        }
        Ok(resp) => {
            debug!(
                work_item_id,
                status = %resp.status(),
                "intel graph mirror rejected (best-effort, ignoring)"
            );
        }
        Err(e) => {
            debug!(
                work_item_id,
                "intel graph unreachable, skipping mirror (best-effort): {e}"
            );
        }
    }
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
    // Turn-level failures (provider errors, model empty responses) use the
    // classifier shared with the hotel's FailTask intake so both sides always
    // produce the same pattern_tag (provider_4xx:{provider},
    // provider_timeout:{provider}, model_empty_response, …) and the A3
    // recurrence counter aggregates them correctly. provider_4xx maps to
    // escalate (→ work-item path), never restart_guest — a 400 is not fixed
    // by restarting.
    if let Some(class) = ansible_mesh_core::heal_queue::classify_turn_failure(text) {
        return Some((class.severity, class.pattern_tag, class.heal_action));
    }
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

#[cfg(test)]
mod tests {
    use super::rule_classify;

    /// Turn-failure tags must classify identically to the hotel's FailTask
    /// intake (shared classifier), and provider_4xx must escalate — never
    /// restart_guest.
    #[test]
    fn rule_classify_recognizes_turn_failure_tags() {
        let (severity, tag, action) = rule_classify(
            "[MODEL_EMPTY_RESPONSE] Model failed: 400 Bad Request \
             | kind=provider_failure | component=model-router | provider=gemini",
        )
        .expect("classified");
        assert_eq!(severity, "medium");
        assert_eq!(tag, "provider_4xx:gemini");
        assert_eq!(action, "escalate", "a 400 is not fixed by restarting");

        let (_, tag, action) =
            rule_classify("request timed out | kind=provider_failure | provider=anthropic")
                .expect("classified");
        assert_eq!(tag, "provider_timeout:anthropic");
        assert_eq!(action, "noop");

        let (_, tag, action) =
            rule_classify("[MODEL_EMPTY_RESPONSE] no usable output").expect("classified");
        assert_eq!(tag, "model_empty_response");
        assert_eq!(action, "escalate");
    }

    /// The turn-failure fast path must not shadow the existing rule table:
    /// non-provider lines keep their legacy tags and actions.
    #[test]
    fn rule_classify_legacy_rules_unaffected() {
        let (severity, tag, action) = rule_classify("connection refused").expect("classified");
        assert_eq!(
            (severity.as_str(), tag.as_str(), action.as_str()),
            ("high", "connection_refused", "restart_guest")
        );

        let (_, tag, action) = rule_classify("muninndb unreachable").expect("classified");
        assert_eq!(tag, "muninn_unreachable");
        assert_eq!(action, "refresh_memory_config");

        // A provider-marked 401 now aggregates per-provider as a 4xx (still
        // escalate); a bare 401 keeps the legacy auth_failure tag.
        let (_, tag, action) =
            rule_classify("401 Unauthorized | kind=provider_failure | provider=gemini")
                .expect("classified");
        assert_eq!(tag, "provider_4xx:gemini");
        assert_eq!(action, "escalate");
        let (_, tag, _) = rule_classify("401 unauthorized").expect("classified");
        assert_eq!(tag, "auth_failure");

        assert!(rule_classify("something entirely novel").is_none());
    }

    /// Pre-triaged tags (hotel intake / philote PushHealEvent) map to the
    /// dispatcher actions via the shared mapping.
    #[test]
    fn pre_triaged_tag_action_mapping() {
        use ansible_mesh_core::heal_queue::heal_action_for_pattern_tag;
        assert_eq!(
            heal_action_for_pattern_tag("stuck_turn_evicted:WaitingTool"),
            "escalate"
        );
        assert_eq!(
            heal_action_for_pattern_tag("fallback_exhausted:gemini"),
            "escalate"
        );
        assert_eq!(
            heal_action_for_pattern_tag("paracrine_budget_exhausted"),
            "escalate"
        );
        assert_eq!(
            heal_action_for_pattern_tag("provider_4xx:gemini"),
            "escalate"
        );
        assert_eq!(
            heal_action_for_pattern_tag("provider_timeout:gemini"),
            "noop"
        );
    }
}
