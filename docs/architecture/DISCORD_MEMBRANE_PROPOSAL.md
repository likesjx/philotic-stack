---
title: Discord Membrane Proposal
doc_type: proposal
domain: membrane-transport
status: proposed
disposition: accepted-current-slice
last_updated: 2026-03-31
tags:
- discord
- membrane
- voice
- webrtc
- transport
- streaming
related_docs:
- ARCHITECTURE_STATUS.md
- MEMBRANE_COMPONENT_PROPOSAL.md
- TELEGRAM_INTEGRATION_PROPOSAL.md
- TELEGRAM_POLL_LEASE_PROPOSAL.md
- VOICE_MACHINE_PROPOSAL.md
- MODEL_CONTROLLER_PROPOSAL.md
- INTER_HOTEL_ROUTING_PROPOSAL.md
- HOTEL_PERIMETER_TRUST_PROPOSAL.md
task_refs:
- docs/task.md
proposal_id: discord-membrane
implements:
- membrane-component
implemented_by: []
active_seams:
- discord-gateway-session
- discord-voice-webrtc-bridge
- discord-agent-routing-reflex
- discord-voice-lease
source_of_truth_targets:
- ARCHITECTURE_STATUS.md
- ARCHITECTURE.md
---

# Discord Membrane Proposal

## Goal

Define `membrane.discord` as a Philotic membrane implementation that:

- connects to Discord's Gateway and Voice Gateway as a bot
- translates Discord text and voice channel events into normalized Philotic transport envelopes
- negotiates voice channel sessions through the hotel's WebRTC interface
- bridges Discord's UDP/Opus voice pipeline into Hotel WebRTC, which routes to the target agent
- lets the agent's routing reflex control voice ingress/egress model selection, voice identity, and pipeline shape
- follows the membrane contract from MEMBRANE_COMPONENT_PROPOSAL.md without becoming a second session authority or cognitive owner

This proposal intentionally models the Discord membrane after the existing Telegram membrane where the contracts overlap, and diverges only where Discord's transport surface demands it — specifically around voice channel lifecycle, UDP media transport, and the dual-gateway connection model.

## Core Recommendation

Treat `membrane.discord` as a voice-first, text-capable membrane that bridges Discord's proprietary voice transport into the hotel's WebRTC interface for agent-routed voice dialogue.

More precisely:

- `membrane.discord` is one membrane implementation under the membrane component type
- the current `crates/membrane` binary remains the Telegram-oriented implementation
- `membrane.discord` should be a separate guest binary (`crates/membrane-discord`)
- voice is the primary contract — text messaging is a secondary transport that reuses the same normalized envelope contract the Telegram membrane already defines
- the Discord membrane does not own agent cognition, session truth, or model execution
- voice pipeline routing is owned by the agent's routing reflex, not by the membrane

The membrane's job is to get Discord audio into Philotic and Philotic audio back into Discord. Everything in between — which model, which voice, which pipeline shape — is the agent's decision, mediated by the hotel.

## Disposition

`proposed`

Track follow-on work in docs/task.md.

## Why Discord Needs Its Own Membrane (Not a Telegram Extension)

Discord's transport surface is fundamentally different from Telegram's in three ways:

1. **Dual-gateway architecture.** Discord requires a persistent WebSocket to the main Gateway (for guild events, voice state, presence) and a second WebSocket to a dynamically-assigned Voice Gateway (for signaling and voice session negotiation). Telegram's single long-poll or webhook ingress cannot model this.

2. **Real-time UDP voice transport.** Discord voice is Opus-over-UDP with RTP framing, Discord-specific encryption (AEAD AES256-GCM or XChaCha20-Poly1305), and a separate SSRC-keyed mixing plane. This is not a file-upload-and-download voice note flow; it is a continuous bidirectional audio stream.

3. **Voice channel as session context.** In Discord, a voice session is bound to a guild + voice channel and can have multiple simultaneous participants. The membrane must manage channel join/leave lifecycle, speaking state, and per-user SSRC demuxing. Telegram voice is single-turn voice memo; Discord voice is an ongoing conversation.

These differences justify a dedicated membrane binary with its own lifecycle, its own lease contract, and its own transport-specific code — while sharing the normalized envelope, session binding, and reply delivery contracts with the Telegram membrane.

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                        Discord Platform                             │
│                                                                     │
│  ┌──────────────┐    ┌──────────────────┐    ┌───────────────────┐  │
│  │   Gateway     │    │  Voice Gateway    │    │  Voice UDP SFU    │  │
│  │   (WSS)       │    │  (WSS)            │    │  (UDP/Opus/RTP)   │  │
│  └──────┬───────┘    └────────┬─────────┘    └────────┬──────────┘  │
│         │                     │                       │              │
└─────────┼─────────────────────┼───────────────────────┼──────────────┘
          │                     │                       │
          │ events, voice       │ signaling             │ encrypted
          │ state updates       │ (identify, select,    │ opus/rtp
          │                     │  session desc,        │ frames
          │                     │  speaking)            │
          ▼                     ▼                       ▼
