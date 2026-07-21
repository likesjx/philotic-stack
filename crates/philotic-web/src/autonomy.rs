//! `phil autonomy` — the Autopoiesis Slice A9 trust-ledger operator surface,
//! extended by the outcome-stamping follow-up slice with `pending`/`stamp`.
//!
//! - `status`: per-lane posture, budget/failure/streak counters, and
//!   promotion eligibility read straight off A1's earn/demote rules. Talks
//!   IPC via `GetConfig("__autonomy_status__"[:{lane}])`.
//! - `pending`: every `autonomy_audit` record across all lanes still awaiting
//!   an operator outcome — the raw review backlog `status`'s counters are
//!   waiting on. Talks IPC via `GetConfig("__autonomy_pending__")`.
//! - `stamp`: report an outcome for one audit record, driving the same
//!   `RecordAutonomyOutcome` IPC path the timeout-to-Neutral sweep uses.
//!
//! All three mirror the `phil heal` precedent so status/pending/stamp always
//! reflect the daemon's live GraphDomain rather than a second copy of the
//! rules re-derived from a raw DB read.

use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};

use crate::start::socket_path;

#[derive(Subcommand, Debug)]
pub enum AutonomyAction {
    /// Per-lane trust-ledger report: posture, actions-today vs daily budget,
    /// consecutive failures vs the freeze ceiling, confirmed-good streak vs
    /// what's required to promote, and promotion eligibility.
    Status {
        /// Show only this lane (default: every lane with a grant).
        #[arg(long)]
        lane: Option<String>,
    },

    /// List every audit record across all lanes still awaiting an operator
    /// outcome (`Pending`), oldest first — the backlog the daily
    /// timeout-to-Neutral sweep will eventually catch up to on its own.
    Pending,

    /// Report the reviewed outcome of one audited autonomous action. Feeds
    /// the same `RecordAutonomyOutcome` IPC path the outcome-stamping sweep
    /// uses. Idempotent: stamping an already-reviewed audit id is refused
    /// (reported, not an error) rather than double-counting toward
    /// promotion.
    Stamp {
        /// The `audit_id` printed by `phil autonomy pending`.
        audit_id: String,
        /// One of `confirmed_good` | `reversed` | `neutral`.
        outcome: String,
    },
}

pub async fn run(action: AutonomyAction) -> Result<()> {
    match action {
        AutonomyAction::Status { lane } => status(lane).await,
        AutonomyAction::Pending => pending().await,
        AutonomyAction::Stamp { audit_id, outcome } => stamp(audit_id, outcome).await,
    }
}

async fn ipc_client() -> Result<PhiloticClient> {
    let socket = socket_path("aiua");
    let identity = GuestIdentity {
        guest_id: "phil-autonomy".into(),
        // "management" is the read/ops role other `phil` IPC verbs use.
        role: "management".into(),
        supported_tools: vec![],
    };
    PhiloticClient::connect_at(&socket, identity)
        .await
        .with_context(|| format!("connect to aiua at {socket}"))
}

async fn status(lane: Option<String>) -> Result<()> {
    let mut client = ipc_client().await?;
    let key = match &lane {
        Some(l) => format!("__autonomy_status__:{l}"),
        None => "__autonomy_status__".to_string(),
    };
    match client.send_request(IpcRequest::GetConfig { key }).await? {
        IpcResponse::ConfigData { value_json, .. } => {
            let Some(json) = value_json else {
                println!("no autonomy status available (unexpected empty response)");
                return Ok(());
            };
            match &lane {
                Some(l) => {
                    let report: Option<serde_json::Value> =
                        serde_json::from_str(&json).unwrap_or(None);
                    match report {
                        Some(r) => print_report(&r),
                        None => println!(
                            "no autonomy grant for lane '{l}' yet (never consulted at this posture)"
                        ),
                    }
                }
                None => {
                    let reports: Vec<serde_json::Value> =
                        serde_json::from_str(&json).unwrap_or_default();
                    if reports.is_empty() {
                        println!("no autonomy grants yet");
                    } else {
                        for r in &reports {
                            print_report(r);
                        }
                    }
                }
            }
            Ok(())
        }
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected status response: {other:?}")),
    }
}

