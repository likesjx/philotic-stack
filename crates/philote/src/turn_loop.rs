//! Turn-loop core for [`AgentRuntime`]: the main `run()` event loop dispatch,
//! model/tool result handling, the stuck-turn watchdog, fallback-tier
//! escalation, provider-failure retry, and final reply delivery.
//!
//! Mechanically extracted from `runtime.rs` (declared there as a `#[path]`
//! child module so private `AgentRuntime` fields stay accessible). No
//! behavior change.

use super::*;

use crate::plan_eval::{
    DEFAULT_PLAN_CONTINUATION_BUDGET, PlanEvalVerdict, PriorPlanState, evaluate_plan,
    interim_reply_admissible, plan_continuation_brief, plan_continuation_disabled,
    plan_stop_notice, scaled_continuation_budget, unix_now,
};
use crate::session::CarryoverPlan;

/// Current wall-clock time as epoch milliseconds. Used by the Slice 2
/// `FallbackOverride` bookkeeping (`since_epoch_ms`, `last_probe_epoch_ms`).
fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

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
        // Sized from observed healthy runs, not intuition. On mac-jane, successful
        // turns sit at p95 = 79s but a healthy `life.flywheel.brief` (Memgraph over
        // Tailscale) took 154s — while 14 turns were evicted at exactly 90/92/94s in
        // WaitingTool. The old 90s budget was below the real tool population and was
        // killing healthy-but-slow work, which is why scheduled briefs never landed.
        // 300s matches WAITING_MODEL_SECS.
        //
        // This is a PER-CALL budget. MAX_TOTAL_ACTIVE_SECS is the aggregate one, and
        // now that it actually enforces it is the binding constraint on a multi-call
        // turn: four calls each inside 300s are still evicted once the turn passes
        // 600s total. Raising this alone buys nothing past the ceiling. 600s remains
        // ample for the observed population — the slowest healthy multi-call turn
        // (life.recall x3 + life.flywheel.brief) totalled 154s.
        const WAITING_TOOL_SECS: u64 = 300;
        const WAITING_VOICE_SECS: u64 = 60;
        const WAITING_APPROVAL_SECS: u64 = 300; // 5 min — operator may be slow
        // A blocking `delegate.whisper` waits for a specialist turn that itself may
        // run up to MAX_TOTAL_ACTIVE_SECS on its own hotel PLUS materialization and
        // dispatch overhead. This deadline MUST exceed the specialist's total budget,
        // otherwise the orchestrator gives up before a slow-but-healthy specialist can
        // ever reply (the timeout inversion that made blocking whispers unusable).
        const PARACRINE_WHISPER_WAIT_SECS: u64 = 660; // > MAX_TOTAL_ACTIVE_SECS + overhead
        // Hard ceiling: any active turn alive longer than this in ANY phase gets
        // evicted. Prevents InProgress or unknown-phase turns from sticking forever.
        // Exception: a turn blocking on a paracrine whisper is bounded by
        // PARACRINE_WHISPER_WAIT_SECS instead (see the CatchAll skip below), so this
        // ceiling does not preempt its longer, deliberately-larger deadline.
        const MAX_TOTAL_ACTIVE_SECS: u64 = 600; // 10 min overall budget

        let now = std::time::Instant::now();

        // Step 0: maintain total_active_since — track ALL sessions with an active
        // turn, regardless of phase, so InProgress turns are also bounded.
        //
        // The stamp is read from `SessionState::active_turn_since` (set at turn start)
        // rather than stamped here with the tick's `now`. That distinction is the whole
        // point: `entry().or_insert(now)` only holds the ceiling if this function runs
        // regularly, and when ticks were missed the budget silently restarted from the
        // resume. Turns escaped the 600s ceiling for 823–3177s that way while reporting
        // 90s. Anchoring to a stamp taken outside the tick makes the deadline survive
        // any gap: the next tick to run sees the true elapsed time and evicts.
        let all_session_ids: Vec<String> = self.sessions.keys().cloned().collect();
        for sid in &all_session_ids {
            let turn_started = self
                .sessions
                .get(sid)
                .filter(|s| s.active_turn.is_some())
                .map(|s| s.active_turn_since);
            match turn_started {
                // Active turn with an authoritative start stamp.
                Some(Some(started)) => {
                    self.total_active_since.insert(sid.clone(), started);
                }
                // Active turn that reached `active_turn` without going through
                // `start_turn` (e.g. a parked turn restored inline for failure).
                // Fall back to the old tick-local behaviour so it stays covered.
                Some(None) => {
                    self.total_active_since.entry(sid.clone()).or_insert(now);
                }
                None => {
                    self.total_active_since.remove(sid);
                }
            }
        }
        self.total_active_since
            .retain(|id, _| self.sessions.contains_key(id));

        // Step 1: reconcile stuck_turn_first_seen against current session state.
        // Add sessions newly in a waiting phase; reset when the exact wait signature changes.
        // Parked approval turns count as waiting (they live in parked_approval_turn, not active_turn).
        //
        // Each waiting state also yields the instant it actually began — `turn_waiting_since`
        // for an active turn (stamped in `set_active_turn_phase`), `parked_*_since` for a
        // parked one. Those stamps are taken at the transition, so they are immune to
        // missed watchdog ticks; the tick's `now` is only a fallback for the rare state
        // that carries no stamp. Signature-change semantics are unchanged: a turn making
        // real progress through several tool calls still re-stamps, and therefore still
        // earns a fresh phase budget per call.
        let session_ids: Vec<String> = self.sessions.keys().cloned().collect();
        for session_id in &session_ids {
            let wait_state = self.sessions.get(session_id).and_then(|s| {
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
                        return Some((
                            format!(
                                "active:{}:{:?}:{}:{}",
                                turn.turn_id, turn.phase, turn.iteration, pending_tool
                            ),
                            s.turn_waiting_since,
                        ));
                    }
                }
                if let Some(turn) = s.parked_approval_turn.as_ref() {
                    return Some((
                        format!("parked_approval:{}", turn.turn_id),
                        s.parked_approval_since,
                    ));
                }
                if let Some(turn) = s.parked_plan_turn.as_ref() {
                    return Some((format!("parked_plan:{}", turn.turn_id), s.parked_plan_since));
                }
                None
            });

            if let Some((signature, waiting_since)) = wait_state {
                let signature_changed = self
                    .stuck_turn_signature
                    .get(session_id)
                    .map(|current| current != &signature)
                    .unwrap_or(true);
                if signature_changed {
                    self.stuck_turn_signature
                        .insert(session_id.clone(), signature);
                }
                match waiting_since {
                    // Authoritative: re-anchor every tick. The stamp only moves when the
                    // phase actually changes, so this is stable across ticks — and a gap
                    // in ticks can no longer rewind the deadline.
                    Some(started) => {
                        self.stuck_turn_first_seen
                            .insert(session_id.clone(), started);
                    }
                    // No stamp for this state — preserve the original tick-local
                    // behaviour rather than leaving the session uncovered.
                    None if signature_changed => {
                        self.stuck_turn_first_seen.insert(session_id.clone(), now);
                    }
                    None => {
                        self.stuck_turn_first_seen
                            .entry(session_id.clone())
                            .or_insert(now);
                    }
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
                        PARACRINE_WHISPER_WAIT_SECS
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
                // A turn blocking on a paracrine whisper has its own, deliberately
                // larger deadline (PARACRINE_WHISPER_WAIT_SECS) handled above. Skip it
                // here so the 600s ceiling never evicts it before a slow specialist can
                // reply — otherwise this catch-all would silently re-introduce the
                // timeout inversion the whisper deadline exists to prevent.
                let blocking_on_whisper = matches!(turn.phase, TurnPhase::WaitingTool)
                    && turn
                        .pending_tool_call
                        .as_ref()
                        .map(|tool| tool.tool_name == "delegate.whisper")
                        .unwrap_or(false);
                if blocking_on_whisper && elapsed < PARACRINE_WHISPER_WAIT_SECS {
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
            // Name the tool, not just its presence. `has_pending_tool=true` alone cannot
            // tell you which tool overran, so diagnosing an eviction meant reconstructing
            // the call from `session_event` rows in the hotel graph. One field removes
            // that whole step.
            let pending_tool = self
                .sessions
                .get(&session_id)
                .and_then(|s| s.active_turn.as_ref())
                .and_then(|t| t.pending_tool_call.as_ref())
                .map(|tool| tool.tool_name.clone());
            let has_pending_tool = pending_tool.is_some();
            let pending_tool = pending_tool.unwrap_or_else(|| "-".to_string());

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
                    pending_tool = %pending_tool,
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
                        None,
                        "The model did not respond before the turn timeout.".into(),
                    )
                    .await;
                continue;
            }

            warn!(
                session_id = %session_id,
                phase = %phase,
                elapsed_secs = %elapsed_secs,
                has_pending_tool = %has_pending_tool,
                pending_tool = %pending_tool,
                "Turn watchdog: evicting stuck turn"
            );

            self.stuck_turn_first_seen.remove(&session_id);
            self.stuck_turn_signature.remove(&session_id);
            self.total_active_since.remove(&session_id);
            // Sentence-pipelined TTS: the evicted turn takes its pipeline with it.
            self.clear_voice_chunk_pipeline(&session_id);

            if let Some(state) = self.sessions.get_mut(&session_id) {
                state.active_turn = None;
                state.parked_approval_turn = None;
                state.parked_approval_since = None;
                state.turn_waiting_since = None;
                state.active_turn_since = None;

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

            // Turn-failure heal intake: watchdog evictions flow into the
            // self-heal queue so recurring stuck turns surface as A3 work
            // items instead of only being discovered by the operator.
            self.push_heal_event(&format!("stuck_turn_evicted:{phase}"), &reason)
                .await;

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

        // Step 5: origin-tier probes for degraded sessions (Slice 2 fallback
        // override auto-recovery). Piggybacked on this same tick so no second
        // timer is needed.
        self.probe_degraded_sessions().await;
    }

    /// Fire a minimal fire-and-forget probe at the origin tier for every
    /// session that (a) has an active `FallbackOverride`, (b) has no active
    /// turn (a probe must never compete with or block a real user turn), and
    /// (c) hasn't been probed in the last `PROBE_INTERVAL_MS`. Success clears
    /// the override (see the probe interception at the top of
    /// `handle_model_response`); failure or no response just leaves the
    /// session degraded — `last_probe_epoch_ms` is stamped at send time so a
    /// dead origin tier isn't hammered every tick.
    pub(super) async fn probe_degraded_sessions(&mut self) {
        const PROBE_INTERVAL_MS: u64 = 300_000;
        let now_ms = now_epoch_ms();

        let eligible: Vec<(String, String)> = self
            .sessions
            .iter()
            .filter_map(|(session_id, state)| {
                if state.active_turn.is_some() {
                    return None;
                }
                if self.pending_fallback_probes.contains_key(session_id) {
                    return None;
                }
                let ov = state.fallback_override.as_ref()?;
                if now_ms.saturating_sub(ov.last_probe_epoch_ms) < PROBE_INTERVAL_MS {
                    return None;
                }
                Some((session_id.clone(), ov.origin_tier_role.clone()))
            })
            .collect();

        for (session_id, origin_role) in eligible {
            if let Err(e) = self
                .send_origin_probe(session_id.clone(), origin_role.clone(), now_ms)
                .await
            {
                warn!(
                    session_id = %session_id,
                    origin_role = %origin_role,
                    "Origin-tier probe dispatch failed: {}",
                    e
                );
                // Dispatch never made it out — don't leave a phantom in-flight
                // marker blocking the next eligible tick.
                self.pending_fallback_probes.remove(&session_id);
            }
        }
    }

    /// Send one degenerate `generate_text` request ("ping") to `origin_role`
    /// for `session_id`, correlated by a dedicated probe `turn_id` rather than
    /// a real `WorkingTurn` — see the module-level report on why turn_id
    /// correlation was chosen over a probe_turn_id field on `FallbackOverride`.
    /// Does not create a `WorkingTurn`; the eventual response is intercepted
    /// at the top of `handle_model_response` before the normal active-turn
    /// machinery ever sees it.
    async fn send_origin_probe(
        &mut self,
        session_id: String,
        origin_role: String,
        now_ms: u64,
    ) -> Result<()> {
        let probe_turn_id = format!("probe-{}", Uuid::new_v4());

        // Stamp last_probe_epoch_ms and persist immediately (before dispatch)
        // so a crash mid-probe can't tight-loop retries on the next restart.
        let (mem_type, checkpoint) = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                return Ok(());
            };
            let Some(ov) = state.fallback_override.as_mut() else {
                return Ok(());
            };
            ov.last_probe_epoch_ms = now_ms;
            (state.checkpoint_memory_type(), state.checkpoint_json())
        };
        if let Err(e) = self
            .ipc_client
            .sync_apartment(&self.agent_id, &mem_type, checkpoint)
            .await
        {
            warn!(session_id = %session_id, "Failed to persist probe checkpoint: {}", e);
        }

        self.pending_fallback_probes
            .insert(session_id.clone(), probe_turn_id.clone());

        let mut provider_options = serde_json::Map::new();
        provider_options.insert("probe".to_string(), serde_json::Value::Bool(true));

        let model_req = ModelRequestPayload {
            action: "generate_text".to_string(),
            // `request_class` is validated server-side against a fixed enum
            // (cognitive/transform/synthesis/embedding) — "probe" would bail
            // before ever reaching a provider. Use the same class a real
            // minimal text turn would carry; `provider_options.probe` is the
            // marker instead.
            request_class: Some("cognitive".to_string()),
            session_id: session_id.clone(),
            turn_id: probe_turn_id,
            prompt: "ping".to_string(),
            user_content: "ping".to_string(),
            context: None,
            context_projection: None,
            affordances: None,
            attachments: Vec::new(),
            tools_for_model: Vec::new(),
            response_contract: None,
            response_route: None,
            ligand: None,
            model: role_model_binding(self.sessions.get(&session_id), &origin_role),
            provider_options,
            chat_id: String::new(),
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            reply_guest_id: Some(self.own_guest_id()),
            final_reply_to: local_node_id(),
            final_reply_role: "agent".into(),
            final_reply_guest_id: None,
            agent_id: Some(self.agent_id.clone()),
            oracle_pick: None,
            oracle_agreement: None,
        };

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: local_node_id(),
                target_role: origin_role,
                target_guest_id: None,
                task_json: serde_json::to_string(&model_req)?,
            })
            .await?;

        Ok(())
    }

    /// Handle a model response correlated to an outstanding origin-tier probe
    /// (see `send_origin_probe`). Always clears the in-flight marker first —
    /// the bound is "at most one probe in flight per session" regardless of
    /// outcome. A successful (non-error) response means the origin tier is
    /// healthy again: clear the session's `FallbackOverride` so the next turn
    /// resolves back to it. An error response (or any other shape) leaves the
    /// session degraded; `last_probe_epoch_ms` was already stamped at send
    /// time in `send_origin_probe`, so the next tick simply waits out the
    /// cadence again rather than retrying immediately.
    async fn handle_fallback_probe_response(
        &mut self,
        session_id: String,
        task: InboundTaskPayload,
    ) -> Result<()> {
        self.pending_fallback_probes.remove(&session_id);

        if extract_model_error_payload(&task).is_some() {
            return Ok(());
        }

        let cleared = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                return Ok(());
            };
            state.fallback_override.take()
        };
        if let Some(ov) = cleared {
            info!(
                session_id = %session_id,
                "Origin-tier probe succeeded — clearing fallback override"
            );
            if let Some(state) = self.sessions.get(&session_id) {
                let mem_type = state.checkpoint_memory_type();
                let checkpoint = state.checkpoint_json();
                if let Err(e) = self
                    .ipc_client
                    .sync_apartment(&self.agent_id, &mem_type, checkpoint)
                    .await
                {
                    warn!(session_id = %session_id, "Failed to persist checkpoint after clearing fallback override: {}", e);
                }
            }
            // Slice 3: only announce recovery if the user was actually told
            // about the degradation in the first place (`notice_sent`) — a
            // fallback they never heard about doesn't need a recovery
            // announcement. Operator-driven clears (`/role`, `/back`,
            // `/model`) reset `fallback_override` directly and never reach
            // this path, so they never emit either.
            if ov.notice_sent {
                let message = format!(
                    "↪️ Model fallback cleared: {} (was {})",
                    ov.origin_tier_role, ov.active_tier_role
                );
                let _ = self
                    .emit_turn_event(&session_id, "model_fallback_cleared", Some(message))
                    .await;
            }
        }
        Ok(())
    }

    pub async fn run(&mut self) -> Result<()> {
        info!("Listening for inbound Persona tasks from the Philotic Web...");
        self.fetch_agent_profile().await;
        self.fetch_role_names().await;
        self.fetch_memory_config().await;

        // Publish command manifest to the hotel so membrane can discover it.
        let manifest = command_manifest(&[]);
        if let Ok(content_json) = serde_json::to_value(&manifest) {
            let sync_result = self
                .ipc_client
                .send_request_with_timeout(
                    IpcRequest::SyncApartment {
                        agent_id: self.agent_id.clone(),
                        memory_type: "command_manifest".into(),
                        content_json,
                    },
                    std::time::Duration::from_secs(5),
                )
                .await;
            match sync_result {
                Ok(_) => info!("Command manifest published ({} entries).", manifest.len()),
                Err(e) if philotic_client::is_ipc_timeout(&e) => {
                    warn!("Command manifest sync timed out (startup race) — continuing")
                }
                Err(e) => warn!("Failed to publish command manifest: {}", e),
            }
        }

        // Sweep all session apartments for stale active turns left over from a prior
        // crash or unclean shutdown. This runs once at startup so callers don't have
        // to wait for the next inbound message to get a clean checkpoint.
        self.sweep_stale_session_turns().await;

        // Re-advertise this philote's MCP tool routes to the hotel so that the
        // membrane-mcp guest picks them up immediately on restart.
        self.register_mcp_routes().await;

        // Load projected upstream MCP tools (mcp:<upstream>.<tool>) so granted
        // remote tools are in the catalog from the first turn.
        self.refresh_mcp_upstream_projection().await;
        self.refresh_http_integration_projection().await;

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
                        Ok(task) if task.action.as_deref() == Some("tool_progress") => {
                            self.handle_tool_progress(&task);
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
            None => {
                // DEF-051: never drop a model response without a trace — a
                // swallowed response leaves the turn riding the watchdog to a
                // 600s eviction with nothing in the log to explain why.
                warn!(
                    turn_id = %task.turn_id.as_deref().unwrap_or(""),
                    "handle_model_response: dropping response with empty session_id"
                );
                return Ok(());
            }
        };
        let turn_id = match task.turn_id.as_deref().filter(|s| !s.is_empty()) {
            Some(turn_id) => turn_id.to_string(),
            None => {
                warn!(
                    session_id = %session_id,
                    "handle_model_response: dropping response with empty turn_id"
                );
                return Ok(());
            }
        };

        // Sentence-pipelined TTS: per-sentence synthesis responses carry a
        // synthetic "<turn_id>::vchunk::<seq>" turn id so they can never be
        // mistaken for the turn's own model response. Route them to the voice
        // chunk handler before any active-turn bookkeeping.
        if let Some((base_turn_id, dispatch_seq)) = parse_voice_chunk_turn_id(&turn_id) {
            return self
                .handle_voice_chunk_synthesis_response(session_id, base_turn_id, dispatch_seq, task)
                .await;
        }

        self.ensure_session_loaded(&session_id, "unknown").await?;

        // Slice 2: intercept a response correlated to an outstanding
        // origin-tier probe BEFORE the stale-turn drop logic below — a probe
        // never has an active turn, so it would otherwise be silently
        // dropped by the `None` arm of the match just below. Handled and
        // returned here regardless of outcome; the normal turn machinery
        // never sees it.
        if self.pending_fallback_probes.get(&session_id) == Some(&turn_id) {
            return self.handle_fallback_probe_response(session_id, task).await;
        }

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
                // the first one already resolved the turn). Benign for duplicates, but
                // logged (DEF-051): if the session SHOULD have an active turn, this is
                // the arm that strands it until the watchdog eviction.
                info!(
                    session_id = %session_id,
                    turn_id = %turn_id,
                    "handle_model_response: no active turn for session — dropping response"
                );
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
            match classify_provider_error(&error_payload) {
                // Transient failures (network / 5xx / streaming stall): retry
                // may succeed — same tier once for streaming_timeout, next
                // tier otherwise.
                ProviderErrorClass::RetrySameProvider => {
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
                            error_payload.provider.clone(),
                            error_payload.message.clone(),
                        )
                        .await;
                }
                // Contract failures (4xx / INVALID_ARGUMENT / refusal / rate
                // limit): retrying the same provider fails identically —
                // transparently switch providers mid-turn. Skips same-provider
                // ladder tiers; consults the routing oracle on exhaustion.
                ProviderErrorClass::SwitchProvider => {
                    warn!(
                        session_id = %session_id,
                        sub_kind = %sub_kind,
                        status = ?error_payload.status,
                        provider = %error_payload.provider.as_deref().unwrap_or("unknown"),
                        "Provider contract failure — switching providers mid-turn"
                    );
                    return self
                        .advance_turn_to_next_fallback_tier(
                            session_id,
                            turn_id,
                            NoResponseClass::ProviderContractFailure,
                            error_payload.provider.clone(),
                            error_payload.message.clone(),
                        )
                        .await;
                }
                // Auth/key misconfiguration: no retry against this provider
                // helps until an operator fixes the key — fail fast and flag
                // the heal queue so the outage becomes a work item.
                ProviderErrorClass::Fatal => {
                    let provider = error_payload
                        .provider
                        .clone()
                        .unwrap_or_else(|| "unknown".into());
                    warn!(
                        session_id = %session_id,
                        sub_kind = %sub_kind,
                        provider = %provider,
                        "Fatal provider auth failure — failing turn fast"
                    );
                    self.push_heal_event(
                        &format!("provider_auth:{provider}"),
                        &format!(
                            "Provider {provider} rejected credentials for session {session_id} \
                             turn {turn_id}: {}",
                            error_payload.message
                        ),
                    )
                    .await;
                    return self
                        .fail_active_turn(
                            session_id,
                            turn_id,
                            format!(
                                "Model provider {provider} rejected its credentials \
                                 (auth/key error). An operator needs to fix the key."
                            ),
                        )
                        .await;
                }
                // Provider-side content/safety block: a DISTINCT, non-escalating
                // outcome from SwitchProvider. Switching to a different-behaving
                // model mid-conversation is exactly the jarring failover this
                // class exists to avoid — fail the turn cleanly with a message
                // that names what happened, and flag the heal queue as its own
                // `content_blocked` tag (escalate-not-restart: no guest restart
                // or provider swap fixes a safety-filtered prompt).
                ProviderErrorClass::ContentBlocked => {
                    let provider = error_payload
                        .provider
                        .clone()
                        .unwrap_or_else(|| "unknown".into());
                    warn!(
                        session_id = %session_id,
                        sub_kind = %sub_kind,
                        provider = %provider,
                        "Provider content/safety block — failing turn cleanly (no provider switch)"
                    );
                    self.push_heal_event(
                        &format!("content_blocked:{provider}"),
                        &format!(
                            "Provider {provider} blocked the response on content/safety \
                             grounds for session {session_id} turn {turn_id}: {}",
                            error_payload.message
                        ),
                    )
                    .await;
                    return self
                        .fail_active_turn(
                            session_id,
                            turn_id,
                            // "content_blocked" marker: `classify_turn_failure`
                            // (ansible-mesh-core) skips lines carrying it so the
                            // FailTask intake path (which otherwise always sees
                            // fail_active_turn's hardcoded MODEL_EMPTY_RESPONSE
                            // error_code) doesn't double-report this as a
                            // second, differently-tagged heal-queue entry.
                            format!(
                                "content_blocked: The model provider ({provider}) declined to \
                                 respond due to its content/safety filter. No fallback provider \
                                 was tried — ask again, or rephrase, or check the agent's \
                                 content_policy setting."
                            ),
                        )
                        .await;
                }
                // Not a model-provider escalation signal — fall through to the
                // generic fail path below.
                ProviderErrorClass::Unclassified => {}
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
                // Speaking and continuing used to be mutually exclusive: the
                // model could populate `partial_replies`, but the loop only
                // flushed them on the Respond arm — the moment the turn ends.
                // So the only way to be heard was to stop, which is why a long
                // plan ran silent to its wall-clock budget and why "say what
                // you need" had to be a terminal exit. Flushing here lets a
                // turn report and keep working; the gate keeps it from
                // becoming the per-step receipt stream operators objected to.
                if !partial_replies.is_empty() {
                    let admissible = self
                        .sessions
                        .get(&session_id)
                        .and_then(|s| s.active_turn.as_ref())
                        .map(|t| {
                            interim_reply_admissible(
                                t.started_at_unix,
                                t.last_interim_at_unix,
                                t.iteration,
                                unix_now(),
                            )
                        })
                        .unwrap_or(false);
                    if admissible {
                        // One note per opportunity: the model sometimes emits
                        // several, and sending them all is the chatter the gate
                        // is meant to prevent.
                        if let Some(partial) = partial_replies.into_iter().next() {
                            self.emit_partial_reply(&session_id, partial).await?;
                        }
                    } else {
                        debug!(
                            session_id = %session_id,
                            "Declining mid-turn interim reply (too soon since the last one)."
                        );
                    }
                }

                let forced_stop_reply = self.sessions.get(&session_id).and_then(|state| {
                    let turn = state.active_turn.as_ref()?;
                    let iteration_cap =
                        effective_iteration_cap(state.settings.execution.iteration_cap, turn);
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

    /// A still-running tool reports it is alive: refresh this session's PHASE
    /// watchdog so a slow-but-healthy tool is not evicted mid-flight.
    ///
    /// Before this existed, the phase watchdog could not distinguish a slow tool
    /// from a dead one, so the only way to survive being slow was a bespoke
    /// per-tool constant (`delegate.whisper`'s 660s) — which does not
    /// generalise, and left every other long tool one eviction away from having
    /// its turn poisoned by its own late result.
    ///
    /// SAFETY PROPERTY — this must never let a wedged tool run forever. It
    /// clears only `stuck_turn_first_seen`, the PHASE clock. The total-turn
    /// ceiling (`MAX_TOTAL_ACTIVE_SECS`) is measured from `total_active_since`,
    /// a SEPARATE map this function does not touch, so the 600s catch-all still
    /// fires on schedule no matter how many pings arrive. Keep it that way: if
    /// these two clocks are ever merged, pings become an unbounded stay of
    /// execution and a hung tool hangs the session forever — strictly worse
    /// than eviction.
    ///
    /// Ignores a ping for anything but the current active turn, so a late ping
    /// from an already-evicted tool cannot hold a NEW turn's watchdog open.
    pub(super) fn handle_tool_progress(&mut self, task: &InboundTaskPayload) {
        let Some(session_id) = task.session_id.as_deref().filter(|s| !s.is_empty()) else {
            return;
        };
        let matches_active_turn = self
            .sessions
            .get(session_id)
            .and_then(|state| state.active_turn.as_ref())
            .is_some_and(|turn| {
                task.turn_id
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .is_none_or(|incoming| incoming == turn.turn_id)
            });
        if !matches_active_turn {
            return;
        }
        let now = std::time::Instant::now();
        // Refresh the AUTHORITATIVE phase clock, not just the watchdog's derived map.
        // `evict_timed_out_turns` re-anchors `stuck_turn_first_seen` from
        // `turn_waiting_since` on every tick, so stamping only the map would have this
        // ping silently overwritten within one tick (~5s) and the keepalive would do
        // nothing. Both are stamped: the state field is what actually survives.
        //
        // `active_turn_since` is deliberately NOT touched — that is the total-turn
        // ceiling, and a wedged tool that keeps pinging must never be able to push it
        // back. The two clocks stay separate; see
        // `tool_progress_refreshes_phase_clock_but_never_the_total_turn_ceiling`.
        if let Some(state) = self.sessions.get_mut(session_id) {
            state.turn_waiting_since = Some(now);
        }
        if self
            .stuck_turn_first_seen
            .insert(session_id.to_string(), now)
            .is_some()
        {
            debug!(
                session_id = %session_id,
                tool = task.tool_name.as_deref().unwrap_or("unknown"),
                "tool progress ping — phase watchdog refreshed (total-turn ceiling unaffected)"
            );
        }
    }

    /// Describe what a dropped tool result says it wrote, or `None` when the
    /// payload shows nothing durable landed.
    ///
    /// Only used on the stale-drop path, where the tool ran to completion but
    /// its turn is gone. Read-only tools (`life.recall`, `*.status`) and clean
    /// rejections change nothing and must NOT raise a heal event — otherwise
    /// every evicted read would file diagnostic noise.
    ///
    /// Conservative by construction: it reports only what the payload proves,
    /// so an unparseable or unrecognised body yields `None` rather than a
    /// guess. A missed orphan is a quiet gap; a fabricated one would send
    /// someone hunting writes that never happened.
    pub(super) fn orphaned_write_summary(tool: &str, content: Option<&str>) -> Option<String> {
        let value: serde_json::Value = serde_json::from_str(content?).ok()?;

        // Batch shape: any successful item is a durable write.
        if let Some(succeeded) = value.get("succeeded").and_then(serde_json::Value::as_u64) {
            if succeeded == 0 {
                return None;
            }
            let requested = value
                .get("requested")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(succeeded);
            return Some(format!(
                "{succeeded} of {requested} observation(s) were written before the result was \
                 dropped"
            ));
        }

        // Single-write shape: the terminal states that mean the graph changed.
        let status = value.get("status").and_then(serde_json::Value::as_str)?;
        if !matches!(
            status,
            "proposed" | "committed" | "resolved" | "applied" | "patch_applied"
        ) {
            return None;
        }
        let node = value
            .get("node_id")
            .or_else(|| value.get("patch_id"))
            .or_else(|| value.get("conflict_id"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("(id not reported)");
        Some(format!("{tool} wrote {node} (status={status})"))
    }

    pub(super) async fn handle_tool_result(&mut self, task: InboundTaskPayload) -> Result<()> {
        let session_id = match task.session_id.as_deref().filter(|s| !s.is_empty()) {
            Some(session_id) => session_id.to_string(),
            None => return Ok(()),
        };
        // Staleness guard — `handle_model_response` has always dropped a
        // response whose turn_id is not the active turn; tool results had no
        // such check, so a LATE result was applied to whatever turn happened to
        // be active. That is reachable whenever a tool outlives its turn: a long
        // `life.observe.batch` blows the 90s WaitingTool watchdog, the turn is
        // evicted (`active_turn = None`), the operator sends another message,
        // and the runner's reply — which still carries the ORIGINAL turn_id, and
        // which nothing cancels — lands on the new turn, pushing a foreign tool
        // result into its history and clearing its pending tool call, so the new
        // turn then hangs on a call that can never complete. One slow tool call
        // thus poisons the following turn.
        //
        // Deliberately narrower than the model-response guard: it fires ONLY on
        // an explicit, non-empty, mismatched turn_id. The ~100 in-process
        // callers in `tool_exec.rs` either pass the current turn's id (match,
        // unaffected) or pass none (the fallback below, unaffected), and some
        // legitimately run with no active turn at all — so "no active turn" is
        // NOT treated as stale here.
        let stale_drop = {
            let active_turn = self
                .sessions
                .get(&session_id)
                .and_then(|state| state.active_turn.as_ref());
            match (
                task.turn_id.as_deref().filter(|s| !s.is_empty()),
                active_turn.map(|turn| turn.turn_id.as_str()),
            ) {
                (Some(incoming), Some(active)) if incoming != active => {
                    Some((incoming.to_string(), active.to_string()))
                }
                _ => None,
            }
        };
        if let Some((stale_turn_id, active_turn_id)) = stale_drop {
            let tool = task.tool_name.as_deref().unwrap_or("unknown").to_string();
            warn!(
                session_id = %session_id,
                stale_turn_id = %stale_turn_id,
                active_turn_id = %active_turn_id,
                tool = %tool,
                "handle_tool_result: dropping stale tool result — its turn is no longer \
                 active (likely evicted by the watchdog while the tool ran)"
            );
            // Reconciliation: dropping the result keeps the NEW turn clean, but
            // the tool already ran. A write tool that reached the graph has
            // changed durable state for a turn the operator was told had died —
            // silently discarding that report would leave the graph and the
            // conversation disagreeing, and the agent would happily write the
            // same observations again. Surface it into the self-heal queue so
            // the divergence is recoverable rather than invisible.
            if let Some(landed) = Self::orphaned_write_summary(&tool, task.content.as_deref()) {
                let detail = format!(
                    "session={session_id} stale_turn={stale_turn_id} active_turn={active_turn_id} \
                     tool={tool} — the turn was evicted while this write tool was still running; \
                     {landed}. The writes are DURABLE and already in the graph: reconcile before \
                     re-observing the same facts."
                );
                self.push_heal_event("orphaned_tool_write", &detail).await;
            }
            return Ok(());
        }

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
            let tool_call = {
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
                // NOTE: iteration is bumped below, once, on whichever path is
                // actually taken — the retry path below already counts its
                // own re-entry via retry_active_turn_after_provider_failure,
                // so incrementing here too would double-count one round-trip
                // against iteration_cap.
                tool_call
            };

            // A life.observe failure tagged sub_kind "invalid_request" is a
            // pre-write contract/parameter error (see
            // datasource::controller::CONTRACT_ERROR_MARKER, set by
            // LifeGraphProvider::handle_observe) — something the model can
            // reasonably self-correct, as opposed to a transport/routing
            // failure (unmarked; handled by the existing paths below/
            // elsewhere, unchanged). 2026-07-10 LifeGraph forensic: today
            // these get apologized-and-dropped with no retry.
            let is_contract_error = step_failed
                && task.error.as_ref().and_then(|e| e.sub_kind.as_deref())
                    == Some("invalid_request");
            // The direct-command parser (memory_integration.rs) has no model
            // in the loop to act on a corrective note — a bad payload there
            // is a parser bug, not a retryable model mistake, so it always
            // skips straight to the heal_queue branch below.
            let is_direct_origin = is_direct_life_observe_origin(&tool_call.arguments);
            let repair_attempts = self
                .sessions
                .get(&session_id)
                .map(|s| s.provider_repair_attempts())
                .unwrap_or(0);

            if is_contract_error && !is_direct_origin && repair_attempts < 1 {
                let cause = task
                    .error
                    .as_ref()
                    .map(|e| e.message.clone())
                    .unwrap_or_default();
                warn!(
                    session_id = %session_id,
                    turn_id = %turn_id,
                    "life.observe contract error; giving the model one bounded retry with cause surfaced"
                );
                return self
                    .retry_active_turn_after_provider_failure(
                        session_id,
                        turn_id,
                        Some(life_observe_repair_note(&cause)),
                    )
                    .await;
            }

            // Not retrying (success, non-contract failure, direct-command
            // origin, or an already-exhausted retry): this life.observe call
            // is finalizing as its own tool round-trip, so count it the same
            // way the generic re-entry path below does.
            if let Some(turn) = self
                .sessions
                .get_mut(&session_id)
                .and_then(|s| s.active_turn.as_mut())
            {
                turn.iteration += 1;
            }

            if is_contract_error {
                // Either this call can't retry (direct-command origin) or the
                // model's one bounded retry already failed again — don't drop
                // it silently. File a heal_queue entry so it's A3-visible
                // instead of just an apology to the operator.
                let cause = task
                    .error
                    .as_ref()
                    .map(|e| e.message.clone())
                    .unwrap_or_default();
                self.push_heal_event(
                    "life_observe_parse_failed",
                    &format!(
                        "life.observe contract error for session {session_id} turn {turn_id} \
                         ({}): {cause}",
                        if is_direct_origin {
                            "direct-command origin, no retry attempted"
                        } else {
                            "retry exhausted"
                        }
                    ),
                )
                .await;
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

            // Earned streak (cognitive-loop-streak-extension seam): judge the
            // step against the history BEFORE it is pushed — a successful,
            // novel, non-diagnostic step is forward progress, not loop
            // evidence, and buys the turn one extra iteration below.
            let earns_streak = state
                .active_turn
                .as_ref()
                .map(|turn| tool_step_earns_streak(turn, &tool_call, &tool_result, step_failed))
                .unwrap_or(false);

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
                if earns_streak {
                    turn.streak_extension = turn.streak_extension.saturating_add(1);
                }
                turn.iteration += 1;
                turn.phase = TurnPhase::WaitingModel;
            }

            let iteration = state.active_turn.as_ref().map(|t| t.iteration).unwrap_or(0);
            let configured_cap = state.settings.execution.iteration_cap;
            let iteration_cap = state
                .active_turn
                .as_ref()
                .map(|turn| effective_iteration_cap(configured_cap, turn))
                .unwrap_or(configured_cap);
            if iteration_cap > configured_cap && iteration >= configured_cap {
                info!(
                    session_id = %session_id,
                    iteration,
                    configured_cap,
                    effective_cap = iteration_cap,
                    "Turn running on earned streak-extended iteration budget."
                );
            }
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

                let response_contract = Some(cognitive_response_contract(&[
                    "spoken_text",
                    "memory_candidate",
                    "active_plan",
                ]));
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
                let mut model_req = ModelRequestPayload {
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
                    model: None,
                    provider_options: resolve_content_policy_provider_options(
                        self.sessions.get(&session_id),
                    ),
                    chat_id,
                    reply_to: local_node_id(),
                    reply_role: "agent".into(),
                    reply_guest_id: Some(self.own_guest_id()),
                    final_reply_to,
                    final_reply_role,
                    final_reply_guest_id,
                    agent_id: Some(self.agent_id.clone()),
                    oracle_pick: None,
                    oracle_agreement: None,
                };

                let (target_node, target_role, target_guest_id) = resolve_model_execution_target(
                    self.sessions.get(&session_id),
                    "text.generate",
                    DEFAULT_TEXT_MODEL_ROLE,
                );
                model_req.model = role_model_binding(self.sessions.get(&session_id), &target_role);

                // Shadow-mode (PHILOTIC_SHADOW_ORACLE, default OFF): annotate the
                // task with the oracle's top pick vs this ladder-resolved role.
                // Log-only — the dispatch target below is unchanged. Zero cost
                // when the flag is off (guarded inside `shadow_oracle_pick`).
                let (shadow_pick, shadow_agreement) = self.shadow_oracle_pick(&target_role).await;
                model_req.oracle_pick = shadow_pick;
                model_req.oracle_agreement = shadow_agreement;

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

        let response_contract = Some(cognitive_response_contract(&[
            "spoken_text",
            "memory_candidate",
            "active_plan",
        ]));
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
        let mut model_req = ModelRequestPayload {
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
            model: None,
            provider_options: resolve_content_policy_provider_options(
                self.sessions.get(&session_id),
            ),
            chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            reply_guest_id: Some(self.own_guest_id()),
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            agent_id: Some(self.agent_id.clone()),
            oracle_pick: None,
            oracle_agreement: None,
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
        model_req.model = role_model_binding(self.sessions.get(&session_id), &target_role);

        // Shadow-mode (PHILOTIC_SHADOW_ORACLE, default OFF): log-only oracle-vs-
        // ladder annotation. Never alters the dispatch target. Zero cost when
        // the flag is off (guarded inside `shadow_oracle_pick`).
        let (shadow_pick, shadow_agreement) = self.shadow_oracle_pick(&target_role).await;
        model_req.oracle_pick = shadow_pick;
        model_req.oracle_agreement = shadow_agreement;

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
    ///
    /// When the static ladder is exhausted, the hotel's routing oracle is
    /// consulted for the next-best *different* provider before giving up
    /// (`failed_provider` and every ladder provider are excluded, so the
    /// reroute can never land back on a provider that already failed this
    /// turn). Operator intent wins: the configured `fallback_tiers` always
    /// run first; the oracle is only the safety net beneath them, capped at
    /// [`MAX_ORACLE_EXTRA_TIERS`] extra dispatches and disabled entirely by
    /// `PHILOTIC_DISABLE_ROUTING_ORACLE=1`. On `EvictTurn` (all tiers and
    /// oracle options exhausted) the turn is failed with a user-visible error.
    ///
    /// When the turn's `selection_source` is `OperatorExplicit` (session
    /// pinned via `/model <tier>`), automatic escalation is refused entirely
    /// — silently switching tiers would violate the operator's explicit
    /// intent — and the turn fails fast via `fail_active_turn` with
    /// `trigger_message` as the user-visible error. Unlike ladder/oracle
    /// exhaustion, this path does not push a heal event: the pinned tier
    /// failing is expected operator-directed behavior, not an outage worth
    /// flagging for the self-heal queue.
    pub(super) async fn advance_turn_to_next_fallback_tier(
        &mut self,
        session_id: String,
        turn_id: String,
        class: NoResponseClass,
        failed_provider: Option<String>,
        trigger_message: String,
    ) -> Result<()> {
        let selection_source = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| t.selection_source)
            .unwrap_or_default();
        if selection_source == SelectionSource::OperatorExplicit {
            return self
                .fail_active_turn(session_id, turn_id, trigger_message)
                .await;
        }

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

        // Per-agent model NAME bindings (Layer 1 — same precedence source as
        // `configured_tiers` above, so a live `ConfigureRole` edit and the
        // persisted role record agree). Resolved per-tier below by role key,
        // so each fallback tier gets its own bound model, not just the
        // primary dispatch.
        let configured_model_bindings: std::collections::BTreeMap<String, String> =
            active_role_name
                .as_deref()
                .and_then(|rn| self.configured_roles.get(rn))
                .map(|c| c.turn_loop_config.model_bindings.clone())
                .or_else(|| {
                    self.sessions
                        .get(&session_id)
                        .and_then(|s| s.role_activation.as_ref())
                        .and_then(|ra| ra.turn_loop_config.as_ref())
                        .map(|tlc| tlc.model_bindings.clone())
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

        // Off-by-one fix: `fallback_tier` starts at 0 for every turn regardless
        // of whether the *primary* dispatch actually came from the configured
        // ladder (see `resolve_model_execution_target`'s precedence — a hotel
        // route or explicit binding can win over the ladder). If tier 0 was
        // never dispatched from the ladder, the walk must start at tiers[0] on
        // failure, not tiers[1] — otherwise a single-tier ladder is never
        // reachable at all. `None` here means "the ladder hasn't been
        // consulted yet"; `next_ladder_tier` then starts the walk at 0.
        //
        // `primary_dispatch_used_ladder` is a stateless re-derivation from
        // session config — it can't tell "virgin primary" apart from "tier 0
        // was already (re-)dispatched via this same bypass path on a prior
        // failure" (both look like `current_tier == 0` with the primary
        // bypassing the ladder). `ladder_tier0_dispatched` is the per-turn
        // memory that breaks that tie, so a second/third/... failure on the
        // same turn advances past tier 0 instead of re-dispatching it forever.
        let ladder_tier0_already_dispatched = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| t.ladder_tier0_dispatched)
            .unwrap_or(false);
        let last_ladder_tier: Option<u8> = if !configured_tiers.is_empty()
            && current_tier == 0
            && !ladder_tier0_already_dispatched
            && !primary_dispatch_used_ladder(self.sessions.get(&session_id), "text.generate")
        {
            None
        } else {
            Some(current_tier)
        };

        // On a contract failure the failed provider's remaining ladder tiers
        // are skipped — the same request fails identically there. The skip can
        // exhaust the ladder early, which then falls to the oracle as usual.
        let skip_failed_provider = class == NoResponseClass::ProviderContractFailure;
        let ladder_next = next_ladder_tier(
            &configured_tiers,
            last_ladder_tier,
            max_tier,
            failed_provider.as_deref(),
            skip_failed_provider,
        );
        let tiers_remaining = ladder_next <= max_tier;

        // Ladder exhausted: ask the routing oracle for a next-best different
        // provider before giving up. Static tiers keep precedence — this
        // branch is only reachable when no configured tier remains.
        let oracle_role: Option<String> = if tiers_remaining {
            None
        } else {
            self.consult_routing_oracle(
                &configured_tiers,
                current_tier,
                max_tier,
                failed_provider.as_deref(),
            )
            .await
        };

        if oracle_role.is_none()
            && decide_no_response_action(class, tiers_remaining) == NoResponseAction::EvictTurn
        {
            // Turn-failure heal intake: ladder + oracle exhaustion flows into
            // the self-heal queue so recurring provider outages surface as A3
            // work items instead of only being discovered by the operator.
            let last_provider = failed_provider.as_deref().unwrap_or("unknown");
            self.push_heal_event(
                &format!("fallback_exhausted:{last_provider}"),
                &format!(
                    "All model providers failed for session {session_id} turn {turn_id} \
                     (tier {current_tier}/{max_tier}, class {class:?}, last provider {last_provider})."
                ),
            )
            .await;
            return self
                .fail_active_turn(
                    session_id,
                    turn_id,
                    "All model providers failed. Please try again later.".into(),
                )
                .await;
        }

        // Oracle dispatches count tiers linearly (current + 1) so the
        // MAX_ORACLE_EXTRA_TIERS budget stays exact; ladder dispatches land on
        // the (possibly skip-advanced) next live tier.
        let is_oracle_dispatch = oracle_role.is_some();
        let next_tier = if is_oracle_dispatch {
            current_tier.saturating_add(1)
        } else {
            ladder_next
        };
        let next_role =
            oracle_role.unwrap_or_else(|| role_for_tier(&configured_tiers, next_tier).to_string());

        // Gather everything we need before dropping the mutable state borrow.
        let plan = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                return Ok(());
            };

            if let Some(turn) = state.active_turn.as_mut() {
                turn.fallback_tier = next_tier;
                // Mark the ladder as engaged whenever a real ladder tier (not
                // an oracle role) was dispatched, so a later failure on this
                // same turn advances the walk instead of re-deriving the same
                // "bypassed" verdict from static session config and getting
                // stuck re-dispatching tier 0.
                if !is_oracle_dispatch {
                    turn.ladder_tier0_dispatched = true;
                }
                turn.phase = TurnPhase::WaitingModel;
                turn.iteration += 1;
                turn.selection_source = SelectionSource::AutoFallback;
            }

            // Slice 2: persist a narrow FallbackOverride so the NEXT turn (not
            // just this one) starts on the tier that just took over, and so
            // the periodic origin-tier probe (`probe_degraded_sessions`) knows
            // what to health-check. Origin (the tier the session would use
            // with no override) and `since_epoch_ms` are set once and held
            // stable across repeated escalations on the same degraded session;
            // only `active_tier_role`/`reason`/`last_probe_epoch_ms` track the
            // most recent failure.
            let reason = no_response_reason_str(class);
            let now_ms = now_epoch_ms();
            // Slice 3: the `model_fallback` operational notice fires exactly
            // once per degraded stretch — latched by `notice_sent` on the
            // override itself so sticky turns (further tier walks on an
            // already-notified session) stay silent. `should_notify_fallback`
            // is decided here (before/at the same time notice_sent flips)
            // because the escalation's checkpoint write below is the same
            // write that persists the latch — see `escalation_writes_fallback_override_then_updates_active_only`.
            let should_notify_fallback = match state.fallback_override.as_mut() {
                Some(existing) => {
                    existing.active_tier_role = next_role.clone();
                    existing.reason = reason.to_string();
                    let should_notify = !existing.notice_sent;
                    if should_notify {
                        existing.notice_sent = true;
                    }
                    should_notify
                }
                None => {
                    state.fallback_override = Some(FallbackOverride {
                        origin_tier_role: role_for_tier(&configured_tiers, 0).to_string(),
                        active_tier_role: next_role.clone(),
                        reason: reason.to_string(),
                        since_epoch_ms: now_ms,
                        last_probe_epoch_ms: now_ms,
                        notice_sent: true,
                    });
                    true
                }
            };
            let fallback_notice = should_notify_fallback.then(|| {
                let ov = state
                    .fallback_override
                    .as_ref()
                    .expect("just written above");
                (
                    ov.active_tier_role.clone(),
                    ov.origin_tier_role.clone(),
                    ov.reason.clone(),
                )
            });

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
                        fallback_notice,
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
            fallback_notice,
        ) = plan;

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .emit_turn_event(&session_id, "loop_recovering", None)
            .await;

        // Surface the provider switch (from → to, why) so membranes can show
        // "switching models…" through the existing turn-event path.
        {
            let switch_from = failed_provider.as_deref().unwrap_or("unknown");
            let switch_to = provider_for_role(&next_role).unwrap_or_else(|| next_role.clone());
            let switch_reason = no_response_reason_str(class);
            let _ = self
                .emit_turn_event(
                    &session_id,
                    "provider_switch",
                    Some(format!("{switch_from} -> {switch_to} ({switch_reason})")),
                )
                .await;
            // Heal intake for mid-ladder degradation: a SUCCESSFUL fallback
            // hides tier loss entirely — the task never fails, so FailTask
            // classification never runs and zero heal rows are filed while an
            // origin tier is down (2026-07-20: a schema-induced Gemini 400
            // silenced Aria's origin tier for hours, invisible to the heal
            // lane because the fallback ladder kept answering). Recurrence in
            // the dispatcher turns a persistent burst into an A3 work item;
            // the hotel's per-(guest, tag) 60s flood window absorbs sticky
            // sessions walking tiers repeatedly.
            self.push_heal_event(
                &format!("provider_fallback:{switch_from}"),
                &format!(
                    "Model tier fallback {switch_from} -> {switch_to} ({switch_reason}) \
                     for session {session_id}: origin tier degraded but the ladder answered."
                ),
            )
            .await;
        }

        // Slice 3: one-time operational notice so the operator learns their
        // session degraded to a fallback tier — latched by `notice_sent` above
        // so sticky turns (further tier walks while already-notified) stay
        // silent. This is a plain informational message, not an assistant
        // reply: membranes must render it without draft-edit streaming or TTS
        // (see membrane-telegram's `model_fallback` turn_event arm).
        if let Some((active_role, origin_role, reason)) = fallback_notice {
            let message = format!("↪️ Model fallback: {active_role} (was {origin_role}; {reason})");
            let _ = self
                .emit_turn_event(&session_id, "model_fallback", Some(message))
                .await;
        }

        let response_contract = Some(cognitive_response_contract(&[
            "spoken_text",
            "memory_candidate",
            "active_plan",
        ]));
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
            model: configured_model_bindings.get(&next_role).cloned(),
            provider_options: resolve_content_policy_provider_options(
                self.sessions.get(&session_id),
            ),
            chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            reply_guest_id: Some(self.own_guest_id()),
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            agent_id: Some(self.agent_id.clone()),
            oracle_pick: None,
            oracle_agreement: None,
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

    /// Ask the hotel's routing oracle for the next-best live model controller
    /// once the static fallback ladder is exhausted. Returns the controller
    /// role to dispatch to, or `None` when the oracle is disabled, the extra-
    /// tier budget is spent, the IPC query fails, or every ranked option is a
    /// role/provider this turn already tried.
    ///
    /// The exclude list is every provider implied by the ladder's tiers plus
    /// the provider that just failed, so the oracle can never route straight
    /// back into the failure (the hotel also filters `exclude_providers`
    /// server-side and only returns roles with a live guest).
    async fn consult_routing_oracle(
        &mut self,
        configured_tiers: &[String],
        current_tier: u8,
        max_tier: u8,
        failed_provider: Option<&str>,
    ) -> Option<String> {
        if ansible_mesh_core::model_oracle::routing_oracle_disabled() {
            return None;
        }
        if current_tier >= max_tier.saturating_add(MAX_ORACLE_EXTRA_TIERS) {
            return None;
        }

        let ladder: Vec<String> = if configured_tiers.is_empty() {
            DEFAULT_FALLBACK_TIERS
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            configured_tiers.to_vec()
        };
        let tried_roles: std::collections::HashSet<String> = ladder.iter().cloned().collect();
        let mut exclude_providers: Vec<String> =
            ladder.iter().filter_map(|r| provider_for_role(r)).collect();
        if let Some(p) = failed_provider {
            if !exclude_providers.iter().any(|x| x == p) {
                exclude_providers.push(p.to_string());
            }
        }

        let resp = match self
            .ipc_client
            .send_request(IpcRequest::QueryModelRoute {
                request_class: "cognitive".to_string(),
                needs_tools: true,
                needs_structured: true,
                approx_context_tokens: 0,
                latency_class: "interactive".to_string(),
                trust_ceiling: "remote_cloud".to_string(),
                exclude_providers,
            })
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                warn!("Routing oracle query failed: {e}");
                return None;
            }
        };

        let (role, provider) = match resp {
            IpcResponse::Standard {
                ok: true,
                data: Some(data),
                ..
            } => pick_oracle_role(&data, &tried_roles)?,
            _ => return None,
        };

        info!(
            provider_from = %failed_provider.unwrap_or("unknown"),
            provider_to = %provider,
            target_role = %role,
            reason = "fallback_ladder_exhausted",
            "Routing oracle reroute: dispatching beneath exhausted ladder"
        );
        Some(role)
    }

    /// Shadow-mode (`PHILOTIC_SHADOW_ORACLE`, default OFF) oracle-vs-ladder
    /// comparison for the healthy dispatch path (Model Oracle Primary
    /// Authority, slice 1).
    ///
    /// Returns `(oracle_pick, agreement)` to stamp onto the outgoing model task
    /// so the model-router's trace store can persist whether the oracle's top
    /// pick over the FULL healthy provider set matched `resolved_role` (the
    /// role the ladder actually chose). This is **strictly log-only** — callers
    /// never change their dispatch target based on the result.
    ///
    /// Invariants:
    /// - When the flag is OFF this does ZERO work (no IPC, no allocation) and
    ///   returns `(None, None)` before touching `ipc_client`.
    /// - The query uses an EMPTY `exclude_providers` so the hotel ranks the
    ///   full healthy set AND takes the pure read path in
    ///   `handle_query_model_route` (the heal-queue reroute write is gated on a
    ///   non-empty exclude list, so shadow queries have no side effects).
    /// - On any oracle error / non-success / empty ranking it returns
    ///   `(None, None)` (divergence-unknown) and never fails the turn.
    pub(super) async fn shadow_oracle_pick(
        &mut self,
        resolved_role: &str,
    ) -> (Option<String>, Option<bool>) {
        if !ansible_mesh_core::model_oracle::shadow_oracle_enabled() {
            return (None, None);
        }

        let resp = self
            .ipc_client
            .send_request(IpcRequest::QueryModelRoute {
                request_class: "cognitive".to_string(),
                needs_tools: true,
                needs_structured: true,
                approx_context_tokens: 0,
                latency_class: "interactive".to_string(),
                trust_ceiling: "remote_cloud".to_string(),
                // Empty exclude → rank the full healthy set AND keep the hotel
                // handler on its read-only path (no reroute heal-queue write).
                exclude_providers: Vec::new(),
            })
            .await;

        // Reuse `pick_oracle_role` with an empty tried-set so it yields the
        // oracle's unconditional top-ranked entry.
        let top = match resp {
            Ok(IpcResponse::Standard {
                ok: true,
                data: Some(data),
                ..
            }) => pick_oracle_role(&data, &std::collections::HashSet::new()),
            Ok(_) => None,
            Err(e) => {
                warn!("shadow-oracle query failed (divergence-unknown): {e}");
                None
            }
        };

        let (pick, agreement) = ansible_mesh_core::model_oracle::shadow_oracle_agreement(
            top.as_ref().map(|(r, p)| (r.as_str(), p.as_str())),
            resolved_role,
        );

        match agreement {
            Some(agree) => info!(
                resolved_role = %resolved_role,
                oracle_pick = %pick.as_deref().unwrap_or("?"),
                agreement = agree,
                "shadow-oracle: oracle-vs-ladder agreement (log-only)"
            ),
            None => info!(
                resolved_role = %resolved_role,
                "shadow-oracle: oracle pick unavailable (divergence-unknown)"
            ),
        }

        (pick, agreement)
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

            // Streaming mode (operator_chat): the sentence pipeline was armed
            // at turn start and has been synthesizing while tokens streamed.
            // Flush the tail and drain instead of one batch synthesis.
            if self.voice_chunk_pipeline_matches(&session_id, &turn_id) {
                return self
                    .finalize_streaming_voice_turn(
                        session_id,
                        turn_id,
                        content,
                        spoken_text,
                        voice_policy,
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
        // Sentence-pipelined TTS: the turn is over — any leftover pipeline is
        // stale (the streaming path removes its own pipeline before calling
        // here, so this only clears policy-flip / non-streaming residue).
        self.clear_voice_chunk_pipeline(&session_id);

        // LifeGraph auto-capture fork (Slice E2): lived-fact candidates ALSO
        // flow to the graph as proposed nodes. Fork, not move — the Muninn
        // Attend hook below still receives the same candidate. Runs before
        // turn completion so the turn event has an active turn to attach to;
        // fire-and-forget, so it never blocks or fails the reply.
        self.maybe_autocapture_life_fact(&session_id, memory_candidate.as_ref())
            .await;

        let plan_budget = self.plan_continuation_budget_for(&session_id);
        let (completed_turn, checkpoint_memory_type, checkpoint_json, index_state, plan_followup) = {
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

            // Plan-eval-repeat: derive the completion verdict for this turn's
            // plan (or a deferred carryover) and update the checkpointed
            // carryover BEFORE the checkpoint below is built.
            let plan_followup = plan_followup_after_turn(state, &completed_turn, plan_budget);

            (
                completed_turn,
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
                plan_followup,
            )
        };

        // Routing snapshot for post-completion plan events / continuation, taken
        // before `completed_turn` fields are moved into the reply payload below.
        let plan_route = plan_followup.as_ref().map(|_| {
            (
                completed_turn.turn_id.clone(),
                completed_turn.chat_id.clone(),
                completed_turn.final_reply_to.clone(),
                completed_turn.final_reply_role.clone(),
                completed_turn.final_reply_guest_id.clone(),
            )
        });

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

        // Capture whether the model actually produced user-facing text BEFORE the
        // attribution tag is appended (the tag would otherwise make an empty reply
        // look non-empty). Used by the reflective-re-entry branch to decide between
        // surfacing the reply and absorbing a genuinely silent completion.
        let reply_is_empty = content.trim().is_empty();

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
                .unwrap_or(&completed_turn.chat_id)
                .to_string();
            // Reflective re-entry, top-of-chain: reply_session_id loops back to our
            // own session, meaning this was the orchestrator's reflection turn after
            // receiving the specialist's response.
            let is_top_of_chain = reply_session_id == session_id;
            if is_top_of_chain {
                // The orchestrator did not call delegate.merge explicitly. If it still
                // produced user-facing text, surface it (implicit merge) — that is
                // almost always what the reply was for, and silently dropping it is the
                // "I never see it work" failure mode. Only a genuinely empty completion
                // is absorbed, which preserves the deliberate "reflect privately and
                // stay quiet" path (the model completes with no content to stay silent).
                if reply_is_empty {
                    info!(
                        "deliver_text_reply: reflective re-entry turn {} completed empty — absorbing silently",
                        turn_id
                    );
                    self.drain_next_user_task(&attend_session_id);
                    return Ok(());
                }
                info!(
                    "deliver_text_reply: reflective re-entry turn {} produced text without delegate.merge — surfacing to user (implicit merge)",
                    turn_id
                );
                let reply_payload = FinalReplyPayload {
                    action: "send_reply",
                    session_id,
                    turn_id,
                    chat_id: reply_chat_id,
                    content,
                    audio_artifact,
                    send_text_caption,
                    reply_markup: None,
                };
                serde_json::to_string(&reply_payload)?
            } else {
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
            }
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

        // Plan-eval-repeat: emit the plan_eval event and either synthesize a
        // budgeted continuation turn or notify the operator why the loop stopped.
        // Runs after the drain so a queued user task keeps priority — the
        // carryover then resumes after that user turn completes.
        if let (Some(followup), Some((p_turn_id, p_chat_id, p_reply_to, p_reply_role, p_guest))) =
            (plan_followup, plan_route)
        {
            if let Err(e) = self
                .dispatch_plan_followup(
                    &attend_session_id,
                    followup,
                    plan_budget,
                    p_turn_id,
                    p_chat_id,
                    p_reply_to,
                    p_reply_role,
                    p_guest,
                )
                .await
            {
                warn!(
                    session_id = %attend_session_id,
                    "Plan follow-up dispatch failed (non-fatal): {}",
                    e
                );
            }
        }

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
        // Sentence-pipelined TTS: drop any streaming-voice state with the turn.
        self.clear_voice_chunk_pipeline(&session_id);
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
            state.active_turn_since = None;
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

    // ── Plan-eval-repeat ────────────────────────────────────────────────────

    /// Effective auto-continuation budget for this session's active role.
    /// `configured_roles` (live `role.configure` state) wins over the
    /// `turn_loop_config` embedded in the role activation; default 3.
    pub(super) fn plan_continuation_budget_for(&self, session_id: &str) -> u32 {
        let state = self.sessions.get(session_id);
        let role_name = state
            .and_then(|s| s.role_activation.as_ref())
            .map(|ra| ra.role_name.clone());
        role_name
            .as_deref()
            .and_then(|rn| self.configured_roles.get(rn))
            .and_then(|c| c.turn_loop_config.plan_continuation_budget)
            .or_else(|| {
                state
                    .and_then(|s| s.role_activation.as_ref())
                    .and_then(|ra| ra.turn_loop_config.as_ref())
                    .and_then(|tlc| tlc.plan_continuation_budget)
            })
            .unwrap_or(DEFAULT_PLAN_CONTINUATION_BUDGET)
    }

    /// Emit a plan-lifecycle turn event (`plan_eval` / `plan_continuation`)
    /// with explicit routing — the turn has already completed, so the standard
    /// `emit_turn_event` (which reads `active_turn`) cannot be used.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn emit_plan_turn_event(
        &mut self,
        session_id: &str,
        event: &str,
        detail: Option<String>,
        turn_id: &str,
        chat_id: &str,
        reply_to: &str,
        reply_role: &str,
        reply_guest_id: Option<String>,
    ) -> Result<()> {
        let payload = TurnEventPayload {
            action: "turn_event",
            event: event.to_string(),
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            chat_id: chat_id.to_string(),
            partial_content: detail,
        };
        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: reply_to.to_string(),
                target_role: reply_role.to_string(),
                target_guest_id: reply_guest_id,
                task_json: serde_json::to_string(&payload)?,
            })
            .await?;
        Ok(())
    }

    /// Act on the plan-eval outcome after a turn completed: emit the
    /// `plan_eval` event, then synthesize a continuation turn through
    /// `pending_drains` (verdict continue, budget remaining, no user work
    /// waiting) or send the operator one tight stop notice.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn dispatch_plan_followup(
        &mut self,
        session_id: &str,
        followup: PlanFollowup,
        budget: u32,
        turn_id: String,
        chat_id: String,
        reply_to: String,
        reply_role: String,
        reply_guest_id: Option<String>,
    ) -> Result<()> {
        match followup {
            PlanFollowup::Settled { eval_json } => {
                self.emit_plan_turn_event(
                    session_id,
                    "plan_eval",
                    Some(eval_json.to_string()),
                    &turn_id,
                    &chat_id,
                    &reply_to,
                    &reply_role,
                    reply_guest_id,
                )
                .await
            }
            PlanFollowup::Stop { eval_json, notice } => {
                if let Some(eval_json) = eval_json {
                    let _ = self
                        .emit_plan_turn_event(
                            session_id,
                            "plan_eval",
                            Some(eval_json.to_string()),
                            &turn_id,
                            &chat_id,
                            &reply_to,
                            &reply_role,
                            reply_guest_id.clone(),
                        )
                        .await;
                }
                // Recorded, not sent. This used to go to the user verbatim —
                // "*(Plan paused: a step failed or no forward progress was
                // made. … /plan drop to discard.)*" — which is the machinery
                // narrating itself into the conversation, and the operator
                // told us it read as transactional and robotic. Steps now run
                // and get verified inside the turn, so the model's own reply
                // is what the user should see; this stays as an operator- and
                // debug-facing turn event.
                let _ = self
                    .emit_plan_turn_event(
                        session_id,
                        "plan_stopped",
                        Some(notice),
                        &turn_id,
                        &chat_id,
                        &reply_to,
                        &reply_role,
                        reply_guest_id,
                    )
                    .await;
                Ok(())
            }
            PlanFollowup::Continue { eval_json } => {
                if let Some(eval_json) = eval_json {
                    let _ = self
                        .emit_plan_turn_event(
                            session_id,
                            "plan_eval",
                            Some(eval_json.to_string()),
                            &turn_id,
                            &chat_id,
                            &reply_to,
                            &reply_role,
                            reply_guest_id.clone(),
                        )
                        .await;
                }

                // User priority: if a user task is queued (or already drained
                // for dispatch), let it run first. The carryover stays put and
                // resumes after that turn completes.
                let user_waiting = self
                    .sessions
                    .get(session_id)
                    .map(|s| s.pending_user_task_count() > 0)
                    .unwrap_or(false)
                    || self
                        .pending_drains
                        .iter()
                        .any(|(_, t)| t.session_id.as_deref() == Some(session_id));
                if user_waiting {
                    info!(
                        session_id = %session_id,
                        "Plan continuation deferred — queued user work takes priority; \
                         carryover will resume after it completes."
                    );
                    return Ok(());
                }

                // Charge the budget and synthesize the continuation brief. The
                // budget shown to the model ("continuation 2/6") has to be the
                // scaled one the eval actually enforces, or the brief tells the
                // plan it has less room than it does.
                let (brief, budget) = {
                    let Some(state) = self.sessions.get_mut(session_id) else {
                        return Ok(());
                    };
                    let Some(carry) = state.carryover_plan.as_mut() else {
                        return Ok(());
                    };
                    let budget = scaled_continuation_budget(
                        budget,
                        carry
                            .plan
                            .steps
                            .len()
                            .saturating_sub(carry.steps_done_count()),
                    );
                    let brief = plan_continuation_brief(carry, budget);
                    carry.continuations_used += 1;
                    (brief, budget)
                };
                // Re-persist so the charged budget survives a restart.
                let _ = self.persist_session_checkpoint(session_id).await;

                let continuation_turn_id = Uuid::new_v4().to_string();
                let continuation = InboundTaskPayload {
                    action: Some("plan_continuation".into()),
                    session_id: Some(session_id.to_string()),
                    turn_id: Some(continuation_turn_id),
                    chat_id: Some(chat_id.clone()),
                    content: Some(brief),
                    final_reply_to: Some(reply_to.clone()),
                    final_reply_role: Some(reply_role.clone()),
                    final_reply_guest_id: reply_guest_id.clone(),
                    ..Default::default()
                };
                self.pending_drains
                    .push_back((Uuid::new_v4(), continuation));

                let used = self
                    .sessions
                    .get(session_id)
                    .and_then(|s| s.carryover_plan.as_ref())
                    .map(|c| c.continuations_used)
                    .unwrap_or(0);
                info!(
                    session_id = %session_id,
                    continuations_used = used,
                    budget,
                    "Plan continuation synthesized"
                );
                let detail = serde_json::json!({
                    "continuations_used": used,
                    "budget": budget,
                })
                .to_string();
                let _ = self
                    .emit_plan_turn_event(
                        session_id,
                        "plan_continuation",
                        Some(detail),
                        &turn_id,
                        &chat_id,
                        &reply_to,
                        &reply_role,
                        reply_guest_id,
                    )
                    .await;
                Ok(())
            }
        }
    }
}

/// Follow-up decision derived from a completed turn's plan eval.
#[derive(Debug)]
pub(super) enum PlanFollowup {
    /// Plan settled (complete, or continuation disabled) — emit the eval event only.
    Settled { eval_json: Value },
    /// Plan cannot proceed (blocked, or budget exhausted) — notify the operator.
    Stop {
        eval_json: Option<Value>,
        notice: String,
    },
    /// Plan should continue — synthesize a continuation if no user work waits.
    Continue { eval_json: Option<Value> },
}

/// Run the plan eval for a completed turn and update the session's carryover.
///
/// Mutates `state.carryover_plan` (create / update / clear) so the caller's
/// subsequent `checkpoint_json()` persists the new carryover state. Returns
/// `None` when the turn has no plan involvement (or is a scripted-loop /
/// paracrine-specialist turn, which the continuation loop deliberately skips).
pub(super) fn plan_followup_after_turn(
    state: &mut SessionState,
    completed_turn: &WorkingTurn,
    budget: u32,
) -> Option<PlanFollowup> {
    if completed_turn.scripted_loop_context.is_some() || completed_turn.paracrine_origin.is_some() {
        return None;
    }
    let disabled = plan_continuation_disabled();

    if let Some(plan) = completed_turn.active_plan.as_ref() {
        if plan.steps.is_empty() {
            state.carryover_plan = None;
            return None;
        }
        // The same plan (by goal) continues an existing carryover's budget,
        // evidence and stall count; a different plan replaces it (user
        // redirected the work).
        let (prior_carry, used, origin) = match state.carryover_plan.as_ref() {
            Some(c) if c.plan.goal == plan.goal => (
                Some(c.clone()),
                c.continuations_used,
                c.created_turn_id.clone(),
            ),
            _ => (None, 0, completed_turn.turn_id.clone()),
        };

        // Evidence from this turn merges with evidence carried from earlier
        // ones. `plan_steps_verified` is latched by `push_tool_history` against
        // real results, so it is safe to promote to carryover evidence;
        // `steps_done` is not, and is deliberately never fed back in.
        let mut carried_verified: Vec<u32> = prior_carry
            .as_ref()
            .map(|c| c.verified_step_ids.clone())
            .unwrap_or_default();
        for (i, step) in plan.steps.iter().enumerate() {
            if completed_turn
                .plan_steps_verified
                .get(i)
                .copied()
                .unwrap_or(false)
                && !carried_verified.contains(&step.id)
            {
                carried_verified.push(step.id);
            }
        }

        let prior_state = prior_carry.as_ref().map(|c| PriorPlanState {
            verified_step_ids: carried_verified.as_slice(),
            settled_count: c.steps_done_count(),
            stalls: c.stalled_continuations,
        });

        let outcome = evaluate_plan(plan, prior_state, &completed_turn.working_tool_history);
        // Evidence is sticky across the whole plan lifetime: a step proven in
        // turn 1 must stay proven in turn 3, long after its tool result has
        // scrolled out of the working history.
        let mut verified_step_ids = carried_verified;
        for id in &outcome.verified_step_ids {
            if !verified_step_ids.contains(id) {
                verified_step_ids.push(*id);
            }
        }
        // The budget has to fit the work that is actually left, or a large plan
        // is truncated at the same arbitrary place as a small one.
        let budget = scaled_continuation_budget(budget, outcome.outstanding_step_ids.len());
        let eval_json = outcome.event_json();
        info!(
            session_id = %state.session_id,
            steps_done = outcome.steps_done,
            steps_verified = outcome.steps_verified,
            steps_total = outcome.steps_total,
            stalls = outcome.stalled_continuations,
            verdict = outcome.verdict.as_str(),
            basis = outcome.basis.as_str(),
            "Plan eval"
        );

        match outcome.verdict {
            PlanEvalVerdict::Complete => {
                state.carryover_plan = None;
                Some(PlanFollowup::Settled { eval_json })
            }
            PlanEvalVerdict::Blocked => {
                let carry = CarryoverPlan {
                    plan: plan.clone(),
                    steps_done: outcome.steps_done_flags.clone(),
                    verified_step_ids,
                    stalled_continuations: outcome.stalled_continuations,
                    continuations_used: used,
                    created_turn_id: origin,
                };
                state.carryover_plan = None;
                let notice =
                    plan_stop_notice(&carry, "a step failed or no forward progress was made");
                Some(PlanFollowup::Stop {
                    eval_json: Some(eval_json),
                    notice,
                })
            }
            PlanEvalVerdict::Continue => {
                if disabled {
                    state.carryover_plan = None;
                    return Some(PlanFollowup::Settled { eval_json });
                }
                let carry = CarryoverPlan {
                    plan: plan.clone(),
                    steps_done: outcome.steps_done_flags.clone(),
                    verified_step_ids,
                    stalled_continuations: outcome.stalled_continuations,
                    continuations_used: used,
                    created_turn_id: origin,
                };
                if used >= budget {
                    state.carryover_plan = None;
                    let notice = plan_stop_notice(
                        &carry,
                        &format!("auto-continuation budget of {budget} exhausted"),
                    );
                    return Some(PlanFollowup::Stop {
                        eval_json: Some(eval_json),
                        notice,
                    });
                }
                state.carryover_plan = Some(carry);
                Some(PlanFollowup::Continue {
                    eval_json: Some(eval_json),
                })
            }
        }
    } else if state.carryover_plan.is_some() {
        // A turn without its own plan completed while a carryover exists — an
        // interleaved user turn finished. Resume the deferred carryover without
        // re-evaluating against this unrelated turn's tool history.
        if disabled {
            state.carryover_plan = None;
            return None;
        }
        let used = state
            .carryover_plan
            .as_ref()
            .map(|c| c.continuations_used)
            .unwrap_or(0);
        // Scale on the same basis as the evaluated path, or a plan deferred by
        // an interleaved user turn would be held to a narrower budget than the
        // one it was running under.
        let outstanding = state
            .carryover_plan
            .as_ref()
            .map(|c| c.plan.steps.len().saturating_sub(c.steps_done_count()))
            .unwrap_or(0);
        let budget = scaled_continuation_budget(budget, outstanding);
        if used >= budget {
            let carry = state.carryover_plan.take().expect("checked above");
            let notice = plan_stop_notice(
                &carry,
                &format!("auto-continuation budget of {budget} exhausted"),
            );
            return Some(PlanFollowup::Stop {
                eval_json: None,
                notice,
            });
        }
        Some(PlanFollowup::Continue { eval_json: None })
    } else {
        None
    }
}

#[cfg(test)]
mod watchdog_clock_tests {
    use super::super::tests::{run_recording_hotel, test_working_turn};
    use super::*;
    use crate::session::SessionState;
    use std::time::{Duration, Instant};

    /// Build a runtime wired to a stub hotel over a throwaway UDS.
    async fn runtime_with_stub_hotel(
        tag: &str,
    ) -> (
        AgentRuntime,
        String,
        tokio::task::JoinHandle<()>,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        let socket_path = format!(
            "/tmp/philote-watchdog-{}-{}.sock",
            tag,
            Uuid::new_v4().simple()
        );
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));
        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-watchdog".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let runtime = AgentRuntime::new(client, "agent-watchdog");
        (runtime, socket_path, server, emitted)
    }

    fn run_big_stack<F, Fut>(f: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()>,
    {
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build current-thread runtime")
                    .block_on(f());
            })
            .expect("spawn big-stack test thread")
            .join()
            .expect("big-stack test thread panicked");
    }

    /// THE regression. A turn sat in `WaitingTool` far past its budget while the
    /// watchdog missed every intervening tick. Under the old tick-local clock the
    /// first tick after the gap stamped `first_seen = now`, so `elapsed` restarted
    /// at zero and the turn survived — that is how a live 881s turn was reported and
    /// evicted as "90s", and how turns ran 823–3177s past a 600s ceiling.
    ///
    /// The clock is now stamped at the phase transition, so the very first tick
    /// after a gap sees the true elapsed time and evicts.
    #[test]
    fn evicts_on_first_tick_after_a_missed_tick_gap() {
        run_big_stack(|| async {
            let (mut runtime, socket_path, server, _emitted) = runtime_with_stub_hotel("gap").await;

            let session_id = "cron:flywheel:mac-jane".to_string();
            let mut state =
                SessionState::new(session_id.clone(), "agent-watchdog".into(), "cron".into());
            state.start_turn(test_working_turn(TurnPhase::WaitingTool));
            // The turn entered WaitingTool 400s ago (> the 300s tool budget) and the
            // watchdog has not run once since — exactly the starved-tick scenario.
            let long_ago = Instant::now() - Duration::from_secs(400);
            state.turn_waiting_since = Some(long_ago);
            state.active_turn_since = Some(long_ago);
            runtime.sessions.insert(session_id.clone(), state);

            // One single tick — the first since the gap.
            runtime.evict_timed_out_turns().await;

            let turn_still_active = runtime
                .sessions
                .get(&session_id)
                .and_then(|s| s.active_turn.as_ref())
                .is_some();
            assert!(
                !turn_still_active,
                "a turn 400s past its WaitingTool budget must be evicted on the first \
                 tick after a missed-tick gap, not granted a fresh budget"
            );

            drop(runtime);
            let _ = server.await;
            let _ = std::fs::remove_file(&socket_path);
        });
    }

    /// The total-active ceiling must also survive a tick gap. `LoadingContext` carries
    /// no phase deadline, so `MAX_TOTAL_ACTIVE_SECS` is the only thing bounding it — and
    /// it was anchored to a tick-local stamp, which is why it never held in practice.
    #[test]
    fn total_active_ceiling_holds_across_a_missed_tick_gap() {
        run_big_stack(|| async {
            let (mut runtime, socket_path, server, _emitted) =
                runtime_with_stub_hotel("ceiling").await;

            let session_id = "telegram:7898847424:agent-bjork-01".to_string();
            let mut state = SessionState::new(
                session_id.clone(),
                "agent-watchdog".into(),
                "telegram".into(),
            );
            state.start_turn(test_working_turn(TurnPhase::LoadingContext));
            // Alive 700s — past the 600s hard ceiling — with no tick in between.
            state.active_turn_since = Some(Instant::now() - Duration::from_secs(700));
            runtime.sessions.insert(session_id.clone(), state);

            runtime.evict_timed_out_turns().await;

            let turn_still_active = runtime
                .sessions
                .get(&session_id)
                .and_then(|s| s.active_turn.as_ref())
                .is_some();
            assert!(
                !turn_still_active,
                "a turn past MAX_TOTAL_ACTIVE_SECS must be evicted on the first tick \
                 after a gap, whatever phase it is in"
            );

            drop(runtime);
            let _ = server.await;
            let _ = std::fs::remove_file(&socket_path);
        });
    }

    /// Guard the other direction: the budget raise must actually buy headroom. A
    /// healthy-but-slow tool call at 154s (the measured duration of a real
    /// `life.flywheel.brief` against Memgraph over Tailscale) used to be killed by the
    /// old 90s budget. It must now survive.
    #[test]
    fn healthy_slow_tool_call_survives_the_raised_budget() {
        run_big_stack(|| async {
            let (mut runtime, socket_path, server, _emitted) =
                runtime_with_stub_hotel("slow").await;

            let session_id = "cron:flywheel-slow:mac-jane".to_string();
            let mut state =
                SessionState::new(session_id.clone(), "agent-watchdog".into(), "cron".into());
            state.start_turn(test_working_turn(TurnPhase::WaitingTool));
            let since = Instant::now() - Duration::from_secs(154);
            state.turn_waiting_since = Some(since);
            state.active_turn_since = Some(since);
            runtime.sessions.insert(session_id.clone(), state);

            runtime.evict_timed_out_turns().await;

            let turn_still_active = runtime
                .sessions
                .get(&session_id)
                .and_then(|s| s.active_turn.as_ref())
                .is_some();
            assert!(
                turn_still_active,
                "a 154s tool call is healthy for this tool population and must not be \
                 evicted — the old 90s budget is what stopped scheduled briefs landing"
            );

            drop(runtime);
            let _ = server.await;
            let _ = std::fs::remove_file(&socket_path);
        });
    }

    /// The binding constraint on a multi-call turn is the total ceiling, not the phase
    /// budget. A turn on its fourth tool call — fresh 300s phase budget, only 100s into
    /// it — is still evicted once the turn as a whole passes MAX_TOTAL_ACTIVE_SECS.
    /// That is intended (the ceiling is the backstop and it finally enforces), but it
    /// means the effective per-turn tool allowance is 600s in aggregate, however
    /// generous any single phase budget is. Documented here so the next person sizing
    /// WAITING_TOOL_SECS knows raising it alone buys nothing past the ceiling.
    #[test]
    fn total_ceiling_binds_a_multi_call_turn_before_the_phase_budget_does() {
        run_big_stack(|| async {
            let (mut runtime, socket_path, server, _emitted) =
                runtime_with_stub_hotel("multicall").await;

            let session_id = "cron:multi-call:mac-jane".to_string();
            let mut state =
                SessionState::new(session_id.clone(), "agent-watchdog".into(), "cron".into());
            state.start_turn(test_working_turn(TurnPhase::WaitingTool));
            // Turn alive 700s across several tool calls; the current call started 100s
            // ago, well inside its own 300s budget.
            state.active_turn_since = Some(Instant::now() - Duration::from_secs(700));
            state.turn_waiting_since = Some(Instant::now() - Duration::from_secs(100));
            runtime.sessions.insert(session_id.clone(), state);

            runtime.evict_timed_out_turns().await;

            let turn_still_active = runtime
                .sessions
                .get(&session_id)
                .and_then(|s| s.active_turn.as_ref())
                .is_some();
            assert!(
                !turn_still_active,
                "the total ceiling must bind a long multi-call turn even when the \
                 current phase is comfortably inside its own budget"
            );

            drop(runtime);
            let _ = server.await;
            let _ = std::fs::remove_file(&socket_path);
        });
    }

    /// Cross-feature guard. The #372 tool keepalive refreshes the phase clock so a
    /// slow-but-live tool is not evicted. Once the watchdog re-anchors
    /// `stuck_turn_first_seen` from `turn_waiting_since` each tick, a keepalive that
    /// stamped only the map would be silently overwritten within one tick and the
    /// feature would quietly stop working. The ping must survive a tick — and must
    /// still never push back the total ceiling.
    #[test]
    fn tool_progress_ping_survives_a_watchdog_tick_but_not_the_total_ceiling() {
        run_big_stack(|| async {
            let (mut runtime, socket_path, server, _emitted) =
                runtime_with_stub_hotel("pingtick").await;

            let session_id = "cron:slow-tool:mac-jane".to_string();
            let mut state =
                SessionState::new(session_id.clone(), "agent-watchdog".into(), "cron".into());
            let mut turn = test_working_turn(TurnPhase::WaitingTool);
            turn.turn_id = "turn-ping".into();
            turn.pending_tool_call = Some(ToolCall {
                tool_name: "life.observe.batch".into(),
                arguments: serde_json::json!({}),
            });
            state.start_turn(turn);
            // 400s into the phase — already PAST the 300s budget, so only a keepalive
            // can save this turn — but 500s into the turn overall, still under the
            // 600s ceiling. Without an effective ping the next tick must evict.
            state.turn_waiting_since = Some(Instant::now() - Duration::from_secs(400));
            state.active_turn_since = Some(Instant::now() - Duration::from_secs(500));
            runtime.sessions.insert(session_id.clone(), state);

            // The tool reports progress, then the watchdog ticks.
            runtime.handle_tool_progress(&InboundTaskPayload {
                action: Some("tool_progress".into()),
                session_id: Some(session_id.clone()),
                turn_id: Some("turn-ping".into()),
                tool_name: Some("life.observe.batch".into()),
                ..Default::default()
            });
            runtime.evict_timed_out_turns().await;

            assert!(
                runtime
                    .sessions
                    .get(&session_id)
                    .and_then(|s| s.active_turn.as_ref())
                    .is_some(),
                "a keepalive ping must still be in effect after a watchdog tick — \
                 otherwise the tick re-anchors the clock and the ping is a no-op"
            );

            // The ceiling is untouched by pings: push the turn past it and the very
            // next tick must evict, however recently the tool pinged.
            if let Some(state) = runtime.sessions.get_mut(&session_id) {
                state.active_turn_since = Some(Instant::now() - Duration::from_secs(700));
            }
            runtime.handle_tool_progress(&InboundTaskPayload {
                action: Some("tool_progress".into()),
                session_id: Some(session_id.clone()),
                turn_id: Some("turn-ping".into()),
                tool_name: Some("life.observe.batch".into()),
                ..Default::default()
            });
            runtime.evict_timed_out_turns().await;

            assert!(
                runtime
                    .sessions
                    .get(&session_id)
                    .and_then(|s| s.active_turn.as_ref())
                    .is_none(),
                "no amount of pinging may push back the total-turn ceiling"
            );

            drop(runtime);
            let _ = server.await;
            let _ = std::fs::remove_file(&socket_path);
        });
    }

    /// Multi-step turns must still get a fresh phase budget per tool call. The clock
    /// swap changes only where the timestamp comes from, never this behaviour.
    #[test]
    fn progressing_turn_earns_a_fresh_budget_per_tool_call() {
        run_big_stack(|| async {
            let (mut runtime, socket_path, server, _emitted) =
                runtime_with_stub_hotel("progress").await;

            let session_id = "cron:multi-step:mac-jane".to_string();
            let mut state =
                SessionState::new(session_id.clone(), "agent-watchdog".into(), "cron".into());
            state.start_turn(test_working_turn(TurnPhase::WaitingTool));
            // 200s into the first tool call: under the 300s budget, and the total
            // ceiling has plenty of room.
            state.turn_waiting_since = Some(Instant::now() - Duration::from_secs(200));
            state.active_turn_since = Some(Instant::now() - Duration::from_secs(200));
            runtime.sessions.insert(session_id.clone(), state);
            runtime.evict_timed_out_turns().await;

            // The tool returns and the turn dispatches the next one — the phase
            // transition re-stamps the clock, so the new call starts from zero.
            if let Some(state) = runtime.sessions.get_mut(&session_id) {
                state.set_active_turn_phase(TurnPhase::WaitingTool);
            }
            runtime.evict_timed_out_turns().await;

            let turn_still_active = runtime
                .sessions
                .get(&session_id)
                .and_then(|s| s.active_turn.as_ref())
                .is_some();
            assert!(
                turn_still_active,
                "a turn making real progress must get a fresh phase budget per tool call"
            );

            drop(runtime);
            let _ = server.await;
            let _ = std::fs::remove_file(&socket_path);
        });
    }
}
