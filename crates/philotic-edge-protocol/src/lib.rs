//! # Philotic Edge Protocol
//!
//! Wire types for the **edge-mesh** tier of the Philotic mesh: a client-server
//! protocol spoken between edge devices (the iOS/macOS app) and a hotel-side
//! termination point (philotic-web / an edge gateway).
//!
//! ## Protocol tier
//!
//! The backbone mesh (hotel <-> hotel, UDP beacons on :8999) authenticates
//! peers with a shared `MeshAuth` PSK. Edge clients are deliberately kept out
//! of that trust tier: **edge clients never hold backbone MeshAuth PSKs**.
//! Instead they authenticate with a per-device bearer token (`edge_token`)
//! obtained through one-time enrollment ([`EnrollmentRequest`] /
//! [`EnrollmentResponse`] over HTTPS, gated by an operator-issued invite
//! code). Compromise of an edge device therefore never exposes backbone
//! credentials — the server can revoke the single device token.
//!
//! ## Transport & framing
//!
//! - Enrollment is plain HTTP request/response (not WebSocket).
//! - Everything else flows over a WebSocket as JSON-encoded
//!   [`EdgeEnvelope`] text frames. `EdgeMessage` is internally tagged with
//!   `"type"` so Swift `Codable` mirrors can decode with a simple
//!   discriminated enum.
//!
//! ## Sequencing, acks, and cursor resume (store-and-forward)
//!
//! Every envelope carries a sender-local monotonically increasing `seq` and
//! may piggyback an `ack` of the highest contiguous peer `seq` received.
//! Server-to-client traffic is store-and-forward: the server persists
//! outbound events and associates each with an **opaque cursor** (a plain
//! `String` everywhere in this crate — clients must never parse it). On
//! reconnect the client presents its last durable cursor in
//! [`EdgeHello::cursor`]; the server replays everything after it and reports
//! the replay origin in `HelloAck::replay_from`. A client with no cursor
//! (fresh install) sends `None` and receives only new traffic.
//!
//! ## Versioning
//!
//! [`PROTOCOL_VERSION`] is carried in every envelope as `"v"`. Servers reject
//! unknown major versions with a fatal `Error` message.
//!
//! This crate is intentionally self-contained (serde + serde_json only) so
//! the types remain trivially mirrorable to Swift `Codable`.

use serde::{Deserialize, Serialize};

/// Current wire protocol version, carried in every [`EdgeEnvelope`] as `"v"`.
pub const PROTOCOL_VERSION: u16 = 1;

/// Top-level frame exchanged over the edge WebSocket.
///
/// `seq` is sender-local and monotonically increasing; `ack` (when present)
/// acknowledges the highest contiguous `seq` received from the peer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeEnvelope {
    /// Protocol version ([`PROTOCOL_VERSION`]).
    pub v: u16,
    /// Sender-local monotonically increasing sequence number.
    pub seq: u64,
    /// Highest contiguous peer sequence number received, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack: Option<u64>,
    /// The message payload.
    pub msg: EdgeMessage,
}

impl EdgeEnvelope {
    /// Convenience constructor for an envelope at [`PROTOCOL_VERSION`].
    pub fn new(seq: u64, ack: Option<u64>, msg: EdgeMessage) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            seq,
            ack,
            msg,
        }
    }
}

