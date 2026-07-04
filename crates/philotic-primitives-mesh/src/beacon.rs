use crate::event::NodeId;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::ops::Deref;
use std::sync::atomic::{AtomicU8, Ordering};
use uuid::Uuid;

/// The core envelope for UDP mesh communication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeaconMessage {
    /// Protocol version (e.g., 1)
    pub version: u8,
    /// Unique identifier for this message
    pub msg_id: Uuid,
    /// Originating node
    pub src_node: NodeId,
    /// Destination node (or "broadcast" / "group:X")
    pub dest_node: String,
    /// Message type identifier
    pub msg_type: MsgType,
    /// Sequence number for fragmented messages (0 for unfragmented)
    pub seq: u32,
    /// Total fragments in this sequence (1 for unfragmented)
    pub total: u32,
    /// Encoded payload (JSON, MsgPack, or CBOR). See [`BeaconPayload`] for the
    /// dual wire encoding (legacy JSON int array vs base64 string).
    pub payload: BeaconPayload,
    /// Creation timestamp (Unix epoch secs)
    pub timestamp: u64,
    /// HMAC signature for integrity and authenticity. Uses the same dual wire
    /// encoding as `payload`.
    pub hmac: BeaconPayload,
}

/// Wire encoding selector for [`BeaconPayload`] serialization.
///
/// serde_json encodes a plain `Vec<u8>` as a JSON integer array
/// (`[123,34,...]`) — a payload-dependent 2.5–4x wire inflation that blows
/// realistic `HotelStateSync` datagrams past macOS's 9216-byte
/// `net.inet.udp.maxdgram` ceiling. Base64 encoding costs only ~1.33x.
///
/// Rollout is two-step safe: receivers ALWAYS accept both encodings
/// ([`BeaconPayload`]'s `Deserialize` is dual-read), while senders keep
/// emitting the legacy int-array encoding until the operator flips
/// `PHILOTIC_BEACON_PAYLOAD_B64=1` (or code calls
/// [`BeaconPayload::set_wire_encoding`]). Flip senders only after every
/// receiver on the mesh runs a dual-read build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireEncoding {
    /// JSON array of integers — the historical serde default for `Vec<u8>`.
    LegacyIntArray,
    /// Base64 (standard alphabet, padded) JSON string.
    Base64,
}

const ENCODING_UNSET: u8 = 0;
const ENCODING_LEGACY: u8 = 1;
const ENCODING_BASE64: u8 = 2;

/// Process-wide wire-encoding mode. Starts unset; resolved from
/// `PHILOTIC_BEACON_PAYLOAD_B64` on first serialize (read once, cached) unless
/// [`BeaconPayload::set_wire_encoding`] has already pinned it.
static WIRE_ENCODING: AtomicU8 = AtomicU8::new(ENCODING_UNSET);

/// Raw beacon payload bytes with a dual-read wire encoding.
///
/// Deserializes from EITHER a JSON integer array (legacy) or a base64 string
/// (new), regardless of the process-wide mode. Serializes according to the
/// active [`WireEncoding`] (default: legacy, so updated senders stay parseable
/// by not-yet-updated receivers until the fleet-wide flag flip).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BeaconPayload(Vec<u8>);

impl BeaconPayload {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Consumes the wrapper, returning the raw payload bytes.
    pub fn into_inner(self) -> Vec<u8> {
        self.0
    }

    /// Pins the process-wide serialize encoding, overriding any value cached
    /// from `PHILOTIC_BEACON_PAYLOAD_B64`.
    pub fn set_wire_encoding(mode: WireEncoding) {
        let value = match mode {
            WireEncoding::LegacyIntArray => ENCODING_LEGACY,
            WireEncoding::Base64 => ENCODING_BASE64,
        };
        WIRE_ENCODING.store(value, Ordering::SeqCst);
    }

