//! `phil config` — thin operator surface over the hotel's `node_config`
//! key/value store (`GraphDomain::get_config_value` / `set_config_value`,
//! reached via the existing `GetConfig` / `SetConfig` IPC verbs — see
//! `heal.rs` / `mesh.rs` for the same connect-and-call shape).
//!
//! Not chaos-specific: this is the general read/write surface for
//! `node_config`. Substrate Hardening Slice S4 (`chaos-smoke.sh`) uses it to
//! write and restore a dedicated sacrificial canary key
//! (`chaos_smoke.canary_value`) without touching any real config, but any
//! caller with `management`-role IPC access can use it for any key.
//!
//! `value_json` is stored and returned as a raw JSON string (matching the
//! wire type of `GetConfig`/`SetConfig`) — callers are responsible for
//! quoting plain strings themselves, e.g. `phil config set foo '"bar"'`.

use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};

use crate::start::socket_path;

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Read a `node_config` value. Prints the raw JSON value, or "null" if unset.
    Get {
        /// Config key to read.
        key: String,

        /// Hotel name to target (default: "default").
        #[arg(long, default_value = "default")]
        hotel: String,
    },

    /// Write a `node_config` value (operator/management only).
    ///
    /// `value_json` must be valid JSON, e.g. `phil config set my_key '"a string"'`
    /// or `phil config set my_key '{"nested":true}'`.
    Set {
        /// Config key to write.
        key: String,

        /// Raw JSON value to store.
        value_json: String,

        /// Hotel name to target (default: "default").
        #[arg(long, default_value = "default")]
        hotel: String,
    },
}

pub async fn run(action: ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Get { key, hotel } => get(&hotel, &key).await,
        ConfigAction::Set {
            key,
            value_json,
            hotel,
        } => set(&hotel, &key, &value_json).await,
    }
}

async fn ipc_client(hotel_name: &str) -> Result<PhiloticClient> {
    let identity = GuestIdentity {
        guest_id: "phil-config".into(),
        // "management" is the read/ops role other `phil` IPC verbs use
        // (see heal.rs, mesh.rs) — GetConfig/SetConfig are documented as
        // operator/management-only in philotic-client's IpcRequest.
        role: "management".into(),
        supported_tools: vec![],
    };
    let socket = socket_path(hotel_name);
    PhiloticClient::connect_at(&socket, identity)
        .await
        .with_context(|| format!("failed to connect to hotel IPC at {socket}"))
}

async fn get(hotel: &str, key: &str) -> Result<()> {
    if key.trim().is_empty() {
        anyhow::bail!("key cannot be empty");
    }
    let mut client = ipc_client(hotel).await?;
    match client
        .send_request(IpcRequest::GetConfig { key: key.into() })
        .await
        .context("GetConfig IPC request failed")?
    {
        IpcResponse::ConfigData { value_json, .. } => {
            println!("{}", value_json.unwrap_or_else(|| "null".to_string()));
            Ok(())
        }
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected GetConfig response: {other:?}")),
    }
}

async fn set(hotel: &str, key: &str, value_json: &str) -> Result<()> {
    if key.trim().is_empty() {
        anyhow::bail!("key cannot be empty");
    }
    // Fail fast on invalid JSON rather than silently storing a malformed
    // value_json string the graph will happily persist but nothing can parse.
    serde_json::from_str::<serde_json::Value>(value_json)
        .with_context(|| format!("value_json is not valid JSON: {value_json}"))?;

    let mut client = ipc_client(hotel).await?;
    match client
        .send_request(IpcRequest::SetConfig {
            key: key.into(),
            value_json: value_json.into(),
        })
        .await
        .context("SetConfig IPC request failed")?
    {
        IpcResponse::Standard { ok: true, .. } => {
            println!("set {key}");
            Ok(())
        }
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected SetConfig response: {other:?}")),
    }
}