┌─────────────────────────────────────────────────────────────────────┐
│                   membrane.discord  (guest binary)                   │
│                                                                     │
│  ┌──────────────┐  ┌───────────────────┐  ┌──────────────────────┐  │
│  │ Gateway       │  │ Voice Gateway      │  │ Opus/RTP             │  │
│  │ Client        │  │ Client             │  │ Transport            │  │
│  │               │  │                    │  │                      │  │
│  │ • guild events│  │ • identify/ready   │  │ • UDP recv/send      │  │
│  │ • voice state │  │ • select protocol  │  │ • RTP de/encap       │  │
│  │ • message     │  │ • session desc     │  │ • Opus decode/encode │  │
│  │   create      │  │ • speaking state   │  │ • encryption         │  │
│  │ • presence    │  │ • heartbeat        │  │ • SSRC demux         │  │
│  └──────┬───────┘  └────────┬──────────┘  └────────┬─────────────┘  │
│         │                    │                      │                │
│         ▼                    ▼                      ▼                │
│  ┌──────────────────────────────────────────────────────────────┐    │
│  │                    Session Coordinator                        │    │
│  │                                                              │    │
│  │  • maps guild+channel → philotic session_id                  │    │
│  │  • maps discord user_id → philotic sender identity           │    │
│  │  • manages voice channel join/leave lifecycle                │    │
│  │  • holds voice lease from hotel                              │    │
│  │  • bridges decoded opus ↔ hotel WebRTC peer connection       │    │
│  │  • normalizes text messages into transport envelopes         │    │
│  │  • projects agent replies back into discord text/voice       │    │
│  └──────────────────────────┬───────────────────────────────────┘    │
│                              │                                      │
└──────────────────────────────┼──────────────────────────────────────┘
                               │ IPC (TCP)
                               ▼
┌──────────────────────────────────────────────────────────────────────┐
│                          aiua  (hotel daemon)                        │
│                                                                     │
│  ┌────────────┐  ┌──────────────┐  ┌────────────────────────────┐   │
│  │ Guest       │  │ Lease         │  │ WebRTC Guest               │   │
│  │ Registry    │  │ Registry      │  │ (SDP/ICE signaling         │   │
│  │             │  │               │  │  + DataChannel bridge)     │   │
│  └──────┬─────┘  └──────┬───────┘  └────────────┬───────────────┘   │
│         │               │                        │                   │
│         ▼               ▼                        ▼                   │
│  ┌───────────────────────────────────────────────────────────────┐   │
│  │                     Router / Placement                         │   │
│  │                                                               │   │
│  │  routes voice.dialogue / response.generate / voice.transcribe │   │
│  │  routes text.generate for text channel messages                │   │
│  │  agent routing reflex governs pipeline shape                  │   │
│  └───────────────────────────────────────┬───────────────────────┘   │
│                                          │                           │
│                                          ▼                           │
│  ┌───────────────────────────────────────────────────────────────┐   │
│  │  philote  (agent loop)  ←→  model-router  ←→  model guests    │   │
│  │                                                               │   │
│  │  agent decides: voice model, voice id, pipeline shape,        │   │
│  │  text fallback, turn boundaries, interrupt policy             │   │
│  └───────────────────────────────────────────────────────────────┘   │
│                                                                     │
└──────────────────────────────────────────────────────────────────────┘
```

## What The Discord Membrane Is

The Discord membrane is the translator, guard, and delivery provider between Discord's communication surface and the internal Philotic world.

It is responsible for:

- maintaining a persistent Gateway WebSocket connection for guild/channel/message events
- maintaining a secondary Voice Gateway WebSocket connection when the bot is in a voice channel
- authenticating with Discord using a bot token
- managing voice channel join/leave lifecycle via Gateway Opcode 4
- negotiating voice transport via Voice Gateway (Identify → Ready → Select Protocol → Session Description)
- operating the UDP voice transport: receiving encrypted Opus/RTP from Discord's SFU, decrypting, decoding, and bridging to Hotel WebRTC
- encoding agent audio output as Opus, encrypting with Discord's transport encryption, and transmitting as RTP over UDP back to Discord's SFU
- translating text channel messages into normalized Philotic transport envelopes
- projecting agent text/voice replies back into the appropriate Discord channel
- binding Discord guild+channel+user identities to Philotic session state
- holding a voice-channel lease from the hotel (modeled after the Telegram poll lease)
- respecting the membrane boundary: not owning cognition, session truth, or model execution

It is not:

- the owner of which model handles voice
- the owner of which voice identity the agent uses
- the owner of turn boundary detection or interrupt policy
- a voice-processing pipeline (that's the voice machine / model-controller domain)
- a second session authority that competes with the context graph

## Membrane Responsibilities

### 1. Gateway Ingress

The Discord membrane maintains a persistent WebSocket connection to Discord's main Gateway.

Relevant events consumed:

| Discord Event | Membrane Action |
|---|---|
| `READY` | Populate guild/channel cache, confirm bot identity |
| `GUILD_CREATE` | Register available guilds and voice channels |
| `MESSAGE_CREATE` | Normalize into `MembraneIngressEnvelope` → forward to philote |
| `VOICE_STATE_UPDATE` | Track user join/leave/move in voice channels |
| `VOICE_SERVER_UPDATE` | Capture voice server endpoint + token for voice session setup |
| `INTERACTION_CREATE` | Handle slash commands (elevated to philote if agent-scoped) |
| `MESSAGE_REACTION_ADD` | Optional: map to callback/approval flows |

The Gateway connection requires:

- `GUILDS`, `GUILD_MESSAGES`, `GUILD_VOICE_STATES`, `MESSAGE_CONTENT` intents
- Bot token authentication
- Heartbeat maintenance (Opcode 1 → Opcode 11 ACK on the main Gateway)
- Resume/reconnect handling for Gateway disconnects

### 2. Voice Gateway Signaling

When the bot joins a voice channel, a second WebSocket connection is opened to the dynamically-assigned Voice Gateway endpoint.

Voice Gateway flow (mirrors Discord's documented sequence):

```
Bot → Main Gateway:  Opcode 4 Voice State Update
                     { guild_id, channel_id, self_mute: false, self_deaf: false }

