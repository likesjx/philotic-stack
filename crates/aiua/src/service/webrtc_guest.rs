use ansible_mesh_core::webrtc::{SignalPayload, WebRtcSignalMessage};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tracing::{info, warn};
use webrtc::api::APIBuilder;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::data_channel::RTCDataChannel;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

type PendingSessionRegistry =
    Mutex<HashMap<String, Arc<webrtc::peer_connection::RTCPeerConnection>>>;
type SessionStatusRegistry = Mutex<HashMap<String, String>>;
const SMOKE_PING_PREFIX: &str = "philotic-webrtc-ping:";
const SMOKE_PONG_PREFIX: &str = "philotic-webrtc-pong:";

fn pending_sessions() -> &'static PendingSessionRegistry {
    static REGISTRY: OnceLock<PendingSessionRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn session_statuses() -> &'static SessionStatusRegistry {
    static REGISTRY: OnceLock<SessionStatusRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn set_session_status(session_id: &str, status: impl Into<String>) {
    session_statuses()
        .lock()
        .await
        .insert(session_id.to_string(), status.into());
}

async fn set_session_status_if_incomplete(session_id: &str, status: impl Into<String>) {
    let mut statuses = session_statuses().lock().await;
    let should_preserve = matches!(
        statuses.get(session_id).map(String::as_str),
        Some("completed") | Some("pong_sent")
    );
    if !should_preserve {
        statuses.insert(session_id.to_string(), status.into());
    }
}

/// A lightweight Transceiver for peer-to-peer data channels bypassing the Philotic ledger.
pub struct WebRtcGuest {
    session_id: String,
    local_node_id: String,
    target_node_id: String,
    target_guest_id: Option<String>,
    sender_guest_id: Option<String>,
    signal_tx: mpsc::Sender<WebRtcSignalMessage>,
}

impl WebRtcGuest {
    pub async fn session_status(session_id: &str) -> Option<String> {
        session_statuses().lock().await.get(session_id).cloned()
    }

    pub async fn apply_answer(session_id: &str, answer_sdp: String) -> Result<bool> {
        let pc = {
            let registry = pending_sessions().lock().await;
            registry.get(session_id).cloned()
        };

        let Some(pc) = pc else {
            return Ok(false);
        };

        let desc = RTCSessionDescription::answer(answer_sdp).expect("Invalid SDP Answer format");
        pc.set_remote_description(desc).await?;
        set_session_status(session_id, "answer_applied").await;
        info!(
            "Applied remote SDP answer for WebRTC session {}",
            session_id
        );
        Ok(true)
    }

    pub async fn close_session(session_id: &str) -> Result<bool> {
        let pc = {
            let mut registry = pending_sessions().lock().await;
            registry.remove(session_id)
        };

        let Some(pc) = pc else {
            return Ok(false);
        };

        pc.close().await?;
        set_session_status_if_incomplete(session_id, "closed").await;
        info!("Closed WebRTC session {}", session_id);
        Ok(true)
    }

    pub fn new(
        session_id: String,
        local_node_id: String,
        target_node_id: String,
        target_guest_id: Option<String>,
        sender_guest_id: Option<String>,
        signal_tx: mpsc::Sender<WebRtcSignalMessage>,
    ) -> Self {
        Self {
            session_id,
            local_node_id,
            target_node_id,
            target_guest_id,
            sender_guest_id,
            signal_tx,
        }
    }

    pub async fn start_offering(
        session_id: String,
        local_node_id: String,
        target_node_id: String,
        target_guest_id: Option<String>,
        sender_guest_id: Option<String>,
        signal_tx: mpsc::Sender<WebRtcSignalMessage>,
    ) -> Result<()> {
        info!("Starting outbound WebRTC offer for session {}", session_id);
        set_session_status(&session_id, "starting_offer").await;

        let mut m = MediaEngine::default();
        m.register_default_codecs()?;

        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut m)?;

