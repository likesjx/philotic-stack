//! Sentence-pipelined streaming TTS for operator-chat voice turns.
//!
//! For turns arriving over transport `operator_chat` with an active
//! (non-native-audio) voice policy, philote synthesizes speech PER SENTENCE
//! while the model is still generating, instead of one batch synthesis at
//! turn completion. Each synthesized sentence is forwarded to the turn's
//! final reply target as an `action: "voice_chunk"` task:
//!
//! ```json
//! {"action":"voice_chunk","session_id":...,"turn_id":...,"chat_id":...,
//!  "content":"<sentence text>","audio_artifact":"<AudioArtifact JSON>",
//!  "chunk_seq":N,"is_final":true|false}
//! ```
//!
//! Ordering contract: chunks are emitted in `chunk_seq` order and the final
//! text reply (`action: "send_reply"`, no `audio_artifact`) is emitted only
//! after the last chunk. Ordering is enforced by synthesizing strictly
//! sequentially — sentence N's audio is forwarded before sentence N+1 is
//! dispatched — and by giving every synthesis request a synthetic
//! `"<turn_id>::vchunk::<seq>"` turn id so the responses come back to
//! philote (never to the transport) and can never be mistaken for the
//! turn's own model response.
//!
//! Failure honesty: a failed sentence synthesis is logged and skipped — the
//! text still streams as `partial_reply`s and the final `send_reply` always
//! happens. If every chunk fails the turn degrades to today's text-only
//! behavior. Synthesis failures never stall or kill the turn.

use super::*;
use std::collections::VecDeque;

/// Minimum sentence length (in chars) before a mid-stream sentence boundary
/// is honored. Shorter fragments ("Hi.") are merged into the next sentence so
/// we don't waste a synthesis round-trip on two words.
pub(super) const MIN_VOICE_CHUNK_CHARS: usize = 20;

/// Marker embedded in the synthetic turn id of per-sentence synthesis
/// requests. Responses carrying it are routed to the chunk handler instead of
/// the normal model-response path.
const VOICE_CHUNK_TURN_MARKER: &str = "::vchunk::";

/// A sentence dispatched to the model-router and awaiting its audio.
#[derive(Debug, Clone)]
pub(super) struct InFlightVoiceChunk {
    pub dispatch_seq: u64,
    pub text: String,
}

/// The turn's final reply, stashed while the tail of the sentence queue
/// drains. Delivered via `deliver_text_reply` after the last chunk.
#[derive(Debug)]
pub(super) struct PendingFinalVoiceReply {
    pub content: String,
    pub send_text_caption: bool,
    pub memory_concept: Option<String>,
    pub memory_candidate: Option<MemoryCandidate>,
}

/// Per-turn streaming synthesis state. Lives on the runtime (never
/// checkpointed): a crash mid-turn simply falls back to the watchdog path,
/// exactly like the batch WaitingVoice flow.
#[derive(Debug, Default)]
pub(super) struct VoiceChunkPipeline {
    /// The real turn id this pipeline belongs to. Stale pipelines (turn ended,
    /// new turn started) are detected by comparing against the active turn.
    pub turn_id: String,
    /// Byte offset into the turn's `streamed_content` already sentence-split.
    pub consumed_bytes: usize,
    /// Sentences awaiting synthesis, in order.
    pub queue: VecDeque<String>,
    /// The sentence currently at the model-router. At most one — sequential
    /// dispatch is what guarantees chunk_seq ordering.
    pub in_flight: Option<InFlightVoiceChunk>,
    /// Correlation counter for synthesis dispatches (includes failed ones).
    pub dispatch_seq: u64,
    /// Next `chunk_seq` for a successfully synthesized chunk.
    pub next_chunk_seq: u64,
    /// Chunks actually forwarded to the listener.
    pub chunks_emitted: u64,
    /// Set when the model finished generating: the remaining queue drains and
    /// then `final_reply` is delivered.
    pub finalizing: bool,
    pub final_reply: Option<PendingFinalVoiceReply>,
}

impl VoiceChunkPipeline {
    pub fn new(turn_id: String) -> Self {
        Self {
            turn_id,
            ..Self::default()
        }
    }
}

/// True when this turn should use sentence-pipelined streaming TTS instead of
/// the batch whole-reply synthesis. Only operator_chat transports qualify —
/// membranes like Telegram must keep receiving a single voice note.
pub(super) fn streaming_voice_eligible(
    transport: Option<&str>,
    source: Option<&str>,
    policy: &VoiceResponsePolicy,
    had_voice_input: bool,
) -> bool {
    let is_operator_chat = transport == Some("operator_chat") || source == Some("operator_chat");
    is_operator_chat
        && policy.is_active(had_voice_input)
        && !policy.delivery_mode.is_native_audio()
        && policy.stream_sentences.unwrap_or(true)
}

/// Synthetic turn id for a per-sentence synthesis request.
pub(super) fn voice_chunk_turn_id(turn_id: &str, dispatch_seq: u64) -> String {
    format!("{turn_id}{VOICE_CHUNK_TURN_MARKER}{dispatch_seq}")
}

/// Recognize (and decompose) a synthetic voice-chunk turn id.
pub(super) fn parse_voice_chunk_turn_id(turn_id: &str) -> Option<(String, u64)> {
    let idx = turn_id.rfind(VOICE_CHUNK_TURN_MARKER)?;
    let seq = turn_id[idx + VOICE_CHUNK_TURN_MARKER.len()..]
        .parse()
        .ok()?;
    Some((turn_id[..idx].to_string(), seq))
}