Main Gateway → Bot:  VOICE_STATE_UPDATE  (session_id)
Main Gateway → Bot:  VOICE_SERVER_UPDATE (token, endpoint)

Bot → Voice GW:      Opcode 0 Identify
                     { server_id, user_id, session_id, token }

Voice GW → Bot:      Opcode 8 Hello      (heartbeat_interval)
Voice GW → Bot:      Opcode 2 Ready      (ssrc, ip, port, modes)

Bot → Voice UDP:     IP Discovery         (discover external IP/port)

Bot → Voice GW:      Opcode 1 Select Protocol
                     { protocol: "udp", data: { address, port, mode } }

Voice GW → Bot:      Opcode 4 Session Description
                     { mode, secret_key }

Bot → Voice GW:      Opcode 5 Speaking    { speaking: 1, ssrc }
```

Encryption mode preference order:

1. `aead_aes256_gcm_rtpsize` (preferred)
2. `aead_xchacha20_poly1305_rtpsize` (required fallback)

The membrane must maintain Voice Gateway heartbeats independently of the main Gateway heartbeat.

### 3. Voice UDP Transport

Once the Voice Gateway handshake completes, the membrane operates a UDP socket for bidirectional Opus audio.

#### Inbound (Discord → Philotic)

```
Discord SFU  →  UDP  →  membrane  →  Hotel WebRTC
                         │
                         ├── receive encrypted RTP packet
                         ├── validate RTP header (version 0x80, payload type 0x78)
                         ├── decrypt using session secret_key + nonce
                         ├── extract Opus frame from RTP payload
                         ├── demux by SSRC (identify which Discord user is speaking)
                         ├── decode Opus → PCM (48kHz stereo)
                         ├── feed PCM into Hotel WebRTC peer connection audio track
                         └── (or: feed raw Opus into WebRTC if codec-compatible)
```

#### Outbound (Philotic → Discord)

```
Hotel WebRTC  →  membrane  →  UDP  →  Discord SFU
                  │
                  ├── receive PCM/Opus from Hotel WebRTC audio track
                  ├── encode PCM → Opus (48kHz, stereo, 20ms frames) if needed
                  ├── construct RTP header (seq++, timestamp += 960, bot SSRC)
                  ├── encrypt Opus payload with session secret_key
                  ├── send Speaking (Opcode 5) before first frame
                  ├── transmit encrypted RTP over UDP to Discord SFU
                  └── send 5 silence frames (0xF8 0xFF 0xFE) on speech end
