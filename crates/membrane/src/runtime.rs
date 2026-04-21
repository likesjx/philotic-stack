//! [`MembraneRuntime`] — generic IPC lifecycle driver for membrane guests.
//!
//! Handles: IPC registration, guest setup, inbound dispatch, outbound delivery,
//! lease renewal, IPC reconnect with backoff, and clean shutdown.

use anyhow::Result;
use philotic_client::{GuestIdentity, IpcResponse, PhiloticClient};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::MembraneGuest;
use crate::envelope::{InboundEnvelope, OutboundReply};
use crate::lease::LeaseRenewResult;

const RECONNECT_BASE_MS: u64 = 500;
const RECONNECT_MAX_MS: u64 = 30_000;
const LEASE_RENEW_INTERVAL: Duration = Duration::from_secs(20);

// ── Context passed to guest.start() ──────────────────────────────────────────

/// Handle given to a [`MembraneGuest`] during `setup()`.
///
/// The guest uses `inbound_tx` to forward protocol events into the runtime's
/// IPC dispatch loop, and `shutdown_rx` to detect clean shutdown requests.
pub struct MembraneContext {
    /// Send inbound envelopes here; the runtime dispatches them to the hotel via IPC.
    pub inbound_tx: mpsc::Sender<InboundEnvelope>,
    /// Fires when the runtime wants the guest to stop its listener.
    pub shutdown_rx: tokio::sync::watch::Receiver<bool>,
}

// ── Runtime ───────────────────────────────────────────────────────────────────

/// Drives the full IPC lifecycle for any [`MembraneGuest`] implementation.
pub struct MembraneRuntime {
    pub guest_id: String,
    pub node_id: String,
    pub ipc_socket: String,
    /// External inbound channel receiver. When provided, the runtime reads
    /// from this channel instead of creating its own. The caller retains
    /// the sender and feeds it directly (e.g. from HTTP handlers).
    inbound_rx: Option<mpsc::Receiver<InboundEnvelope>>,
}

impl MembraneRuntime {
    pub fn new(
        ipc_socket: impl Into<String>,
        guest_id: impl Into<String>,
        node_id: impl Into<String>,
    ) -> Self {
        Self {
            ipc_socket: ipc_socket.into(),
            guest_id: guest_id.into(),
            node_id: node_id.into(),
            inbound_rx: None,
        }
    }

    /// Provide an external inbound channel receiver. The caller creates the
    /// channel, keeps the sender (e.g. in `MembraneState`), and hands the
    /// receiver to the runtime here. Useful for HTTP-facing membrane variants
    /// where request handlers send envelopes instead of a listener task.
    pub fn with_inbound_rx(mut self, rx: mpsc::Receiver<InboundEnvelope>) -> Self {
        self.inbound_rx = Some(rx);
        self
    }