/// Split the leading COMPLETE sentences off `buffer`.
///
/// A sentence boundary is sentence-ending punctuation (`.` `!` `?` `…`)
/// followed by whitespace, or a newline. A boundary is only honored once the
/// accumulated candidate is at least `min_chars` chars (shorter candidates
/// merge into the following sentence). Punctuation at the very end of the
/// buffer is NOT a boundary mid-stream — more tokens may still arrive (e.g.
/// `"3."` + `"14"`); the caller flushes the remainder at generation end.
///
/// Returns the extracted sentences (trimmed) and the number of BYTES of
/// `buffer` they consumed; `buffer[consumed..]` is the still-incomplete tail.
pub(super) fn split_complete_sentences(buffer: &str, min_chars: usize) -> (Vec<String>, usize) {
    let mut sentences = Vec::new();
    let mut consumed = 0usize;
    let mut seg_start = 0usize;
    let mut chars = buffer.char_indices().peekable();

    while let Some((i, ch)) = chars.next() {
        let boundary_end = if ch == '\n' {
            Some(i + ch.len_utf8())
        } else if matches!(ch, '.' | '!' | '?' | '…') {
            match chars.peek() {
                Some((_, next)) if next.is_whitespace() => Some(i + ch.len_utf8()),
                _ => None,
            }
        } else {
            None
        };

        if let Some(end) = boundary_end {
            let candidate = buffer[seg_start..end].trim();
            if !candidate.is_empty() && candidate.chars().count() >= min_chars {
                sentences.push(candidate.to_string());
                seg_start = end;
                consumed = end;
            }
            // Too short: leave seg_start where it is so the fragment merges
            // into the next sentence.
        }
    }

    (sentences, consumed)
}

impl AgentRuntime {
    /// True when a streaming-voice pipeline is armed for this exact turn.
    pub(super) fn voice_chunk_pipeline_matches(&self, session_id: &str, turn_id: &str) -> bool {
        self.voice_chunk_pipelines
            .get(session_id)
            .map(|p| p.turn_id == turn_id)
            .unwrap_or(false)
    }

    /// Drop any streaming-voice state for this session (turn over/failed/evicted).
    pub(super) fn clear_voice_chunk_pipeline(&mut self, session_id: &str) {
        self.voice_chunk_pipelines.remove(session_id);
    }

    /// Arm a fresh pipeline for a new turn (replacing any stale one).
    /// Production arms inline in `handle_user_message` (borrow-split around
    /// the active `state` borrow); this helper serves the tests.
    #[cfg(test)]
    pub(super) fn arm_voice_chunk_pipeline(&mut self, session_id: &str, turn_id: &str) {
        self.voice_chunk_pipelines.insert(
            session_id.to_string(),
            VoiceChunkPipeline::new(turn_id.to_string()),
        );
    }

    /// Called from `handle_streaming_token` after a token was appended to the
    /// turn's `streamed_content`: split any newly completed sentences into the
    /// queue and (if idle) dispatch the next synthesis. No-op without an armed
    /// pipeline for the active turn.
    pub(super) async fn ingest_streaming_tokens_for_voice(
        &mut self,
        session_id: &str,
    ) -> Result<()> {
        {
            let Some(pipeline) = self.voice_chunk_pipelines.get_mut(session_id) else {
                return Ok(());
            };
            if pipeline.finalizing {
                return Ok(());
            }
            let active = self
                .sessions
                .get(session_id)
                .and_then(|s| s.active_turn.as_ref());
            let Some(turn) = active else {
                self.voice_chunk_pipelines.remove(session_id);
                return Ok(());
            };
            if turn.turn_id != pipeline.turn_id {
                self.voice_chunk_pipelines.remove(session_id);
                return Ok(());
            }
            let tail = turn
                .streamed_content
                .get(pipeline.consumed_bytes..)
                .unwrap_or("");
            let (sentences, consumed) = split_complete_sentences(tail, MIN_VOICE_CHUNK_CHARS);
            pipeline.consumed_bytes += consumed;
            for sentence in sentences {
                pipeline.queue.push_back(sentence);
            }
        }
        self.pump_voice_chunk_pipeline(session_id).await
    }

