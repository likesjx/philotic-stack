use ansible_mesh_core::webrtc::{SignalPayload, WebRtcSignalMessage};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::mpsc;
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

/// A lightweight Transceiver for peer-to-peer data channels bypassing the Philotic ledger.
pub struct WebRtcGuest {
    session_id: String,
    _target_node: String,
    signal_tx: mpsc::Sender<WebRtcSignalMessage>,
}

impl WebRtcGuest {
    pub fn new(
        session_id: String,
        target_node: String,
        signal_tx: mpsc::Sender<WebRtcSignalMessage>,
    ) -> Self {
        Self {
            session_id,
            _target_node: target_node,
            signal_tx,
        }
    }

    /// Run the transceiver loop. In a real environment, this handles SDP offers,
    /// answers, and ICE routing for Live API DataChannels.
    pub async fn run_answering(self, offer_sdp: String) -> Result<()> {
        info!(
            "Spinning up WebRTC Transceiver Guest for session {}",
            self.session_id
        );

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

            d.on_message(Box::new(move |msg: DataChannelMessage| {
                let msg_str = String::from_utf8(msg.data.to_vec())
                    .unwrap_or_else(|_| "[Binary Data]".to_string());
                info!("P2P Message from DataChannel '{}': '{}'", d_label, msg_str);
                Box::pin(async {})
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
                target_guest_id: "remote-requester".to_string(), // In reality we map this back to the requesting agent id
                sender_node: "local-ansible-01".to_string(),     // In reality map from node caps
                signal: SignalPayload::Answer(local_desc.sdp),
            };
            let _ = self.signal_tx.send(signal).await;
            info!(
                "Generated SDP Answer and dispatched to Mesh Control Plane for session {}",
                self.session_id
            );
        }

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