    /// Run the membrane runtime loop until shutdown.
    ///
    /// Internally: registers with hotel, calls `guest.setup()`, dispatches
    /// inbound/outbound, renews lease, and reconnects on IPC disconnect.
    pub async fn run<G: MembraneGuest>(mut self, mut guest: G) -> Result<()> {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // If an external channel was wired (e.g. membrane-mcp HTTP handlers),
        // use it; otherwise create an internal one for listener-based variants.
        let (default_tx, default_rx) = mpsc::channel::<InboundEnvelope>(64);
        let (inbound_tx, mut inbound_rx) = if let Some(rx) = self.inbound_rx.take() {
            (default_tx, rx) // external rx; default_tx passed to ctx but unused
        } else {
            (default_tx, default_rx)
        };

        // Signal handler: clean shutdown on SIGTERM/SIGINT.
        let shutdown_tx_sig = shutdown_tx.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            let _ = shutdown_tx_sig.send(true);
        });

        let mut reconnect_delay_ms = RECONNECT_BASE_MS;

        loop {
            if *shutdown_rx.borrow() {
                info!("membrane-runtime: shutdown signal received");
                break;
            }

            // Connect + register.
            let identity = GuestIdentity {
                guest_id: self.guest_id.clone(),
                role: guest.role().to_string(),
                supported_tools: vec![],
            };

            let mut client = match PhiloticClient::connect_at(&self.ipc_socket, identity).await {
                Ok(c) => {
                    info!(guest_id = %self.guest_id, "IPC connected");
                    reconnect_delay_ms = RECONNECT_BASE_MS;
                    c
                }
                Err(e) => {
                    warn!(err = %e, delay_ms = reconnect_delay_ms, "IPC connect failed, retrying");
                    tokio::time::sleep(Duration::from_millis(reconnect_delay_ms)).await;
                    reconnect_delay_ms = (reconnect_delay_ms * 2).min(RECONNECT_MAX_MS);
                    continue;
                }
            };

            // Guest setup: acquires lease, starts protocol listener.
            let ctx = MembraneContext {
                inbound_tx: inbound_tx.clone(),
                shutdown_rx: shutdown_rx.clone(),
            };
            if let Err(e) = guest.setup(&mut client).await {
                warn!(err = %e, "guest setup failed, reconnecting");
                tokio::time::sleep(Duration::from_millis(reconnect_delay_ms)).await;
                reconnect_delay_ms = (reconnect_delay_ms * 2).min(RECONNECT_MAX_MS);
                continue;
            }
            drop(ctx); // context is now held by the guest's spawned tasks

            // Main loop: inbound dispatch, outbound delivery, lease renewal.
            let mut renew_tick =
                tokio::time::interval(LEASE_RENEW_INTERVAL);
            renew_tick.tick().await; // consume immediate first tick

            let disconnect = loop {
                let mut shutdown_watch = shutdown_rx.clone();
                tokio::select! {
                    // Shutdown signal.
                    _ = shutdown_watch.changed() => {
                        if *shutdown_rx.borrow() {
                            break false; // clean exit
                        }
                    }

                    // Inbound from guest → forward to hotel.
                    Some(envelope) = inbound_rx.recv() => {
                        if let Err(e) = dispatch_inbound(&mut client, envelope).await {
                            if philotic_client::is_ipc_disconnect(&e) {
                                warn!("IPC disconnect while dispatching inbound");
                                break true;
                            }
                            error!(err = %e, "inbound dispatch error");
                        }
                    }

                    // Push from hotel → deliver to guest.
                    push = client.recv_task() => {
                        match push {
                            Ok(msg) => {
                                match guest.handle_push(&msg).await {
                                    Ok(true) => {} // handled by variant
                                    Ok(false) => {
                                        if let Some(reply) = extract_outbound_reply(&msg) {
                                            if let Err(e) = guest.deliver(reply).await {
                                                error!(err = %e, "deliver error");
                                            }
                                        }
                                    }
                                    Err(e) => error!(err = %e, "handle_push error"),
                                }
                            }
                            Err(e) => {
                                if philotic_client::is_ipc_disconnect(&e) {
                                    warn!("IPC disconnect while receiving push");
                                    break true;
                                }
                                error!(err = %e, "recv_task error");
                            }
                        }
                    }

                    // Lease renewal tick.
                    _ = renew_tick.tick() => {
                        match guest.renew(&mut client).await {
                            Ok(LeaseRenewResult::Ok { .. }) => {}
                            Ok(LeaseRenewResult::NeedsReacquire) => {
                                warn!("lease needs reacquire, reconnecting");
                                break true;
                            }
                            Ok(LeaseRenewResult::Lost { owner }) => {
                                warn!(?owner, "lease lost to another holder, reconnecting");
                                break true;
                            }
                            Err(e) => {
                                if philotic_client::is_ipc_disconnect(&e) {
                                    break true;
                                }
                                warn!(err = %e, "lease renew error");
                            }
                        }
                    }
                }
            };

            if !disconnect {
                // Clean shutdown path.
                guest.teardown(&mut client).await;
                break;
            }

            // Reconnect path.
            warn!("IPC disconnected, reconnecting in {}ms", reconnect_delay_ms);
            tokio::time::sleep(Duration::from_millis(reconnect_delay_ms)).await;
            reconnect_delay_ms = (reconnect_delay_ms * 2).min(RECONNECT_MAX_MS);
        }

        info!("membrane-runtime: exited");
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Forward an inbound envelope to the hotel as a `CreateTask`.
async fn dispatch_inbound(
    client: &mut PhiloticClient,
    envelope: InboundEnvelope,
) -> Result<()> {
    use philotic_client::IpcRequest;

    let payload = serde_json::to_value(&envelope)?;
    let req = IpcRequest::CreateTask {
        target_role: "philote".into(),
        payload,
    };
    client.send_request(req).await?;
    Ok(())
}

/// Extract an [`OutboundReply`] from a hotel push message if it contains
/// a membrane-directed reply action.
fn extract_outbound_reply(msg: &IpcResponse) -> Option<OutboundReply> {
    use crate::envelope::OutboundReply;

    let task_json = match msg {
        IpcResponse::InboundTask { task_json, .. } => task_json,
        _ => return None,
    };

    let payload: serde_json::Value = serde_json::from_str(task_json).ok()?;

    let action = payload.get("action").and_then(|v: &serde_json::Value| v.as_str())?;

    match action {
        "send_reply" | "text_reply" => {
            let session_id = payload.get("session_id")?.as_str()?.to_string();
            let turn_id = payload.get("turn_id")?.as_str()?.to_string();
            let content = payload.get("content")?.as_str()?.to_string();
            let reply_to = payload.get("reply_to")
                .and_then(|v: &serde_json::Value| v.as_str())
                .map(str::to_string);
            Some(OutboundReply::Text { session_id, turn_id, content, reply_to })
        }
        "streaming_token" => {
            let session_id = payload.get("session_id")?.as_str()?.to_string();
            let turn_id = payload.get("turn_id")?.as_str()?.to_string();
            let token = payload.get("token")?.as_str()?.to_string();
            Some(OutboundReply::StreamingToken { session_id, turn_id, token })
        }
        "approval_required" => {
            let session_id = payload.get("session_id")?.as_str()?.to_string();
            let turn_id = payload.get("turn_id")?.as_str()?.to_string();
            let description = payload.get("description")
                .and_then(|v: &serde_json::Value| v.as_str())
                .unwrap_or("")
                .to_string();
            let options = payload.get("options")
                .and_then(|v: &serde_json::Value| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            Some(OutboundReply::ApprovalRequired { session_id, turn_id, description, options })
        }
        _ => None,
    }
}
