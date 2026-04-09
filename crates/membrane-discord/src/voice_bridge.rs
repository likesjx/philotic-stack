/// Discord Voice Bridge — wires Discord's UDP/RTP voice stream to the Philotic hotel.
///
/// Pipeline (inbound):
///   Discord UDP (Opus/RTP) → decrypt → decode Opus → PCM i16 → VAD → utterance → EmitTask
///
/// Pipeline (outbound):
///   hotel send_voice_reply (PCM i16 base64) → encode Opus → encrypt → Discord UDP (RTP)
///
/// Slice 2 scope:
///   - Per-SSRC Opus decoders (one per remote speaker)
///   - Shared outbound Opus encoder
///   - Simple energy-threshold VAD with silence-gated utterance segmentation
///   - `VoiceBridge` struct + background task
///
/// Does NOT handle:
///   - SDP / WebRTC — that's Slice 3 if hotel grows a WebRtcGuest
///   - Multi-speaker diarization — loudest-speaker-wins for now (OQ2 decision)
use anyhow::Result;
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use opus::{Application, Channels, Decoder as OpusDecoder, Encoder as OpusEncoder};
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::crypto::VoiceEncryptionState;
use crate::voice_gateway::VoiceSession;
use crate::voice_udp::VoiceUdpTransport;

// ── Audio constants ────────────────────────────────────────────────────────────
/// Opus frame size for 20 ms at 48 kHz (standard Discord frame duration).
const FRAME_SAMPLES_PER_CHANNEL: usize = 960;
/// Discord voice is always stereo.
const CHANNELS: usize = 2;
/// Total i16 samples per 20 ms stereo frame.
const FRAME_SAMPLES: usize = FRAME_SAMPLES_PER_CHANNEL * CHANNELS;
/// Maximum Opus encoded frame bytes (safe upper bound).
const MAX_OPUS_BYTES: usize = 4000;
/// Sample rate used throughout the Discord voice pipeline.
pub const SAMPLE_RATE: u32 = 48_000;

// ── VAD constants ──────────────────────────────────────────────────────────────
/// RMS energy level above which a frame is considered speech.
/// 300 ≈ -40 dBFS for i16 range — tune empirically.
const VAD_SPEECH_THRESHOLD: f64 = 300.0;
/// Number of consecutive silent 20 ms frames before an utterance is flushed.
/// 25 frames × 20 ms = 500 ms trailing silence.
const VAD_SILENCE_FRAMES: usize = 25;
/// Minimum speech frames before emitting (suppress very short spurts / noise).
/// 5 frames × 20 ms = 100 ms minimum utterance.
const VAD_MIN_SPEECH_FRAMES: usize = 5;
/// Maximum frames to buffer per utterance before force-flushing (~10 s of audio).
const VAD_MAX_FRAMES: usize = 500;

// ── Public types ───────────────────────────────────────────────────────────────

/// An decoded utterance ready to send to the hotel as a `voice.dialogue` task.
#[derive(Debug)]
pub struct VoiceUtteranceEvent {
    pub guild_id: String,
    pub channel_id: String,
    pub session_id: String,
    /// SSRC of the loudest/only detected speaker.
    pub speaker_ssrc: u32,
    /// Base64-encoded i16 LE PCM at 48 kHz stereo.
    pub pcm_b64: String,
}

/// Handle to a running VoiceBridge background task.
///
/// Drop the handle (or send on `_shutdown_tx`) to stop the background task.
pub struct VoiceBridge {
    pub guild_id: String,
    pub channel_id: String,
    pub session_id: String,
    /// Text channel where /join was invoked — replies from the agent go here.
    pub text_channel_id: String,
    /// Send PCM i16 samples here to have them encoded as Opus and transmitted to Discord.
    pub audio_tx: mpsc::Sender<Vec<i16>>,
    _shutdown_tx: oneshot::Sender<()>,
}