fn print_report(r: &serde_json::Value) {
    let lane = r.get("lane").and_then(|v| v.as_str()).unwrap_or("?");
    let posture = r.get("posture").and_then(|v| v.as_str()).unwrap_or("?");
    let frozen = r
        .get("frozen_until_operator_review")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let actions_today = r.get("actions_today").and_then(|v| v.as_u64()).unwrap_or(0);
    let max_actions = r
        .get("max_actions_per_day")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let failures = r
        .get("consecutive_failures")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let max_failures = r
        .get("max_consecutive_failures")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let streak = r
        .get("confirmed_good_streak")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let required = r
        .get("required_for_promotion")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let eligible = r
        .get("promotion_eligible")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    println!(
        "{lane}  posture={posture}{frozen_note}  actions_today={actions_today}/{max_actions}  \
         consecutive_failures={failures}/{max_failures}  confirmed_good_streak={streak}/{required}  \
         promotion_eligible={eligible}",
        frozen_note = if frozen { " [FROZEN: awaiting operator review]" } else { "" },
    );
}

async fn pending() -> Result<()> {
    let mut client = ipc_client().await?;
    match client
        .send_request(IpcRequest::GetConfig {
            key: "__autonomy_pending__".into(),
        })
        .await?
    {
        IpcResponse::ConfigData { value_json, .. } => {
            let Some(json) = value_json else {
                println!("no pending autonomy outcomes (unexpected empty response)");
                return Ok(());
            };
            let items: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap_or_default();
            if items.is_empty() {
                println!("no autonomy outcomes pending review");
                return Ok(());
            }
            for item in &items {
                let audit_id = item.get("audit_id").and_then(|v| v.as_str()).unwrap_or("?");
                let lane = item.get("lane").and_then(|v| v.as_str()).unwrap_or("?");
                let summary = item
                    .get("action_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let age_secs = item.get("age_secs").and_then(|v| v.as_u64()).unwrap_or(0);
                println!(
                    "{audit_id}  lane={lane}  age={age}  {summary}",
                    age = format_age(age_secs),
                );
            }
            Ok(())
        }
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected pending response: {other:?}")),
    }
}

fn format_age(age_secs: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    if age_secs >= DAY {
        format!("{}d", age_secs / DAY)
    } else if age_secs >= HOUR {
        format!("{}h", age_secs / HOUR)
    } else if age_secs >= MINUTE {
        format!("{}m", age_secs / MINUTE)
    } else {
        format!("{age_secs}s")
    }
}

async fn stamp(audit_id: String, outcome: String) -> Result<()> {
    if audit_id.trim().is_empty() {
        anyhow::bail!("audit_id cannot be empty");
    }
    let normalized = outcome.trim().to_ascii_lowercase();
    if !matches!(
        normalized.as_str(),
        "confirmed_good" | "reversed" | "neutral"
    ) {
        anyhow::bail!("unknown outcome '{outcome}' (expected confirmed_good | reversed | neutral)");
    }

    let mut client = ipc_client().await?;
    match client
        .send_request(IpcRequest::RecordAutonomyOutcome {
            audit_id: audit_id.clone(),
            outcome: normalized,
        })
        .await?
    {
        IpcResponse::Standard { ok: true, data, .. } => {
            let data = data.unwrap_or_default();
            let recorded = data
                .get("recorded")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !recorded {
                let reason = data
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                println!("audit {audit_id} not recorded: {reason}");
                return Ok(());
            }
            let lane = data.get("lane").and_then(|v| v.as_str()).unwrap_or("?");
            let transition = data
                .get("transition")
                .and_then(|v| v.as_str())
                .unwrap_or("no_change");
            let posture = data.get("posture").and_then(|v| v.as_str());
            println!(
                "stamped {audit_id}  lane={lane}  transition={transition}{posture_note}",
                posture_note = posture
                    .map(|p| format!("  posture={p}"))
                    .unwrap_or_default(),
            );
            Ok(())
        }
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected stamp response: {other:?}")),
    }
}
