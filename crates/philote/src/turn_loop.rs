//! Turn-loop core for [`AgentRuntime`]: the main `run()` event loop dispatch,
//! model/tool result handling, the stuck-turn watchdog, fallback-tier
//! escalation, provider-failure retry, and final reply delivery.
//!
//! Mechanically extracted from `runtime.rs` (declared there as a `#[path]`
//! child module so private `AgentRuntime` fields stay accessible). No
//! behavior change.

use super::*;

impl AgentRuntime {
    /// Scan all active sessions and evict (or escalate) any turn stuck in a waiting
    /// phase past its deadline. Deadlines:
    ///   WaitingModel  — 300 s  (escalates to next fallback tier; evicts when all tiers exhausted)
    ///   WaitingTool   — 90 s   (tool runners should be fast)
    ///   WaitingVoice  — 60 s   (ElevenLabs is normally < 10 s)
    ///
    /// Uses `stuck_turn_first_seen` to track when a waiting-phase turn was first
    /// observed. This map is reconciled each tick — entries are added on first
    /// observation and cleared when the session leaves the waiting phase or has no
    /// active turn. This approach works regardless of which code path set the phase.
    ///
    /// On WaitingModel timeout: call `advance_turn_to_next_fallback_tier`, which either
    /// dispatches to the next provider or calls `fail_active_turn` when all tiers are
    /// exhausted. Other phases: evict directly (clear the active turn, persist a clean
    /// checkpoint, and send the user a brief notice so they know the session is unblocked).
    pub(super) async fn evict_timed_out_turns(&mut self) {
        const WAITING_MODEL_SECS: u64 = 300;
        const THINKING_SECS: u64 = 90; // post-model, dispatching actions or building reply
        const WAITING_TOOL_SECS: u64 = 90;
        const WAITING_VOICE_SECS: u64 = 60;
        const WAITING_APPROVAL_SECS: u64 = 300; // 5 min — operator may be slow
        // Hard ceiling: any active turn alive longer than this in ANY phase gets
        // evicted. Prevents InProgress or unknown-phase turns from sticking forever.
        const MAX_TOTAL_ACTIVE_SECS: u64 = 600; // 10 min overall budget

        let now = std::time::Instant::now();

        // Step 0: maintain total_active_since — track ALL sessions with an active
        // turn, regardless of phase, so InProgress turns are also bounded.
        let all_session_ids: Vec<String> = self.sessions.keys().cloned().collect();
        for sid in &all_session_ids {
            if self
                .sessions
                .get(sid)
                .map(|s| s.active_turn.is_some())
                .unwrap_or(false)
            {
                self.total_active_since.entry(sid.clone()).or_insert(now);
            } else {
                self.total_active_since.remove(sid);
            }
        }
        self.total_active_since
            .retain(|id, _| self.sessions.contains_key(id));

        // Step 1: reconcile stuck_turn_first_seen against current session state.
        // Add sessions newly in a waiting phase; reset when the exact wait signature changes.
        // Parked approval turns count as waiting (they live in parked_approval_turn, not active_turn).
        let session_ids: Vec<String> = self.sessions.keys().cloned().collect();
        for session_id in &session_ids {
            let wait_signature = self.sessions.get(session_id).and_then(|s| {
                if let Some(turn) = s.active_turn.as_ref() {
                    if matches!(
                        turn.phase,
                        TurnPhase::WaitingModel
                            | TurnPhase::Thinking
                            | TurnPhase::WaitingTool
                            | TurnPhase::WaitingVoice
                    ) {
                        let pending_tool = turn
                            .pending_tool_call
                            .as_ref()
                            .map(|tool| tool.tool_name.as_str())
                            .unwrap_or("-");
                        return Some(format!(
                            "active:{}:{:?}:{}:{}",
                            turn.turn_id, turn.phase, turn.iteration, pending_tool
                        ));
                    }
                }
                if let Some(turn) = s.parked_approval_turn.as_ref() {
                    return Some(format!("parked_approval:{}", turn.turn_id));
                }
                if let Some(turn) = s.parked_plan_turn.as_ref() {
                    return Some(format!("parked_plan:{}", turn.turn_id));
                }
                None
            });

            if let Some(signature) = wait_signature {
                let signature_changed = self
                    .stuck_turn_signature
                    .get(session_id)
                    .map(|current| current != &signature)
                    .unwrap_or(true);
                if signature_changed {
                    self.stuck_turn_first_seen.insert(session_id.clone(), now);
                    self.stuck_turn_signature
                        .insert(session_id.clone(), signature);
                } else {
                    self.stuck_turn_first_seen
                        .entry(session_id.clone())
                        .or_insert(now);
                }
            } else {
                self.stuck_turn_first_seen.remove(session_id);
                self.stuck_turn_signature.remove(session_id);
            }
        }
        // Also remove entries for sessions that no longer exist.
        self.stuck_turn_first_seen
            .retain(|id, _| self.sessions.contains_key(id));
        self.stuck_turn_signature
            .retain(|id, _| self.sessions.contains_key(id));

        // Step 2: collect sessions whose waiting turn has exceeded the deadline.
        // Parked approval turns (in parked_approval_turn) use WAITING_APPROVAL_SECS.
        let timed_out: Vec<(
            String,
            uuid::Uuid,
            String,
            String,
            String,
            Option<String>,
            String,
            String,
            u64,
        )> = self
            .sessions
            .iter()
            .filter_map(|(session_id, state)| {
                let first_seen = *self.stuck_turn_first_seen.get(session_id)?;
                let elapsed = first_seen.elapsed().as_secs();
                // Parked approval turn: check it first, since active_turn is None when parked.
                if let Some(turn) = state.parked_approval_turn.as_ref() {
                    if elapsed >= WAITING_APPROVAL_SECS {
                        return Some((
                            session_id.clone(),
                            turn.task_id,
                            turn.turn_id.clone(),
                            turn.final_reply_to.clone(),
                            turn.final_reply_role.clone(),
                            turn.final_reply_guest_id.clone(),
                            turn.chat_id.clone(),
                            "WaitingApproval(parked)".into(),
                            elapsed,
                        ));
                    }
                    return None;
                }
                // Parked plan turn: same timeout budget as approval — operator may be slow.
                if let Some(turn) = state.parked_plan_turn.as_ref() {
                    if elapsed >= WAITING_APPROVAL_SECS {
                        return Some((
                            session_id.clone(),
                            turn.task_id,
                            turn.turn_id.clone(),
                            turn.final_reply_to.clone(),
                            turn.final_reply_role.clone(),
                            turn.final_reply_guest_id.clone(),
                            turn.chat_id.clone(),
                            "PlanningDiscussion(parked)".into(),
                            elapsed,
                        ));
                    }
                    return None;
                }
                let turn = state.active_turn.as_ref()?;
                let limit = match turn.phase {
                    TurnPhase::WaitingModel => WAITING_MODEL_SECS,
                    TurnPhase::Thinking => THINKING_SECS,
                    TurnPhase::WaitingTool
                        if turn
                            .pending_tool_call
                            .as_ref()
                            .map(|tool| tool.tool_name == "delegate.whisper")
                            .unwrap_or(false) =>
                    {
                        WAITING_APPROVAL_SECS
                    }
                    TurnPhase::WaitingTool => WAITING_TOOL_SECS,
                    TurnPhase::WaitingVoice => WAITING_VOICE_SECS,
                    _ => return None,
                };
                if elapsed < limit {
                    return None;
                }
                Some((
                    session_id.clone(),
                    turn.task_id,
                    turn.turn_id.clone(),
                    turn.final_reply_to.clone(),
                    turn.final_reply_role.clone(),
                    turn.final_reply_guest_id.clone(),
                    turn.chat_id.clone(),
                    format!("{:?}", turn.phase),
                    elapsed,
                ))
            })
            .collect();

        // Step 2b: catch-all — any active turn alive longer than MAX_TOTAL_ACTIVE_SECS
        // in any phase (including InProgress) that wasn't already caught above.
        let already_caught: std::collections::HashSet<String> =
            timed_out.iter().map(|(id, ..)| id.clone()).collect();
        let catch_all: Vec<(
            String,
            uuid::Uuid,
            String,
            String,
            String,
            Option<String>,
            String,
            String,
            u64,
        )> = self
            .total_active_since
            .iter()
            .filter_map(|(session_id, &started)| {
                if already_caught.contains(session_id) {
                    return None;
                }
                let elapsed = started.elapsed().as_secs();
                if elapsed < MAX_TOTAL_ACTIVE_SECS {
                    return None;
                }
                let state = self.sessions.get(session_id)?;
                let turn = state.active_turn.as_ref()?;
                Some((
                    session_id.clone(),
                    turn.task_id,
                    turn.turn_id.clone(),
                    turn.final_reply_to.clone(),
                    turn.final_reply_role.clone(),
                    turn.final_reply_guest_id.clone(),
                    turn.chat_id.clone(),
                    format!("CatchAll({:?})", turn.phase),
                    elapsed,
                ))
            })
            .collect();
        let timed_out: Vec<_> = timed_out.into_iter().chain(catch_all).collect();

        // Step 3: escalate or evict.
        for (
            session_id,
            task_id,
            turn_id,
            reply_to,
            reply_role,
            reply_guest_id,
            chat_id,
            phase,
            elapsed_secs,
        ) in timed_out
        {
            let has_pending_tool = self
                .sessions
                .get(&session_id)
                .and_then(|s| s.active_turn.as_ref())
                .map(|t| t.pending_tool_call.is_some())
                .unwrap_or(false);

            // WaitingModel: escalate to the next fallback tier instead of evicting.
            // The model-router's IPC connection may have dropped silently (no error
            // signal arrives, only the watchdog fires). Tier escalation gives another
            // provider a chance. total_active_since is preserved so the 600s CatchAll
            // hard ceiling still applies.
            if phase == "WaitingModel" {
                warn!(
                    session_id = %session_id,
                    phase = %phase,
                    elapsed_secs = %elapsed_secs,
                    has_pending_tool = %has_pending_tool,
                    "Turn watchdog: WaitingModel timeout — escalating to next fallback tier"
                );
                self.stuck_turn_first_seen.remove(&session_id);
                self.stuck_turn_signature.remove(&session_id);
                // Do NOT remove total_active_since — the 600s CatchAll budget still applies.
                let _ = self
                    .advance_turn_to_next_fallback_tier(
                        session_id,
                        turn_id,
                        NoResponseClass::WatchdogTimeout,
                    )
                    .await;
                continue;
            }

            warn!(
                session_id = %session_id,
                phase = %phase,
                elapsed_secs = %elapsed_secs,
                has_pending_tool = %has_pending_tool,
                "Turn watchdog: evicting stuck turn"
            );

            self.stuck_turn_first_seen.remove(&session_id);
            self.stuck_turn_signature.remove(&session_id);
            self.total_active_since.remove(&session_id);

            if let Some(state) = self.sessions.get_mut(&session_id) {
                state.active_turn = None;
                state.parked_approval_turn = None;
                state.parked_approval_since = None;
                state.turn_waiting_since = None;

                // Persist clean checkpoint so a restart also starts unblocked.
                let mem_type = state.checkpoint_memory_type();
                let clean_checkpoint = state.checkpoint_json();
                if let Err(e) = self
                    .ipc_client
                    .sync_apartment(&self.agent_id, &mem_type, clean_checkpoint)
                    .await
                {
                    warn!("Turn watchdog: failed to persist clean checkpoint: {}", e);
                }
            }

            let reason =
                format!("Turn watchdog evicted stuck turn after {elapsed_secs}s in {phase}.");
            if let Err(e) = self
                .ipc_client
                .send_request(IpcRequest::FailTask {
                    task_id,
                    error_code: "TURN_WATCHDOG_TIMEOUT".into(),
                    reason: reason.clone(),
                    session_id: Some(session_id.clone()),
                    turn_id: Some(turn_id.clone()),
                })
                .await
            {
                warn!("Turn watchdog: failed to mark task failed: {}", e);
            }

            // Notify the user that the session is unblocked.
            let notify_req = IpcRequest::EmitTask {
                target_node: reply_to,
                target_role: reply_role,
                target_guest_id: reply_guest_id,
                task_json: serde_json::json!({
                    "action": "send_reply",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": chat_id,
                    "content": "*(I seem to have gotten stuck waiting for a response. The session is unblocked — please try again.)*",
                    "final": true,
                })
                .to_string(),
            };
            if let Err(e) = self.ipc_client.send_request(notify_req).await {
                warn!("Turn watchdog: failed to send unblock notification: {}", e);
            }
        }

        // Step 4: evict stale queued tasks from all sessions.
        const QUEUE_STALE_SECS: u64 = 120;
        let session_ids_for_stale: Vec<String> = self.sessions.keys().cloned().collect();
        for session_id in session_ids_for_stale {
            if let Some(state) = self.sessions.get_mut(&session_id) {
                let dropped = state.evict_stale_queued_tasks(QUEUE_STALE_SECS);
                if dropped > 0 {
                    warn!(
                        session_id = %session_id,
                        dropped = dropped,
                        "Watchdog: evicted stale queued tasks older than {}s",
                        QUEUE_STALE_SECS
                    );
                }
            }
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        info!("Listening for inbound Persona tasks from the Philotic Web...");
        self.fetch_agent_profile().await;
        self.fetch_role_names().await;
        self.fetch_memory_config().await;

        // Publish command manifest to the hotel so membrane can discover it.
        let manifest = command_manifest(&[]);
        if let Ok(content_json) = serde_json::to_value(&manifest) {
            let sync_result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.ipc_client.send_request(IpcRequest::SyncApartment {
                    agent_id: self.agent_id.clone(),
                    memory_type: "command_manifest".into(),
                    content_json,
                }),
            )
            .await;
            match sync_result {
                Ok(Ok(_)) => info!("Command manifest published ({} entries).", manifest.len()),
                Ok(Err(e)) => warn!("Failed to publish command manifest: {}", e),
                Err(_) => warn!("Command manifest sync timed out (startup race) — continuing"),
            }
        }

        // Sweep all session apartments for stale active turns left over from a prior
        // crash or unclean shutdown. This runs once at startup so callers don't have
        // to wait for the next inbound message to get a clean checkpoint.
        self.sweep_stale_session_turns().await;

        // Re-advertise this philote's MCP tool routes to the hotel so that the
        // membrane-mcp guest picks them up immediately on restart.
        self.register_mcp_routes().await;

        loop {
            // Run the watchdog every loop, not only on an idle receive timeout. A busy
            // inbox must not keep a stuck active turn alive indefinitely.
            self.evict_timed_out_turns().await;

            // Dispatch any tasks that were dequeued from a session's pending_user_tasks
            // after the previous turn completed. Processed before blocking on IPC so that
            // back-to-back voice memos are handled in order without additional round-trips.
            while let Some((drain_task_id, drain_task)) = self.pending_drains.pop_front() {
                if let Err(e) = self.handle_user_message(drain_task, drain_task_id).await {
                    warn!("Failed to dispatch drained queued task: {}", e);
                }
            }

            match tokio::time::timeout(Duration::from_secs(5), self.ipc_client.recv_task()).await {
                Ok(Ok(IpcResponse::InboundTask {
                    source_node,
                    task_id,
                    task_json,
                })) => {
                    info!(
                        "Agent [{}] received task [{}] from [{}]",
                        self.agent_id, task_id, source_node
                    );

                    if let Ok(peek) = serde_json::from_str::<serde_json::Value>(&task_json) {
                        let peeked_action = peek
                            .get("action")
                            .and_then(|v| v.as_str())
                            .unwrap_or("<none>");
                        let snippet: String = task_json.chars().take(300).collect();
                        info!(
                            task_id = %task_id,
                            action = %peeked_action,
                            payload_bytes = %task_json.len(),
                            snippet = %snippet,
                            "Agent dispatch: action peek"
                        );
                    }

                    match serde_json::from_str::<InboundTaskPayload>(&task_json) {
                        Ok(task) if task.is_model_response() => {
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_model_response(task).await {
                                error!("Failed to handle model response: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Ok(task) if task.is_tool_result() => {
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_tool_result(task).await {
                                error!("Failed to handle tool result: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("handoff_bundle") => {
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_handoff_bundle(task, task_id).await {
                                error!("Failed to handle handoff_bundle: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("handoff_return") => {
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_handoff_return(task, task_id).await {
                                error!("Failed to handle handoff_return: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("peer.delegate") => {
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_user_message(task, task_id).await {
                                error!("Failed to handle peer delegation as user message: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("voice.dialogue") => {
                            let _task_ref = task.clone();
                            if let Err(err) = self.handle_voice_dialogue(task, task_id).await {
                                error!("Failed to handle voice.dialogue: {}", err);
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("paracrine_request") => {
                            // Paracrine sub-call — this philote is the specialist receiving
                            // an Exosome from a peer or Orchestrator. final_reply_to/role
                            // are set by the ParacrineEmit caller to route the response.
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_user_message(task, task_id).await {
                                error!("Failed to handle paracrine_request: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("paracrine_response") => {
                            // Exosome response arriving back from a prior paracrine
                            // dispatch. Route through the lookaside reflex — not the
                            // main user-message path.
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_paracrine_response(task, task_id).await {
                                error!("Failed to handle paracrine_response: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("paracrine_signal") => {
                            // Low-agency heartbeat/background signal. Observe it, but do
                            // not enter the conversational model path.
                            if let Err(err) = self.handle_paracrine_signal(task, task_id).await {
                                warn!("Failed to handle paracrine_signal: {}", err);
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("streaming_token") => {
                            // LLM token fragment emitted by model-router during a streaming
                            // response. Forward immediately to membrane for progressive display.
                            if let Err(err) = self.handle_streaming_token(task).await {
                                warn!("Failed to forward streaming_token: {}", err);
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("model_dispatch_status") => {
                            // Transient dispatch state from the model controller retry loop.
                            // Forward to membrane so the user sees "(retrying...)" etc.
                            if let Err(err) = self.handle_model_dispatch_status(task).await {
                                warn!("Failed to forward model_dispatch_status: {}", err);
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("datasource_response") => {
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_datasource_response(task).await {
                                error!("Failed to handle datasource_response: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Ok(task) if task.command.as_deref() == Some("context.capture") => {
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_context_capture(task, task_id).await {
                                error!("Failed to handle context.capture: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Ok(task) => {
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_user_message(task, task_id).await {
                                error!("Failed to handle user message: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Err(err) => warn!("Could not parse inbound task payload: {}", err),
                    }
                }
                Ok(Ok(IpcResponse::NetworkState { online })) => {
                    self.network_offline = !online;
                    if !online {
                        warn!("Network offline — routing text.generate to local fallback tier");
                    } else {
                        info!("Network restored — cloud model tiers re-enabled");
                    }
                }
                Ok(Ok(IpcResponse::MuninnStatus {
                    available,
                    endpoint,
                })) => {
                    self.muninn_available = available;
                    if !available {
                        warn!(
                            endpoint = %endpoint,
                            "MuninnDB unreachable — memory tools will return empty until restored"
                        );
                    } else {
                        info!(endpoint = %endpoint, "MuninnDB restored — memory tools re-enabled");
                    }
                }
                Ok(Ok(IpcResponse::PerimeterShift { previous, current })) => {
                    use ansible_mesh_core::ExposureTier;
                    if current > previous {
                        // Ceiling rose: enforcement tightened (e.g. Lan → Internet).
                        // In-flight turns that assumed lower-trust access may now require
                        // re-authentication. Log and let the turn complete — the fence on
                        // the listener side will enforce on the next inbound request.
                        warn!(
                            ?previous,
                            ?current,
                            "Security perimeter ceiling raised — stricter enforcement now active"
                        );
                    } else {
                        // Ceiling dropped: less exposed (e.g. Internet → Lan). Relax.
                        info!(?previous, ?current, "Security perimeter ceiling lowered");
                    }
                    // Fail any in-flight turns that were operating at a tier above the
                    // new ceiling. This is conservative but prevents stale-auth continuations.
                    if current < ExposureTier::Mesh {
                        // Below Mesh means no external callers — but if we had in-flight
                        // mesh-origin turns, they should be re-evaluated. For now, log only.
                        // Active turn interruption (if needed) would go here.
                    }
                }
                Ok(Ok(IpcResponse::GracefulShutdown { drain_timeout_secs })) => {
                    info!(
                        "Hotel requested graceful shutdown (drain window: {}s). \
                         Finishing in-flight work then exiting.",
                        drain_timeout_secs
                    );
                    // Wait up to drain_timeout_secs for any active turns to complete,
                    // then exit. We poll once per second.
                    let deadline = std::time::Instant::now()
                        + std::time::Duration::from_secs(drain_timeout_secs);
                    loop {
                        let has_active = self.sessions.values().any(|s| s.active_turn.is_some());
                        if !has_active || std::time::Instant::now() >= deadline {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                    info!("Graceful shutdown drain complete; philote exiting.");
                    return Ok(());
                }
                Ok(Ok(other)) => {
                    info!("Jane received non-task IPC message: {:?}", other);
                }
                Ok(Err(err)) => {
                    if is_ipc_disconnect(&err) {
                        info!("Hotel IPC disconnected; philote exiting.");
                        return Ok(());
                    }
                    warn!("IPC Recv error: {}", err);
                }
                Err(_) => {
                    // 5-second tick — no task arrived. Check for stuck turns.
                    self.evict_timed_out_turns().await;
                }
            }
        }
    }

    pub(super) async fn handle_model_response(&mut self, task: InboundTaskPayload) -> Result<()> {
        let session_id = match task.session_id.as_deref().filter(|s| !s.is_empty()) {
            Some(session_id) => session_id.to_string(),
            None => return Ok(()),
        };
        let turn_id = match task.turn_id.as_deref().filter(|s| !s.is_empty()) {
            Some(turn_id) => turn_id.to_string(),
            None => return Ok(()),
        };
        self.ensure_session_loaded(&session_id, "unknown").await?;

        // Guard: if the active turn's turn_id doesn't match the incoming response, drop it.
        // This prevents stale model or synthesis responses from corrupting a newer active turn
        // (e.g., two overlapping voice memos where VM1's synthesis arrives after VM2's turn started).
        let active_turn_id = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| t.turn_id.clone());

        match active_turn_id {
            None => {
                // No active turn — the turn may have already completed (e.g. a duplicate
                // response from a second controller on the same role inbox arriving after
                // the first one already resolved the turn). Drop silently.
                return Ok(());
            }
            Some(ref active_id) if active_id != &turn_id => {
                warn!(
                    "handle_model_response: dropping stale response for turn {} (active turn is {})",
                    turn_id, active_id
                );
                return Ok(());
            }
            Some(_) => {}
        }

        // If the turn is waiting for voice synthesis, this is the audio response — route it
        // directly to the voice handler regardless of the agent_action kind.
        let waiting_voice = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| t.phase == TurnPhase::WaitingVoice)
            .unwrap_or(false);

        if waiting_voice {
            if let Some(model_error) = extract_model_error(&task) {
                warn!(
                    "Session [{}] voice synthesis failed before audio delivery: {}",
                    session_id, model_error
                );
                // Fire TTS failure reflex — ensures fallback_to_text is on and
                // any future turns know synthesis is unreliable.
                if let Some(state) = self.sessions.get_mut(&session_id) {
                    state.fire_reflex_event(ReflexEvent::TtsFailure {
                        provider: state
                            .agent_profile
                            .voice_response_policy
                            .provider
                            .clone()
                            .unwrap_or_else(|| "unknown".into()),
                    });
                }

                let voice_policy = self
                    .sessions
                    .get(&session_id)
                    .map(|s| s.agent_profile.voice_response_policy.clone())
                    .unwrap_or_default();

                if !voice_policy.fallback_to_text {
                    return self
                        .fail_active_turn(
                            session_id,
                            turn_id,
                            format!("Voice synthesis failed: {}", model_error),
                        )
                        .await;
                }
            }

            let raw_content = task.content.unwrap_or_default();
            return self
                .handle_voice_synthesis_response(session_id, turn_id, raw_content)
                .await;
        }

        if let Some(error_payload) = extract_model_error_payload(&task) {
            let sub_kind = error_payload.sub_kind.as_deref().unwrap_or("unknown");
            // Content errors (malformed tool call): repair with prompt injection, once.
            if is_content_error(&error_payload)
                && should_attempt_provider_repair(&error_payload, self.sessions.get(&session_id))
            {
                warn!(
                    session_id = %session_id,
                    sub_kind = %sub_kind,
                    "Retrying model turn with corrective note after content error"
                );
                return self
                    .retry_active_turn_after_provider_failure(
                        session_id,
                        turn_id,
                        Some(provider_repair_note(&error_payload)),
                    )
                    .await;
            }
            // Network / timeout / rate-limit errors: escalate to next fallback tier.
            if should_escalate_tier(&error_payload) {
                // For streaming_timeout specifically, allow one same-tier retry before
                // escalating — handles transient empty SSE responses from Gemini.
                if error_payload.sub_kind.as_deref() == Some("streaming_timeout") {
                    let attempts = self
                        .sessions
                        .get(&session_id)
                        .map(|s| s.streaming_retry_attempts())
                        .unwrap_or(0);
                    if attempts < 1 {
                        if let Some(state) = self.sessions.get_mut(&session_id) {
                            state.increment_streaming_retry_attempts();
                        }
                        warn!(
                            session_id = %session_id,
                            "Retrying same tier after streaming_timeout (attempt 1)"
                        );
                        return self
                            .retry_active_turn_after_provider_failure(session_id, turn_id, None)
                            .await;
                    }
                }
                warn!(
                    session_id = %session_id,
                    sub_kind = %sub_kind,
                    provider = %error_payload.provider.as_deref().unwrap_or("unknown"),
                    "Escalating to next fallback tier after provider failure"
                );
                return self
                    .advance_turn_to_next_fallback_tier(
                        session_id,
                        turn_id,
                        NoResponseClass::ProviderFailure,
                    )
                    .await;
            }
        }

        if let Some(model_error) = extract_model_error(&task) {
            warn!(
                "Session [{}] model request failed before turn completion: {}",
                session_id, model_error
            );
            return self
                .fail_active_turn(
                    session_id,
                    turn_id,
                    format!("Model failed: {}", model_error),
                )
                .await;
        }

        if let Some(state) = self.sessions.get_mut(&session_id) {
            state.set_active_turn_phase(TurnPhase::Thinking);
        }

        let model_result = task
            .agent_action
            .as_ref()
            .and_then(|a| a.get("model_result"))
            .and_then(|mr| mr.get("result"));

        let spoken_text = model_result
            .and_then(|r| r.get("spoken_text"))
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string);

        let partial_replies = model_result
            .and_then(|r| r.get("partial_replies"))
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let memory_concept = model_result
            .and_then(|r| r.get("memory_concept"))
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string);
        let memory_candidate =
            parse_memory_candidate(model_result.and_then(|r| r.get("memory_candidate")));
        let audio_artifact = extract_audio_artifact(model_result);

        // Capture active_plan from the model response and store it on the turn.
        // Present whenever the model outputs a structured plan — optional on all turns.
        if let Some(plan_value) = model_result.and_then(|r| r.get("active_plan")) {
            if let Ok(plan) = serde_json::from_value::<ActivePlan>(plan_value.clone()) {
                if let Some(state) = self.sessions.get_mut(&session_id) {
                    let is_first = state
                        .active_turn
                        .as_ref()
                        .map(|t| t.active_plan.is_none())
                        .unwrap_or(false);
                    state.set_active_plan(plan);
                    // Expand the iteration cap based on declared step count so complex
                    // plans don't hit the cap mid-execution. 4 iterations per step gives
                    // headroom for retries/recalls; hard ceiling 50, never shrinks.
                    let n_steps = state
                        .active_turn
                        .as_ref()
                        .and_then(|t| t.active_plan.as_ref())
                        .map(|p| p.steps.len() as u32)
                        .unwrap_or(0);
                    if n_steps > 0 {
                        let plan_cap = (n_steps * 4)
                            .max(state.settings.execution.iteration_cap)
                            .min(50);
                        if plan_cap > state.settings.execution.iteration_cap {
                            info!(
                                session_id = %session_id,
                                n_steps,
                                old_cap = state.settings.execution.iteration_cap,
                                new_cap = plan_cap,
                                "Plan-scaled iteration cap"
                            );
                            state.settings.execution.iteration_cap = plan_cap;
                        }
                    }
                    // state last used above; NLL ends the borrow before emit_turn_event.
                    if is_first {
                        let _ = self.emit_turn_event(&session_id, "plan_ready", None).await;
                    }
                }
            }
        }

        let awaiting_transcription_reentry = self
            .sessions
            .get(&session_id)
            .map(|state| state.active_turn_awaiting_transcription_reentry())
            .unwrap_or(false);

        let action = interpret_model_payload(task.agent_action.as_ref(), task.content.as_deref());
        if awaiting_transcription_reentry {
            return match action {
                AgentAction::Respond { content } => {
                    self.reenter_turn_after_transcription(session_id, turn_id, content)
                        .await
                }
                AgentAction::Fail { message } => {
                    self.fail_active_turn(session_id, turn_id, message).await
                }
                other => {
                    self.fail_active_turn(
                        session_id,
                        turn_id,
                        format!(
                            "voice.transcribe returned unexpected action {:?}; expected transcript text",
                            other
                        ),
                    )
                    .await
                }
            };
        }

        // Scripted loop fork: when the turn is running under a LoopScript,
        // model responses are consumed by the ScriptedLoopExecutor rather than
        // dispatched through the standard AgentAction match below.
        // Full routing (ParkForApproval, ExecuteNextTool, etc.) is implemented
        // in handle_scripted_loop_model_response — stubbed here until Phase 1b.
        let is_scripted = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| t.scripted_loop_context.is_some())
            .unwrap_or(false);

        if is_scripted {
            let model_result_owned = model_result.cloned();
            return self
                .handle_scripted_loop_model_response(
                    session_id,
                    turn_id,
                    model_result_owned,
                    spoken_text,
                    memory_concept,
                    memory_candidate,
                )
                .await;
        }

        match action {
            AgentAction::Respond { content } => {
                for partial in partial_replies {
                    self.emit_partial_reply(&session_id, partial).await?;
                }
                self.complete_agent_response(
                    session_id,
                    turn_id,
                    content,
                    spoken_text,
                    audio_artifact,
                    memory_concept,
                    memory_candidate,
                )
                .await
            }
            AgentAction::ToolCall(tool_call) => {
                let forced_stop_reply = self.sessions.get(&session_id).and_then(|state| {
                    let turn = state.active_turn.as_ref()?;
                    let iteration_cap = state.settings.execution.iteration_cap;
                    let reason = if turn.iteration >= iteration_cap {
                        Some("the turn reached its maximum tool-iteration limit")
                    } else {
                        loop_stop_reason(turn, iteration_cap)
                    }?;
                    Some(loop_stop_fallback_reply(
                        &turn.user_content,
                        &turn.working_tool_history,
                        reason,
                    ))
                });

                if let Some(reply) = forced_stop_reply {
                    warn!(
                        session_id = %session_id,
                        tool_name = %tool_call.tool_name,
                        "Suppressing tool call after loop stop condition; delivering fallback reply."
                    );
                    return self
                        .deliver_text_reply(session_id, turn_id, reply, None, false, None, None)
                        .await;
                }
                self.handle_tool_call(session_id, turn_id, tool_call).await
            }
            AgentAction::RequestApproval(approval) => {
                self.handle_approval_request(session_id, turn_id, approval, false)
                    .await
            }
            AgentAction::PlanProposal(proposal) => {
                self.handle_plan_proposal(session_id, turn_id, proposal)
                    .await
            }
            AgentAction::Fail { message } => {
                self.fail_active_turn(session_id, turn_id, message).await
            }
        }
    }

    pub(super) async fn handle_tool_result(&mut self, task: InboundTaskPayload) -> Result<()> {
        let session_id = match task.session_id.as_deref().filter(|s| !s.is_empty()) {
            Some(session_id) => session_id.to_string(),
            None => return Ok(()),
        };
        let (turn_id, pending_tool_name) = {
            let active_turn = self
                .sessions
                .get(&session_id)
                .and_then(|state| state.active_turn.as_ref());
            let pending_tool_name = active_turn
                .and_then(|turn| turn.pending_tool_call.as_ref())
                .map(|tool| tool.tool_name.clone());
            let turn_id = task
                .turn_id
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| active_turn.map(|turn| turn.turn_id.clone()));
            let Some(turn_id) = turn_id else {
                warn!(
                    session_id = %session_id,
                    "Dropping tool result without turn_id and without active turn"
                );
                return Ok(());
            };
            (turn_id, pending_tool_name)
        };
        let tool_name = task
            .tool_name
            .clone()
            .filter(|name| !name.is_empty())
            .or(pending_tool_name)
            .or_else(|| task.capability.clone().filter(|name| !name.is_empty()))
            .unwrap_or_else(|| "unknown".into());
        let tool_result = ToolResult {
            tool_name,
            content: task.content.clone().unwrap_or_default(),
        };

        // step_failed is determined by the presence of a non-empty error payload.
        let step_failed = task.error.is_some();
        let stream_events = self
            .sessions
            .get(&session_id)
            .map(|s| s.settings.execution.stream_tool_events)
            .unwrap_or(true);

        // Scripted-loop fork: if this turn is running under a LoopScript, accumulate
        // the tool result into the executor and dispatch the next scripted action
        // instead of re-entering the open-ended model loop.
        let is_scripted_turn = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| t.scripted_loop_context.is_some())
            .unwrap_or(false);

        // Only treat an incoming datasource response as the life.observe result when
        // the response's own tool_name confirms it — prevents a co-occurring
        // graph-datasource failure (tool_name="unknown") from completing a WaitingTool
        // turn that is legitimately waiting for the life-graph-runner response.
        let response_is_life_observe =
            matches!(tool_result.tool_name.as_str(), "life.observe" | "");
        let direct_life_observe_command = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .and_then(|turn| {
                turn.pending_tool_call.as_ref().and_then(|call| {
                    (call.tool_name == "life.observe" && response_is_life_observe)
                        .then(|| direct_life_observe_command_from_arguments(&call.arguments))
                        .flatten()
                })
            })
            .filter(|command| !command.claim_summary.trim().is_empty());

        if let Some(command) = direct_life_observe_command {
            {
                let Some(state) = self.sessions.get_mut(&session_id) else {
                    warn!("Tool result returned for unknown session {}", session_id);
                    return Ok(());
                };
                let tool_call = state
                    .active_turn
                    .as_ref()
                    .and_then(|t| t.pending_tool_call.clone())
                    .unwrap_or_else(|| ToolCall {
                        tool_name: tool_result.tool_name.clone(),
                        arguments: serde_json::json!({}),
                    });
                state.push_tool_history(tool_call, tool_result.clone());
                state.clear_pending_tool_call();
                if let Some(turn) = state.active_turn.as_mut() {
                    turn.iteration += 1;
                }
            }

            let reply = if step_failed {
                direct_life_observe_failure_reply(&command, &tool_result.content)
            } else {
                direct_life_observe_success_reply(&command, &tool_result.content)
            };
            return self
                .deliver_text_reply(session_id, turn_id, reply, None, false, None, None)
                .await;
        }

        if is_scripted_turn {
            let (checkpoint_memory_type, checkpoint_json, index_state) = {
                let Some(state) = self.sessions.get_mut(&session_id) else {
                    return Ok(());
                };
                let tool_call = state
                    .active_turn
                    .as_ref()
                    .and_then(|t| t.pending_tool_call.clone())
                    .unwrap_or_else(|| ToolCall {
                        tool_name: tool_result.tool_name.clone(),
                        arguments: serde_json::json!({}),
                    });
                state.push_tool_history(tool_call, tool_result.clone());
                state.clear_pending_tool_call();
                let result_value = Value::String(tool_result.content.clone());
                state.with_scripted_executor_mut(|exec| {
                    exec.record_tool_result(result_value);
                    exec.advance_tool_cursor();
                });
                (
                    state.checkpoint_memory_type(),
                    state.checkpoint_json(),
                    state.clone(),
                )
            };
            if stream_events {
                let event = if step_failed {
                    "step_failed"
                } else {
                    "step_completed"
                };
                let _ = self.emit_turn_event(&session_id, event, None).await;
            }
            self.ipc_client
                .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
                .await?;
            self.sync_session_index(&index_state).await?;
            return self
                .scripted_dispatch_after_advance(session_id, turn_id, None, None, None)
                .await;
        }

        // Pair the result with the pending tool call, push to history, check iteration cap.
        let mut is_finalizing = false;
        let loop_outcome = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                warn!("Tool result returned for unknown session {}", session_id);
                return Ok(());
            };

            let tool_call = state
                .active_turn
                .as_ref()
                .and_then(|t| t.pending_tool_call.clone())
                .unwrap_or_else(|| ToolCall {
                    tool_name: tool_result.tool_name.clone(),
                    arguments: serde_json::json!({}),
                });

            state.push_tool_history(tool_call.clone(), tool_result.clone());
            state.clear_pending_tool_call();

            // Update conditional-preapproval streak tracking.
            if step_failed {
                state.record_tool_streak_failure(&tool_call.tool_name);
            } else {
                state.record_tool_streak_success(&tool_call.tool_name);
            }

            // Track consecutive failures for stall detection.
            let consecutive_failures = if step_failed {
                state.increment_step_failures()
            } else {
                state.reset_step_failures();
                0
            };
            let stall_threshold = state.settings.execution.stall_detection_threshold;

            // Increment iteration before building the envelope so the context projection
            // carries the correct (post-increment) iteration number. This ensures
            // turn/iteration-aware stubs and any model-side telemetry see the right value.
            if let Some(turn) = state.active_turn.as_mut() {
                turn.iteration += 1;
                turn.phase = TurnPhase::WaitingModel;
            }

            let iteration = state.active_turn.as_ref().map(|t| t.iteration).unwrap_or(0);
            let iteration_cap = state.settings.execution.iteration_cap;
            let stop_reason = state
                .active_turn
                .as_ref()
                .and_then(|turn| loop_stop_reason(turn, iteration_cap));

            if consecutive_failures >= stall_threshold {
                Err(format!(
                    "Stall detected: {consecutive_failures} consecutive step failures \
                     (threshold: {stall_threshold}). Surfacing to user."
                ))
            } else if let Some(stop_reason) = stop_reason {
                // No-progress stop: give the model one stripped-tool chance to answer
                // before the hard cap. This catches diagnostic/status spirals while
                // preserving the useful evidence already collected.
                warn!(
                    session_id = %session_id,
                    iteration,
                    iteration_cap,
                    stop_reason,
                    "Session reached loop stop condition; doing final no-tool wrap-up."
                );
                is_finalizing = true;
                match state.build_reentry_context_envelope() {
                    Some((mut prompt, context, context_projection, _tools)) => {
                        prompt.push_str(&format!(
                            "\n\n[Loop control: {stop_reason}. Do not call any more tools. \
                             Review the tool history and provide your final response to the user now.]"
                        ));
                        let active_turn = state.active_turn.as_ref().expect("turn exists");
                        Ok((
                            prompt,
                            context,
                            context_projection,
                            active_turn.task_id,
                            active_turn.user_content.clone(),
                            active_turn.chat_id.clone(),
                            active_turn.final_reply_to.clone(),
                            active_turn.final_reply_role.clone(),
                            active_turn.final_reply_guest_id.clone(),
                            vec![], // strip tools — forces text-only reply
                            state.checkpoint_memory_type(),
                            state.checkpoint_json(),
                            state.clone(),
                        ))
                    }
                    None => Err("Active turn vanished at loop stop wrap-up".into()),
                }
            } else if iteration == iteration_cap {
                // Soft cap: one final no-tool call so the model can wrap up gracefully.
                warn!(
                    "Session [{}] reached iteration cap ({}); doing final no-tool wrap-up.",
                    session_id, iteration_cap
                );
                is_finalizing = true;
                match state.build_reentry_context_envelope() {
                    Some((mut prompt, context, context_projection, _tools)) => {
                        prompt.push_str(
                            "\n\n[You have reached the maximum number of tool calls for this turn. \
                             Do not call any more tools. Provide your final response to the user now.]",
                        );
                        let active_turn = state.active_turn.as_ref().expect("turn exists");
                        Ok((
                            prompt,
                            context,
                            context_projection,
                            active_turn.task_id,
                            active_turn.user_content.clone(),
                            active_turn.chat_id.clone(),
                            active_turn.final_reply_to.clone(),
                            active_turn.final_reply_role.clone(),
                            active_turn.final_reply_guest_id.clone(),
                            vec![], // strip tools — forces text-only reply
                            state.checkpoint_memory_type(),
                            state.checkpoint_json(),
                            state.clone(),
                        ))
                    }
                    None => Err("Active turn vanished at iteration cap".into()),
                }
            } else if iteration > iteration_cap {
                // Hard cap: finalizing call itself produced another tool call (shouldn't happen
                // with empty tool list, but guard anyway).
                Err(format!(
                    "Turn exceeded maximum tool iterations ({iteration_cap}). Aborting."
                ))
            } else if state
                .active_turn
                .as_ref()
                .and_then(|t| t.active_plan.as_ref())
                .map(|plan| {
                    plan.status == "done"
                        || (!plan.steps.is_empty()
                            && plan
                                .steps
                                .iter()
                                .all(|s| s.status == "done" || s.status == "failed"))
                })
                .unwrap_or(false)
            {
                // Plan-done early exit: the model has declared its plan complete.
                // Force a final no-tool wrap-up so the model delivers its summary
                // without being prompted to call more tools.
                info!(
                    "Session [{}] plan marked done after tool result; doing no-tool wrap-up.",
                    session_id
                );
                is_finalizing = true;
                match state.build_reentry_context_envelope() {
                    Some((mut prompt, context, context_projection, _tools)) => {
                        prompt.push_str(
                            "\n\n[Your plan is complete. All steps have been executed. \
                             Do not call any more tools. Provide your final response to the user now.]",
                        );
                        let active_turn = state.active_turn.as_ref().expect("turn exists");
                        Ok((
                            prompt,
                            context,
                            context_projection,
                            active_turn.task_id,
                            active_turn.user_content.clone(),
                            active_turn.chat_id.clone(),
                            active_turn.final_reply_to.clone(),
                            active_turn.final_reply_role.clone(),
                            active_turn.final_reply_guest_id.clone(),
                            vec![], // strip tools — forces text-only reply
                            state.checkpoint_memory_type(),
                            state.checkpoint_json(),
                            state.clone(),
                        ))
                    }
                    None => Err("Active turn vanished at plan-done wrap-up".into()),
                }
            } else {
                // Build the full cognitive context envelope for re-entry.
                // This ensures identity, instructions, memory, dialogue_window, active_turn,
                // and tool_history all reach model-router — not just a flat prompt.
                match state.build_reentry_context_envelope() {
                    Some((prompt, context, context_projection, tools)) => {
                        let active_turn = state.active_turn.as_ref().expect("turn exists");
                        Ok((
                            prompt,
                            context,
                            context_projection,
                            active_turn.task_id,
                            active_turn.user_content.clone(),
                            active_turn.chat_id.clone(),
                            active_turn.final_reply_to.clone(),
                            active_turn.final_reply_role.clone(),
                            active_turn.final_reply_guest_id.clone(),
                            tools,
                            state.checkpoint_memory_type(),
                            state.checkpoint_json(),
                            state.clone(),
                        ))
                    }
                    None => {
                        Err("Active turn vanished before re-entry context could be built".into())
                    }
                }
            }
        };

        // Emit step_completed or step_failed event before continuing the loop.
        if stream_events {
            let event = if step_failed {
                "step_failed"
            } else {
                "step_completed"
            };
            let _ = self.emit_turn_event(&session_id, event, None).await;
        }

        if is_finalizing && stream_events {
            let _ = self
                .emit_turn_event(&session_id, "loop_finalizing", None)
                .await;
        }

        match loop_outcome {
            Err(msg) => {
                if stream_events && !step_failed && !is_finalizing {
                    // stall/hard-cap hit — emit loop_recovering so observers know we stopped
                    let _ = self
                        .emit_turn_event(&session_id, "loop_recovering", None)
                        .await;
                }
                let fallback_reply = if msg.contains("maximum tool iterations") {
                    self.sessions.get(&session_id).and_then(|state| {
                        let turn = state.active_turn.as_ref()?;
                        Some(loop_stop_fallback_reply(
                            &turn.user_content,
                            &turn.working_tool_history,
                            "the turn reached its maximum tool-iteration limit",
                        ))
                    })
                } else {
                    None
                };
                if let Some(reply) = fallback_reply {
                    warn!(
                        session_id = %session_id,
                        "Delivering loop-stop fallback instead of failing active turn."
                    );
                    return self
                        .deliver_text_reply(session_id, turn_id, reply, None, false, None, None)
                        .await;
                }
                self.fail_active_turn(session_id, turn_id, msg).await
            }
            Ok((
                prompt,
                context,
                context_projection,
                _task_id,
                user_content,
                chat_id,
                final_reply_to,
                final_reply_role,
                final_reply_guest_id,
                tools_for_model,
                checkpoint_memory_type,
                checkpoint_json,
                index_state,
            )) => {
                self.ipc_client
                    .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
                    .await?;
                self.sync_session_index(&index_state).await?;

                let _ = self
                    .emit_turn_event(&session_id, "waiting_tool", None)
                    .await;

                let response_contract = Some(
                    serde_json::json!({ "channels": ["spoken_text", "memory_candidate", "active_plan"] }),
                );
                let response_route = Some(model_response_route(
                    self.sessions.get(&session_id),
                    response_contract.as_ref(),
                    &Map::new(),
                    &Vec::new(),
                ));
                let ligand =
                    planning_ligand(self.sessions.get(&session_id), &prompt, &tools_for_model);
                let affordances = model_affordances(
                    self.sessions.get(&session_id),
                    &user_content,
                    &tools_for_model,
                );
                let model_req = ModelRequestPayload {
                    action: "generate_text".to_string(),
                    request_class: Some("cognitive".to_string()),
                    session_id: session_id.clone(),
                    turn_id,
                    prompt,
                    user_content,
                    context: Some(context),
                    context_projection: Some(context_projection),
                    affordances,
                    attachments: Vec::new(),
                    tools_for_model,
                    response_contract,
                    response_route,
                    ligand,
                    provider_options: Map::new(),
                    chat_id,
                    reply_to: local_node_id(),
                    reply_role: "agent".into(),
                    final_reply_to,
                    final_reply_role,
                    final_reply_guest_id,
                };

                let (target_node, target_role, target_guest_id) = resolve_model_execution_target(
                    self.sessions.get(&session_id),
                    "text.generate",
                    DEFAULT_TEXT_MODEL_ROLE,
                );

                info!(
                    "Session [{}] re-entering model loop (iteration {})",
                    session_id,
                    self.sessions
                        .get(&session_id)
                        .and_then(|s| s.active_turn.as_ref())
                        .map(|t| t.iteration)
                        .unwrap_or(0)
                );

                self.ipc_client
                    .send_request(IpcRequest::EmitTask {
                        target_node,
                        target_role,
                        target_guest_id,
                        task_json: serde_json::to_string(&model_req)?,
                    })
                    .await?;

                Ok(())
            }
        }
    }

    pub(super) async fn retry_active_turn_after_provider_failure(
        &mut self,
        session_id: String,
        turn_id: String,
        note: Option<String>,
    ) -> Result<()> {
        let retry_plan = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                return Ok(());
            };
            if let Some(n) = note {
                state.set_provider_repair_note(n);
            }
            state.increment_provider_repair_attempts();

            match state.build_reentry_context_envelope() {
                Some((prompt, context, context_projection, tools_for_model)) => {
                    if let Some(turn) = state.active_turn.as_mut() {
                        turn.iteration += 1;
                        turn.phase = TurnPhase::WaitingModel;
                    }
                    let active_turn = state.active_turn.as_ref().expect("turn exists");
                    Ok((
                        prompt,
                        context,
                        context_projection,
                        active_turn.user_content.clone(),
                        active_turn.chat_id.clone(),
                        active_turn.final_reply_to.clone(),
                        active_turn.final_reply_role.clone(),
                        active_turn.final_reply_guest_id.clone(),
                        tools_for_model,
                        state.checkpoint_memory_type(),
                        state.checkpoint_json(),
                        state.clone(),
                    ))
                }
                None => Err(anyhow::anyhow!(
                    "Active turn vanished before provider-failure retry context could be built"
                )),
            }
        }?;

        let (
            prompt,
            context,
            context_projection,
            user_content,
            chat_id,
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            tools_for_model,
            checkpoint_memory_type,
            checkpoint_json,
            index_state,
        ) = retry_plan;

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .emit_turn_event(&session_id, "loop_recovering", None)
            .await;

        let response_contract = Some(
            serde_json::json!({ "channels": ["spoken_text", "memory_candidate", "active_plan"] }),
        );
        let response_route = Some(model_response_route(
            self.sessions.get(&session_id),
            response_contract.as_ref(),
            &Map::new(),
            &Vec::new(),
        ));
        let ligand = planning_ligand(self.sessions.get(&session_id), &prompt, &tools_for_model);
        let affordances = model_affordances(
            self.sessions.get(&session_id),
            &user_content,
            &tools_for_model,
        );
        let model_req = ModelRequestPayload {
            action: "generate_text".to_string(),
            request_class: Some("cognitive".to_string()),
            session_id: session_id.clone(),
            turn_id,
            prompt,
            user_content,
            context: Some(context),
            context_projection: Some(context_projection),
            affordances,
            attachments: Vec::new(),
            tools_for_model,
            response_contract,
            response_route,
            ligand,
            provider_options: serde_json::Map::new(),
            chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
        };

        if debug_model_requests_enabled() {
            match serde_json::to_string_pretty(&model_req) {
                Ok(json) => info!(
                    "PHILOTIC_DEBUG_MODEL_REQUESTS philote retry model request session={} turn={}:\n{}",
                    session_id, model_req.turn_id, json
                ),
                Err(err) => warn!(
                    "PHILOTIC_DEBUG_MODEL_REQUESTS could not serialize retry model request: {}",
                    err
                ),
            }
        }

        let (target_node, target_role, target_guest_id) = resolve_model_execution_target(
            self.sessions.get(&session_id),
            "text.generate",
            DEFAULT_TEXT_MODEL_ROLE,
        );

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node,
                target_role,
                target_guest_id,
                task_json: serde_json::to_string(&model_req)?,
            })
            .await?;

        Ok(())
    }

    /// Advance the active turn to the next fallback tier and re-dispatch the
    /// model request to that tier's role. The shared [`decide_no_response_action`]
    /// policy — the same one the watchdog consults via this funnel — chooses
    /// escalate-vs-evict from the failure class and whether a live tier remains.
    /// On `EvictTurn` (all tiers exhausted) the turn is failed with a
    /// user-visible error.
    pub(super) async fn advance_turn_to_next_fallback_tier(
        &mut self,
        session_id: String,
        turn_id: String,
        class: NoResponseClass,
    ) -> Result<()> {
        // Extract configured tiers before any mutable session borrow.
        let active_role_name: Option<String> = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.role_activation.as_ref())
            .map(|ra| ra.role_name.clone());
        // Prefer configured_roles (set via ConfigureRole tool calls); fall back to the
        // turn_loop_config embedded in the active role_activation (set at session load
        // from the DB record via fetch_role_activation).
        let configured_tiers: Vec<String> = active_role_name
            .as_deref()
            .and_then(|rn| self.configured_roles.get(rn))
            .map(|c| c.turn_loop_config.fallback_tiers.clone())
            .or_else(|| {
                self.sessions
                    .get(&session_id)
                    .and_then(|s| s.role_activation.as_ref())
                    .and_then(|ra| ra.turn_loop_config.as_ref())
                    .map(|tlc| tlc.fallback_tiers.clone())
            })
            .unwrap_or_default();

        // Check tier boundaries before any mutable borrow.
        let current_tier = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| t.fallback_tier)
            .unwrap_or(0);
        let max_tier = if !configured_tiers.is_empty() {
            configured_tiers.len().saturating_sub(1) as u8
        } else {
            DEFAULT_FALLBACK_TIERS.len().saturating_sub(1) as u8
        };

        let tiers_remaining = current_tier < max_tier;
        if decide_no_response_action(class, tiers_remaining) == NoResponseAction::EvictTurn {
            return self
                .fail_active_turn(
                    session_id,
                    turn_id,
                    "All model providers failed. Please try again later.".into(),
                )
                .await;
        }

        let next_tier = current_tier + 1;
        let next_role = role_for_tier(&configured_tiers, next_tier).to_string();

        // Gather everything we need before dropping the mutable state borrow.
        let plan = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                return Ok(());
            };

            if let Some(turn) = state.active_turn.as_mut() {
                turn.fallback_tier = next_tier;
                turn.phase = TurnPhase::WaitingModel;
                turn.iteration += 1;
            }

            match state.build_reentry_context_envelope() {
                Some((prompt, context, context_projection, tools_for_model)) => {
                    let active_turn = state.active_turn.as_ref().expect("turn exists");
                    Ok((
                        prompt,
                        context,
                        context_projection,
                        active_turn.user_content.clone(),
                        active_turn.chat_id.clone(),
                        active_turn.final_reply_to.clone(),
                        active_turn.final_reply_role.clone(),
                        active_turn.final_reply_guest_id.clone(),
                        tools_for_model,
                        state.checkpoint_memory_type(),
                        state.checkpoint_json(),
                        state.clone(),
                    ))
                }
                None => Err(anyhow::anyhow!(
                    "Active turn vanished before fallback tier re-dispatch"
                )),
            }
        }?;

        let (
            prompt,
            context,
            context_projection,
            user_content,
            chat_id,
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            tools_for_model,
            checkpoint_memory_type,
            checkpoint_json,
            index_state,
        ) = plan;

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .emit_turn_event(&session_id, "loop_recovering", None)
            .await;

        let response_contract = Some(
            serde_json::json!({ "channels": ["spoken_text", "memory_candidate", "active_plan"] }),
        );
        let response_route = Some(model_response_route(
            self.sessions.get(&session_id),
            response_contract.as_ref(),
            &Map::new(),
            &Vec::new(),
        ));
        let ligand = planning_ligand(self.sessions.get(&session_id), &prompt, &tools_for_model);
        let affordances = model_affordances(
            self.sessions.get(&session_id),
            &user_content,
            &tools_for_model,
        );
        let model_req = ModelRequestPayload {
            action: "generate_text".to_string(),
            request_class: Some("cognitive".to_string()),
            session_id: session_id.clone(),
            turn_id,
            prompt,
            user_content,
            context: Some(context),
            context_projection: Some(context_projection),
            affordances,
            attachments: Vec::new(),
            tools_for_model,
            response_contract,
            response_route,
            ligand,
            provider_options: serde_json::Map::new(),
            chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
        };

        info!(
            session_id = %session_id,
            fallback_tier = next_tier,
            target_role = %next_role,
            "Dispatching model request to fallback tier"
        );

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: local_node_id(),
                target_role: next_role,
                target_guest_id: None,
                task_json: serde_json::to_string(&model_req)?,
            })
            .await?;

        Ok(())
    }

    pub(super) async fn complete_agent_response(
        &mut self,
        session_id: String,
        turn_id: String,
        content: String,
        spoken_text: Option<String>,
        audio_artifact: Option<String>,
        memory_concept: Option<String>,
        memory_candidate: Option<MemoryCandidate>,
    ) -> Result<()> {
        let voice_policy = self
            .sessions
            .get(&session_id)
            .map(|s| s.agent_profile.voice_response_policy.clone())
            .unwrap_or_default();

        let had_voice_input = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| t.had_voice_input)
            .unwrap_or(false);

        if voice_policy.is_active(had_voice_input) {
            if voice_policy.delivery_mode.is_native_audio() {
                if audio_artifact.is_none() && !voice_policy.fallback_to_text {
                    return self
                        .fail_active_turn(
                            session_id,
                            turn_id,
                            "Provider-native audio response was requested but no audio artifact was returned.".into(),
                        )
                        .await;
                }

                return self
                    .deliver_text_reply(
                        session_id,
                        turn_id,
                        content,
                        audio_artifact,
                        voice_policy.caption_enabled(),
                        memory_concept,
                        memory_candidate,
                    )
                    .await;
            }

            return self
                .start_voice_synthesis(session_id, turn_id, content, spoken_text, voice_policy)
                .await;
        }

        self.deliver_text_reply(
            session_id,
            turn_id,
            content,
            None,
            false,
            memory_concept,
            memory_candidate,
        )
        .await
    }

    /// Final step: complete the turn, sync state, and emit `FinalReplyPayload` to membrane.
    pub(super) async fn deliver_text_reply(
        &mut self,
        session_id: String,
        turn_id: String,
        content: String,
        audio_artifact: Option<String>,
        send_text_caption: bool,
        memory_concept: Option<String>,
        memory_candidate: Option<MemoryCandidate>,
    ) -> Result<()> {
        let (completed_turn, checkpoint_memory_type, checkpoint_json, index_state) = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                warn!("deliver_text_reply: unknown session {}", session_id);
                return Ok(());
            };

            let Some(completed_turn) = state.complete_active_turn(content.clone()) else {
                warn!(
                    "deliver_text_reply: no active turn for session {}",
                    session_id
                );
                return Ok(());
            };

            if completed_turn.turn_id != turn_id {
                warn!(
                    "Turn mismatch for session {}: active={} response={}",
                    session_id, completed_turn.turn_id, turn_id
                );
            }
            state.set_active_turn_phase(TurnPhase::Completed);

            (
                completed_turn,
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .ipc_client
            .send_request(IpcRequest::CompleteTask {
                task_id: completed_turn.task_id,
                result: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": completed_turn.chat_id,
                    "content": content,
                }),
            })
            .await?;

        // LifeGraph auto-recall lane: refresh the prefetch cache after each
        // completed turn so the NEXT turn starts with current graph context
        // (staleness-by-one-turn is the intended latency design).
        self.dispatch_life_recall_prefetch(&session_id, &completed_turn.user_content)
            .await;

        // Capture for attend hook before moving into reply_payload.
        let _attend_turn_id = turn_id.clone();
        let _attend_content = content.clone();
        let attend_session_id = session_id.clone();

        // Named specialist philotes always append an @agent attribution tag so the
        // membrane (or receiving philote) knows which role secreted this Exosome.
        // Format: `@agent:<role_name>` on its own line at the end of content.
        // Membrane strips the tag before transport delivery and attaches an affordance.
        let content = if let Ok(role_name) = std::env::var("PHILOTIC_ROLE_NAME") {
            if !role_name.is_empty() {
                format!("{}\n\n@agent:{}", content, role_name)
            } else {
                content
            }
        } else {
            content
        };

        // If this turn was triggered by a paracrine_request, reply as a
        // `paracrine_response` so A's routing reflex handles it correctly.
        // Use source_session_id / source_chat_id from the exosome (stored on the turn)
        // so the orchestrator routes the reply to the originating conversation channel
        // rather than to this specialist's ephemeral session.
        //
        // If delegate.merge was called explicitly during this turn, the paracrine_response
        // was already emitted — skip the auto-emit to avoid double-delivery.
        // Otherwise use the normal `send_reply` path.
        let task_json = if let Some(ref pid) = completed_turn.paracrine_origin {
            if completed_turn.paracrine_merge_completed {
                // Explicit merge already fired — complete the task but don't re-send.
                info!(
                    "deliver_text_reply: delegate.merge already emitted paracrine_response for turn {}; skipping auto-emit",
                    turn_id
                );
                self.drain_next_user_task(&attend_session_id);
                return Ok(());
            }
            let reply_session_id = completed_turn
                .paracrine_reply_session_id
                .as_deref()
                .unwrap_or(&session_id);
            let reply_chat_id = completed_turn
                .paracrine_reply_chat_id
                .as_deref()
                .unwrap_or(&completed_turn.chat_id);
            // Reflective re-entry, top-of-chain: reply_session_id loops back to our
            // own session, meaning this was Astrid's reflection turn after receiving
            // brain's response. She chose not to call delegate.merge → absorb silently.
            if reply_session_id == session_id {
                info!(
                    "deliver_text_reply: reflective re-entry turn {} completed without delegate.merge — absorbing silently",
                    turn_id
                );
                self.drain_next_user_task(&attend_session_id);
                return Ok(());
            }
            serde_json::json!({
                "action": "paracrine_response",
                "session_id": reply_session_id,
                "turn_id": turn_id,
                "chat_id": reply_chat_id,
                "content": content,
                "exosome": {
                    "prompt": "",
                    "paracrine_id": pid,
                    "response_routing": completed_turn.paracrine_response_routing,
                    "source_session_id": reply_session_id,
                    "source_chat_id": reply_chat_id,
                },
            })
            .to_string()
        } else {
            let reply_payload = FinalReplyPayload {
                action: "send_reply",
                session_id,
                turn_id,
                chat_id: completed_turn.chat_id,
                content,
                audio_artifact,
                send_text_caption,
                reply_markup: None,
            };
            serde_json::to_string(&reply_payload)?
        };

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: completed_turn.final_reply_to,
                target_role: completed_turn.final_reply_role,
                target_guest_id: completed_turn.final_reply_guest_id,
                task_json,
            })
            .await?;

        // After completing this turn, schedule the next pending user task for dispatch.
        self.drain_next_user_task(&attend_session_id);

        // Attend hook (Slice E): fire-and-forget autobiographical memory write.
        // Only saves when the model provided an explicit memory_candidate — raw turn
        // content is never written as a fallback so the vault stays signal-only.
        if let (Some(engine), Some(candidate)) = (
            self.memory_engine_for(&self.agent_id, &self.agent_id),
            memory_candidate,
        ) {
            let agent_id = self.agent_id.clone();
            let mut tags = vec![
                format!("agent:{}", agent_id),
                format!("session:{}", attend_session_id),
            ];
            tags.extend(candidate.tags);
            let concept = memory_concept.unwrap_or(candidate.concept);
            let content_snapshot = candidate.content;
            tokio::spawn(async move {
                use memory_core::MemoryEngine as _;
                if let Err(e) = engine
                    .remember(MemoryScope::SelfOnly, &concept, &content_snapshot, tags)
                    .await
                {
                    warn!(agent = %agent_id, error = %e, "Attend: memory write failed (non-fatal)");
                }
            });
        }

        Ok(())
    }

    pub(super) async fn fail_active_turn(
        &mut self,
        session_id: String,
        turn_id: String,
        message: String,
    ) -> Result<()> {
        let (
            task_id,
            checkpoint_memory_type,
            checkpoint_json,
            index_state,
            chat_id,
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
        ) = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                warn!("Received fail action for unknown session {}", session_id);
                return Ok(());
            };
            let Some(active_turn) = state.active_turn.as_ref() else {
                warn!(
                    "Received fail action for session {} with no active turn",
                    session_id
                );
                return Ok(());
            };
            let task_id = active_turn.task_id;
            let chat_id = active_turn.chat_id.clone();
            let final_reply_to = active_turn.final_reply_to.clone();
            let final_reply_role = active_turn.final_reply_role.clone();
            let final_reply_guest_id = active_turn.final_reply_guest_id.clone();
            state.set_active_turn_phase(TurnPhase::Failed);
            let checkpoint_memory_type = state.checkpoint_memory_type();
            let checkpoint_json = state.checkpoint_json();
            let index_state = state.clone();
            // Clear the active turn NOW so is_turn_active() returns false before the
            // drain runs. Without this, drain_next_user_task moves the next queued task
            // into pending_drains, then handle_user_message sees an active turn and
            // re-queues it — creating an infinite queue/drain loop.
            state.active_turn = None;
            state.turn_waiting_since = None;
            (
                task_id,
                checkpoint_memory_type,
                checkpoint_json,
                index_state,
                chat_id,
                final_reply_to,
                final_reply_role,
                final_reply_guest_id,
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .ipc_client
            .send_request(IpcRequest::FailTask {
                task_id,
                error_code: "MODEL_EMPTY_RESPONSE".into(),
                reason: message.clone(),
                session_id: None,
                turn_id: None,
            })
            .await?;

        let drain_session_id = session_id.clone();
        let reply_payload = FinalReplyPayload {
            action: "send_reply",
            session_id,
            turn_id,
            chat_id,
            content: message,
            audio_artifact: None,
            send_text_caption: false,
            reply_markup: None,
        };

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: final_reply_to,
                target_role: final_reply_role,
                target_guest_id: final_reply_guest_id,
                task_json: serde_json::to_string(&reply_payload)?,
            })
            .await?;

        // After failing this turn, schedule the next pending user task for dispatch.
        self.drain_next_user_task(&drain_session_id);

        Ok(())
    }
}