/// All messages that flow over the edge WebSocket, in both directions.
///
/// Internally tagged with `"type"` so the JSON form is
/// `{"type": "hello", ...fields...}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EdgeMessage {
    /// Client -> server: first frame after connecting; identifies the device
    /// and presents the resume cursor.
    Hello(EdgeHello),
    /// Server -> client: accepts the hello and opens the session.
    HelloAck {
        /// Server-assigned session identifier.
        session_id: String,
        /// Opaque cursor the replay starts from, if any replay occurs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay_from: Option<String>,
    },
    /// Client -> server: submit an operator/agent turn to a target agent.
    TurnSubmit {
        /// Hotel node that hosts the target agent.
        target_node_id: String,
        /// Agent (guest) the turn is addressed to.
        target_agent_id: String,
        /// Existing conversation to continue, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation_id: Option<String>,
        /// Turn text content.
        content: String,
        /// Attached blobs (voice notes, images, files).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blob_refs: Vec<BlobRef>,
        /// Inbound modality marker ("voice" | "audio") — mirrors the
        /// membrane `message_kind` field philote reads to set
        /// `had_voice_input`, which drives the agent's own
        /// `voice_response_policy` (persona TTS). Absent = plain text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_kind: Option<String>,
    },
    /// Client -> server: open an uplink audio stream bound to a target
    /// agent. Chunks follow as `AudioChunk`; `AudioStreamEnd` seals the
    /// stream, at which point the server assembles the audio, stores it,
    /// and submits the voice turn — the client never waits on an upload
    /// after it stops speaking.
    AudioStreamStart {
        /// Client-chosen stream identifier (unique per session).
        stream_id: String,
        /// Hotel node that hosts the target agent.
        target_node_id: String,
        /// Agent the eventual voice turn is addressed to.
        target_agent_id: String,
        /// Existing conversation to continue, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation_id: Option<String>,
        /// MIME type of the assembled audio (e.g. "audio/mp4").
        mime_type: String,
    },
    /// Client -> server: one chunk of an open audio stream. Chunks are
    /// base64 in protocol v1; a future protocol rev moves them to binary
    /// WS frames (seam: edge-audio-frames).
    AudioChunk {
        /// Stream this chunk belongs to.
        stream_id: String,
        /// 0-based chunk ordinal — must arrive in order (WS is ordered;
        /// this guards reconnect/duplication bugs, not reordering).
        chunk_seq: u64,
        /// Base64-encoded audio bytes.
        data_base64: String,
    },
    /// Client -> server: seal (or abandon) an open audio stream.
    AudioStreamEnd {
        /// Stream to seal.
        stream_id: String,
        /// True to discard the stream without submitting a turn.
        #[serde(default)]
        cancel: bool,
    },
    /// Server -> client: inline synthesized voice audio for a completed
    /// turn. Audio rides inline (base64) because hotel TTS artifacts are
    /// inline and blob download URLs embed loopback hosts a remote device
    /// cannot reach.
    VoiceReply {
        /// Conversation the spoken reply belongs to.
        conversation_id: String,
        /// Turn the audio was synthesized for, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        /// Base64-encoded audio bytes.
        audio_base64: String,
        /// MIME type of the audio (e.g. "audio/mpeg").
        mime_type: String,
        /// Text the audio speaks, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transcript: Option<String>,
        /// Ordinal of this chunk within a streamed voice reply (sentence
        /// pipelining). Absent = a whole-reply voice message.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chunk_seq: Option<u64>,
        /// True on the last chunk of a streamed voice reply. Absent on
        /// whole-reply voice messages.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_final: Option<bool>,
    },
    /// Server -> client: streamed turn output and status.
    TurnEvent {
        /// Conversation this event belongs to.
        conversation_id: String,
        /// What kind of event this is.
        event_kind: TurnEventKind,
        /// Token text, final text, status description, or error message.
        content: String,
        /// Turn identifier, once known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
    },
    /// Server -> client: an action awaits operator approval.
    ApprovalRequest {
        /// Identifier to echo back in [`EdgeMessage::ApprovalResolve`].
        approval_id: String,
        /// Human-readable description of the pending action.
        description: String,
        /// Optional risk assessment (e.g. "low", "high").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        risk: Option<String>,
    },
    /// Client -> server: resolve a pending approval.
    ApprovalResolve {
        /// The approval being resolved.
        approval_id: String,
        /// Whether the action was approved.
        approved: bool,
        /// Optional operator note.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Server -> client: a LifeGraph node changed.
    LifeGraphChange {
        /// Kind of change (e.g. "created", "updated", "linked").
        change_kind: String,
        /// LifeGraph node identifier.
        node_id: String,
        /// Node label, if available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// Short human-readable summary of the change.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    /// Server -> client: a voice blob is ready for download/playback.
    VoiceBlob {
        /// Blob identifier in the hotel blob store.
        blob_id: String,
        /// URL the client can fetch the blob from.
        download_url: String,
        /// MIME type (e.g. "audio/ogg").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime: Option<String>,
        /// Transcript of the audio, if available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transcript: Option<String>,
    },
    /// Server -> client: invoke a tool the device advertised in its
    /// capabilities (e.g. HealthKit read, Shortcuts run).
    ToolInvoke {
        /// Identifier to echo back in [`EdgeMessage::ToolResult`].
        invocation_id: String,
        /// Tool reference (e.g. "os.ios.healthkit.read@1").
        tool_ref: String,
        /// Tool arguments as a JSON-encoded string.
        args_json: String,
    },
    /// Client -> server: result of a [`EdgeMessage::ToolInvoke`].
    ToolResult {
        /// The invocation being answered.
        invocation_id: String,
        /// Whether the tool ran successfully.
        ok: bool,
        /// Tool result (or error detail) as a JSON-encoded string.
        result_json: String,
    },
    /// Client -> server: the device's capabilities changed mid-session.
    CapabilitiesUpdate {
        /// The full replacement capability set.
        capabilities: EdgeCapabilities,
    },
    /// Either direction: keepalive probe.
    Ping {
        /// Echoed back in the matching [`EdgeMessage::Pong`].
        nonce: u64,
    },
    /// Either direction: keepalive reply.
    Pong {
        /// Nonce copied from the matching [`EdgeMessage::Ping`].
        nonce: u64,
    },
    /// Either direction: protocol or application error.
    Error {
        /// Machine-readable error code.
        code: String,
        /// Human-readable message.
        message: String,
        /// If true, the sender will close the connection.
        fatal: bool,
    },
}

/// Payload of [`EdgeMessage::Hello`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeHello {
    /// The edge node id assigned at enrollment.
    pub node_id: String,
    /// What this device offers the mesh.
    pub capabilities: EdgeCapabilities,
    /// Last durable resume cursor (opaque), or `None` for a fresh session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// What an edge device offers the mesh.
///
/// Mirrors the spirit of the backbone `NodeCapabilities`
/// (`ansible-mesh-core/ios_capabilities.json`) without depending on that
/// crate, so the type stays mirrorable to Swift.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeCapabilities {
    /// Human-friendly device name (e.g. "Jared's iPhone").
    pub device_name: String,
    /// Platform identifier (e.g. "ios", "macos").
    pub platform: String,
    /// Mesh roles this device can play (e.g. "ClientNode", "ModelNode").
    #[serde(default)]
    pub roles: Vec<String>,
    /// Tool refs the device exposes (e.g. "os.ios.healthkit.read@1").
    #[serde(default)]
    pub tools: Vec<String>,
    /// Model refs the device can run locally.
    #[serde(default)]
    pub models: Vec<String>,
}