    /// The encoding serialization will use right now. Resolves and caches the
    /// `PHILOTIC_BEACON_PAYLOAD_B64` env var on first call if no explicit
    /// [`Self::set_wire_encoding`] happened earlier.
    pub fn wire_encoding() -> WireEncoding {
        match WIRE_ENCODING.load(Ordering::SeqCst) {
            ENCODING_LEGACY => WireEncoding::LegacyIntArray,
            ENCODING_BASE64 => WireEncoding::Base64,
            _ => {
                let from_env = match std::env::var("PHILOTIC_BEACON_PAYLOAD_B64") {
                    Ok(value) if value.trim() == "1" => ENCODING_BASE64,
                    _ => ENCODING_LEGACY,
                };
                // Only claim the slot if still unset so a concurrent explicit
                // set_wire_encoding always wins.
                let _ = WIRE_ENCODING.compare_exchange(
                    ENCODING_UNSET,
                    from_env,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
                match WIRE_ENCODING.load(Ordering::SeqCst) {
                    ENCODING_BASE64 => WireEncoding::Base64,
                    _ => WireEncoding::LegacyIntArray,
                }
            }
        }
    }
}

impl Deref for BeaconPayload {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<[u8]> for BeaconPayload {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for BeaconPayload {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl From<BeaconPayload> for Vec<u8> {
    fn from(payload: BeaconPayload) -> Self {
        payload.0
    }
}

impl Serialize for BeaconPayload {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match Self::wire_encoding() {
            WireEncoding::LegacyIntArray => self.0.serialize(serializer),
            WireEncoding::Base64 => serializer.serialize_str(&BASE64_STANDARD.encode(&self.0)),
        }
    }
}

impl<'de> Deserialize<'de> for BeaconPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PayloadVisitor;

        impl<'de> Visitor<'de> for PayloadVisitor {
            type Value = BeaconPayload;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a byte integer array or a base64-encoded string")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut bytes = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(byte) = seq.next_element::<u8>()? {
                    bytes.push(byte);
                }
                Ok(BeaconPayload(bytes))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                BASE64_STANDARD
                    .decode(value)
                    .map(BeaconPayload)
                    .map_err(|err| E::custom(format!("invalid base64 beacon payload: {err}")))
            }

            fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(BeaconPayload(value.to_vec()))
            }

            fn visit_byte_buf<E>(self, value: Vec<u8>) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(BeaconPayload(value))
            }
        }

        deserializer.deserialize_any(PayloadVisitor)
    }
}

/// Identifiers for the types of messages sent over the beacon.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MsgType {
    /// Deploy an agent bundle to a remote node
    DeployAgent,
    /// Execute a deployed agent with an input
    RunAgent,
    /// Invoke a specific tool returning results
    ToolCall,
    /// Pull secrets from the authority node
    SecretPull,
    /// Mesh presence and health update
    Heartbeat,
    /// A chunk of capability advertisements and reachability metadata.
    CapabilitySync,
    /// Asynchronous result delivery
    Result,
    /// Streaming execution logs
    Log,
    /// Model Manager routing requests
    ModelManager,
    /// Reading/writing from the Graph memory apartments
    MemoryOp,
    /// A batch of EventEnvelopes dispatched over the durable mesh
    MeshEventBatch,
    /// An acknowledgment of durably received mesh events
    MeshEventAck,
    /// A batch of EventEnvelopes sent over the reliable execution plane
    ExecutionEventBatch,
    /// An acknowledgment of durably received execution-plane events
    ExecutionEventAck,
    /// A membership acceptance packet emitted after an invite is accepted
    MeshMembershipAccept,
    /// Propagated mesh membership records used to converge the shared trust view
    MeshMembershipSync,
    /// Propagated mesh-global tool, skill, and profile catalog records
    MeshCatalogSync,
    /// WebRTC Session Description Protocol (SDP) and ICE candidate signaling
    WebRtcSignal,
    /// Full hotel roster (guests + agents) broadcast so every peer can route correctly
    HotelStateSync,
}

#[cfg(test)]
mod tests {
    use super::{BeaconMessage, BeaconPayload, MsgType, WireEncoding};
    use std::sync::Mutex;
    use uuid::Uuid;

    /// The wire-encoding mode is process-wide; tests that pin it must not
    /// interleave. (Dual-read deserialization is mode-independent, so only
    /// serialize-side assertions need this guard.)
    static ENCODING_GUARD: Mutex<()> = Mutex::new(());

    fn with_wire_encoding<T>(mode: WireEncoding, run: impl FnOnce() -> T) -> T {
        let _guard = ENCODING_GUARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        BeaconPayload::set_wire_encoding(mode);
        let out = run();
        BeaconPayload::set_wire_encoding(WireEncoding::LegacyIntArray);
        out
    }

    fn sample_message(payload: &[u8]) -> BeaconMessage {
        BeaconMessage {
            version: 1,
            msg_id: Uuid::new_v4(),
            src_node: "node-a".to_string(),
            dest_node: "broadcast".to_string(),
            msg_type: MsgType::HotelStateSync,
            seq: 0,
            total: 1,
            payload: payload.to_vec().into(),
            timestamp: 1_720_000_000,
            hmac: vec![7u8; 32].into(),
        }
    }