        let api = APIBuilder::new()
            .with_media_engine(m)
            .with_interceptor_registry(registry)
            .build();

        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                ..Default::default()
            }],
            ..Default::default()
        };

        let pc = Arc::new(api.new_peer_connection(config).await?);
        let label = format!("philotic-session-{}", session_id);
        let dc = pc.create_data_channel(&label, None).await?;

        let ping_session_id = session_id.clone();
        let ping_dc = dc.clone();
        dc.on_open(Box::new(move || {
            let ping_dc = ping_dc.clone();
            let ping_session_id = ping_session_id.clone();
            Box::pin(async move {
                let ping = format!("{SMOKE_PING_PREFIX}{ping_session_id}");
                match ping_dc.send_text(ping.clone()).await {
                    Ok(_) => {
                        set_session_status(&ping_session_id, "ping_sent").await;
                        info!("Sent WebRTC ping for session {}", ping_session_id)
                    }
                    Err(err) => warn!(
                        "Failed to send WebRTC ping for session {}: {}",
                        ping_session_id, err
                    ),
                }
            })
        }));

        let pong_session_id = session_id.clone();
        let pong_target_node_id = target_node_id.clone();
        let pong_target_guest_id = target_guest_id.clone();
        let pong_local_node_id = local_node_id.clone();
        let pong_sender_guest_id = sender_guest_id.clone();
        let pong_signal_tx = signal_tx.clone();
        dc.on_message(Box::new(move |msg: DataChannelMessage| {
            let pong_session_id = pong_session_id.clone();
            let pong_target_node_id = pong_target_node_id.clone();
            let pong_target_guest_id = pong_target_guest_id.clone();
            let pong_local_node_id = pong_local_node_id.clone();
            let pong_sender_guest_id = pong_sender_guest_id.clone();
            let pong_signal_tx = pong_signal_tx.clone();
            Box::pin(async move {
                let msg_str = String::from_utf8(msg.data.to_vec())
                    .unwrap_or_else(|_| "[Binary Data]".to_string());
                info!(
                    "P2P Message on outbound DataChannel for session {}: '{}'",
                    pong_session_id, msg_str
                );
                if let Some(returned_session_id) = msg_str.strip_prefix(SMOKE_PONG_PREFIX) {
                    set_session_status(returned_session_id, "completed").await;
                    info!("Received WebRTC pong for session {}", returned_session_id);
                    let signal = WebRtcSignalMessage {
                        session_id: pong_session_id.clone(),
                        target_node_id: pong_target_node_id,
                        target_guest_id: pong_target_guest_id,
                        sender_node: pong_local_node_id,
                        sender_guest_id: pong_sender_guest_id,
                        signal: SignalPayload::SessionEnded,
                    };
                    if let Err(err) = pong_signal_tx.send(signal).await {
                        warn!(
                            "Failed to emit WebRTC session end for session {}: {}",
                            pong_session_id, err
                        );
                    }
                    if let Err(err) = WebRtcGuest::close_session(&pong_session_id).await {
                        warn!(
                            "Failed to close local WebRTC offer session {} after pong: {}",
                            pong_session_id, err
                        );
                    }
                }
            })
        }));

        pc.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
            info!("Outbound Peer Connection State has changed: {}", s);
            if s == RTCPeerConnectionState::Failed {
                warn!("Outbound Peer Connection has gone to failed exiting");
            }
            Box::pin(async {})
        }));

        let offer = pc.create_offer(None).await?;
        let mut gather_complete = pc.gathering_complete_promise().await;
        pc.set_local_description(offer.clone()).await?;
        let _ = gather_complete.recv().await;

        pending_sessions()
            .lock()
            .await
            .insert(session_id.clone(), pc.clone());
        set_session_status(&session_id, "offer_created").await;

        if let Some(local_desc) = pc.local_description().await {
            let session_id_for_signal = session_id.clone();
            let signal = WebRtcSignalMessage {
                session_id,
                target_node_id,
                target_guest_id,
                sender_node: local_node_id,
                sender_guest_id,
                signal: SignalPayload::Offer(local_desc.sdp),
            };
            let _ = signal_tx.send(signal).await;
            set_session_status(&session_id_for_signal, "offer_sent").await;
        }

        Ok(())
    }

    /// Run the transceiver loop. In a real environment, this handles SDP offers,
    /// answers, and ICE routing for Live API DataChannels.
    pub async fn run_answering(self, offer_sdp: String) -> Result<()> {
        info!(
            "Spinning up WebRTC Transceiver Guest for session {}",
            self.session_id
        );
        set_session_status(&self.session_id, "answering_offer").await;

        let mut m = MediaEngine::default();
        m.register_default_codecs()?;

        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut m)?;

        let api = APIBuilder::new()
            .with_media_engine(m)
            .with_interceptor_registry(registry)
            .build();

        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                ..Default::default()
            }],
            ..Default::default()
        };

        let pc = Arc::new(api.new_peer_connection(config).await?);

        pc.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
            info!("Peer Connection State has changed: {}", s);
            if s == RTCPeerConnectionState::Failed {
                warn!("Peer Connection has gone to failed exiting");
            }
            Box::pin(async {})
        }));

        pc.on_data_channel(Box::new(move |d: Arc<RTCDataChannel>| {
            let d_label = d.label().to_owned();
            let d_id = d.id();
            info!("New DataChannel {} {}", d_label, d_id);

            let d_for_message = d.clone();
            d.on_message(Box::new(move |msg: DataChannelMessage| {
                let d_label = d_label.clone();
                let d_for_message = d_for_message.clone();
                Box::pin(async move {
                    let msg_str = String::from_utf8(msg.data.to_vec())
                        .unwrap_or_else(|_| "[Binary Data]".to_string());
                    info!("P2P Message from DataChannel '{}': '{}'", d_label, msg_str);
                    if let Some(session_id) = msg_str.strip_prefix(SMOKE_PING_PREFIX) {
                        set_session_status(session_id, "ping_received").await;
                        let pong = format!("{SMOKE_PONG_PREFIX}{session_id}");
                        match d_for_message.send_text(pong.clone()).await {
                            Ok(_) => {
                                set_session_status(session_id, "pong_sent").await;
                                info!("Sent WebRTC pong for session {}", session_id)
                            }
                            Err(err) => warn!(
                                "Failed to send WebRTC pong for session {}: {}",
                                session_id, err
                            ),
                        }
                    }
                })
            }));

            Box::pin(async {})
        }));

        // Set remote description from the received mesh UDP signal
        let desc = RTCSessionDescription::offer(offer_sdp).expect("Invalid SDP Offer format");
        pc.set_remote_description(desc).await?;

        // Create answer
        let answer = pc.create_answer(None).await?;
        let mut gather_complete = pc.gathering_complete_promise().await;
        pc.set_local_description(answer.clone()).await?;

        // Wait for ICE gathering
        let _ = gather_complete.recv().await;

        if let Some(local_desc) = pc.local_description().await {
            // Send the Answer back out over the Mesh UDP control plane
            let signal = WebRtcSignalMessage {
                session_id: self.session_id.clone(),
                target_node_id: self.target_node_id.clone(),
                target_guest_id: self.sender_guest_id.clone(),
                sender_node: self.local_node_id.clone(),
                sender_guest_id: self.target_guest_id.clone(),
                signal: SignalPayload::Answer(local_desc.sdp),
            };
            let _ = self.signal_tx.send(signal).await;
            set_session_status(&self.session_id, "answer_sent").await;
            info!(
                "Generated SDP Answer and dispatched to Mesh Control Plane for session {}",
                self.session_id
            );
        }

        pending_sessions()
            .lock()
            .await
            .insert(self.session_id.clone(), pc.clone());

        // Keep the task alive until the connection dies
        pc.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
            info!("WebRTC Transceiver State: {}", s);
            Box::pin(async {})
        }));

        // This is a minimal stub. In the live implementation we bound this loop to the `pc` lifecycle
        // or a manual termination signal.
        tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;

        Ok(())
    }
}