```

RTP packet structure (per Discord spec):

| Field | Value | Size |
|---|---|---|
| Version + Flags | `0x80` | 1 byte |
| Payload Type | `0x78` | 1 byte |
| Sequence | big-endian u16, incrementing | 2 bytes |
| Timestamp | big-endian u32, +960 per 20ms frame | 4 bytes |
| SSRC | bot's SSRC from Opcode 2 Ready | 4 bytes |
| Encrypted Opus | AEAD-encrypted Opus frame | variable |

### 4. Hotel WebRTC Bridge

This is the critical seam: the membrane bridges Discord's proprietary voice transport into the hotel's existing WebRTC interface.

The bridge operates as follows:

1. **Membrane initiates a WebRTC session with the hotel.** When a voice channel session is established, the membrane creates a local `RTCPeerConnection` and generates an SDP offer.

2. **SDP exchange over IPC.** The membrane sends the SDP offer to the hotel via the existing `WebRtcSignalMessage` IPC path. The hotel's `WebRtcGuest` answers with its SDP answer. ICE candidates are exchanged over the same IPC channel.

3. **Audio track bridging.** Once the WebRTC peer connection is established:
   - Inbound: decoded Opus/PCM from Discord UDP is written to a `MediaStreamTrack` on the WebRTC peer connection → flows to the hotel → routed to the agent's voice pipeline
   - Outbound: audio from the agent (via model-router → voice machine → WebRTC) arrives on the peer connection's remote audio track → membrane encodes to Opus → encrypts → sends as RTP over Discord UDP

4. **DataChannel for metadata.** A WebRTC DataChannel carries sideband metadata between the membrane and the hotel:
   - speaker identity (which Discord user is talking)
   - speaking start/stop events
   - turn boundary signals
   - agent routing reflex directives (model switch, voice change, pipeline reconfiguration)
   - session lifecycle events (user joined/left channel)

```rust
/// The Discord membrane's bridge to the hotel WebRTC interface.
pub struct DiscordVoiceBridge {
    /// The WebRTC peer connection to the hotel
    peer_connection: Arc<RTCPeerConnection>,
    /// Audio track: membrane → hotel (user speech)
    ingress_track: Arc<TrackLocalStaticSample>,
    /// Audio track: hotel → membrane (agent speech)
    egress_track: Option<Arc<TrackRemote>>,
    /// DataChannel for sideband metadata
    metadata_channel: Arc<RTCDataChannel>,
    /// Discord UDP socket for voice transport
    discord_udp: Arc<UdpSocket>,
    /// Voice session encryption state
    crypto: VoiceEncryptionState,
    /// Bot's SSRC assigned by Discord
    bot_ssrc: u32,
    /// RTP sequence number
    rtp_sequence: AtomicU16,
    /// RTP timestamp
    rtp_timestamp: AtomicU32,
}
```

### 5. Agent Routing Reflex Integration

The agent's routing reflex is the policy authority for how voice flows through the system. The membrane does not decide these things; it executes what the reflex dictates.

The routing reflex controls:

| Decision | Owner | Mechanism |
|---|---|---|
| Which model handles voice input | Agent routing reflex | `voice.dialogue` or `voice.transcribe` → `text.generate` capability route |
| Which voice identity speaks | Agent routing reflex | `VoiceResponsePolicy.voice_id` on the agent profile |
| Whether to use native multimodal voice | Agent routing reflex | `response.generate` with `audio` output modality |
| Whether to use TTS pipeline | Agent routing reflex | `voice.synthesize` capability route to ElevenLabs / other provider |
| Turn boundary detection | Agent routing reflex | VAD policy, silence threshold, interrupt handling |
| Interrupt policy | Agent routing reflex | Whether agent speech can be interrupted by user speech |
| Fallback on voice failure | Agent routing reflex | `VoiceResponsePolicy.fallback_to_text` → text channel reply |

The membrane communicates with the routing reflex through the hotel's standard IPC:

```
membrane → hotel:  IngressEnvelope { transport: "discord", kind: "voice_stream", ... }
hotel → agent:     (routing reflex evaluates, selects pipeline)
agent → hotel:     voice.dialogue / response.generate / voice.transcribe + text.generate
hotel → model:     (model-router dispatches to selected provider)
model → hotel:     audio artifact / text response
hotel → membrane:  FinalReplyPayload { audio_artifact, display_text, ... }
membrane → discord: opus frames over UDP / text message via REST
```

#### Pipeline Shapes

The routing reflex can select from at least three pipeline shapes:

**Shape A: Native multimodal voice dialogue**
```
discord opus → hotel WebRTC → agent → response.generate (Gemini Live / OpenAI Realtime)
                                        → native audio + text
                                          → hotel WebRTC → membrane → discord opus
                                          → text channel (if send_text_caption)
```

**Shape B: Transcribe → Reason → Synthesize**
```
discord opus → hotel WebRTC → voice.transcribe (Whisper / Gemini)
                                → transcript text
                                  → text.generate (any LLM)
                                    → response text
                                      → voice.synthesize (ElevenLabs)
                                        → audio artifact
                                          → hotel WebRTC → membrane → discord opus
```

**Shape C: Transcribe → Reason → Text-only reply**
```
discord opus → hotel WebRTC → voice.transcribe
                                → transcript text
                                  → text.generate
                                    → response text
                                      → text channel message (no voice reply)
