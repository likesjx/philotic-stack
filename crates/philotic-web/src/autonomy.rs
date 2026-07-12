//! `phil autonomy status` — the Autopoiesis Slice A9 trust-ledger report:
//! per-lane posture, budget/failure/streak counters, and promotion
//! eligibility read straight off A1's earn/demote rules. Talks IPC to the
//! local hotel via `GetConfig("__autonomy_status__"[:{lane}])` (mirrors the
//! `phil heal` precedent) so status always reflects the daemon's live
//! GraphDomain rather than a second copy of the rules re-derived from a raw
//! DB read.

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
}

pub async fn run(action: AutonomyAction) -> Result<()> {
    match action {
        AutonomyAction::Status { lane } => status(lane).await,
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
