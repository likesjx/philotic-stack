/// Discord Voice UDP transport.
///
/// Handles:
/// - IP Discovery (RFC-style discovery to learn our external IP/port)
/// - Building and parsing RTP packets (Discord's wire format)
/// - Bidirectional Opus/RTP over UDP to Discord's SFU
/// - Calling into crypto.rs for AEAD encryption/decryption
///
/// Does NOT handle:
/// - Opus codec encode/decode (that's voice_codec.rs)
/// - WebRTC bridging (that's voice_bridge.rs, Slice 2)
use anyhow::{bail, Result};
use byteorder::{BigEndian, ByteOrder};
use std::{
    net::{IpAddr, SocketAddr},
    sync::{
        atomic::{AtomicU16, AtomicU32, Ordering},
        Arc,
    },
};
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

use crate::crypto::VoiceEncryptionState;

// RTP header constants per Discord spec
const RTP_VERSION_FLAGS: u8 = 0x80;
const RTP_PAYLOAD_TYPE: u8 = 0x78; // 120

/// State for the Discord Voice UDP socket.
pub struct VoiceUdpTransport {
    socket: Arc<UdpSocket>,
    discord_addr: SocketAddr,
    bot_ssrc: u32,
    rtp_sequence: AtomicU16,
    rtp_timestamp: AtomicU32,
}

impl VoiceUdpTransport {
    /// Bind a UDP socket, perform Discord IP Discovery, return the transport
    /// and our discovered external (IP, port).
    pub async fn connect_and_discover(
        discord_ip: &str,
        discord_port: u16,
        bot_ssrc: u32,
    ) -> Result<(Self, IpAddr, u16)> {
        // Bind to an ephemeral port on all interfaces
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        let discord_addr: SocketAddr = format!("{}:{}", discord_ip, discord_port).parse()?;
        socket.connect(discord_addr).await?;

        info!(
            "Voice UDP: bound to {}, connecting to Discord SFU {}:{}",
            socket.local_addr()?,
            discord_ip,
            discord_port
        );

        // IP Discovery packet: 74 bytes
        // [0..2]   = 0x0001 (discovery request type)
        // [2..4]   = 70 (payload length)
        // [4..8]   = SSRC (big-endian)
        // [8..72]  = null-padded address
        // [72..74] = 0x0000 (port placeholder)
        let mut discovery = [0u8; 74];
        BigEndian::write_u16(&mut discovery[0..2], 0x0001);
        BigEndian::write_u16(&mut discovery[2..4], 70);
        BigEndian::write_u32(&mut discovery[4..8], bot_ssrc);

        socket.send(&discovery).await?;
        debug!("Voice UDP: sent IP discovery packet");

        // Read response (also 74 bytes)
        let mut response = [0u8; 74];
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            socket.recv(&mut response),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Timeout waiting for IP discovery response"))?
        .map_err(|e| anyhow::anyhow!("UDP recv error: {}", e))?;

        if n < 74 {
            bail!("IP discovery response too short: {} bytes", n);
        }

        // Response type should be 0x0002
        let resp_type = BigEndian::read_u16(&response[0..2]);
        if resp_type != 0x0002 {
            bail!("Unexpected IP discovery response type: 0x{:04x}", resp_type);
        }

        // External IP is null-terminated ASCII at bytes 8..72
        let ip_bytes = &response[8..72];
        let ip_end = ip_bytes.iter().position(|&b| b == 0).unwrap_or(64);
        let ip_str = std::str::from_utf8(&ip_bytes[..ip_end])
            .map_err(|_| anyhow::anyhow!("Invalid UTF-8 in IP discovery response"))?;
        let external_ip: IpAddr = ip_str
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid IP in discovery response: {}", ip_str))?;

        // External port is at bytes 72..74
        let external_port = BigEndian::read_u16(&response[72..74]);

        info!(
            "Voice UDP: discovered external address {}:{}",
            external_ip, external_port
        );

        Ok((
            Self {
                socket: Arc::new(socket),
                discord_addr,
                bot_ssrc,
                rtp_sequence: AtomicU16::new(rand_u16()),
                rtp_timestamp: AtomicU32::new(0),
            },
            external_ip,
            external_port,
        ))
    }

