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
            let mut renew_tick = tokio::time::interval(LEASE_RENEW_INTERVAL);
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

/// Forward an inbound envelope to the hotel as a `CreateTask` (local) or
/// `EmitTask` (cross-hotel when `raw_transport.target_node` is set).
///
/// Maps `InboundEnvelope` fields to the field names that philote's
/// `InboundTaskPayload` expects. The two structs serve different layers
/// (protocol-agnostic vs. philote-specific) so names don't match directly.
async fn dispatch_inbound(client: &mut PhiloticClient, envelope: InboundEnvelope) -> Result<()> {
    let req = build_inbound_request(&envelope);
    client.send_request(req).await?;
    Ok(())
}

/// Build the IPC request for an inbound envelope. Pure so variants can unit-test
/// their dispatch payload shape without a live IPC connection.
fn build_inbound_request(envelope: &InboundEnvelope) -> philotic_client::IpcRequest {
    use philotic_client::IpcRequest;

    // Extract transport hint from raw_transport metadata.
    let transport = envelope
        .raw_transport
        .get("transport")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // Cross-hotel routing: explicit envelope field first (e.g. Telegram
    // seats emitting to the local node), then the raw_transport hint used
    // by MCP route targets.
    let target_node = envelope.target_node.clone().or_else(|| {
        envelope
            .raw_transport
            .get("target_node")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    });

    let hinted_target_id = envelope.target_guest_id.clone().or_else(|| {
        envelope
            .raw_transport
            .get("target_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    });

    let target_kind = envelope
        .raw_transport
        .get("target_kind")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    let (target_role, target_guest_id) = resolve_target(target_kind, hinted_target_id);

    let mut payload = if target_kind == Some("datasource") {
        datasource_payload(envelope, transport)
    } else {
        serde_json::json!({
        "session_id":             envelope.session_id,
        "turn_id":                envelope.turn_id,
        "content":                envelope.content,
        "command":                envelope.command,
        "attachments":            envelope.attachments,
        "transport":              transport,
        "sender_id":              envelope.sender.id,
        "sender_username":        envelope.sender.username,
        "raw_transport_event":    envelope.raw_transport,
        "requires_approval":      envelope.requires_approval,
        // Routing: philote uses these to know where to emit replies.
        "final_reply_to":         envelope.final_reply_to,
        "final_reply_role":       envelope.final_reply_role,
        "final_reply_guest_id":   envelope.final_reply_guest_id,
        })
    };

    // Transport-specific extras override same-named standard fields
    // (e.g. Telegram sets "transport": "telegram" and adds chat_id).
    if !envelope.extra.is_empty() {
        if let Some(obj) = payload.as_object_mut() {
            for (key, value) in &envelope.extra {
                obj.insert(key.clone(), value.clone());
            }
        }
    }

    if let Some(node) = target_node {
        // Route to a specific node (possibly the local one) via mesh dispatch.
        IpcRequest::EmitTask {
            target_node: node,
            target_role,
            target_guest_id,
            task_json: payload.to_string(),
        }
    } else {
        IpcRequest::CreateTask {
            target_role,
            payload,
        }
    }
}

fn resolve_target(
    target_kind: Option<&str>,
    hinted_target_id: Option<String>,
) -> (String, Option<String>) {
    match target_kind {
        Some("datasource") => (
            hinted_target_id.unwrap_or_else(|| "graph-datasource".into()),
            None,
        ),
        Some("tool") => ("tool-runner".into(), hinted_target_id),
        Some("philote") => ("agent".into(), hinted_target_id),
        _ => ("agent".into(), hinted_target_id),
    }
}

fn datasource_payload(envelope: &InboundEnvelope, transport: Option<String>) -> serde_json::Value {
    let content: serde_json::Value =
        serde_json::from_str(&envelope.content).unwrap_or_else(|_| serde_json::json!({}));
    let tool_name = content
        .get("action")
        .and_then(|v| v.as_str())
        .or(envelope.command.as_deref())
        .unwrap_or("execute_tool");
    let arguments = content
        .get("payload")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    serde_json::json!({
        "action": "execute_tool",
        "tool_name": tool_name,
        "arguments": arguments,
        "session_id": envelope.session_id,
        "turn_id": envelope.turn_id,
        "chat_id": envelope.sender.id.as_deref().unwrap_or(&envelope.session_id),
        "transport": transport,
        "sender_id": envelope.sender.id,
        "sender_username": envelope.sender.username,
        "raw_transport_event": envelope.raw_transport,
        "requires_approval": envelope.requires_approval,
        "reply_to": envelope.final_reply_to,
        "reply_role": envelope.final_reply_role,
        "reply_guest_id": envelope.final_reply_guest_id,
    })
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

    let action = payload
        .get("action")
        .and_then(|v: &serde_json::Value| v.as_str())?;

    match action {
        "send_reply" | "text_reply" => {
            let session_id = payload.get("session_id")?.as_str()?.to_string();
            let turn_id = payload.get("turn_id")?.as_str()?.to_string();
            let content = payload.get("content")?.as_str()?.to_string();
            let reply_to = payload
                .get("reply_to")
                .and_then(|v: &serde_json::Value| v.as_str())
                .map(str::to_string);
            Some(OutboundReply::Text {
                session_id,
                turn_id,
                content,
                reply_to,
            })
        }
        "streaming_token" => {
            let session_id = payload.get("session_id")?.as_str()?.to_string();
            let turn_id = payload.get("turn_id")?.as_str()?.to_string();
            let token = payload.get("token")?.as_str()?.to_string();
            Some(OutboundReply::StreamingToken {
                session_id,
                turn_id,
                token,
            })
        }
        "approval_required" => {
            let session_id = payload.get("session_id")?.as_str()?.to_string();
            let turn_id = payload.get("turn_id")?.as_str()?.to_string();
            let description = payload
                .get("description")
                .and_then(|v: &serde_json::Value| v.as_str())
                .unwrap_or("")
                .to_string();
            let options = payload
                .get("options")
                .and_then(|v: &serde_json::Value| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            Some(OutboundReply::ApprovalRequired {
                session_id,
                turn_id,
                description,
                options,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::SenderInfo;

    fn mcp_datasource_envelope() -> InboundEnvelope {
        InboundEnvelope {
            session_id: "mcp-lifegraph".into(),
            turn_id: "turn-1".into(),
            sender: SenderInfo {
                id: Some("lifegraph-reader".into()),
                display_name: None,
                username: None,
                is_operator: false,
            },
            content: serde_json::json!({
                "action": "life.recall",
                "payload": {
                    "query_text": "open loops",
                    "max_context_packets": 3
                },
                "target_kind": "datasource",
                "target_id": "life-graph-runner"
            })
            .to_string(),
            attachments: vec![],
            command: Some("life.recall".into()),
            reply_to: Some("turn-1".into()),
            raw_transport: serde_json::json!({
                "transport": "mcp",
                "tool": "life.recall",
                "target_kind": "datasource",
                "target_id": "life-graph-runner"
            }),
            requires_approval: false,
            final_reply_to: Some("vps-jane-aiua-01".into()),
            final_reply_role: Some("mcp-membrane".into()),
            final_reply_guest_id: Some("mcp-membrane-lifegraph-readonly".into()),
            target_node: None,
            target_guest_id: None,
            extra: serde_json::Map::new(),
        }
    }

    #[test]
    fn datasource_target_routes_to_named_role() {
        let (role, guest_id) = resolve_target(Some("datasource"), Some("life-graph-runner".into()));
        assert_eq!(role, "life-graph-runner");
        assert_eq!(guest_id, None);
    }

    #[test]
    fn datasource_payload_is_runner_compatible() {
        let envelope = mcp_datasource_envelope();
        let payload = datasource_payload(&envelope, Some("mcp".into()));

        assert_eq!(payload["action"], "execute_tool");
        assert_eq!(payload["tool_name"], "life.recall");
        assert_eq!(payload["arguments"]["query_text"], "open loops");
        assert_eq!(payload["reply_to"], "vps-jane-aiua-01");
        assert_eq!(payload["reply_role"], "mcp-membrane");
        assert_eq!(payload["reply_guest_id"], "mcp-membrane-lifegraph-readonly");
    }

    #[test]
    fn explicit_target_node_routes_via_emit_task_with_extra_merged() {
        use philotic_client::IpcRequest;

        let mut envelope = mcp_datasource_envelope();
        // Telegram-style dispatch: explicit local-node routing + transport extras.
        envelope.raw_transport = serde_json::json!({ "update_id": 42 });
        envelope.content = "hello".into();
        envelope.target_node = Some("mac-jane-aiua-01".into());
        envelope.target_guest_id = Some("agent-bjork-01".into());
        envelope.extra = serde_json::json!({
            "source": "telegram",
            "transport": "telegram",
            "chat_id": "12345",
            "thread_id": null,
            "message_kind": "text",
        })
        .as_object()
        .cloned()
        .unwrap();

        let req = build_inbound_request(&envelope);
        let IpcRequest::EmitTask {
            target_node,
            target_role,
            target_guest_id,
            task_json,
        } = req
        else {
            panic!("expected EmitTask, got {req:?}");
        };
        assert_eq!(target_node, "mac-jane-aiua-01");
        assert_eq!(target_role, "agent");
        assert_eq!(target_guest_id.as_deref(), Some("agent-bjork-01"));

        let payload: serde_json::Value = serde_json::from_str(&task_json).unwrap();
        // Extra fields land at the top level and override standard ones.
        assert_eq!(payload["source"], "telegram");
        assert_eq!(payload["transport"], "telegram");
        assert_eq!(payload["chat_id"], "12345");
        assert_eq!(payload["message_kind"], "text");
        // Standard fields still present.
        assert_eq!(payload["session_id"], "mcp-lifegraph");
        assert_eq!(payload["content"], "hello");
        assert_eq!(payload["raw_transport_event"]["update_id"], 42);
    }

    #[test]
    fn empty_extra_and_no_target_keeps_local_create_task() {
        use philotic_client::IpcRequest;

        let mut envelope = mcp_datasource_envelope();
        envelope.raw_transport = serde_json::json!({ "transport": "mcp" });

        let req = build_inbound_request(&envelope);
        let IpcRequest::CreateTask {
            target_role,
            payload,
        } = req
        else {
            panic!("expected CreateTask, got {req:?}");
        };
        assert_eq!(target_role, "agent");
        assert_eq!(payload["transport"], "mcp");
        assert_eq!(payload["requires_approval"], false);
    }
}