    #[test]
    fn payload_deserializes_from_legacy_int_array() {
        let bytes: Vec<u8> = vec![0, 1, 127, 128, 255, 34, 92];
        let json = serde_json::to_string(&bytes).expect("encode int array");
        let decoded: BeaconPayload = serde_json::from_str(&json).expect("decode legacy array");
        assert_eq!(&*decoded, bytes.as_slice());
    }

    #[test]
    fn payload_deserializes_from_base64_string() {
        let bytes: Vec<u8> = b"{\"hello\":\"mesh\"}".to_vec();
        let json = format!(
            "\"{}\"",
            base64::Engine::encode(&super::BASE64_STANDARD, &bytes)
        );
        let decoded: BeaconPayload = serde_json::from_str(&json).expect("decode base64 string");
        assert_eq!(&*decoded, bytes.as_slice());

        let bad: Result<BeaconPayload, _> = serde_json::from_str("\"not!!valid@@base64\"");
        assert!(bad.is_err(), "invalid base64 must be rejected");
    }

    #[test]
    fn legacy_mode_serializes_as_int_array_and_round_trips() {
        with_wire_encoding(WireEncoding::LegacyIntArray, || {
            let msg = sample_message(b"legacy payload bytes");
            let wire = serde_json::to_string(&msg).expect("serialize");
            assert!(
                wire.contains("\"payload\":[108,101,103"),
                "legacy mode must emit an int array, got: {wire}"
            );
            let decoded: BeaconMessage = serde_json::from_str(&wire).expect("dual-read parse");
            assert_eq!(&*decoded.payload, b"legacy payload bytes");
            assert_eq!(&*decoded.hmac, &[7u8; 32]);
        });
    }

    #[test]
    fn base64_mode_serializes_as_string_and_round_trips_via_dual_read() {
        with_wire_encoding(WireEncoding::Base64, || {
            let msg = sample_message(b"base64 payload bytes");
            let wire = serde_json::to_string(&msg).expect("serialize");
            assert!(
                wire.contains("\"payload\":\""),
                "base64 mode must emit a string payload, got: {wire}"
            );
            assert!(
                !wire.contains("\"payload\":["),
                "base64 mode must not emit an int array"
            );
            let decoded: BeaconMessage = serde_json::from_str(&wire).expect("dual-read parse");
            assert_eq!(&*decoded.payload, b"base64 payload bytes");
            assert_eq!(&*decoded.hmac, &[7u8; 32]);
        });
    }

    #[test]
    fn base64_mode_shrinks_wire_size_versus_legacy() {
        // A JSON inner payload: legacy int-array encoding inflates ~4x, the
        // base64 encoding only ~1.33x.
        let inner = serde_json::json!({
            "node_id": "mbp-jane-aiua-01",
            "guests": (0..100).map(|ix| format!("hotel:model-controller-guest-{ix}")).collect::<Vec<_>>(),
        });
        let inner_bytes = serde_json::to_vec(&inner).expect("encode inner");

        let legacy_len = with_wire_encoding(WireEncoding::LegacyIntArray, || {
            serde_json::to_vec(&sample_message(&inner_bytes))
                .expect("serialize")
                .len()
        });
        let base64_len = with_wire_encoding(WireEncoding::Base64, || {
            serde_json::to_vec(&sample_message(&inner_bytes))
                .expect("serialize")
                .len()
        });
        assert!(
            base64_len * 2 < legacy_len,
            "base64 wire ({base64_len}B) should be far smaller than legacy ({legacy_len}B)"
        );
    }

    #[test]
    fn execution_plane_msg_types_serialize_with_stable_names() {
        let batch = serde_json::to_string(&MsgType::ExecutionEventBatch).expect("serialize batch");
        let ack = serde_json::to_string(&MsgType::ExecutionEventAck).expect("serialize ack");

        assert_eq!(batch, "\"EXECUTION_EVENT_BATCH\"");
        assert_eq!(ack, "\"EXECUTION_EVENT_ACK\"");
        assert_eq!(
            serde_json::from_str::<MsgType>(&batch).expect("deserialize batch"),
            MsgType::ExecutionEventBatch
        );
        assert_eq!(
            serde_json::from_str::<MsgType>(&ack).expect("deserialize ack"),
            MsgType::ExecutionEventAck
        );
    }
}
