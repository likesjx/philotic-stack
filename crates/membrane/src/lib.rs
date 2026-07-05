//! `membrane` — base primitives for protocol gateway guests.
//!
//! Defines [`MembraneGuest`], the protocol-agnostic primitive types, and
//! [`MembraneRuntime`] which handles the full IPC lifecycle (registration,
//! lease, reconnect loop, inbound/outbound dispatch).
//!
//! # External use
//!
//! External crates can import `membrane` and implement [`MembraneGuest`] to
//! build a custom protocol gateway that plugs into any compatible Philotic mesh.

pub mod envelope;
pub mod lease;
pub mod runtime;

pub use envelope::{InboundEnvelope, OutboundReply, SenderInfo};
pub use lease::{
    LeaseAcquireOutcome, LeaseBackend, LeaseDriver, LeaseDriverConfig, LeaseEvent, LeaseRenewResult,
};
pub use runtime::{MembraneContext, MembraneRuntime};

use anyhow::Result;
use async_trait::async_trait;
use philotic_client::{IpcResponse, PhiloticClient};

// ── Core trait ────────────────────────────────────────────────────────────────

/// Implemented by every protocol gateway guest (Telegram, Discord, MCP, …).
///
/// The [`MembraneRuntime`] handles IPC registration, the lease lifecycle, the
/// reconnect loop, and inbound/outbound message dispatch. Implementors only
/// provide the protocol-specific behaviour.
#[async_trait]
pub trait MembraneGuest: Send + Sync + 'static {
    /// IPC guest role name. Must be unique within the hotel
    /// (e.g. `"telegram-membrane"`, `"mcp-membrane"`).
    fn role(&self) -> &'static str;

    /// Stable lease key for acquire / renew / release.
    /// Typically `"<protocol>:<node-id>"`.
    fn lease_key(&self) -> String;

    /// Protocol-specific setup: acquire the lease, start the external listener
    /// (polling loop, HTTP server, WebSocket gateway, etc.).
    ///
    /// Called once after IPC registration. On IPC reconnect the runtime calls
    /// this again so the variant re-acquires its lease and re-starts its listener.
    async fn setup(&mut self, client: &mut PhiloticClient) -> Result<()>;

    /// Deliver an outbound reply from the hotel to the external caller.
    ///
    /// Called by the runtime whenever the hotel pushes a reply for an active
    /// session. The variant formats and sends it over its protocol.
    async fn deliver(&mut self, reply: OutboundReply) -> Result<()>;

    /// Renew the lease. Called on the renew interval by the runtime.
    ///
    /// The variant sends the appropriate `Renew*Lease` IPC request and
    /// returns the renewal outcome so the runtime can react to
    /// [`LeaseRenewResult::NeedsReacquire`] or [`LeaseRenewResult::Lost`].
    async fn renew(&mut self, client: &mut PhiloticClient) -> Result<LeaseRenewResult>;

    /// Release the lease and perform any protocol-level teardown.
    /// Called by the runtime on clean shutdown (SIGTERM / SIGINT).
    async fn teardown(&mut self, client: &mut PhiloticClient);

    /// Handle a raw push message from the hotel before the default
    /// [`OutboundReply`] extraction path runs.
    ///
    /// Return `Ok(true)` if the message was fully handled (runtime skips
    /// `extract_outbound_reply` + `deliver`). Return `Ok(false)` to fall
    /// through to the default path.
    ///
    /// Use this for variant-specific control messages such as route table
    /// updates pushed by the hotel (e.g. `action=update_mcp_routes`).
    async fn handle_push(&mut self, msg: &IpcResponse) -> Result<bool> {
        let _ = msg;
        Ok(false)
    }
}