```

The agent profile configures the default shape. The routing reflex can override per-turn based on context (e.g., switch to text-only for code output).

### 6. Session Binding

The Discord membrane maps Discord identities to Philotic session state using the same patterns as the Telegram membrane, with Discord-specific keying:

| Discord Concept | Philotic Mapping |
|---|---|
| Guild ID | Namespace / tenant boundary |
| Channel ID (text) | `session_id` component |
| Channel ID (voice) | `session_id` component + voice session qualifier |
| Thread ID | `thread_id` (if applicable) |
| User ID | `sender_id` |
| Username#Discriminator | `sender_username` |
| Message ID | `turn_id` basis |
| Bot Application ID | `agent_id` binding |

Session key format: `discord:{guild_id}:{channel_id}[:voice]`

Voice sessions carry both a text session binding (for the text channel associated with the voice channel or a designated text channel) and a voice session binding (for the audio stream).

### 7. Egress Projection

The membrane projects Philotic replies back into Discord-native UX:

| Philotic Reply Type | Discord Projection |
|---|---|
| Text reply | `POST /channels/{id}/messages` (with markdown → Discord markdown conversion) |
| Audio artifact (from voice pipeline) | Opus frames transmitted over UDP to Discord SFU |
| Audio artifact (from non-streaming TTS) | Opus-encode and transmit, or upload as attachment |
| Image/file attachment | Discord file upload via multipart |
| Approval request | Discord button components (`INTERACTION_CREATE` callback) |
| Streaming partial reply | Edit existing message progressively (like Telegram membrane's draft editing) |

For voice egress specifically:

- Streaming audio from `response.generate` or `voice.synthesize` should be transmitted as Opus frames in real time, not buffered
- The membrane must send Speaking (Opcode 5) before the first audio frame and silence frames after the last
- If the agent produces both text and audio, both are delivered: audio over UDP, text to the associated text channel

## Lease Model

Following the Telegram poll lease pattern from TELEGRAM_POLL_LEASE_PROPOSAL.md, the Discord membrane requires two lease types:

### Gateway Lease

Prevents split-brain when multiple Discord membrane instances could be configured with the same bot token.

- The agent's home hotel owns the canonical Gateway lease record for each Discord bot token
- Only one Gateway connection may exist per bot token at a time
- Lease grants carry a fencing epoch
- Standby membranes wait for lease acquisition, not speculative connection
- Lease is released on intentional shutdown, expires on disconnect

This is structurally identical to the Telegram poll lease, keyed by Discord bot token fingerprint instead of Telegram bot token fingerprint.

### Voice Channel Lease

Per-channel voice session authority:

- Acquired when the bot joins a voice channel
- Scoped to `{bot_token_fingerprint}:{guild_id}:{channel_id}`
- Prevents two membrane instances from attempting to operate the same voice channel simultaneously
- Released when the bot leaves the voice channel or the voice session ends
- Expires if the membrane disconnects without clean release

```rust
/// Discord-specific lease types managed by the hotel
pub enum DiscordLeaseKind {
    /// Authority to maintain the Gateway WebSocket for this bot token
    Gateway {
        token_fingerprint: String,
    },
    /// Authority to operate a voice session in a specific channel
    VoiceChannel {
        token_fingerprint: String,
        guild_id: String,
        channel_id: String,
    },
}
```

## Normalized Envelope Contract

The Discord membrane normalizes inbound events into the same `MembraneIngressEnvelope` shape used by the Telegram membrane, extended for voice:

```rust
/// Normalized inbound transport envelope (shared with Telegram membrane)
pub struct MembraneIngressEnvelope {
    pub transport: &'static str,       // "discord"
    pub session_id: String,            // discord:{guild}:{channel}[:voice]
    pub turn_id: String,               // message snowflake or voice turn UUID
    pub chat_id: String,               // channel_id
    pub thread_id: Option<String>,     // thread_id if in a thread
    pub sender_id: Option<String>,     // Discord user snowflake
    pub sender_username: Option<String>,// username
    pub message_kind: &'static str,    // "text", "voice_stream", "slash_command", "reaction"
    pub content: String,               // text content or transcript
    pub attachments: Vec<Value>,       // blob-backed attachments
    pub command: Option<String>,       // slash command name
    pub callback_data: Option<String>, // interaction callback data
    pub raw_transport_event: Value,    // raw Discord event JSON

    // Discord voice extensions
    pub voice_session: Option<VoiceSessionRef>,
}

/// Reference to an active voice streaming session
pub struct VoiceSessionRef {
    pub guild_id: String,
    pub channel_id: String,
    pub ssrc: u32,
    pub webrtc_session_id: String,     // hotel-side WebRTC session identifier
    pub speaker_ssrc_map: HashMap<u32, String>,  // SSRC → Discord user_id
    /// Text channel for agent text replies during this voice session.
    /// Resolved at join time from /join invocation channel, or default_text_channel config.
    pub text_channel_id: String,
}
```

## Guest Identity and IPC Registration

The Discord membrane registers with the hotel using the established `GuestIdentity` contract:

```rust
let identity = GuestIdentity {
    guest_id: format!("membrane-discord-{}", bot_token_fingerprint),
    role: "membrane".to_string(),
    node: local_node_id(),
    display_name: Some(format!("Discord ({})", bot_username)),
    transport: Some("discord".to_string()),
    ..Default::default()
};

// Register with hotel
client.send(IpcRequest::Register(identity)).await?;

// Subscribe to the membrane inbox for this agent
client.send(IpcRequest::SubscribeInbox {
    role: "membrane".to_string(),
}).await?;
```

The membrane also registers itself as a component:

```rust
client.send(IpcRequest::RegisterComponent {
    guest_id: guest_id.clone(),
    component_type: "membrane.discord".to_string(),
    capabilities: vec![
        "transport.text".to_string(),
        "transport.voice".to_string(),
        "transport.voice_stream".to_string(),
        "transport.slash_command".to_string(),
    ],
}).await?;
```

## Voice Pipeline Lifecycle

### Join Flow

```
1. Trigger: operator command, agent decision, or user @mention in voice channel

2. membrane → hotel:
     IpcRequest::AcquireLease {
       kind: DiscordLeaseKind::VoiceChannel { ... },
       agent_id,
       requested_duration_secs: 3600,
     }

3. hotel → membrane:
     IpcResponse::LeaseGranted { lease_id, epoch, expires_at }

4. membrane → Discord Gateway:
     Opcode 4 Voice State Update { guild_id, channel_id }

5. Discord → membrane:
     VOICE_STATE_UPDATE { session_id }
     VOICE_SERVER_UPDATE { token, endpoint }

6. membrane → Discord Voice Gateway:
     connect WSS to endpoint
     Opcode 0 Identify { server_id, user_id, session_id, token }

