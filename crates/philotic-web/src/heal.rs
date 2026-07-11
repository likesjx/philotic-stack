//! `phil heal` — operator surface for the self-heal circuit's work items
//! (Autopoiesis Slice A3). Talks IPC to the local hotel (not the DB directly)
//! so a `close` can never race the daemon's open SQLite handle.

use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};

use crate::start::socket_path;

#[derive(Subcommand, Debug)]
pub enum HealAction {
    /// List heal work items filed by the self-heal circuit.
    List {
        /// Include closed items as well as open ones.
        #[arg(long)]
        all: bool,
    },

    /// Close a filed heal work item once its underlying fault is repaired.
    ///
    /// Idempotent: closing an already-closed item still reports closed=true;
    /// an unknown id reports closed=false.
    Close {
        /// The `work_item_id` printed by `phil heal list`.
        work_item_id: String,
    },
}

pub async fn run(action: HealAction) -> Result<()> {
    match action {
        HealAction::List { all } => list(all).await,
        HealAction::Close { work_item_id } => close(work_item_id).await,
    }
}

async fn ipc_client() -> Result<PhiloticClient> {
    let socket = socket_path("aiua");
    let identity = GuestIdentity {
        guest_id: "phil-heal".into(),
        // "management" is the read/ops role other `phil` IPC verbs use.
        role: "management".into(),
        supported_tools: vec![],
    };
    PhiloticClient::connect_at(&socket, identity)
        .await
        .with_context(|| format!("connect to aiua at {socket}"))
}

async fn close(work_item_id: String) -> Result<()> {
    if work_item_id.trim().is_empty() {
        anyhow::bail!("work_item_id cannot be empty");
    }
    let mut client = ipc_client().await?;
    match client
        .send_request(IpcRequest::CloseHealWorkItem {
            work_item_id: work_item_id.clone(),
        })
        .await?
    {
        IpcResponse::Standard { ok: true, data, .. } => {
            let closed = data
                .as_ref()
                .and_then(|d| d.get("closed"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if closed {
                println!("closed heal work item {work_item_id}");
            } else {
                println!("no heal work item with id {work_item_id} (nothing to close)");
            }
            Ok(())
        }
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected close response: {other:?}")),
    }
}

async fn list(all: bool) -> Result<()> {
    let mut client = ipc_client().await?;
    // Reuse the existing management read surface: the graph exposes heal work
    // items through GetConfig("__heal_work_items__"). If that key is not served
    // by this hotel build, fall back to a clear message rather than erroring.
    match client
        .send_request(IpcRequest::GetConfig {
            key: "__heal_work_items__".into(),
        })
        .await?
    {
        IpcResponse::ConfigData { value_json, .. } => {
            let Some(json) = value_json else {
                println!("no heal work items");
                return Ok(());
            };
            let items: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap_or_default();
            let mut shown = 0usize;
            for item in &items {
                let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("");
                if !all && status != "open" {
                    continue;
                }
                shown += 1;
                println!(
                    "{}  [{}]  pattern={} guest={} count={}",
                    item.get("work_item_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?"),
                    status,
                    item.get("pattern_tag")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?"),
                    item.get("guest_id").and_then(|v| v.as_str()).unwrap_or("?"),
                    item.get("count").and_then(|v| v.as_u64()).unwrap_or(0),
                );
            }
            if shown == 0 {
                println!(
                    "no {} heal work items",
                    if all { "" } else { "open" }.trim()
                );
            }
            Ok(())
        }
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected list response: {other:?}")),
    }
}