    /// Streaming-mode replacement for `start_voice_synthesis`, called from
    /// `complete_agent_response` when a pipeline is armed for this turn:
    /// flush the un-synthesized tail of the stream as the last sentence(s),
    /// stash the final text reply, and let the pipeline drain. If no chunk was
    /// (or will be) produced at all, the whole reply becomes a single chunk.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn finalize_streaming_voice_turn(
        &mut self,
        session_id: String,
        turn_id: String,
        content: String,
        spoken_text: Option<String>,
        voice_policy: VoiceResponsePolicy,
        memory_concept: Option<String>,
        memory_candidate: Option<MemoryCandidate>,
    ) -> Result<()> {
        let streamed = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| t.streamed_content.clone());
        let Some(streamed) = streamed else {
            // No active turn — mirror start_voice_synthesis's warn-and-drop.
            warn!(
                "finalize_streaming_voice_turn: no active turn for session {}",
                session_id
            );
            self.voice_chunk_pipelines.remove(&session_id);
            return Ok(());
        };

        {
            let Some(pipeline) = self.voice_chunk_pipelines.get_mut(&session_id) else {
                // Pipeline vanished between the caller's check and now — fall
                // back to the classic batch synthesis so voice still happens.
                return self
                    .start_voice_synthesis(session_id, turn_id, content, spoken_text, voice_policy)
                    .await;
            };
            pipeline.finalizing = true;

            // Flush the tail: complete sentences first, then whatever remains.
            let tail = streamed.get(pipeline.consumed_bytes..).unwrap_or("");
            let (sentences, consumed) = split_complete_sentences(tail, MIN_VOICE_CHUNK_CHARS);
            for sentence in sentences {
                pipeline.queue.push_back(sentence);
            }
            let remainder = tail.get(consumed..).unwrap_or("").trim();
            if !remainder.is_empty() {
                pipeline.queue.push_back(remainder.to_string());
            }
            pipeline.consumed_bytes = streamed.len();

            // Nothing streamed at all (non-streaming provider): synthesize the
            // whole reply as one chunk so the listener still gets audio.
            if pipeline.chunks_emitted == 0
                && pipeline.in_flight.is_none()
                && pipeline.queue.is_empty()
            {
                let whole = spoken_text.unwrap_or_else(|| strip_markup(&content));
                let whole = whole.trim();
                if !whole.is_empty() {
                    pipeline.queue.push_back(whole.to_string());
                }
            }

            pipeline.final_reply = Some(PendingFinalVoiceReply {
                content,
                send_text_caption: voice_policy.caption_enabled(),
                memory_concept,
                memory_candidate,
            });
        }

        info!(
            session_id = %session_id,
            turn_id = %turn_id,
            "Streaming voice turn finalizing — draining sentence pipeline"
        );

        if let Some(state) = self.sessions.get_mut(&session_id) {
            state.set_active_turn_phase(TurnPhase::WaitingVoice);
        }
        // Give the drain its own watchdog budget (same reset as the batch path);
        // each forwarded chunk resets it again, so the 60s WaitingVoice budget
        // is per-chunk, not for the whole tail.
        self.stuck_turn_first_seen.remove(&session_id);

        self.pump_voice_chunk_pipeline(&session_id).await
    }

    /// Advance the pipeline: dispatch the next queued sentence if idle, or —
    /// when finalizing and fully drained — deliver the stashed text reply.
    /// Dispatch failures skip the sentence (failure honesty) and keep pumping.
    pub(super) async fn pump_voice_chunk_pipeline(&mut self, session_id: &str) -> Result<()> {
        enum Step {
            Idle,
            Dispatch {
                sentence: String,
                dispatch_seq: u64,
                turn_id: String,
            },
            Finish {
                reply: PendingFinalVoiceReply,
                turn_id: String,
            },
        }

        loop {
            let step = {
                let Some(pipeline) = self.voice_chunk_pipelines.get_mut(session_id) else {
                    return Ok(());
                };
                if pipeline.in_flight.is_some() {
                    Step::Idle
                } else if let Some(sentence) = pipeline.queue.pop_front() {
                    pipeline.dispatch_seq += 1;
                    pipeline.in_flight = Some(InFlightVoiceChunk {
                        dispatch_seq: pipeline.dispatch_seq,
                        text: sentence.clone(),
                    });
                    Step::Dispatch {
                        sentence,
                        dispatch_seq: pipeline.dispatch_seq,
                        turn_id: pipeline.turn_id.clone(),
                    }
                } else if pipeline.finalizing {
                    match pipeline.final_reply.take() {
                        Some(reply) => Step::Finish {
                            reply,
                            turn_id: pipeline.turn_id.clone(),
                        },
                        None => Step::Idle,
                    }
                } else {
                    Step::Idle
                }
            };

            match step {
                Step::Idle => return Ok(()),
                Step::Dispatch {
                    sentence,
                    dispatch_seq,
                    turn_id,
                } => {
                    match self
                        .dispatch_voice_chunk_synthesis(
                            session_id,
                            &turn_id,
                            dispatch_seq,
                            &sentence,
                        )
                        .await
                    {
                        Ok(()) => return Ok(()), // wait for the response
                        Err(err) => {
                            warn!(
                                session_id = %session_id,
                                dispatch_seq,
                                "voice chunk dispatch failed; skipping sentence: {err}"
                            );
                            if let Some(pipeline) = self.voice_chunk_pipelines.get_mut(session_id) {
                                pipeline.in_flight = None;
                            }
                            // Try the next sentence (or finish).
                        }
                    }
                }
                Step::Finish { reply, turn_id } => {
                    self.voice_chunk_pipelines.remove(session_id);
                    // Final text reply — AFTER the last chunk, never with audio.
                    return self
                        .deliver_text_reply(
                            session_id.to_string(),
                            turn_id,
                            reply.content,
                            None,
                            reply.send_text_caption,
                            reply.memory_concept,
                            reply.memory_candidate,
                        )
                        .await;
                }
            }
        }
    }

    /// Emit one `voice.synthesize` task for a single sentence. The synthetic
    /// `::vchunk::` turn id routes the response back to philote's chunk
    /// handler; the reply route targets philote itself so the transport never
    /// sees raw synthesis payloads.
    async fn dispatch_voice_chunk_synthesis(
        &mut self,
        session_id: &str,
        turn_id: &str,
        dispatch_seq: u64,
        sentence: &str,
    ) -> Result<()> {
        let spoken = strip_markup(sentence);
        if spoken.trim().is_empty() {
            anyhow::bail!("sentence reduced to empty spoken text after markup strip");
        }

        let (policy, chat_id, final_reply_to, final_reply_role, final_reply_guest_id) = {
            let state = self
                .sessions
                .get(session_id)
                .ok_or_else(|| anyhow::anyhow!("unknown session {session_id}"))?;
            let turn = state
                .active_turn
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no active turn for session {session_id}"))?;
            (
                state.agent_profile.voice_response_policy.clone(),
                turn.chat_id.clone(),
                turn.final_reply_to.clone(),
                turn.final_reply_role.clone(),
                turn.final_reply_guest_id.clone(),
            )
        };

        let voice_role_fallback = policy
            .provider
            .as_deref()
            .map(implementation_to_model_role)
            .unwrap_or_else(|| DEFAULT_VOICE_MODEL_ROLE.into());
        let (target_node, target_role, target_guest_id) = resolve_model_execution_target(
            self.sessions.get(session_id),
            "voice.synthesize",
            &voice_role_fallback,
        );

        let provider_options = if let Some(speed_percent) = policy.speed_percent {
            let speed = f64::from(speed_percent) / 100.0;
            serde_json::json!({ "voice_settings": { "speed": speed } })
        } else {
            serde_json::json!({})
        };

        let voice_task = serde_json::json!({
            "kind": "voice.synthesize",
            "request_class": "synthesis",
            "provider": policy.provider,
            "spoken_text": spoken,
            "voice_id": policy.effective_voice_id(),
            "model": policy.model,
            "provider_options": provider_options,
            "session_id": session_id,
            "turn_id": voice_chunk_turn_id(turn_id, dispatch_seq),
            "chat_id": chat_id,
            "reply_to": local_node_id(),
            "reply_role": "agent",
            "final_reply_to": final_reply_to,
            "final_reply_role": final_reply_role,
            "final_reply_guest_id": final_reply_guest_id,
        });

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node,
                target_role,
                target_guest_id,
                task_json: serde_json::to_string(&voice_task)?,
            })
            .await?;

        Ok(())
    }

    /// Handle the model-router's response to a per-sentence synthesis request
    /// (routed here by the `::vchunk::` turn-id marker before the normal
    /// model-response path). Forwards the audio as a `voice_chunk` to the
    /// turn's final reply target, then pumps the pipeline. A failed sentence
    /// is logged and skipped — text delivery is never blocked by synthesis.
    pub(super) async fn handle_voice_chunk_synthesis_response(
        &mut self,
        session_id: String,
        base_turn_id: String,
        dispatch_seq: u64,
        task: InboundTaskPayload,
    ) -> Result<()> {
        let active_turn_matches = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| t.turn_id == base_turn_id)
            .unwrap_or(false);
        if !active_turn_matches {
            if self.voice_chunk_pipelines.remove(&session_id).is_some() {
                warn!(
                    session_id = %session_id,
                    turn_id = %base_turn_id,
                    "voice chunk response arrived after its turn ended; dropping pipeline"
                );
            }
            return Ok(());
        }

        // Claim the in-flight slot — stale/duplicate responses are dropped.
        let sentence = {
            let Some(pipeline) = self.voice_chunk_pipelines.get_mut(&session_id) else {
                return Ok(());
            };
            if pipeline.turn_id != base_turn_id {
                return Ok(());
            }
            match pipeline.in_flight.as_ref() {
                Some(in_flight) if in_flight.dispatch_seq == dispatch_seq => {
                    pipeline.in_flight.take().map(|f| f.text)
                }
                _ => {
                    warn!(
                        session_id = %session_id,
                        dispatch_seq,
                        "stale voice chunk response (no matching in-flight dispatch); dropping"
                    );
                    return Ok(());
                }
            }
        };
        let Some(sentence) = sentence else {
            return Ok(());
        };

        let artifact = if let Some(err) = extract_model_error(&task) {
            warn!(
                session_id = %session_id,
                dispatch_seq,
                "sentence synthesis failed; skipping voice chunk: {err}"
            );
            None
        } else {
            let raw = task.content.unwrap_or_default();
            if raw.trim_start().starts_with('{') {
                Some(raw)
            } else {
                warn!(
                    session_id = %session_id,
                    dispatch_seq,
                    "sentence synthesis response is not an audio artifact; skipping voice chunk"
                );
                None
            }
        };

        if let Some(artifact) = artifact {
            let (chunk_seq, is_final) = {
                let Some(pipeline) = self.voice_chunk_pipelines.get_mut(&session_id) else {
                    return Ok(());
                };
                let is_final = pipeline.finalizing && pipeline.queue.is_empty();
                let chunk_seq = pipeline.next_chunk_seq;
                pipeline.next_chunk_seq += 1;
                pipeline.chunks_emitted += 1;
                (chunk_seq, is_final)
            };

            let routing = self
                .sessions
                .get(&session_id)
                .and_then(|s| s.active_turn.as_ref())
                .map(|t| {
                    (
                        t.chat_id.clone(),
                        t.final_reply_to.clone(),
                        t.final_reply_role.clone(),
                        t.final_reply_guest_id.clone(),
                    )
                });
            if let Some((chat_id, reply_to, reply_role, reply_guest_id)) = routing {
                let payload = serde_json::json!({
                    "action": "voice_chunk",
                    "session_id": session_id,
                    "turn_id": base_turn_id,
                    "chat_id": chat_id,
                    "content": sentence,
                    "audio_artifact": artifact,
                    "chunk_seq": chunk_seq,
                    "is_final": is_final,
                });
                if let Err(err) = self
                    .ipc_client
                    .send_request(IpcRequest::EmitTask {
                        target_node: reply_to,
                        target_role: reply_role,
                        target_guest_id: reply_guest_id,
                        task_json: payload.to_string(),
                    })
                    .await
                {
                    warn!(
                        session_id = %session_id,
                        chunk_seq,
                        "voice chunk emit failed (chunk dropped): {err}"
                    );
                }
                // Progress: the WaitingVoice watchdog budget restarts per chunk.
                self.stuck_turn_first_seen.remove(&session_id);
            }
        }

        self.pump_voice_chunk_pipeline(&session_id).await
    }
}

