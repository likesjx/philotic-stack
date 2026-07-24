//! WebRTC signaling IPC handlers, extracted verbatim from `ipc.rs`.
//!
//! Handles the `StartWebRtcSession` / `GetWebRtcSessionStatus` IPC request
//! variants. No behavior change from the original in-`ipc.rs` implementation.

use ansible_mesh_core::domain::GraphDomain;
use philotic_client::{GuestIdentity, IpcResponse};
use tokio::sync::mpsc;
use tracing::error;
use uuid::Uuid;

use super::ipc::IpcServer;

impl IpcServer {
    pub(crate) fn hotel_exists_for_node(
        graph: &GraphDomain,
        target_node_id: &str,
    ) -> anyhow::Result<bool> {
        Ok(graph
            .list_hotels()?
            .into_iter()
            .any(|hotel| hotel.capabilities.node_id == target_node_id))
    }

    pub(crate) async fn handle_start_webrtc_session(
        graph: &GraphDomain,
        local_node_id: &str,
        current_identity: Option<&GuestIdentity>,
        webrtc_signal_tx: Option<&mpsc::Sender<ansible_mesh_core::webrtc::WebRtcSignalMessage>>,
        target_node_id: String,
        target_guest_id: Option<String>,
        session_id: Option<String>,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
            return IpcResponse::error(
                "webrtc",
                "WEBRTC_UNREGISTERED",
                "guest must register before starting a WebRTC session",
            );
        };

        if target_node_id == local_node_id {
            return IpcResponse::error(
                "webrtc",
                "WEBRTC_LOCAL_TARGET",
                "WebRTC sessions must target a remote node",
            );
        }

        match Self::hotel_exists_for_node(graph, &target_node_id) {
            Ok(true) => {}
            Ok(false) => {
                return IpcResponse::error(
                    "webrtc",
                    "WEBRTC_UNKNOWN_TARGET",
                    format!("unknown target node [{}]", target_node_id),
                );
            }
            Err(err) => {
                return IpcResponse::error("webrtc", "WEBRTC_GRAPH_ERROR", err.to_string());
            }
        }

        let Some(webrtc_signal_tx) = webrtc_signal_tx.cloned() else {
            return IpcResponse::error(
                "webrtc",
                "WEBRTC_DISABLED",
                "hotel runtime is not configured with WebRTC signaling support",
            );
        };

        let session_id = session_id.unwrap_or_else(|| format!("webrtc:{}", Uuid::new_v4()));
        let sender_guest_id = Some(identity.guest_id.clone());
        let local_node_id = local_node_id.to_string();
        let target_node_for_task = target_node_id.clone();
        let target_guest_for_task = target_guest_id.clone();
        let session_id_for_task = session_id.clone();

        tokio::spawn(async move {
            if let Err(err) = crate::service::webrtc_guest::WebRtcGuest::start_offering(
                session_id_for_task,
                local_node_id,
                target_node_for_task,
                target_guest_for_task,
                sender_guest_id,
                webrtc_signal_tx,
            )
            .await
            {
                error!("Failed to start outbound WebRTC session: {}", err);
            }
        });

        IpcResponse::success(
            "webrtc",
            Some(serde_json::json!({
                "session_id": session_id,
                "target_node_id": target_node_id,
                "target_guest_id": target_guest_id,
                "initiator_guest_id": identity.guest_id,
            })),
        )
    }

    pub(crate) async fn handle_get_webrtc_session_status(session_id: String) -> IpcResponse {
        let status = crate::service::webrtc_guest::WebRtcGuest::session_status(&session_id).await;
        IpcResponse::success(
            "webrtc_status",
            Some(serde_json::json!({
                "session_id": session_id,
                "status": status,
            })),
        )
    }
}