impl VoiceBridge {
    /// Spawn the bridge background task and return the handle.
    ///
    /// `utterance_tx` is a shared channel that all bridges write decoded utterances to.
    /// The caller (main loop) reads from the corresponding receiver and emits tasks.
    pub fn start(
        session: VoiceSession,
        utterance_tx: mpsc::Sender<VoiceUtteranceEvent>,
        hotel_session_id: String,
        text_channel_id: String,
    ) -> Result<Self> {
        let guild_id = session.guild_id.clone();
        let channel_id = session.channel_id.clone();
        let bot_ssrc = session.bot_ssrc;

        // Separate inbound/outbound crypto state — they share the same key but
        // track independent nonce counters (decrypt reads nonce from packet,
        // encrypt increments an outbound counter).
        let crypto_in =
            VoiceEncryptionState::new(session.mode.clone(), session.crypto.secret_key().to_vec());
        let crypto_out = session.crypto;

        let encoder = OpusEncoder::new(SAMPLE_RATE, Channels::Stereo, Application::Voip)?;

        let udp = std::sync::Arc::new(session.udp_transport);
        let (audio_tx, audio_rx) = mpsc::channel::<Vec<i16>>(16);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let guild_id_bg = guild_id.clone();
        let channel_id_bg = channel_id.clone();
        let session_id_bg = hotel_session_id.clone();

        tokio::spawn(async move {
            run_bridge(
                udp,
                crypto_in,
                crypto_out,
                encoder,
                guild_id_bg,
                channel_id_bg,
                session_id_bg,
                bot_ssrc,
                utterance_tx,
                audio_rx,
                shutdown_rx,
            )
            .await;
        });

        Ok(VoiceBridge {
            guild_id,
            channel_id,
            session_id: hotel_session_id,
            text_channel_id,
            audio_tx,
            _shutdown_tx: shutdown_tx,
        })
    }

    /// Feed raw PCM i16 samples (48 kHz stereo) to be encoded and sent to Discord.
    pub async fn send_pcm(&self, samples: Vec<i16>) {
        if let Err(e) = self.audio_tx.send(samples).await {
            warn!("VoiceBridge: audio_tx send error: {}", e);
        }
    }
}

// ── Per-SSRC VAD state machine ─────────────────────────────────────────────────

enum VadState {
    Idle,
    Speaking {
        frames: Vec<Vec<i16>>,
        silent_count: usize,
    },
}

fn frame_rms(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
    (sum_sq / samples.len() as f64).sqrt()
}

fn flush_utterance(
    ssrc: u32,
    frames: Vec<Vec<i16>>,
    guild_id: &str,
    channel_id: &str,
    session_id: &str,
    utterance_tx: &mpsc::Sender<VoiceUtteranceEvent>,
) {
    if frames.len() < VAD_MIN_SPEECH_FRAMES {
        debug!(
            "VoiceBridge: SSRC {} utterance too short ({} frames), discarding",
            ssrc,
            frames.len()
        );
        return;
    }

    // Flatten all frames into a single Vec<i16>
    let total: usize = frames.iter().map(|f| f.len()).sum();
    let mut pcm = Vec::with_capacity(total);
    for frame in frames {
        pcm.extend_from_slice(&frame);
    }

    // Convert i16 samples to bytes (LE), then base64
    let bytes: Vec<u8> = pcm.iter().flat_map(|&s| s.to_le_bytes()).collect();
    let pcm_b64 = B64.encode(&bytes);

    let event = VoiceUtteranceEvent {
        guild_id: guild_id.to_string(),
        channel_id: channel_id.to_string(),
        session_id: session_id.to_string(),
        speaker_ssrc: ssrc,
        pcm_b64,
    };

    // Non-blocking send — if the channel is full the utterance is lost rather
    // than blocking the receive loop.
    if let Err(e) = utterance_tx.try_send(event) {
        warn!(
            "VoiceBridge: utterance channel full, dropping (SSRC {}): {}",
            ssrc, e
        );
    }
}

// ── Bridge background task ─────────────────────────────────────────────────────