#[cfg(test)]
mod voice_stream_tests {
    use super::*;
    use crate::session::TtsMode;

    fn policy_on() -> VoiceResponsePolicy {
        VoiceResponsePolicy {
            mode: TtsMode::On,
            provider: Some("elevenlabs".into()),
            voice_id: Some("voice-1".into()),
            ..Default::default()
        }
    }

    // ── streaming_voice_eligible ────────────────────────────────────────────

    #[test]
    fn eligible_only_for_operator_chat_transport() {
        let policy = policy_on();
        assert!(streaming_voice_eligible(
            Some("operator_chat"),
            None,
            &policy,
            false
        ));
        assert!(streaming_voice_eligible(
            None,
            Some("operator_chat"),
            &policy,
            false
        ));
        assert!(!streaming_voice_eligible(
            Some("telegram"),
            Some("telegram"),
            &policy,
            false
        ));
        assert!(!streaming_voice_eligible(None, None, &policy, false));
    }

    #[test]
    fn eligibility_follows_voice_policy_activation() {
        let mut policy = policy_on();
        policy.mode = TtsMode::Off;
        // Off + text input: no voice at all.
        assert!(!streaming_voice_eligible(
            Some("operator_chat"),
            None,
            &policy,
            false
        ));
        // Off + voice input: voice mirroring is active → streaming applies.
        assert!(streaming_voice_eligible(
            Some("operator_chat"),
            None,
            &policy,
            true
        ));
    }