7. Discord Voice GW → membrane:
     Opcode 8 Hello { heartbeat_interval }
     Opcode 2 Ready { ssrc, ip, port, modes }

8. membrane → Discord Voice UDP:
     IP Discovery (send ssrc bytes, receive external IP/port)

9. membrane → Discord Voice GW:
     Opcode 1 Select Protocol { udp, address, port, aead_aes256_gcm_rtpsize }

10. Discord Voice GW → membrane:
      Opcode 4 Session Description { mode, secret_key }

11. membrane → hotel (IPC):
      WebRtcSignalMessage {
        session_id: voice_session_id,
        signal: SignalPayload::Offer(sdp_offer),
      }

12. hotel → membrane (IPC):
      WebRtcSignalMessage {
        signal: SignalPayload::Answer(sdp_answer),
      }

13. WebRTC peer connection established
    Audio tracks connected
    DataChannel open for metadata

14. membrane → Discord Voice GW:
      Opcode 5 Speaking { speaking: 1, ssrc }

15. Voice pipeline is live.
    Decoded Discord audio → WebRTC → hotel → agent routing reflex → model pipeline
    Agent audio → WebRTC → membrane → encrypted Opus/RTP → Discord SFU
```

### Leave Flow

```
1. Trigger: operator command, agent decision, voice channel empty, lease expiry

2. membrane → Discord Voice GW:
     send 5 silence frames (0xF8 0xFF 0xFE)
     Opcode 5 Speaking { speaking: 0, ssrc }

3. membrane → Discord Gateway:
     Opcode 4 Voice State Update { guild_id, channel_id: null }

4. membrane → hotel (IPC):
     WebRtcSignalMessage { signal: SignalPayload::SessionEnded }

5. Close WebRTC peer connection
   Close Voice Gateway WebSocket
   Close UDP socket

6. membrane → hotel:
     IpcRequest::ReleaseLease { lease_id }

7. Voice session state cleaned up
```

### Interruption Handling

When a user speaks while the agent is speaking (barge-in):

```
1. membrane detects incoming SSRC audio while agent audio is being transmitted
2. membrane sends interrupt signal over DataChannel metadata:
     { "type": "speaker_interrupt", "user_id": "...", "ssrc": ... }
3. hotel forwards interrupt to agent routing reflex
4. agent routing reflex decides:
   a. IGNORE — continue speaking (e.g., finishing a critical instruction)
   b. PAUSE — stop transmitting, listen, resume after silence
   c. ABORT — stop current response, process new input
5. membrane executes the reflex decision:
   - PAUSE/ABORT: stop sending Opus frames, send silence frames
   - New user input flows through the voice pipeline normally
```

## Text Channel Integration

Text messaging in Discord follows the same normalization contract as Telegram, with minor transport differences:

### Inbound Text

```
Discord MESSAGE_CREATE → normalize → MembraneIngressEnvelope {
    transport: "discord",
    session_id: "discord:{guild_id}:{channel_id}",
    message_kind: "text",
    content: message.content,
    attachments: [...],
    ...
}
→ forward to philote via hotel IPC
```

### Outbound Text

```
philote reply → FinalReplyPayload → membrane →
    POST https://discord.com/api/v10/channels/{channel_id}/messages
    {
        "content": discord_formatted_text,
        "message_reference": { "message_id": reply_to_id }
    }
```

Markdown conversion: Philotic's internal markdown should be projected into Discord's markdown dialect (which is close to standard but has some differences in code blocks, spoilers, and mentions).

### Slash Commands

Discord slash commands map to the same command elevation pattern as Telegram:

```rust
// Slash command registration (on bot startup or guild join)
let commands = vec![
    SlashCommand { name: "role", description: "Switch agent role" },
    SlashCommand { name: "tts", description: "Toggle voice response mode" },
    SlashCommand { name: "voice", description: "Voice pipeline controls" },
    SlashCommand { name: "status", description: "Agent status" },
    SlashCommand { name: "join", description: "Join your voice channel" },
    SlashCommand { name: "leave", description: "Leave voice channel" },
];