async fn run_bridge(
    udp: std::sync::Arc<VoiceUdpTransport>,
    crypto_in: VoiceEncryptionState,
    mut crypto_out: VoiceEncryptionState,
    mut encoder: OpusEncoder,
    guild_id: String,
    channel_id: String,
    session_id: String,
    bot_ssrc: u32,
    utterance_tx: mpsc::Sender<VoiceUtteranceEvent>,
    mut audio_rx: mpsc::Receiver<Vec<i16>>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    info!(
        "VoiceBridge: running for guild={} channel={}",
        guild_id, channel_id
    );

    let mut decoders: HashMap<u32, OpusDecoder> = HashMap::new();
    let mut vad: HashMap<u32, VadState> = HashMap::new();

    loop {
        tokio::select! {
            biased;

            // Shutdown signal
            _ = &mut shutdown_rx => {
                info!("VoiceBridge: shutdown received, stopping (guild={})", guild_id);
                break;
            }

            // Outbound: PCM from hotel → Opus encode → Discord UDP
            pcm = audio_rx.recv() => {
                let Some(samples) = pcm else { break };
                match encoder.encode_vec(&samples, MAX_OPUS_BYTES) {
                    Ok(opus_bytes) => {
                        if let Err(e) = udp.send_opus_frame(&mut crypto_out, &opus_bytes).await {
                            warn!("VoiceBridge: send_opus_frame failed: {}", e);
                        }
                    }
                    Err(e) => warn!("VoiceBridge: Opus encode failed: {}", e),
                }
            }

            // Inbound: Discord UDP → Opus decode → VAD → utterance
            rtp_result = udp.recv_rtp_packet(&crypto_in) => {
                match rtp_result {
                    Ok(Some((ssrc, opus_bytes))) => {
                        // Skip our own transmitted audio
                        if ssrc == bot_ssrc {
                            continue;
                        }

                        // Get or create a decoder for this speaker
                        let decoder = decoders.entry(ssrc).or_insert_with(|| {
                            debug!("VoiceBridge: new Opus decoder for SSRC {}", ssrc);
                            OpusDecoder::new(SAMPLE_RATE, Channels::Stereo)
                                .expect("OpusDecoder::new failed")
                        });

                        // Decode to PCM
                        let mut pcm_buf = vec![0i16; FRAME_SAMPLES];
                        match decoder.decode(&opus_bytes, &mut pcm_buf, false) {
                            Ok(n) => {
                                pcm_buf.truncate(n * CHANNELS);

                                // VAD
                                let rms = frame_rms(&pcm_buf);
                                let is_speech = rms >= VAD_SPEECH_THRESHOLD;

                                let state = vad.entry(ssrc).or_insert(VadState::Idle);
                                match state {
                                    VadState::Idle => {
                                        if is_speech {
                                            *state = VadState::Speaking {
                                                frames: vec![pcm_buf],
                                                silent_count: 0,
                                            };
                                        }
                                    }
                                    VadState::Speaking { frames, silent_count } => {
                                        frames.push(pcm_buf);

                                        if is_speech {
                                            *silent_count = 0;
                                        } else {
                                            *silent_count += 1;
                                        }

                                        let should_flush = *silent_count >= VAD_SILENCE_FRAMES
                                            || frames.len() >= VAD_MAX_FRAMES;

                                        if should_flush {
                                            let drained_frames = std::mem::take(frames);
                                            flush_utterance(
                                                ssrc,
                                                drained_frames,
                                                &guild_id,
                                                &channel_id,
                                                &session_id,
                                                &utterance_tx,
                                            );
                                            *state = VadState::Idle;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                debug!("VoiceBridge: Opus decode error for SSRC {}: {}", ssrc, e);
                            }
                        }
                    }
                    Ok(None) => {} // skipped packet
                    Err(e) => {
                        warn!("VoiceBridge: recv_rtp_packet error: {}", e);
                    }
                }
            }
        }
    }

    // Flush any in-progress utterances on shutdown
    for (ssrc, state) in vad {
        if let VadState::Speaking { frames, .. } = state {
            flush_utterance(
                ssrc,
                frames,
                &guild_id,
                &channel_id,
                &session_id,
                &utterance_tx,
            );
        }
    }

    info!("VoiceBridge: task exiting for guild={}", guild_id);
}

// ── Helpers ────────────────────────────────────────────────────────────────────

/// Decode base64 PCM (i16 LE) into a Vec<i16>.
///
/// Used to decode `send_voice_reply` payloads from the hotel.
pub fn decode_pcm_b64(b64: &str) -> Result<Vec<i16>> {
    let bytes = B64.decode(b64)?;
    if bytes.len() % 2 != 0 {
        anyhow::bail!("PCM base64 payload has odd byte count");
    }
    let samples = bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    Ok(samples)
}