/// Reference to a blob in the hotel blob store, attachable to a turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlobRef {
    /// Blob identifier.
    pub blob_id: String,
    /// URL the blob can be fetched from, if already resolvable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
    /// MIME type of the blob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
}

/// Kind of a [`EdgeMessage::TurnEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEventKind {
    /// Incremental streamed token(s).
    Token,
    /// Final complete turn output.
    Final,
    /// Status update (e.g. "thinking", "running tool").
    Status,
    /// The turn failed; `content` carries the error.
    Error,
}

/// One-time device enrollment request (HTTP, not WebSocket).
///
/// The invite code is minted by the operator; the device public key becomes
/// the device identity the server binds the issued `edge_token` to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollmentRequest {
    /// Operator-issued one-time invite code.
    pub invite_code: String,
    /// Device public key, base64-encoded.
    pub device_pubkey_b64: String,
    /// Human-friendly device name.
    pub device_name: String,
    /// Platform identifier (e.g. "ios", "macos").
    pub platform: String,
}

/// Successful enrollment response (HTTP, not WebSocket).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrollmentResponse {
    /// Edge node id assigned to this device.
    pub node_id: String,
    /// Bearer token the device presents on every connection. This is the
    /// only credential an edge device ever holds — never a backbone PSK.
    pub edge_token: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> EdgeCapabilities {
        EdgeCapabilities {
            device_name: "Jared's iPhone".to_string(),
            platform: "ios".to_string(),
            roles: vec!["ClientNode".to_string(), "ModelNode".to_string()],
            tools: vec!["os.ios.healthkit.read@1".to_string()],
            models: vec!["stt.whisper.coreml-tiny-en@1".to_string()],
        }
    }

    fn round_trip(msg: EdgeMessage) {
        let envelope = EdgeEnvelope::new(7, Some(3), msg);
        let json = serde_json::to_string(&envelope).expect("serialize");
        let back: EdgeEnvelope = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(envelope, back, "round trip mismatch for {json}");
    }

    #[test]
    fn round_trip_hello() {
        round_trip(EdgeMessage::Hello(EdgeHello {
            node_id: "edge-abc123".to_string(),
            capabilities: caps(),
            cursor: Some("cur-opaque-42".to_string()),
        }));
        round_trip(EdgeMessage::Hello(EdgeHello {
            node_id: "edge-abc123".to_string(),
            capabilities: caps(),
            cursor: None,
        }));
    }

    #[test]
    fn round_trip_hello_ack() {
        round_trip(EdgeMessage::HelloAck {
            session_id: "sess-1".to_string(),
            replay_from: Some("cur-opaque-42".to_string()),
        });
        round_trip(EdgeMessage::HelloAck {
            session_id: "sess-1".to_string(),
            replay_from: None,
        });
    }

    #[test]
    fn round_trip_turn_submit() {
        round_trip(EdgeMessage::TurnSubmit {
            target_node_id: "mbp-jane".to_string(),
            target_agent_id: "jane".to_string(),
            conversation_id: Some("conv-9".to_string()),
            content: "hello there".to_string(),
            blob_refs: vec![BlobRef {
                blob_id: "blob-1".to_string(),
                download_url: Some("https://example/blob-1".to_string()),
                mime: Some("audio/ogg".to_string()),
            }],
            message_kind: Some("voice".to_string()),
        });
        round_trip(EdgeMessage::TurnSubmit {
            target_node_id: "mbp-jane".to_string(),
            target_agent_id: "jane".to_string(),
            conversation_id: None,
            content: "hello".to_string(),
            blob_refs: vec![],
            message_kind: None,
        });
    }

    #[test]
    fn round_trip_audio_stream() {
        round_trip(EdgeMessage::AudioStreamStart {
            stream_id: "stream-1".to_string(),
            target_node_id: "mac-jane-aiua-01".to_string(),
            target_agent_id: "agent-bjork-01".to_string(),
            conversation_id: Some("conv-9".to_string()),
            mime_type: "audio/mp4".to_string(),
        });
        round_trip(EdgeMessage::AudioChunk {
            stream_id: "stream-1".to_string(),
            chunk_seq: 0,
            data_base64: "bW9jaw==".to_string(),
        });
        round_trip(EdgeMessage::AudioStreamEnd {
            stream_id: "stream-1".to_string(),
            cancel: false,
        });
        round_trip(EdgeMessage::AudioStreamEnd {
            stream_id: "stream-1".to_string(),
            cancel: true,
        });
    }

    /// Golden wire fixtures for the audio-stream family — copy-pastable into
    /// Swift Codable tests. Do not change without bumping PROTOCOL_VERSION.
    #[test]
    fn golden_json_audio_stream() {
        const GOLDEN_START: &str = r#"{"v":1,"seq":10,"msg":{"type":"audio_stream_start","stream_id":"stream-1","target_node_id":"mac-jane-aiua-01","target_agent_id":"agent-bjork-01","conversation_id":"conv-9","mime_type":"audio/mp4"}}"#;
        const GOLDEN_CHUNK: &str = r#"{"v":1,"seq":11,"msg":{"type":"audio_chunk","stream_id":"stream-1","chunk_seq":0,"data_base64":"bW9jaw=="}}"#;
        const GOLDEN_END: &str = r#"{"v":1,"seq":12,"msg":{"type":"audio_stream_end","stream_id":"stream-1","cancel":false}}"#;

        let start = EdgeEnvelope::new(
            10,
            None,
            EdgeMessage::AudioStreamStart {
                stream_id: "stream-1".to_string(),
                target_node_id: "mac-jane-aiua-01".to_string(),
                target_agent_id: "agent-bjork-01".to_string(),
                conversation_id: Some("conv-9".to_string()),
                mime_type: "audio/mp4".to_string(),
            },
        );
        assert_eq!(serde_json::to_string(&start).unwrap(), GOLDEN_START);
        let chunk = EdgeEnvelope::new(
            11,
            None,
            EdgeMessage::AudioChunk {
                stream_id: "stream-1".to_string(),
                chunk_seq: 0,
                data_base64: "bW9jaw==".to_string(),
            },
        );
        assert_eq!(serde_json::to_string(&chunk).unwrap(), GOLDEN_CHUNK);
        let end = EdgeEnvelope::new(
            12,
            None,
            EdgeMessage::AudioStreamEnd {
                stream_id: "stream-1".to_string(),
                cancel: false,
            },
        );
        assert_eq!(serde_json::to_string(&end).unwrap(), GOLDEN_END);
        for golden in [GOLDEN_START, GOLDEN_CHUNK, GOLDEN_END] {
            let back: EdgeEnvelope = serde_json::from_str(golden).unwrap();
            assert_eq!(serde_json::to_string(&back).unwrap(), golden);
        }
    }

    #[test]
    fn round_trip_voice_reply() {
        round_trip(EdgeMessage::VoiceReply {
            conversation_id: "conv-9".to_string(),
            turn_id: Some("turn-3".to_string()),
            audio_base64: "bW9jay1hdWRpbw==".to_string(),
            mime_type: "audio/mpeg".to_string(),
            transcript: Some("hello there".to_string()),
            chunk_seq: None,
            is_final: None,
        });
        round_trip(EdgeMessage::VoiceReply {
            conversation_id: "conv-9".to_string(),
            turn_id: Some("turn-3".to_string()),
            audio_base64: "bW9jay1hdWRpbw==".to_string(),
            mime_type: "audio/mpeg".to_string(),
            transcript: Some("First sentence.".to_string()),
            chunk_seq: Some(0),
            is_final: Some(false),
        });
        round_trip(EdgeMessage::VoiceReply {
            conversation_id: "conv-9".to_string(),
            turn_id: None,
            audio_base64: "bW9jaw==".to_string(),
            mime_type: "audio/mpeg".to_string(),
            transcript: None,
            chunk_seq: None,
            is_final: None,
        });
    }

    #[test]
    fn round_trip_turn_event() {
        for kind in [
            TurnEventKind::Token,
            TurnEventKind::Final,
            TurnEventKind::Status,
            TurnEventKind::Error,
        ] {
            round_trip(EdgeMessage::TurnEvent {
                conversation_id: "conv-9".to_string(),
                event_kind: kind,
                content: "chunk".to_string(),
                turn_id: Some("turn-4".to_string()),
            });
        }
    }

    #[test]
    fn round_trip_approval_request() {
        round_trip(EdgeMessage::ApprovalRequest {
            approval_id: "appr-1".to_string(),
            description: "Run shell command".to_string(),
            risk: Some("high".to_string()),
        });
    }

    #[test]
    fn round_trip_approval_resolve() {
        round_trip(EdgeMessage::ApprovalResolve {
            approval_id: "appr-1".to_string(),
            approved: true,
            note: Some("looks fine".to_string()),
        });
    }

    #[test]
    fn round_trip_life_graph_change() {
        round_trip(EdgeMessage::LifeGraphChange {
            change_kind: "created".to_string(),
            node_id: "lg-77".to_string(),
            label: Some("Project".to_string()),
            summary: Some("New project node".to_string()),
        });
    }

    #[test]
    fn round_trip_voice_blob() {
        round_trip(EdgeMessage::VoiceBlob {
            blob_id: "blob-9".to_string(),
            download_url: "https://example/blob-9".to_string(),
            mime: Some("audio/ogg".to_string()),
            transcript: Some("hi".to_string()),
        });
    }

    #[test]
    fn round_trip_tool_invoke() {
        round_trip(EdgeMessage::ToolInvoke {
            invocation_id: "inv-1".to_string(),
            tool_ref: "os.ios.healthkit.read@1".to_string(),
            args_json: "{\"metric\":\"steps\"}".to_string(),
        });
    }

    #[test]
    fn round_trip_tool_result() {
        round_trip(EdgeMessage::ToolResult {
            invocation_id: "inv-1".to_string(),
            ok: true,
            result_json: "{\"steps\":1200}".to_string(),
        });
    }

    #[test]
    fn round_trip_capabilities_update() {
        round_trip(EdgeMessage::CapabilitiesUpdate {
            capabilities: caps(),
        });
    }

    #[test]
    fn round_trip_ping_pong() {
        round_trip(EdgeMessage::Ping { nonce: 42 });
        round_trip(EdgeMessage::Pong { nonce: 42 });
    }

    #[test]
    fn round_trip_error() {
        round_trip(EdgeMessage::Error {
            code: "bad_version".to_string(),
            message: "unsupported protocol version".to_string(),
            fatal: true,
        });
    }

    #[test]
    fn round_trip_enrollment_types() {
        let req = EnrollmentRequest {
            invite_code: "INV-1234".to_string(),
            device_pubkey_b64: "cHVia2V5".to_string(),
            device_name: "Jared's iPhone".to_string(),
            platform: "ios".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: EnrollmentRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);

        let resp = EnrollmentResponse {
            node_id: "edge-abc123".to_string(),
            edge_token: "tok-secret".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: EnrollmentResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, back);
    }

    #[test]
    fn envelope_version_field_is_v() {
        let envelope = EdgeEnvelope::new(1, None, EdgeMessage::Ping { nonce: 1 });
        let value: serde_json::Value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(
            value.get("v").and_then(|v| v.as_u64()),
            Some(u64::from(PROTOCOL_VERSION))
        );
        // ack is omitted when None
        assert!(value.get("ack").is_none());
    }

    #[test]
    fn unknown_type_tag_fails_cleanly() {
        let json = r#"{"v":1,"seq":1,"msg":{"type":"warp_drive","nonce":1}}"#;
        let err = serde_json::from_str::<EdgeEnvelope>(json).unwrap_err();
        assert!(
            err.to_string().contains("warp_drive"),
            "error should name the unknown tag: {err}"
        );
    }

    /// Golden wire fixture for `Hello` — copy-pastable into Swift Codable
    /// tests. Do not change without bumping PROTOCOL_VERSION.
    #[test]
    fn golden_json_hello() {
        const GOLDEN_HELLO: &str = r#"{"v":1,"seq":1,"msg":{"type":"hello","node_id":"edge-abc123","capabilities":{"device_name":"Jared's iPhone","platform":"ios","roles":["ClientNode","ModelNode"],"tools":["os.ios.healthkit.read@1"],"models":["stt.whisper.coreml-tiny-en@1"]},"cursor":"cur-opaque-42"}}"#;

        let envelope = EdgeEnvelope::new(
            1,
            None,
            EdgeMessage::Hello(EdgeHello {
                node_id: "edge-abc123".to_string(),
                capabilities: caps(),
                cursor: Some("cur-opaque-42".to_string()),
            }),
        );
        assert_eq!(serde_json::to_string(&envelope).unwrap(), GOLDEN_HELLO);
        let back: EdgeEnvelope = serde_json::from_str(GOLDEN_HELLO).unwrap();
        assert_eq!(back, envelope);
    }

    /// Golden wire fixture for `TurnSubmit` (all optional fields present) —
    /// copy-pastable into Swift Codable tests. Client->server: guards the
    /// hand-written Swift `encode(to:)` against key drift. Do not change
    /// without bumping PROTOCOL_VERSION.
    #[test]
    fn golden_json_turn_submit() {
        const GOLDEN_TURN_SUBMIT: &str = r#"{"v":1,"seq":2,"msg":{"type":"turn_submit","target_node_id":"mbp-jane","target_agent_id":"jane","conversation_id":"conv-9","content":"hello there","blob_refs":[{"blob_id":"blob-1","download_url":"https://example/blob-1","mime":"audio/ogg"}],"message_kind":"voice"}}"#;

        let envelope = EdgeEnvelope::new(
            2,
            None,
            EdgeMessage::TurnSubmit {
                target_node_id: "mbp-jane".to_string(),
                target_agent_id: "jane".to_string(),
                conversation_id: Some("conv-9".to_string()),
                content: "hello there".to_string(),
                blob_refs: vec![BlobRef {
                    blob_id: "blob-1".to_string(),
                    download_url: Some("https://example/blob-1".to_string()),
                    mime: Some("audio/ogg".to_string()),
                }],
                message_kind: Some("voice".to_string()),
            },
        );
        assert_eq!(
            serde_json::to_string(&envelope).unwrap(),
            GOLDEN_TURN_SUBMIT
        );
        let back: EdgeEnvelope = serde_json::from_str(GOLDEN_TURN_SUBMIT).unwrap();
        assert_eq!(back, envelope);
    }

    /// Golden wire fixture for a minimal `TurnSubmit`: no conversation_id and
    /// an empty `blob_refs` — both must be OMITTED on the wire, not encoded
    /// as null / []. Do not change without bumping PROTOCOL_VERSION.
    #[test]
    fn golden_json_turn_submit_minimal() {
        const GOLDEN_TURN_SUBMIT_MINIMAL: &str = r#"{"v":1,"seq":3,"msg":{"type":"turn_submit","target_node_id":"mbp-jane","target_agent_id":"jane","content":"hello"}}"#;

        let envelope = EdgeEnvelope::new(
            3,
            None,
            EdgeMessage::TurnSubmit {
                target_node_id: "mbp-jane".to_string(),
                target_agent_id: "jane".to_string(),
                conversation_id: None,
                content: "hello".to_string(),
                blob_refs: vec![],
                message_kind: None,
            },
        );
        assert_eq!(
            serde_json::to_string(&envelope).unwrap(),
            GOLDEN_TURN_SUBMIT_MINIMAL
        );
        let back: EdgeEnvelope = serde_json::from_str(GOLDEN_TURN_SUBMIT_MINIMAL).unwrap();
        assert_eq!(back, envelope);
    }

    /// Golden wire fixture for `VoiceReply` — copy-pastable into Swift
    /// Codable tests. Do not change without bumping PROTOCOL_VERSION.
    #[test]
    fn golden_json_voice_reply() {
        const GOLDEN_VOICE_REPLY: &str = r#"{"v":1,"seq":8,"msg":{"type":"voice_reply","conversation_id":"conv-9","turn_id":"turn-3","audio_base64":"bW9jay1hdWRpbw==","mime_type":"audio/mpeg","transcript":"hello there"}}"#;

        let envelope = EdgeEnvelope::new(
            8,
            None,
            EdgeMessage::VoiceReply {
                conversation_id: "conv-9".to_string(),
                turn_id: Some("turn-3".to_string()),
                audio_base64: "bW9jay1hdWRpbw==".to_string(),
                mime_type: "audio/mpeg".to_string(),
                transcript: Some("hello there".to_string()),
                chunk_seq: None,
                is_final: None,
            },
        );
        assert_eq!(
            serde_json::to_string(&envelope).unwrap(),
            GOLDEN_VOICE_REPLY
        );
        let back: EdgeEnvelope = serde_json::from_str(GOLDEN_VOICE_REPLY).unwrap();
        assert_eq!(back, envelope);
    }

    /// Golden wire fixture for `ApprovalResolve` — copy-pastable into Swift
    /// Codable tests. Do not change without bumping PROTOCOL_VERSION.
    #[test]
    fn golden_json_approval_resolve() {
        const GOLDEN_APPROVAL_RESOLVE: &str = r#"{"v":1,"seq":4,"ack":9,"msg":{"type":"approval_resolve","approval_id":"appr-1","approved":true,"note":"looks fine"}}"#;

        let envelope = EdgeEnvelope::new(
            4,
            Some(9),
            EdgeMessage::ApprovalResolve {
                approval_id: "appr-1".to_string(),
                approved: true,
                note: Some("looks fine".to_string()),
            },
        );
        assert_eq!(
            serde_json::to_string(&envelope).unwrap(),
            GOLDEN_APPROVAL_RESOLVE
        );
        let back: EdgeEnvelope = serde_json::from_str(GOLDEN_APPROVAL_RESOLVE).unwrap();
        assert_eq!(back, envelope);
    }

    /// Golden wire fixture for `ToolResult` — copy-pastable into Swift
    /// Codable tests. Do not change without bumping PROTOCOL_VERSION.
    #[test]
    fn golden_json_tool_result() {
        const GOLDEN_TOOL_RESULT: &str = r#"{"v":1,"seq":5,"msg":{"type":"tool_result","invocation_id":"inv-1","ok":true,"result_json":"{\"steps\":1200}"}}"#;

        let envelope = EdgeEnvelope::new(
            5,
            None,
            EdgeMessage::ToolResult {
                invocation_id: "inv-1".to_string(),
                ok: true,
                result_json: "{\"steps\":1200}".to_string(),
            },
        );
        assert_eq!(
            serde_json::to_string(&envelope).unwrap(),
            GOLDEN_TOOL_RESULT
        );
        let back: EdgeEnvelope = serde_json::from_str(GOLDEN_TOOL_RESULT).unwrap();
        assert_eq!(back, envelope);
    }

    /// Golden wire fixture for `CapabilitiesUpdate` — copy-pastable into
    /// Swift Codable tests. Do not change without bumping PROTOCOL_VERSION.
    #[test]
    fn golden_json_capabilities_update() {
        const GOLDEN_CAPABILITIES_UPDATE: &str = r#"{"v":1,"seq":6,"msg":{"type":"capabilities_update","capabilities":{"device_name":"Jared's iPhone","platform":"ios","roles":["ClientNode","ModelNode"],"tools":["os.ios.healthkit.read@1"],"models":["stt.whisper.coreml-tiny-en@1"]}}}"#;

        let envelope = EdgeEnvelope::new(
            6,
            None,
            EdgeMessage::CapabilitiesUpdate {
                capabilities: caps(),
            },
        );
        assert_eq!(
            serde_json::to_string(&envelope).unwrap(),
            GOLDEN_CAPABILITIES_UPDATE
        );
        let back: EdgeEnvelope = serde_json::from_str(GOLDEN_CAPABILITIES_UPDATE).unwrap();
        assert_eq!(back, envelope);
    }

    /// Golden wire fixture for `TurnEvent` — copy-pastable into Swift Codable
    /// tests. Do not change without bumping PROTOCOL_VERSION.
    #[test]
    fn golden_json_turn_event() {
        const GOLDEN_TURN_EVENT: &str = r#"{"v":1,"seq":12,"ack":5,"msg":{"type":"turn_event","conversation_id":"conv-9","event_kind":"token","content":"Hel","turn_id":"turn-4"}}"#;

        let envelope = EdgeEnvelope::new(
            12,
            Some(5),
            EdgeMessage::TurnEvent {
                conversation_id: "conv-9".to_string(),
                event_kind: TurnEventKind::Token,
                content: "Hel".to_string(),
                turn_id: Some("turn-4".to_string()),
            },
        );
        assert_eq!(serde_json::to_string(&envelope).unwrap(), GOLDEN_TURN_EVENT);
        let back: EdgeEnvelope = serde_json::from_str(GOLDEN_TURN_EVENT).unwrap();
        assert_eq!(back, envelope);
    }
}
