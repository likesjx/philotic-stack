mod notify;
mod recurrence;

use ansible_mesh_core::heal_queue::HealQueueRow;
use anyhow::Result;
use notify::EscalationNotifier;
use philotic_client::{
    GuestIdentity, IpcRequest, IpcResponse, PhiloticClient, RestartReason, is_ipc_disconnect,
};
use recurrence::{Breach, RecurrenceTracker};
use std::future::Future;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

const ROLE: &str = "heal-dispatcher";
const GUEST_ID: &str = "heal-dispatcher-01";

// How often to poll heal_queue for pending entries.
const POLL_INTERVAL_SECS: u64 = 30;
// Max entries fetched per cycle.
const BATCH_LIMIT: usize = 20;

// Per-request IPC timeout (F3). PhiloticClient::send_request has no internal
// timeout, so a single hung hotel-side reply would otherwise wedge the whole
// dispatcher forever while its PID stays alive (the supervisor's PID-liveness
// check never respawns it) and the heal queue backs up. Every send_request in
// the dispatch path is wrapped so an expiry is treated exactly like an IPC
// disconnect: break the cycle and let `main` reconnect.
const IPC_TIMEOUT: Duration = Duration::from_secs(20);

// Application-level liveness key (F3). Each successful poll cycle stamps the
// current unix time here via SetConfig so a wedged-but-alive dispatcher is
// externally detectable. The name is a contract consumed by the phil doctor
// check — do NOT rename without updating that check.
const HEARTBEAT_KEY: &str = "heal_dispatcher.last_cycle_at";

// Ollama circuit breaker (F5). gemma_classify is awaited per-row with a 30s
// HTTP timeout; if Ollama is down and many rows fall through rule_classify,
// one cycle could take BATCH_LIMIT*30s. After this many consecutive Ollama
// failures the breaker opens and gemma_classify is short-circuited to the
// fail-safe ("unknown"/"unclassified"/"noop") for a cooldown window instead of
// re-attempting the 30s timeout per row. A single success resets it.
const OLLAMA_BREAKER_THRESHOLD: u32 = 3;
const OLLAMA_BREAKER_COOLDOWN: Duration = Duration::from_secs(60);

// Prefix of the operator-visibility heal-queue entries the hotel writes when a
// work item is filed. Never track these for recurrence — they are our own echo.
const WORK_ITEM_FILED_PREFIX: &str = "work_item_filed:";

// ── IPC timeout guard (F3) ─────────────────────────────────────────────────────

/// The error a timed-out `send_request` maps to. It wraps a `std::io::Error`
/// with `ErrorKind::BrokenPipe` **specifically** so `is_ipc_disconnect` returns
/// true and the existing reconnect path fires — a plain `anyhow!("timed out")`
/// would NOT downcast and would silently defeat the reconnect. Reconnecting (not
/// reusing the connection) is correct: the request write already went out, so a
/// late reply left in the old socket would desync the length-prefix framing on
/// the next read.
fn ipc_timeout_error() -> anyhow::Error {
    anyhow::Error::new(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        format!("heal-dispatcher IPC request timed out after {IPC_TIMEOUT:?}"),
    ))
}

/// Wrap an in-flight IPC response future with a timeout, mapping expiry to a
/// disconnect-class error. Kept generic so it is unit-testable without a live
/// hotel (a hung responder can be injected as a sleeping future).
async fn with_ipc_timeout<F>(dur: Duration, fut: F) -> Result<IpcResponse>
where
    F: Future<Output = Result<IpcResponse>>,
{
    match tokio::time::timeout(dur, fut).await {
        Ok(res) => res,
        Err(_elapsed) => Err(ipc_timeout_error()),
    }
}

/// `PhiloticClient::send_request` with a per-request timeout. Do NOT change the
/// shared client — the guard lives here at the call site.
async fn send_request_timeout(
    ipc: &mut PhiloticClient,
    req: IpcRequest,
    dur: Duration,
) -> Result<IpcResponse> {
    with_ipc_timeout(dur, ipc.send_request(req)).await
}

// ── Application-level liveness heartbeat (F3) ──────────────────────────────────

/// Build the SetConfig request that stamps the liveness heartbeat.
fn heartbeat_request(now_secs: u64) -> IpcRequest {
    IpcRequest::SetConfig {
        key: HEARTBEAT_KEY.to_string(),
        // Stored verbatim as the config blob; a bare integer is valid JSON.
        value_json: now_secs.to_string(),
    }
}