    #[test]
    fn native_audio_delivery_never_streams_sentences() {
        let mut policy = policy_on();
        policy.delivery_mode = crate::session::VoiceDeliveryMode::NativeAudio;
        assert!(!streaming_voice_eligible(
            Some("operator_chat"),
            None,
            &policy,
            false
        ));
    }

    #[test]
    fn stream_sentences_flag_defaults_on_and_can_disable() {
        let mut policy = policy_on();
        assert_eq!(policy.stream_sentences, None);
        assert!(streaming_voice_eligible(
            Some("operator_chat"),
            None,
            &policy,
            false
        ));
        policy.stream_sentences = Some(false);
        assert!(!streaming_voice_eligible(
            Some("operator_chat"),
            None,
            &policy,
            false
        ));
        policy.stream_sentences = Some(true);
        assert!(streaming_voice_eligible(
            Some("operator_chat"),
            None,
            &policy,
            false
        ));
    }

    #[test]
    fn stream_sentences_field_deserializes_with_default_none() {
        let policy: VoiceResponsePolicy =
            serde_json::from_str(r#"{"mode":"on"}"#).expect("policy deserializes");
        assert_eq!(policy.stream_sentences, None);
        let policy: VoiceResponsePolicy =
            serde_json::from_str(r#"{"mode":"on","stream_sentences":false}"#)
                .expect("policy deserializes");
        assert_eq!(policy.stream_sentences, Some(false));
    }

    // ── sentinel turn ids ───────────────────────────────────────────────────

    #[test]
    fn voice_chunk_turn_id_roundtrips() {
        let id = voice_chunk_turn_id("operator-chat-turn-abc", 7);
        assert_eq!(
            parse_voice_chunk_turn_id(&id),
            Some(("operator-chat-turn-abc".to_string(), 7))
        );
    }

    #[test]
    fn plain_turn_ids_are_not_voice_chunks() {
        assert_eq!(parse_voice_chunk_turn_id("turn-123"), None);
        assert_eq!(parse_voice_chunk_turn_id("turn::vchunk::x"), None);
    }

    // ── sentence splitter ───────────────────────────────────────────────────

    #[test]
    fn splits_complete_sentences_and_leaves_tail() {
        let text = "This is the first full sentence. And here comes the second one! And a tail";
        let (sentences, consumed) = split_complete_sentences(text, 20);
        assert_eq!(
            sentences,
            vec![
                "This is the first full sentence.".to_string(),
                "And here comes the second one!".to_string(),
            ]
        );
        assert_eq!(text[consumed..].trim(), "And a tail");
    }

    #[test]
    fn trailing_punctuation_without_whitespace_is_not_a_boundary() {
        // Mid-stream, "…ends here." may still be followed by more tokens
        // ("14" after "3."), so the boundary is unconfirmed.
        let (sentences, consumed) = split_complete_sentences("This sentence just ends here.", 20);
        assert!(sentences.is_empty());
        assert_eq!(consumed, 0);
    }

    #[test]
    fn short_sentences_merge_forward() {
        let text = "Hi. It is nice to see you again today. ";
        let (sentences, _) = split_complete_sentences(text, 20);
        assert_eq!(
            sentences,
            vec!["Hi. It is nice to see you again today.".to_string()]
        );
    }

    #[test]
    fn decimals_do_not_split_but_ellipsis_before_space_does() {
        let text = "The value of pi is 3.14159 and that is that... which is fine by me. ";
        let (sentences, _) = split_complete_sentences(text, 20);
        assert_eq!(
            sentences,
            vec![
                "The value of pi is 3.14159 and that is that...".to_string(),
                "which is fine by me.".to_string(),
            ]
        );
    }

    #[test]
    fn newline_is_a_boundary() {
        let text = "A heading line that is long enough\nnext paragraph starts";
        let (sentences, consumed) = split_complete_sentences(text, 20);
        assert_eq!(
            sentences,
            vec!["A heading line that is long enough".to_string()]
        );
        assert_eq!(&text[consumed..], "next paragraph starts");
    }

    #[test]
    fn splitter_is_utf8_safe() {
        let text = "Le café était très agréable ce matin… Ensuite nous sommes partis.";
        let (sentences, consumed) = split_complete_sentences(text, 20);
        assert_eq!(
            sentences,
            vec!["Le café était très agréable ce matin…".to_string()]
        );
        assert!(text.is_char_boundary(consumed));
        assert_eq!(text[consumed..].trim(), "Ensuite nous sommes partis.");
    }

    #[test]
    fn splitter_consumes_incrementally() {
        // Simulate the streaming cursor: grow the buffer, only re-split the tail.
        let full = "First complete sentence right here. Second complete sentence follows now. tail";
        let mut consumed_total = 0usize;
        let mut collected = Vec::new();
        for cut in [10, 40, full.len()] {
            let cut = (0..=cut).rev().find(|c| full.is_char_boundary(*c)).unwrap();
            let tail = &full[consumed_total..cut];
            let (sentences, consumed) = split_complete_sentences(tail, 20);
            consumed_total += consumed;
            collected.extend(sentences);
        }
        assert_eq!(
            collected,
            vec![
                "First complete sentence right here.".to_string(),
                "Second complete sentence follows now.".to_string(),
            ]
        );
        assert_eq!(full[consumed_total..].trim(), "tail");
    }
}

#[cfg(test)]
mod pipeline_tests {
    use super::super::tests::{run_recording_hotel, test_working_turn};
    use super::*;
    use crate::r#loop::TurnPhase;
    use crate::session::TtsMode;
    use uuid::Uuid;

    const AUDIO_ARTIFACT: &str = r#"{"audio_base64":"QUJD","mime_type":"audio/mpeg"}"#;

    struct Harness {
        runtime: AgentRuntime,
        emitted: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        server: tokio::task::JoinHandle<()>,
        socket_path: String,
    }

    async fn harness(tag: &str) -> Harness {
        let socket_path = format!(
            "/tmp/philote-vstream-{tag}-{}.sock",
            Uuid::new_v4().simple()
        );
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));
        let identity = philotic_client::GuestIdentity {
            guest_id: format!("agent-vstream-{tag}"),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let runtime = AgentRuntime::new(client, format!("agent-vstream-{tag}"));
        Harness {
            runtime,
            emitted,
            server,
            socket_path,
        }
    }

    async fn seed_streaming_turn(h: &mut Harness, session_id: &str, turn_id: &str) {
        h.runtime
            .ensure_session_loaded(session_id, "operator_chat")
            .await
            .expect("session load");
        let state = h.runtime.sessions.get_mut(session_id).expect("session");
        state.agent_profile.voice_response_policy = VoiceResponsePolicy {
            mode: TtsMode::On,
            provider: Some("elevenlabs".into()),
            voice_id: Some("voice-1".into()),
            ..Default::default()
        };
        let mut turn = test_working_turn(TurnPhase::WaitingModel);
        turn.turn_id = turn_id.into();
        turn.final_reply_to = "web-node-01".into();
        turn.final_reply_role = "membrane".into();
        turn.final_reply_guest_id = Some("operator-chat-guest".into());
        state.start_turn(turn);
        h.runtime.arm_voice_chunk_pipeline(session_id, turn_id);
    }

    async fn stream_token(h: &mut Harness, session_id: &str, token: &str) {
        h.runtime
            .handle_streaming_token(InboundTaskPayload {
                action: Some("streaming_token".into()),
                session_id: Some(session_id.into()),
                content: Some(token.into()),
                ..Default::default()
            })
            .await
            .expect("streaming token");
    }

    fn drain(h: &Harness) -> Vec<serde_json::Value> {
        h.emitted.lock().unwrap().clone()
    }

    fn synth_dispatches(events: &[serde_json::Value]) -> Vec<serde_json::Value> {
        events
            .iter()
            .filter(|e| e["task"]["kind"] == "voice.synthesize")
            .cloned()
            .collect()
    }

    async fn respond_chunk(
        h: &mut Harness,
        session_id: &str,
        sentinel_turn_id: &str,
        content: Option<&str>,
        error: Option<&str>,
    ) {
        h.runtime
            .handle_model_response(InboundTaskPayload {
                action: Some("model_response".into()),
                session_id: Some(session_id.into()),
                turn_id: Some(sentinel_turn_id.into()),
                content: content.map(str::to_string),
                error: error.map(|message| philotic_client::TaskErrorPayload {
                    kind: "provider_failure".into(),
                    message: message.into(),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .expect("chunk response");
    }

    async fn finish(h: Harness) {
        drop(h.runtime);
        let _ = h.server.await;
        let _ = std::fs::remove_file(&h.socket_path);
    }

    #[tokio::test]
    async fn sentences_stream_as_ordered_voice_chunks_then_final_reply() {
        let mut h = harness("happy").await;
        let session_id = "operator_chat:web:agent-vstream-happy";
        let turn_id = "turn-vs-happy";
        seed_streaming_turn(&mut h, session_id, turn_id).await;

        stream_token(&mut h, session_id, "Here is the first full sentence. ").await;

        // First sentence must be at the router; nothing forwarded downstream yet.
        let events = drain(&h);
        let synths = synth_dispatches(&events);
        assert_eq!(synths.len(), 1, "one sentence dispatched: {events:#?}");
        let s1_turn = synths[0]["task"]["turn_id"].as_str().unwrap().to_string();
        assert_eq!(s1_turn, voice_chunk_turn_id(turn_id, 1));
        assert_eq!(
            synths[0]["task"]["spoken_text"],
            "Here is the first full sentence."
        );
        assert_eq!(synths[0]["task"]["reply_role"], "agent");
        assert!(!events.iter().any(|e| e["task"]["action"] == "voice_chunk"));

        // Second sentence completes while the first is still synthesizing —
        // it queues; sequential dispatch means no second router request yet.
        stream_token(
            &mut h,
            session_id,
            "And now the second sentence lands here. And a tail",
        )
        .await;
        assert_eq!(synth_dispatches(&drain(&h)).len(), 1);

        // First audio comes back → chunk 0 forwarded, second sentence dispatched.
        respond_chunk(&mut h, session_id, &s1_turn, Some(AUDIO_ARTIFACT), None).await;
        let events = drain(&h);
        let chunks: Vec<_> = events
            .iter()
            .filter(|e| e["task"]["action"] == "voice_chunk")
            .collect();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0]["task"]["chunk_seq"], 0);
        assert_eq!(chunks[0]["task"]["is_final"], false);
        assert_eq!(
            chunks[0]["task"]["content"],
            "Here is the first full sentence."
        );
        assert_eq!(chunks[0]["task"]["audio_artifact"], AUDIO_ARTIFACT);
        assert_eq!(chunks[0]["target_node"], "web-node-01");
        assert_eq!(chunks[0]["target_guest_id"], "operator-chat-guest");
        let synths = synth_dispatches(&events);
        assert_eq!(
            synths.len(),
            2,
            "second sentence dispatched after first chunk"
        );
        let s2_turn = synths[1]["task"]["turn_id"].as_str().unwrap().to_string();

        // Model completes; the un-synthesized tail becomes the last sentence.
        let final_text =
            "Here is the first full sentence. And now the second sentence lands here. And a tail";
        h.runtime
            .handle_model_response(InboundTaskPayload {
                action: Some("model_response".into()),
                session_id: Some(session_id.into()),
                turn_id: Some(turn_id.into()),
                agent_action: Some(serde_json::json!({
                    "kind": "respond",
                    "content": final_text,
                })),
                content: Some(final_text.into()),
                ..Default::default()
            })
            .await
            .expect("final respond");

        // Batch synthesis must NOT fire: every router request is a vchunk.
        for synth in synth_dispatches(&drain(&h)) {
            let tid = synth["task"]["turn_id"].as_str().unwrap();
            assert!(
                parse_voice_chunk_turn_id(tid).is_some(),
                "batch voice.synthesize leaked: {tid}"
            );
        }
        // No final reply yet — chunks still draining.
        assert!(
            !drain(&h)
                .iter()
                .any(|e| e["task"]["action"] == "send_reply"),
            "send_reply must wait for the last chunk"
        );

        // Second audio → chunk 1 (not final; the tail is still queued).
        respond_chunk(&mut h, session_id, &s2_turn, Some(AUDIO_ARTIFACT), None).await;
        let events = drain(&h);
        let synths = synth_dispatches(&events);
        assert_eq!(synths.len(), 3, "tail sentence dispatched");
        let s3_turn = synths[2]["task"]["turn_id"].as_str().unwrap().to_string();
        assert_eq!(synths[2]["task"]["spoken_text"], "And a tail");

        // Tail audio → final chunk, then the text reply, in that order.
        respond_chunk(&mut h, session_id, &s3_turn, Some(AUDIO_ARTIFACT), None).await;
        let events = drain(&h);
        let chunk_seqs: Vec<(u64, bool)> = events
            .iter()
            .filter(|e| e["task"]["action"] == "voice_chunk")
            .map(|e| {
                (
                    e["task"]["chunk_seq"].as_u64().unwrap(),
                    e["task"]["is_final"].as_bool().unwrap(),
                )
            })
            .collect();
        assert_eq!(chunk_seqs, vec![(0, false), (1, false), (2, true)]);

        let reply_positions: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, e)| e["task"]["action"] == "send_reply")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(reply_positions.len(), 1, "exactly one send_reply");
        let last_chunk_pos = events
            .iter()
            .rposition(|e| e["task"]["action"] == "voice_chunk")
            .unwrap();
        assert!(
            reply_positions[0] > last_chunk_pos,
            "send_reply must come after the final chunk"
        );
        let reply = &events[reply_positions[0]];
        assert_eq!(reply["task"]["content"], final_text);
        assert!(
            reply["task"]["audio_artifact"].is_null(),
            "final reply must not carry audio: {reply:#?}"
        );

        // Turn fully completed and pipeline gone.
        assert!(
            h.runtime
                .sessions
                .get(session_id)
                .unwrap()
                .active_turn
                .is_none()
        );
        assert!(h.runtime.voice_chunk_pipelines.is_empty());
        finish(h).await;
    }

    #[tokio::test]
    async fn failed_sentences_are_skipped_and_text_reply_still_lands() {
        let mut h = harness("fail").await;
        let session_id = "operator_chat:web:agent-vstream-fail";
        let turn_id = "turn-vs-fail";
        seed_streaming_turn(&mut h, session_id, turn_id).await;

        stream_token(&mut h, session_id, "This first sentence will fail loudly. ").await;
        let s1_turn = voice_chunk_turn_id(turn_id, 1);
        respond_chunk(&mut h, session_id, &s1_turn, None, Some("boom")).await;

        let final_text = "This first sentence will fail loudly. Short tail.";
        h.runtime
            .handle_model_response(InboundTaskPayload {
                action: Some("model_response".into()),
                session_id: Some(session_id.into()),
                turn_id: Some(turn_id.into()),
                agent_action: Some(serde_json::json!({
                    "kind": "respond",
                    "content": final_text,
                })),
                content: Some(final_text.into()),
                ..Default::default()
            })
            .await
            .expect("final respond");

        // The tail sentence was dispatched (dispatch_seq 2); fail it too.
        respond_chunk(
            &mut h,
            session_id,
            &voice_chunk_turn_id(turn_id, 2),
            Some("not-json-audio"),
            None,
        )
        .await;

        let events = drain(&h);
        assert!(
            !events.iter().any(|e| e["task"]["action"] == "voice_chunk"),
            "no chunk may be forwarded when synthesis fails: {events:#?}"
        );
        let replies: Vec<_> = events
            .iter()
            .filter(|e| e["task"]["action"] == "send_reply")
            .collect();
        assert_eq!(replies.len(), 1, "text reply must still land: {events:#?}");
        assert_eq!(replies[0]["task"]["content"], final_text);
        assert!(
            h.runtime
                .sessions
                .get(session_id)
                .unwrap()
                .active_turn
                .is_none(),
            "turn must complete despite synthesis failures"
        );
        finish(h).await;
    }

    #[tokio::test]
    async fn non_streaming_reply_becomes_single_final_chunk() {
        let mut h = harness("whole").await;
        let session_id = "operator_chat:web:agent-vstream-whole";
        let turn_id = "turn-vs-whole";
        seed_streaming_turn(&mut h, session_id, turn_id).await;

        // No streaming tokens at all — provider answered in one shot.
        let final_text = "A complete reply that never streamed token by token.";
        h.runtime
            .handle_model_response(InboundTaskPayload {
                action: Some("model_response".into()),
                session_id: Some(session_id.into()),
                turn_id: Some(turn_id.into()),
                agent_action: Some(serde_json::json!({
                    "kind": "respond",
                    "content": final_text,
                })),
                content: Some(final_text.into()),
                ..Default::default()
            })
            .await
            .expect("final respond");

        let events = drain(&h);
        let synths = synth_dispatches(&events);
        assert_eq!(
            synths.len(),
            1,
            "whole reply becomes one chunk: {events:#?}"
        );
        assert_eq!(synths[0]["task"]["spoken_text"], final_text);
        let s1_turn = synths[0]["task"]["turn_id"].as_str().unwrap().to_string();

        respond_chunk(&mut h, session_id, &s1_turn, Some(AUDIO_ARTIFACT), None).await;
        let events = drain(&h);
        let chunks: Vec<_> = events
            .iter()
            .filter(|e| e["task"]["action"] == "voice_chunk")
            .collect();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0]["task"]["chunk_seq"], 0);
        assert_eq!(chunks[0]["task"]["is_final"], true);
        assert_eq!(
            events
                .iter()
                .filter(|e| e["task"]["action"] == "send_reply")
                .count(),
            1
        );
        finish(h).await;
    }

    #[tokio::test]
    async fn non_operator_transports_keep_batch_synthesis() {
        let mut h = harness("batch").await;
        let session_id = "telegram:123:agent-vstream-batch";
        let turn_id = "turn-vs-batch";
        h.runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");
        {
            let state = h.runtime.sessions.get_mut(session_id).expect("session");
            state.agent_profile.voice_response_policy = VoiceResponsePolicy {
                mode: TtsMode::On,
                provider: Some("elevenlabs".into()),
                voice_id: Some("voice-1".into()),
                ..Default::default()
            };
            let mut turn = test_working_turn(TurnPhase::WaitingModel);
            turn.turn_id = turn_id.into();
            state.start_turn(turn);
        }
        // No pipeline armed (transport is telegram) → the classic batch path.
        let final_text = "A voice reply for Telegram stays one single voice note.";
        h.runtime
            .handle_model_response(InboundTaskPayload {
                action: Some("model_response".into()),
                session_id: Some(session_id.into()),
                turn_id: Some(turn_id.into()),
                agent_action: Some(serde_json::json!({
                    "kind": "respond",
                    "content": final_text,
                })),
                content: Some(final_text.into()),
                ..Default::default()
            })
            .await
            .expect("final respond");

        let events = drain(&h);
        let synths = synth_dispatches(&events);
        assert_eq!(synths.len(), 1);
        let tid = synths[0]["task"]["turn_id"].as_str().unwrap();
        assert_eq!(tid, turn_id, "batch synthesis keeps the real turn id");
        assert!(
            !events.iter().any(|e| e["task"]["action"] == "voice_chunk"),
            "no voice chunks on non-operator transports"
        );
        finish(h).await;
    }
}