// Inbound slash command → elevated to philote
MembraneIngressEnvelope {
    message_kind: "slash_command",
    command: Some("role"),
    content: interaction.options_as_string(),
    ...
}
```

## Crate Structure

```
crates/membrane-discord/
├── Cargo.toml
├── src/
│   ├── main.rs                  # Entry point, CLI args, guest registration
│   ├── gateway.rs               # Discord Gateway WebSocket client
│   ├── voice_gateway.rs         # Discord Voice Gateway WebSocket client
│   ├── voice_udp.rs             # UDP transport: RTP framing, encryption, Opus
│   ├── voice_bridge.rs          # Hotel WebRTC ↔ Discord UDP bridge
│   ├── session.rs               # Session binding, guild/channel → philotic session
│   ├── envelope.rs              # Ingress/egress envelope normalization
│   ├── lease.rs                 # Gateway + voice channel lease management
│   ├── commands.rs              # Slash command registration and handling
│   ├── markdown.rs              # Discord markdown projection
│   └── crypto.rs                # AEAD encryption/decryption for voice transport
```

Dependencies (Rust):

- `tokio` — async runtime
- `tokio-tungstenite` — WebSocket for Gateway and Voice Gateway
- `opus` — Opus codec encode/decode
- `webrtc` — Hotel-side WebRTC peer connection (same crate as `webrtc_guest.rs`)
- `ring` or `aes-gcm` + `chacha20poly1305` — Discord voice encryption
- `philotic-client` — IPC to hotel
- `ansible-mesh-core` — WebRTC signal types, event types
- `serde` / `serde_json` — Discord API payloads
- `reqwest` — Discord REST API for message sending, slash command registration

## Parallels With Telegram Membrane

| Concern | Telegram Membrane | Discord Membrane |
|---|---|---|
| **Ingress transport** | Long-poll `getUpdates` / webhook | Gateway WebSocket (persistent) |
| **Lease authority** | Poll lease per bot token | Gateway lease per bot token + voice lease per channel |
| **Normalized envelope** | `TelegramMessageEnvelope` → generic `MembraneIngressEnvelope` | Same `MembraneIngressEnvelope`, extended with `VoiceSessionRef` |
| **Outbound text** | `sendMessage` REST | `POST /channels/{id}/messages` REST |
| **Voice ingress** | Voice note → `getFile` → blob → `voice.transcribe` | Continuous Opus stream → WebRTC → voice pipeline |
| **Voice egress** | `voice.synthesize` → `sendVoice` REST | Audio track → Opus/RTP over UDP |
| **Identity binding** | `chat_id` → session | `guild_id:channel_id` → session |
| **Command surface** | Telegram bot commands | Discord slash commands |
| **Reply threading** | `reply_to_message_id` | `message_reference` |
| **Guest identity** | `membrane-telegram-01` | `membrane-discord-{fingerprint}` |
| **Component type** | `membrane` (transitional) | `membrane.discord` |
| **Markdown projection** | HTML via `pulldown-cmark` | Discord markdown dialect |

## Configuration Shape

Discord membrane configuration should live in the agent identity bundle, following the same pattern as Telegram configuration:

```json
{
  "agent_id": "hermes",
  "membrane_discord": {
    "bot_token_ref": "secret:discord/hermes/bot_token",
    "application_id": "123456789",
    "guild_allowlist": ["guild_id_1", "guild_id_2"],
    "default_text_channel": "channel_id_for_text_replies",
    "voice_auto_join": false,
    "voice_auto_join_channels": [],
    "voice_idle_timeout_secs": 300,
    "voice_max_session_secs": 3600
  },
  "voice_response_policy": {
    "mode": "auto",
    "provider": "elevenlabs",
    "voice_id": "hermes_voice_id",
    "model": "eleven_multilingual_v2",
    "fallback_to_text": true
  },
  "media_routing_policy": {
    "voice_action": "dialogue",
    "strip_tools_on_media": false
  }
}
```

`voice_action: "dialogue"` is the key differentiator from Telegram's `"transcribe"` default. For Discord voice, the default pipeline should be conversational dialogue (Shape A or B), not one-shot transcription.

## Security Posture

### Bot Token

- Never stored in plain config; referenced via `secret:` URI from the hotel's vault
- Token fingerprint (SHA-256 prefix) used for lease keying, not the raw token
- Membrane receives the usable token from the hotel secret IPC at startup

### Voice Encryption

- Discord mandates transport encryption between client and SFU
- Preferred: `aead_aes256_gcm_rtpsize`
- The membrane must implement the encryption correctly; unencrypted audio is rejected by Discord
- DAVE (E2EE) protocol support is deferred from this first slice but should be designed as an extension point

### Guild/Channel Authorization

- `guild_allowlist` restricts which guilds the bot operates in
- Voice channel joins should be gated by the agent's security policy
- The membrane should refuse to join voice channels not in the allowlist
- User identity from Discord should be cross-referenced with any operator/trust policies before allowing voice interaction

### Rate Limiting

- Discord enforces rate limits on REST API calls (message sending, slash commands)
- The membrane must implement rate limit awareness with backoff
- Voice Gateway and UDP transport are not rate-limited in the same way but have connection limits

## Deferred From First Slice

This proposal intentionally does not yet define:

- DAVE protocol (E2EE) support for voice
- Multi-user voice mixing (initial slice: loudest-speaker-wins active speaker demux)
- Speaker diarization / voice-print recognition for per-user identity in group voice (envelope already designed for it via `speaker_ssrc_map`)
- Video channel support
- Stage channel support (one-to-many broadcast)
- Screen share / Go Live streaming
- Discord Activities integration
- Cross-guild voice routing
- Voice channel permission enforcement beyond allowlist
- Advanced VAD (voice activity detection) — initial slice uses Discord's Speaking opcode events
- Opus codec parameter tuning (bitrate, FEC, PLC) beyond Discord defaults
- Watched-live validation (first slice is smoke-green only)

## Implementation Slice Order

### Slice 0: Skeleton + Gateway Text

- `crates/membrane-discord` crate with CLI args, guest registration, hotel IPC
- Gateway WebSocket connection with bot token auth
- Heartbeat, reconnect, resume
- `MESSAGE_CREATE` → normalized `MembraneIngressEnvelope` → philote
- Text reply delivery via Discord REST
- Gateway lease (modeled from Telegram poll lease)
- Slash command registration (`/status`, `/join`, `/leave`)
- Startup smoke test with fake Discord Gateway

### Slice 1: Voice Gateway + UDP Transport

- Voice State Update / Voice Server Update event handling
- Voice Gateway WebSocket client (Identify → Ready → Select Protocol → Session Description)
- UDP socket: IP Discovery, RTP framing, AEAD encryption/decryption
- Opus encode/decode
- Voice channel lease
- Speaking state management (Opcode 5)
- Silence frame transmission on speech end
- Smoke test: bot joins voice channel, receives/sends Opus frames

### Slice 2: Hotel WebRTC Bridge

- SDP offer/answer exchange with hotel's `WebRtcGuest` over IPC
- `RTCPeerConnection` with audio tracks
- DataChannel for metadata (speaker identity, speaking state)
- Decoded Discord Opus → WebRTC audio track → hotel
- Hotel WebRTC audio track → Opus encode → Discord UDP
- End-to-end smoke: Discord voice → hotel → model → hotel → Discord voice

### Slice 3: Agent Routing Reflex Integration

- Wire voice ingress to `voice.dialogue` / `voice.transcribe` capability routes
- Wire agent routing reflex decisions back through DataChannel
- Pipeline shape selection (A/B/C) based on agent profile
- Interrupt handling (barge-in detection and reflex response)
- Voice identity projection from `VoiceResponsePolicy`
- Watched-live validation: full voice conversation with real Discord and real model

### Slice 4: Production Hardening

- Rate limiting and backoff for REST API
- Graceful degradation on voice failure (fallback to text)
- Voice idle timeout (auto-leave after silence)
- Session cleanup on unexpected disconnect
- Multi-guild support with guild-scoped session isolation
- Metrics: voice session duration, latency, packet loss
- DAVE protocol extension point (not full implementation)

## Testing Strategy

### Smoke Tests

Following the Telegram membrane's smoke test pattern:

- `just smoke-discord-gateway` — fake Discord Gateway, text message roundtrip
- `just smoke-discord-voice` — fake Voice Gateway, Opus frame encode/decode/encrypt/decrypt
- `just smoke-discord-voice-bridge` — fake Discord voice + real hotel WebRTC bridge roundtrip
- `just smoke-discord-full` — end-to-end: text + voice through agent loop with fake Discord

### Live Validation

- `just watch-discord-text` — live text message roundtrip with real Discord bot
- `just watch-discord-voice` — live voice session with real Discord bot in real voice channel
- `just watch-discord-voice-agent` — live conversational voice with real model inference

## Decisions

### OQ1: Opus passthrough vs. transcode → **Always transcode through PCM**

Passthrough would save the decode+re-encode roundtrip (~1–2ms compute), but the latency gain is negligible compared to model inference latency. The real concern is security: passing raw Opus bitstream from an external source (Discord's SFU) directly into the hotel's WebRTC pipeline creates a bleed path. A malformed or adversarially crafted Opus frame could reach the hotel's audio processing internals without inspection.

Transcoding through PCM provides a natural sanitization boundary: Opus → PCM is just raw audio samples, which are structurally inert. Re-encoding PCM → Opus produces a clean frame that originated from within the membrane. This is the right architecture — similar to how image pipelines decode and re-encode to strip embedded payloads. The hotel mesh should never see raw bytes from an external stream.

**Decision:** Always transcode Discord Opus → PCM → Opus. No passthrough mode.

### OQ2: Multi-speaker handling → **Loudest speaker wins; speaker diarization deferred**

Initial slice: single active speaker at a time. The membrane demuxes by SSRC but forwards only the dominant speaker (loudest RMS over a short window, matching Discord's own "speaking" indicator) to the agent. Multi-speaker scenarios are out of scope for the first implementation — the expected use case is a one-to-one conversation.

Speaker diarization (voice-print recognition to identify *who* is speaking, not just *that* someone is speaking) is a meaningful future extension — it would let the agent maintain per-user context in a group voice channel. The `speaker_ssrc_map` in `VoiceSessionRef` is already designed to carry this when it's added. No structural changes needed; just a model and policy layer on top.

**Decision:** Loudest speaker wins for initial slice. Speaker diarization is a future add, not a deferred design gap — the envelope already supports it.

### OQ3: Voice channel auto-join → **Controlled by agent profile; default is explicit `/join`**

`voice_auto_join: false` is the safe default. Opt-in via `voice_auto_join_channels` in the membrane config for specific channels where presence-triggered join makes sense.

### OQ4: Text channel association → **Default to invocation channel; stored in voice session state**

Discord text channels and voice channels are separate objects. When the bot is in a voice channel and produces a text reply (code output, captions, fallback text), it needs a target text channel.

**Decision:** The text channel where `/join` was invoked becomes the associated text channel for the duration of that voice session. The membrane stores `invocation_channel_id` in the voice session state at join time. The `default_text_channel` config field is the fallback for auto-join scenarios where there is no invocation context.

This means `VoiceSessionRef` should carry `text_channel_id: String` — resolved at join time from invocation context or config.

### OQ5: Lease duration for voice → **60s heartbeat, 5-minute duration with auto-renewal, 1-hour max**

Suggested values from the original proposal stand. Operator can override max via config.