/// Stamp the heartbeat. A non-disconnect failure is logged and swallowed (the
/// heartbeat is a best-effort signal and must never abort a cycle); a
/// disconnect/timeout propagates so `main` reconnects.
async fn record_heartbeat(ipc: &mut PhiloticClient) -> Result<()> {
    let now = unix_now();
    match send_request_timeout(ipc, heartbeat_request(now), IPC_TIMEOUT).await {
        Ok(_) => Ok(()),
        Err(e) => {
            if is_ipc_disconnect(&e) {
                return Err(e);
            }
            warn!("heal-dispatcher: heartbeat write failed (non-fatal): {e}");
            Ok(())
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Local hotel node id for addressing EmitTask, mirroring philote's convention
/// (`PHILOTIC_NODE_ID`, falling back to the single-hotel default). Guests are
/// spawned with the hotel's configured env block, so this is set in a real
/// deployment; the fallback keeps a bare-socket dev run functional.
fn local_node_id() -> String {
    std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string())
}

// ── Operator notification push seam (F4) ───────────────────────────────────────

/// Throttled operator-notification sender. Owned by `main` (like the recurrence
/// tracker and Ollama breaker) so state survives IPC reconnects.
///
/// The push reuses the existing operator gateway: an `EmitTask` to the
/// operator-facing role, which the hotel routes to that role's inbox (and, when
/// that role has a live operator session, onward to Telegram/Beacon). The push
/// is **strictly best-effort** — every error, including a disconnect, is logged
/// and swallowed. A failed notification must never break the dispatch loop or a
/// filing; the next poll cycle's normal IPC traffic drives any reconnect.
struct OperatorNotifier {
    throttle: EscalationNotifier,
    node_id: String,
    role: String,
}

/// The operator ping the throttle decided to send: the human-readable
/// `content` plus the structured routing fields. Kept as a value (not pushed
/// inline) so the decision is sync and unit-testable without a live IPC client
/// — the caller conditionally hands it to [`OperatorNotifier::push`].
struct OperatorPing {
    pattern_tag: String,
    guest_id: String,
    content: String,
}

impl OperatorNotifier {
    fn from_env() -> Self {
        Self {
            throttle: EscalationNotifier::from_env(|k| std::env::var(k).ok()),
            node_id: local_node_id(),
            role: EscalationNotifier::escalation_role(|k| std::env::var(k).ok()),
        }
    }

    /// Fresh work-item filing (F4a): `Some(ping)` at most once per filing,
    /// throttled per tag; `None` when the cooldown suppresses it.
    fn on_filing(
        &mut self,
        pattern_tag: &str,
        guest_id: &str,
        work_item_id: &str,
        count: u32,
        window_secs: u64,
        now: u64,
    ) -> Option<OperatorPing> {
        if !self.throttle.should_notify_filing(pattern_tag, now) {
            return None;
        }
        Some(OperatorPing {
            pattern_tag: pattern_tag.to_string(),
            guest_id: guest_id.to_string(),
            content: format!(
                "[self-heal] filed work item {work_item_id}: pattern '{pattern_tag}' on guest \
                 '{guest_id}' recurred {count}x within {window_secs}s. Close with `phil heal \
                 close {work_item_id}` once repaired."
            ),
        })
    }

    /// Repeated single-occurrence escalate (F4b): `Some(ping)` only once the
    /// same tag has escalated past the repeat threshold, throttled per tag.
    fn on_escalate(&mut self, pattern_tag: &str, guest_id: &str, now: u64) -> Option<OperatorPing> {
        if !self.throttle.should_notify_escalate(pattern_tag, now) {
            return None;
        }
        Some(OperatorPing {
            pattern_tag: pattern_tag.to_string(),
            guest_id: guest_id.to_string(),
            content: format!(
                "[self-heal] recurring escalation: pattern '{pattern_tag}' on guest '{guest_id}' \
                 keeps classifying to escalate (operator attention required — no automatic \
                 remediation applies)."
            ),
        })
    }

    /// Best-effort EmitTask to the operator-facing role. Swallows ALL errors
    /// (including a disconnect — this is pure side output; the next cycle's
    /// normal IPC traffic drives any reconnect). Never returns an error, so it
    /// can never break the dispatch loop or a filing.
    async fn push(&self, ipc: &mut PhiloticClient, ping: OperatorPing) {
        // `content` is what the operator-facing role's cognitive loop consumes;
        // the structured fields keep the ping auditable.
        let task = serde_json::json!({
            "action": "heal_escalation",
            "source": "heal-dispatcher",
            "transport": "heal",
            "pattern_tag": ping.pattern_tag,
            "guest_id": ping.guest_id,
            "content": ping.content,
        });
        let task_json = match serde_json::to_string(&task) {
            Ok(json) => json,
            Err(e) => {
                warn!("heal escalation: task serialize failed (best-effort, ignoring): {e}");
                return;
            }
        };
        match send_request_timeout(
            ipc,
            IpcRequest::EmitTask {
                target_node: self.node_id.clone(),
                target_role: self.role.clone(),
                target_guest_id: None,
                task_json,
            },
            IPC_TIMEOUT,
        )
        .await
        {
            Ok(_) => {
                info!(
                    pattern_tag = %ping.pattern_tag,
                    guest_id = %ping.guest_id,
                    role = %self.role,
                    "heal escalation: operator notification emitted"
                );
            }
            Err(e) => {
                warn!(
                    pattern_tag = %ping.pattern_tag,
                    guest_id = %ping.guest_id,
                    "heal escalation: operator notification failed (best-effort, ignoring): {e}"
                );
            }
        }
    }
}

// ── Ollama circuit breaker (F5) ────────────────────────────────────────────────

/// Tracks consecutive Ollama classification failures and, once a threshold is
/// crossed, holds the breaker open for a cooldown window so we stop paying the
/// 30s HTTP timeout per row while Ollama is down. A single success resets it.
struct OllamaBreaker {
    consecutive_failures: u32,
    open_until: Option<Instant>,
}

impl OllamaBreaker {
    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            open_until: None,
        }
    }

    fn is_open(&self, now: Instant) -> bool {
        self.open_until.is_some_and(|until| now < until)
    }

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.open_until = None;
    }

    fn record_failure(&mut self, now: Instant) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= OLLAMA_BREAKER_THRESHOLD {
            self.open_until = Some(now + OLLAMA_BREAKER_COOLDOWN);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("heal-dispatcher starting");

    // Recurrence state is in-memory: a dispatcher restart resets every window
    // (acceptable — a recurring pattern re-accumulates within one window, and
    // the hotel dedups any double filing into a count bump). Owned by main so
    // IPC reconnects do NOT reset it.
    let mut tracker = RecurrenceTracker::from_env(|key| std::env::var(key).ok());
    // Circuit-breaker state is likewise owned by main so an IPC reconnect does
    // not forget that Ollama is currently down.
    let mut breaker = OllamaBreaker::new();
    // Operator-notification throttle (F4). Owned by main so per-tag cooldowns
    // survive IPC reconnects — a reconnect must not re-open the spam gate.
    let mut notifier = OperatorNotifier::from_env();
    let intel_graph_url = std::env::var("PHILOTIC_INTEL_GRAPH_URL")
        .ok()
        .map(|url| url.trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty());

    loop {
        match run(
            &mut tracker,
            &mut breaker,
            &mut notifier,
            intel_graph_url.as_deref(),
        )
        .await
        {
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

async fn run(
    tracker: &mut RecurrenceTracker,
    breaker: &mut OllamaBreaker,
    notifier: &mut OperatorNotifier,
    intel_graph_url: Option<&str>,
) -> Result<()> {
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
            breaker,
            notifier,
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

#[allow(clippy::too_many_arguments)]
async fn dispatch_cycle(
    ipc: &mut PhiloticClient,
    http: &reqwest::Client,
    ollama_url: &str,
    ollama_model: &str,
    tracker: &mut RecurrenceTracker,
    breaker: &mut OllamaBreaker,
    notifier: &mut OperatorNotifier,
    intel_graph_url: Option<&str>,
) -> Result<()> {
    // Proactively repair session turns stuck in "running" for more than 5 minutes.
    match send_request_timeout(
        ipc,
        IpcRequest::RepairStaleSessionTurns { min_age_secs: 300 },
        IPC_TIMEOUT,
    )
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
        Err(e) => {
            if is_ipc_disconnect(&e) {
                return Err(e);
            }
            warn!("heal-dispatcher: RepairStaleSessionTurns failed: {e}");
        }
    }

    let resp = send_request_timeout(
        ipc,
        IpcRequest::GetHealQueuePending { limit: BATCH_LIMIT },
        IPC_TIMEOUT,
    )
    .await?;
    let IpcResponse::HealQueuePending { rows } = resp else {
        return Ok(());
    };

    // A successful poll completed — stamp the liveness heartbeat before doing
    // any per-row work so even empty cycles advance it. A disconnect here
    // propagates to reconnect; any other failure is swallowed inside.
    record_heartbeat(ipc).await?;

    if rows.is_empty() {
        return Ok(());
    }

    info!(
        count = rows.len(),
        "heal-dispatcher: processing pending entries"
    );

    for row in rows {
        // A disconnect/timeout while processing a row breaks the cycle so the
        // whole dispatcher reconnects (rather than hammering a wedged socket
        // for every remaining row). At-least-once redelivery is acceptable: the
        // in-flight row's Resolve may not have landed, so it is reprocessed next
        // cycle — the hotel dedups filings and the heal actions are idempotent.
        process_row(
            ipc,
            http,
            ollama_url,
            ollama_model,
            tracker,
            breaker,
            notifier,
            intel_graph_url,
            &row,
        )
        .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn process_row(
    ipc: &mut PhiloticClient,
    http: &reqwest::Client,
    ollama_url: &str,
    ollama_model: &str,
    tracker: &mut RecurrenceTracker,
    breaker: &mut OllamaBreaker,
    notifier: &mut OperatorNotifier,
    intel_graph_url: Option<&str>,
    row: &HealQueueRow,
) -> Result<()> {
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
            None => {
                classify(
                    http,
                    ollama_url,
                    ollama_model,
                    breaker,
                    &row.guest_id,
                    &row.raw_text,
                )
                .await
            }
        };

    // Write triage back.
    if let Err(e) = send_request_timeout(
        ipc,
        IpcRequest::TriageHealEntry {
            id: row.id.clone(),
            severity: severity.clone(),
            pattern_tag: pattern_tag.clone(),
            heal_action: heal_action.clone(),
        },
        IPC_TIMEOUT,
    )
    .await
    {
        if is_ipc_disconnect(&e) {
            return Err(e);
        }
        warn!(id = %row.id, "triage write failed: {e}");
        return Ok(());
    }

    let now = unix_now();

    // Recurrence tracking (Autopoiesis Slice A3): a (pattern_tag, guest_id)
    // pair breaching the sliding-window threshold files a heal work item on
    // the hotel through the fleet.heal_slices autonomy lane.
    if !row.raw_text.starts_with(WORK_ITEM_FILED_PREFIX) {
        if let Some(breach) = tracker.record(&pattern_tag, &row.guest_id, now, &row.raw_text) {
            file_heal_work_item(
                ipc,
                http,
                notifier,
                intel_graph_url,
                &pattern_tag,
                &row.guest_id,
                breach,
                tracker.window_secs(),
                now,
            )
            .await?;
        }
    }

    // Execute the action and record outcome.
    let outcome = execute_action(ipc, &row.guest_id, &heal_action).await?;
    if let Err(e) = send_request_timeout(
        ipc,
        IpcRequest::ResolveHealEntry {
            id: row.id.clone(),
            outcome: outcome.clone(),
        },
        IPC_TIMEOUT,
    )
    .await
    {
        if is_ipc_disconnect(&e) {
            return Err(e);
        }
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

    // Repeated single-occurrence escalate (F4b): a pattern that keeps
    // classifying to `escalate` but never reaches the filing threshold still
    // needs operator eyes. Placed AFTER the resolve write so a best-effort
    // notification failure (swallowed inside) can't disturb row resolution.
    // Skip our own work_item_filed echo rows — they never carry the escalate
    // action, but guard anyway.
    if heal_action == "escalate" && !row.raw_text.starts_with(WORK_ITEM_FILED_PREFIX) {
        if let Some(ping) = notifier.on_escalate(&pattern_tag, &row.guest_id, now) {
            notifier.push(ipc, ping).await;
        }
    }

    Ok(())
}

// ── Work-item filing (Autopoiesis Slice A3) ───────────────────────────────────

/// File a heal work item on the hotel via `IpcRequest::FileHealWorkItem`.
///
/// The hotel owns the autonomy decision (kill switch, grant freeze, daily
/// budget) and the durable record (autonomy_audit + heal_work_item nodes in
/// the hotel context graph). This side only reports the breach — and, when
/// the hotel confirms a fresh filing, mirrors it to the intel graph
/// best-effort.
///
/// Returns `Err` only on a disconnect/timeout (so the cycle reconnects); any
/// other IPC or shape error is logged and swallowed.
#[allow(clippy::too_many_arguments)]
async fn file_heal_work_item(
    ipc: &mut PhiloticClient,
    http: &reqwest::Client,
    notifier: &mut OperatorNotifier,
    intel_graph_url: Option<&str>,
    pattern_tag: &str,
    guest_id: &str,
    breach: Breach,
    window_secs: u64,
    now: u64,
) -> Result<()> {
    let response = send_request_timeout(
        ipc,
        IpcRequest::FileHealWorkItem {
            pattern_tag: pattern_tag.to_string(),
            guest_id: guest_id.to_string(),
            occurrence_count: breach.count,
            window_secs,
            evidence_lines: breach.evidence,
        },
        IPC_TIMEOUT,
    )
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
            return Ok(());
        }
        Err(e) => {
            if is_ipc_disconnect(&e) {
                return Err(e);
            }
            warn!(
                pattern_tag,
                guest_id, "heal work item filing IPC error: {e}"
            );
            return Ok(());
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
        // Push ONE throttled operator notification for the fresh filing (F4a).
        // Best-effort: push swallows every error, so it can never fail the
        // filing or the cycle.
        if let Some(ping) = notifier.on_filing(
            pattern_tag,
            guest_id,
            &work_item_id,
            breach.count,
            window_secs,
            now,
        ) {
            notifier.push(ipc, ping).await;
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
    Ok(())
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
    // Memory Transparency Slice M1: mirror the same provenance the
    // hotel-side `autonomy_audit` record carries (see
    // `handle_file_heal_work_item` in aiua's ipc.rs) onto the intel-graph
    // decision record — evidence pointer is the work item id itself (the
    // durable hotel-graph record this decision references), reversal is
    // the same close-work-item path, trust is observed (the dispatcher
    // directly classified and filed this).
    let body = serde_json::json!({
        "target_node": "doc:AUTOPOIESIS_PROPOSAL",
        "action": "heal_work_item_filed",
        "to_value": work_item_id,
        "reason": format!(
            "recurring heal pattern '{pattern_tag}' on {guest_id} ({count}x in {window_secs}s) \
             filed as hotel-graph heal_work_item {work_item_id}"
        ),
        "agent": "heal-dispatcher",
        "evidence": [format!("heal_work_item:{work_item_id}"), format!("pattern:{pattern_tag}")],
        "reversal": "close_heal_work_item via GraphDomain::close_heal_work_item",
        "trust": "observed",
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
    breaker: &mut OllamaBreaker,
    guest_id: &str,
    raw_text: &str,
) -> (String, String, String) {
    // Rule-based fast path — always runs, no model dependency.
    if let Some(result) = rule_classify(raw_text) {
        return result;
    }

    // Circuit breaker (F5): if Ollama has been failing, skip the 30s HTTP
    // timeout entirely and return the fail-safe classification. This prevents a
    // single cycle full of novel lines from stalling for BATCH_LIMIT*30s.
    if breaker.is_open(Instant::now()) {
        debug!("heal-dispatcher: ollama circuit breaker open, short-circuiting classify to noop");
        return noop_classification();
    }

    // FunctionGemma: call Ollama for novel/unclassified patterns.
    match gemma_classify(http, ollama_url, ollama_model, guest_id, raw_text).await {
        // F7: an LLM-suggested restart_guest on a novel error is only trustworthy
        // when the model is also confident it is serious. Gate it — a restart on a
        // low/unknown-severity novel error is downgraded to escalate (human/work-item
        // path) rather than acted on. Rule-table restarts are NOT gated; only the
        // LLM branch passes through here.
        Ok((severity, pattern_tag, heal_action)) => {
            breaker.record_success();
            let heal_action = gate_llm_action(&severity, &heal_action);
            (severity, pattern_tag, heal_action)
        }
        Err(e) => {
            breaker.record_failure(Instant::now());
            warn!("gemma classify failed ({e}), falling back to noop");
            noop_classification()
        }
    }
}

fn noop_classification() -> (String, String, String) {
    ("unknown".into(), "unclassified".into(), "noop".into())
}

/// Severity floor for an LLM-suggested `restart_guest`. A restart is a
/// disruptive remediation; when the model itself rates the novel error as
/// `low`/`unknown` severity we do not trust it enough to bounce a process —
/// downgrade to `escalate` so a human/work-item decides. Any other action, or a
/// restart at `medium`+ severity, passes through unchanged.
fn gate_llm_action(severity: &str, heal_action: &str) -> String {
    if heal_action == "restart_guest" && matches!(severity, "low" | "unknown" | "") {
        return "escalate".into();
    }
    heal_action.to_string()
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

/// Returns `Err` only on a disconnect/timeout (so the cycle reconnects); every
/// True when `id` is a conversation session id rather than a materialized guest
/// id. Sessions are keyed by their transport/source (`telegram:<chat>:…`,
/// `operator-chat:…`, `paracrine:…`, etc.); materialized guests are keyed by
/// component (`<hotel>:model-controller-…`, `heal-dispatcher-01`, …). Used to
/// stop the heal loop from trying to `restart_guest` a session (always a no-op
/// that returns GUEST_NOT_FOUND).
fn is_session_like(id: &str) -> bool {
    const SESSION_PREFIXES: &[&str] = &[
        "telegram:",
        "operator-chat:",
        "paracrine:",
        "discord:",
        "slack:",
        "web:",
        "smoke",
        "diagnostic",
    ];
    SESSION_PREFIXES.iter().any(|p| id.starts_with(p))
}

/// other outcome — including non-disconnect IPC errors — resolves to an outcome
/// string that the caller records.
async fn execute_action(
    ipc: &mut PhiloticClient,
    guest_id: &str,
    heal_action: &str,
) -> Result<String> {
    match heal_action {
        "restart_guest" => {
            // A conversation SESSION is not a materialized guest — restarting it
            // is meaningless and always returns GUEST_NOT_FOUND. This happens when
            // the classifier flags a stalled/"zombie" session and prescribes
            // restart_guest with the session id as the target. Skip it cleanly so
            // it records as a no-op instead of spamming restart_failed + escalations.
            if is_session_like(guest_id) {
                warn!(
                    guest_id,
                    "heal-dispatcher: refusing restart_guest on a session id (not a materialized guest) — skipping"
                );
                return Ok("restart_skipped_not_a_guest".into());
            }
            info!(guest_id, "heal-dispatcher: requesting guest restart");
            // Emit a restart request to the hotel's materialization surface.
            // The hotel will reclaim and respawn the guest via GuestManager.
            match send_request_timeout(
                ipc,
                IpcRequest::RestartComponent {
                    guest_id: guest_id.to_string(),
                    // Automatic remediation — subject to the hotel's shared respawn
                    // budget so a crash-looping guest cannot be restarted every cycle.
                    reason: RestartReason::Heal,
                },
                IPC_TIMEOUT,
            )
            .await
            {
                Ok(IpcResponse::Standard { ok: true, .. }) => Ok("restarted".into()),
                // The hotel refused because this guest exhausted its respawn budget.
                // Record it distinctly so the loop does not read it as a failure to retry.
                Ok(IpcResponse::Standard { code, .. }) if code == "RESPAWN_BUDGET_EXHAUSTED" => {
                    warn!(guest_id, "restart skipped: respawn budget exhausted");
                    Ok("restart_skipped_budget_exhausted".into())
                }
                Ok(resp) => {
                    warn!(guest_id, ?resp, "restart response unexpected");
                    Ok("restart_failed".into())
                }
                Err(e) => {
                    if is_ipc_disconnect(&e) {
                        return Err(e);
                    }
                    warn!(guest_id, "restart request error: {e}");
                    Ok("restart_error".into())
                }
            }
        }
        "refresh_memory_config" => {
            info!(
                guest_id,
                "heal-dispatcher: triggering immediate MuninnDB probe"
            );
            match send_request_timeout(ipc, IpcRequest::RefreshMemoryConfig, IPC_TIMEOUT).await {
                Ok(IpcResponse::MuninnStatus {
                    available,
                    endpoint,
                }) => {
                    if available {
                        info!(guest_id, endpoint = %endpoint, "MuninnDB probe succeeded — memory restored");
                        Ok("memory_restored".into())
                    } else {
                        warn!(guest_id, endpoint = %endpoint, "MuninnDB still unreachable after probe");
                        Ok("still_unreachable".into())
                    }
                }
                Ok(resp) => {
                    warn!(
                        guest_id,
                        ?resp,
                        "refresh_memory_config got unexpected response"
                    );
                    Ok("probe_failed".into())
                }
                Err(e) => {
                    if is_ipc_disconnect(&e) {
                        return Err(e);
                    }
                    warn!(guest_id, "refresh_memory_config IPC error: {e}");
                    Ok("probe_error".into())
                }
            }
        }
        "escalate" => {
            warn!(
                guest_id,
                "heal-dispatcher: escalating — operator attention required"
            );
            Ok("escalated".into())
        }
        _ => Ok("noop".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HEARTBEAT_KEY, OLLAMA_BREAKER_COOLDOWN, OLLAMA_BREAKER_THRESHOLD, OllamaBreaker,
        OperatorNotifier, gate_llm_action, heartbeat_request, ipc_timeout_error, is_session_like,
        rule_classify, with_ipc_timeout,
    };
    use crate::notify::EscalationNotifier;
    use philotic_client::{IpcRequest, IpcResponse, is_ipc_disconnect};
    use std::time::{Duration, Instant};

    /// Build an OperatorNotifier with an explicit throttle for deterministic
    /// tests (no env, no wall clock).
    fn test_notifier(cooldown: u64, window: u64, repeat_threshold: u32) -> OperatorNotifier {
        OperatorNotifier {
            throttle: EscalationNotifier::new(cooldown, window, repeat_threshold),
            node_id: "test-node".into(),
            role: "orchestrator".into(),
        }
    }

    const TN: u64 = 1_750_000_000;

    // F4a: a fresh work-item filing yields exactly ONE operator ping; a repeat
    // filing of the same tag inside the cooldown yields None. This is the
    // decision the `push` glue guards on — a Some means push is invoked, a None
    // means it is not, so this proves "invoked once per filing, throttled on
    // repeat" without a live IPC client.
    #[test]
    fn filing_notification_fires_once_then_is_throttled() {
        let mut n = test_notifier(3600, 1800, 2);
        let first = n.on_filing("connection_refused", "membrane-01", "wi-1", 5, 1800, TN);
        let ping = first.expect("first filing must produce an operator ping");
        assert_eq!(ping.pattern_tag, "connection_refused");
        assert!(ping.content.contains("wi-1"), "content: {}", ping.content);
        assert!(
            ping.content.contains("phil heal close wi-1"),
            "filing ping tells the operator how to close it: {}",
            ping.content
        );

        // Repeat filing of the same tag inside the cooldown: no second ping.
        assert!(
            n.on_filing(
                "connection_refused",
                "membrane-01",
                "wi-2",
                6,
                1800,
                TN + 60
            )
            .is_none(),
            "a repeat filing inside the cooldown must be throttled"
        );
        // A different tag is unaffected by the first tag's cooldown.
        assert!(
            n.on_filing(
                "api_key_expired",
                "model-router-01",
                "wi-3",
                5,
                1800,
                TN + 61
            )
            .is_some(),
            "distinct tags must not throttle each other"
        );
    }

    // F4b: one escalate is noise; a repeated escalate of the same tag within the
    // window produces exactly ONE ping, and further escalates inside the
    // cooldown are throttled.
    #[test]
    fn repeated_escalate_notifies_once_and_throttles() {
        let mut n = test_notifier(3600, 1800, 2);
        assert!(
            n.on_escalate("auth_failure", "membrane-01", TN).is_none(),
            "a single escalate must not ping the operator"
        );
        let ping = n
            .on_escalate("auth_failure", "membrane-01", TN + 30)
            .expect("the repeated escalate must ping once");
        assert_eq!(ping.pattern_tag, "auth_failure");
        assert!(
            ping.content.contains("recurring escalation"),
            "content: {}",
            ping.content
        );
        // Further escalates inside the cooldown are suppressed — no spam.
        assert!(
            n.on_escalate("auth_failure", "membrane-01", TN + 90)
                .is_none()
        );
        assert!(
            n.on_escalate("auth_failure", "membrane-01", TN + 600)
                .is_none()
        );
    }

    // The filing and escalate paths share ONE per-tag cooldown, so a filing
    // right after a repeated escalate for the same tag does not double-notify.
    #[test]
    fn filing_and_escalate_share_the_cooldown() {
        let mut n = test_notifier(3600, 1800, 2);
        assert!(n.on_escalate("missing_file", "philote-01", TN).is_none());
        assert!(
            n.on_escalate("missing_file", "philote-01", TN + 10)
                .is_some()
        );
        assert!(
            n.on_filing("missing_file", "philote-01", "wi-9", 5, 1800, TN + 20)
                .is_none(),
            "a filing for a tag just escalated must not double-notify"
        );
    }

    // F7: an LLM-suggested restart_guest on a low/unknown-severity novel error is
    // not trustworthy enough to bounce a process — it is downgraded to escalate.
    // Higher severities and non-restart actions pass through unchanged.
    #[test]
    fn gate_llm_action_downgrades_low_confidence_restart() {
        assert_eq!(gate_llm_action("unknown", "restart_guest"), "escalate");
        assert_eq!(gate_llm_action("low", "restart_guest"), "escalate");
        assert_eq!(gate_llm_action("", "restart_guest"), "escalate");
        // Trusted severities keep the restart.
        assert_eq!(
            gate_llm_action("critical", "restart_guest"),
            "restart_guest"
        );
        assert_eq!(gate_llm_action("high", "restart_guest"), "restart_guest");
        assert_eq!(gate_llm_action("medium", "restart_guest"), "restart_guest");
        // Non-restart actions are never touched.
        assert_eq!(gate_llm_action("unknown", "escalate"), "escalate");
        assert_eq!(gate_llm_action("low", "noop"), "noop");
    }

    #[test]
    fn session_ids_are_not_restartable_guests() {
        // Session ids (what the "zombie" misfire targets) must be recognized.
        assert!(is_session_like("telegram:7898847424:agent-jane"));
        assert!(is_session_like(
            "telegram:7898847424:1783786667254:agent-jane"
        ));
        assert!(is_session_like("operator-chat:smoke-agent-turn-abc:agent-jane"));
        assert!(is_session_like("paracrine:7898847424:orchestrator"));
        // Real materialized guests must NOT be treated as sessions.
        assert!(!is_session_like("mbp-jane:model-controller-openrouter"));
        assert!(!is_session_like("model-controller-openrouter-01"));
        assert!(!is_session_like("heal-dispatcher-01"));
    }

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

    // ── F3: IPC timeout guard ─────────────────────────────────────────────────

    /// The timeout error MUST be recognised by `is_ipc_disconnect` so the
    /// existing reconnect path fires. A plain `anyhow!` message would not
    /// downcast to an io error and would silently defeat reconnection.
    #[test]
    fn ipc_timeout_error_maps_to_reconnect() {
        assert!(
            is_ipc_disconnect(&ipc_timeout_error()),
            "a timed-out request must be treated as an IPC disconnect"
        );
    }

    /// A hung responder (injected here as a future that never resolves in time)
    /// must trip the timeout and yield the disconnect-class error that drives
    /// the reconnect path — not hang forever.
    #[tokio::test]
    async fn hung_reply_times_out_and_triggers_reconnect() {
        let hung = async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(IpcResponse::success("never", None))
        };
        let err = with_ipc_timeout(Duration::from_millis(50), hung)
            .await
            .expect_err("hung reply must time out");
        assert!(
            is_ipc_disconnect(&err),
            "timeout must map to the reconnect path"
        );
    }

    // ── F3: heartbeat ─────────────────────────────────────────────────────────

    /// The heartbeat writes the EXACT key the phil doctor check reads, and the
    /// value is valid JSON carrying the unix seconds.
    #[test]
    fn heartbeat_request_writes_exact_key() {
        assert_eq!(HEARTBEAT_KEY, "heal_dispatcher.last_cycle_at");
        match heartbeat_request(1_720_000_000) {
            IpcRequest::SetConfig { key, value_json } => {
                assert_eq!(key, "heal_dispatcher.last_cycle_at");
                let v: serde_json::Value =
                    serde_json::from_str(&value_json).expect("heartbeat value must be valid JSON");
                assert_eq!(v.as_u64(), Some(1_720_000_000));
            }
            other => panic!("expected SetConfig, got {other:?}"),
        }
    }

    // ── F5: Ollama circuit breaker ────────────────────────────────────────────

    /// The breaker opens only after N consecutive failures, short-circuits for
    /// the cooldown window, then closes again once the window elapses; a single
    /// success resets the failure count and closes it early.
    #[test]
    fn ollama_breaker_opens_after_threshold_short_circuits_then_resets() {
        let t0 = Instant::now();
        let mut breaker = OllamaBreaker::new();

        // Below threshold: stays closed.
        for _ in 0..(OLLAMA_BREAKER_THRESHOLD - 1) {
            breaker.record_failure(t0);
            assert!(!breaker.is_open(t0), "must stay closed below threshold");
        }

        // Nth failure opens it.
        breaker.record_failure(t0);
        assert!(breaker.is_open(t0), "must open at the threshold");

        // Short-circuits throughout the cooldown window…
        assert!(breaker.is_open(t0 + OLLAMA_BREAKER_COOLDOWN - Duration::from_secs(1)));
        // …and closes again once the window has fully elapsed.
        assert!(!breaker.is_open(t0 + OLLAMA_BREAKER_COOLDOWN + Duration::from_secs(1)));

        // A fresh run of failures re-opens it; a success then resets and closes.
        breaker.record_failure(t0);
        breaker.record_failure(t0);
        breaker.record_failure(t0);
        assert!(breaker.is_open(t0));
        breaker.record_success();
        assert!(!breaker.is_open(t0), "a success must reset the breaker");
        assert_eq!(breaker.consecutive_failures, 0);
    }
}