    /// Build an RTP header for an outbound Opus frame.
    ///
    /// Returns the 12-byte RTP header. Caller appends the encrypted payload.
    pub fn next_rtp_header(&self) -> [u8; 12] {
        let seq = self.rtp_sequence.fetch_add(1, Ordering::Relaxed);
        // +960 per 20ms Opus frame at 48kHz
        let ts = self.rtp_timestamp.fetch_add(960, Ordering::Relaxed);

        let mut header = [0u8; 12];
        header[0] = RTP_VERSION_FLAGS;
        header[1] = RTP_PAYLOAD_TYPE;
        BigEndian::write_u16(&mut header[2..4], seq);
        BigEndian::write_u32(&mut header[4..8], ts);
        BigEndian::write_u32(&mut header[8..12], self.bot_ssrc);
        header
    }

    /// Send an Opus frame to Discord's SFU.
    ///
    /// Encrypts the frame, builds the full RTP packet, and transmits it.
    pub async fn send_opus_frame(
        &self,
        crypto: &mut VoiceEncryptionState,
        opus_frame: &[u8],
    ) -> Result<()> {
        let header = self.next_rtp_header();
        let encrypted = crypto.encrypt(&header, opus_frame)?;

        let mut packet = Vec::with_capacity(12 + encrypted.len());
        packet.extend_from_slice(&header);
        packet.extend_from_slice(&encrypted);

        self.socket.send(&packet).await?;
        Ok(())
    }

    /// Send the 5 silence frames required by Discord when stopping transmission.
    pub async fn send_silence_frames(&self, crypto: &mut VoiceEncryptionState) -> Result<()> {
        // Discord silence frame for Opus: 0xF8 0xFF 0xFE
        let silence_frame: &[u8] = &[0xF8, 0xFF, 0xFE];
        for _ in 0..5 {
            self.send_opus_frame(crypto, silence_frame).await?;
        }
        Ok(())
    }

    /// Receive a single RTP packet from Discord's SFU.
    ///
    /// Returns (ssrc, opus_payload) on success, or None if the packet should be skipped.
    pub async fn recv_rtp_packet(
        &self,
        crypto: &VoiceEncryptionState,
    ) -> Result<Option<(u32, Vec<u8>)>> {
        let mut buf = [0u8; 4096];
        let n = self.socket.recv(&mut buf).await?;
        let packet = &buf[..n];

        if n < 12 {
            debug!("Voice UDP: packet too short ({} bytes), skipping", n);
            return Ok(None);
        }

        // Validate RTP version byte
        if packet[0] != RTP_VERSION_FLAGS {
            debug!("Voice UDP: unexpected RTP version byte 0x{:02x}, skipping", packet[0]);
            return Ok(None);
        }

        // Parse RTP header
        let ssrc = BigEndian::read_u32(&packet[8..12]);

        // Determine header length (handle RTP extension bit, bit 4 of byte 0)
        let has_extension = (packet[0] & 0x10) != 0;
        let csrc_count = (packet[0] & 0x0F) as usize;
        let base_header_len = 12 + 4 * csrc_count;

        let header_len = if has_extension && packet.len() >= base_header_len + 4 {
            let ext_len = BigEndian::read_u16(&packet[base_header_len + 2..base_header_len + 4]);
            base_header_len + 4 + 4 * ext_len as usize
        } else {
            base_header_len
        };

        if packet.len() <= header_len {
            debug!("Voice UDP: no payload after RTP header, skipping");
            return Ok(None);
        }

        let rtp_header = &packet[..header_len];
        let encrypted_payload = &packet[header_len..];

        match crypto.decrypt(rtp_header, encrypted_payload) {
            Ok(opus_frame) => Ok(Some((ssrc, opus_frame))),
            Err(e) => {
                warn!("Voice UDP: decrypt failed for SSRC {}: {}", ssrc, e);
                Ok(None)
            }
        }
    }

    pub fn socket(&self) -> Arc<UdpSocket> {
        self.socket.clone()
    }

    pub fn discord_addr(&self) -> SocketAddr {
        self.discord_addr
    }
}

fn rand_u16() -> u16 {
    use rand::Rng;
    rand::thread_rng().r#gen()
}
