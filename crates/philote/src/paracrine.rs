//! Paracrine subsystem for [`AgentRuntime`]: the `paracrine_response`
//! lookaside-routing reflex (the nine [`ParacrineRouting`] dispatch branches),
//! the attention-steward `paracrine_signal` handler, and the
//! `delegate.whisper` / `delegate.merge` agent-local tool implementations.
//!
//! Mechanically extracted from `runtime.rs` (declared there as a `#[path]`
//! child module so private `AgentRuntime` fields stay accessible). No
//! behavior change.

use super::*;

impl AgentRuntime {
    pub(super) async fn handle_paracrine_signal(
        &mut self,
        task: InboundTaskPayload,
        task_id: Uuid,
    ) -> Result<()> {
        use ansible_mesh_core::attention_steward::{
            ActivationState, AttentionStewardPolicy, AttentionStewardResponse,
            AttentionStewardSignal,
        };
        use data_memorygraphrag::attention_observer;

        let signal = task
            .paracrine_signal
            .as_ref()
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let attention_signal = match AttentionStewardSignal::from_value(signal) {
            Ok(signal) => signal,
            Err(err) => {
                warn!(
                    task_id = %task_id,
                    error = %err,
                    "paracrine signal deferred: invalid attention steward envelope"
                );
                return Ok(());
            }
        };
        // Slice A5 activation sourcing (v0): the confirmed SIL count travels in
        // the signal payload itself (steward cron includes it; 0 default keeps
        // the gate closed). Counting via the life-graph runner is async/remote,
        // so activation stays hotel-local and synchronous for now.
        let activation = ActivationState::from_signal(&attention_signal);
        let decision = AttentionStewardPolicy::default()
            .evaluate_now_with_activation(&attention_signal, &activation);
        let response = match &decision.response {
            AttentionStewardResponse::RecordObservation => "record_observation",
            AttentionStewardResponse::ProposeSilEntry => "propose_sil_entry",
            AttentionStewardResponse::UpdateSilMetadata => "update_sil_metadata",
            AttentionStewardResponse::DeferSignal => "defer_signal",
            AttentionStewardResponse::ActiveCheckIn { .. } => "active_checkin",
        };

        info!(
            task_id = %task_id,
            signal_id = %attention_signal.signal_id,
            signal_type = %attention_signal.signal_type,
            scope = %attention_signal.scope,
            response = %response,
            reason = %decision.reason,
            "attention steward observed paracrine signal"
        );

        // Slice A5: an authorized check-in consults the hotel's
        // steward.active_checkins autonomy lane. AutoWithAudit (budget
        // available) → deliver as an OOB push to the session's membrane, the
        // same way the turn watchdog surfaces its "I got stuck" notice.
        // ConfirmFirst / ProposalOnly / budget exhausted / kill switch / no
        // route → fall through to the fire-and-forget life.observe path below,
        // which writes the check-in as a proposed Signal tagged
        // `awaiting_operator_posture`.
        if let AttentionStewardResponse::ActiveCheckIn { message, sil_ref } = &decision.response {
            if self
                .try_deliver_steward_checkin(&attention_signal, message, sil_ref.as_deref())
                .await
            {
                return Ok(());
            }
        }

        let now_iso = chrono::Utc::now().to_rfc3339();
        // LifeGraph auto-anchor Slice 2: the paracrine lane IS the observing
        // agent (this hotel's philote evaluating its own attention-steward
        // signal), so its own canonical identity is what every Signal/SIL
        // node written here should anchor to — same resolver
        // (`cypher::scoped_to_anchor_edge`) the model-invoked `life.observe`
        // path uses via `tool_exec::inject_scoped_to_anchor`.
        if let Some(observe_input) = attention_observer::decision_to_observe_input(
            &decision,
            &attention_signal,
            &now_iso,
            &self.agent_id,
            self.role_name.as_deref(),
        ) {
            let node_id = local_node_id();
            let target_node = life_graph_runner_node_id();
            let task_json = serde_json::json!({
                "action": "execute_tool",
                "tool_name": "life.observe",
                "arguments": serde_json::to_value(&observe_input)?,
                "reply_to": node_id,
                "reply_role": "agent",
                "session_id": "",
                "turn_id": "",
                "chat_id": "",
            });
            let _ = self
                .ipc_client
                .send_request(IpcRequest::EmitTask {
                    target_node,
                    target_role: "life-graph-runner".into(),
                    target_guest_id: None,
                    task_json: task_json.to_string(),
                })
                .await;
        }

        Ok(())
    }

    /// Attempt AutoWithAudit push delivery of an earned steward check-in
    /// (Slice A5). Returns `true` only when the check-in was actually pushed —
    /// any refusal (no delivery route, lane posture below AutoWithAudit,
    /// exhausted daily budget, frozen lane, kill switch, IPC failure) returns
    /// `false` so the caller degrades to the `awaiting_operator_posture`
    /// life.observe path.
    async fn try_deliver_steward_checkin(
        &mut self,
        signal: &ansible_mesh_core::attention_steward::AttentionStewardSignal,
        message: &str,
        sil_ref: Option<&str>,
    ) -> bool {
        use ansible_mesh_core::autonomy::LANE_STEWARD_ACTIVE_CHECKINS;

        // Resolve the delivery route BEFORE consuming budget so an
        // undeliverable check-in never burns an autonomy action.
        let Some((session_id, chat_id, target_node, target_role, target_guest_id)) =
            self.steward_checkin_route()
        else {
            warn!(
                signal_id = %signal.signal_id,
                "steward check-in has no membrane delivery route; degrading to awaiting_operator_posture"
            );
            return false;
        };

        let consume = self
            .ipc_client
            .send_request(IpcRequest::ConsumeAutonomyAction {
                lane: LANE_STEWARD_ACTIVE_CHECKINS.into(),
                action_summary: format!("steward active check-in for signal {}", signal.signal_id),
                evidence: format!(
                    "confirmed_sil_entries={}; sil_ref={}; message={}",
                    signal.confirmed_sil_entries,
                    sil_ref.unwrap_or("-"),
                    message,
                ),
                reversal_hint: "operator dismissal/reversal demotes lane \
                                steward.active_checkins one posture level \
                                (record_outcome OperatorReversal)"
                    .into(),
            })
            .await;

        let (allowed, posture, reason) = match &consume {
            Ok(IpcResponse::Standard {
                ok: true,
                data: Some(data),
                ..
            }) => (
                data.get("allowed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                data.get("posture")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                data.get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            ),
            Ok(other) => {
                warn!(
                    signal_id = %signal.signal_id,
                    response = ?other,
                    "steward check-in: unexpected ConsumeAutonomyAction response; degrading"
                );
                (false, "unknown".into(), "unexpected_response".into())
            }
            Err(e) => {
                warn!(
                    signal_id = %signal.signal_id,
                    error = %e,
                    "steward check-in: ConsumeAutonomyAction IPC failed; degrading"
                );
                (false, "unknown".into(), "ipc_error".into())
            }
        };

        if !allowed {
            info!(
                signal_id = %signal.signal_id,
                posture = %posture,
                reason = %reason,
                "steward check-in not cleared for push; writing awaiting_operator_posture signal"
            );
            return false;
        }

        // AutoWithAudit + budget consumed: push the check-in out-of-band via
        // the session's membrane (same emit shape as the watchdog notice).
        let notify_req = IpcRequest::EmitTask {
            target_node,
            target_role,
            target_guest_id,
            task_json: serde_json::json!({
                "action": "send_reply",
                "session_id": session_id,
                "turn_id": "",
                "chat_id": chat_id,
                "content": message,
                "final": true,
            })
            .to_string(),
        };
        match self.ipc_client.send_request(notify_req).await {
            Ok(_) => {
                info!(
                    signal_id = %signal.signal_id,
                    session_id = %session_id,
                    sil_ref = sil_ref.unwrap_or("-"),
                    "steward active check-in delivered via steward.active_checkins lane"
                );
                true
            }
            Err(e) => {
                // Budget already consumed and audited hotel-side; surface the
                // failure but still degrade so the check-in is not lost.
                warn!(
                    signal_id = %signal.signal_id,
                    error = %e,
                    "steward check-in push failed after budget consume; degrading"
                );
                false
            }
        }
    }

    /// Pick the membrane delivery route for a steward check-in: the most
    /// recently active session with a derivable chat, preferring direct
    /// (non-group) Telegram chats. Chat id comes from the active turn when one
    /// exists, else from the `telegram:{chat_id}:…` session id shape. Returns
    /// `(session_id, chat_id, target_node, target_role, target_guest_id)`.
    fn steward_checkin_route(&self) -> Option<(String, String, String, String, Option<String>)> {
        let mut best: Option<(bool, u64, String, String)> = None; // (is_direct, recency, sid, chat)
        for (session_id, state) in &self.sessions {
            let chat_id = state
                .active_turn
                .as_ref()
                .map(|t| t.chat_id.clone())
                .filter(|c| !c.is_empty())
                .or_else(|| {
                    session_id
                        .strip_prefix("telegram:")
                        .and_then(|rest| rest.split(':').next())
                        .filter(|c| !c.is_empty())
                        .map(str::to_string)
                });
            let Some(chat_id) = chat_id else { continue };
            let is_direct = !chat_id.starts_with('-');
            let recency = if state.active_turn.is_some() {
                u64::MAX
            } else {
                state.recent_turns.last().map(|t| t.created_at).unwrap_or(0)
            };
            let better = match &best {
                None => true,
                Some((best_direct, best_recency, _, _)) => {
                    (is_direct, recency) > (*best_direct, *best_recency)
                }
            };
            if better {
                best = Some((is_direct, recency, session_id.clone(), chat_id));
            }
        }
        let (_, _, session_id, chat_id) = best?;
        let target = self
            .sessions
            .get(&session_id)
            .expect("session exists")
            .resolved_transport_reply_target(local_node_id(), "membrane", None);
        Some((
            session_id,
            chat_id,
            target.target_node,
            target.target_role,
            target.target_guest_id,
        ))
    }

    /// Lookaside routing reflex — dispatches an incoming `paracrine_response`
    /// based on the [`ParacrineRouting`] hint carried in the exosome.
    ///
    /// This is a separate path from [`handle_user_message`]: the main cognitive
    /// loop is not re-entered unless the routing explicitly calls for it. The
    /// `paracrine_id` threads through every branch for cross-mesh provenance.
    pub(super) async fn handle_paracrine_response(
        &mut self,
        task: InboundTaskPayload,
        task_id: Uuid,
    ) -> Result<()> {
        // Extract the exosome envelope from the task payload so we can read the
        // paracrine_id and response_routing hint set at dispatch time.
        let exosome: Option<Exosome> = task
            .exosome
            .as_ref()
            .and_then(|v| serde_json::from_value::<Exosome>(v.clone()).ok());

        let paracrine_id = exosome.as_ref().and_then(|e| e.paracrine_id.clone());

        let routing = exosome
            .as_ref()
            .and_then(|e| e.response_routing.clone())
            .unwrap_or(ParacrineRouting::ReflectiveReEntry);

        // Locate the owning turn by matching paracrine_id against active turns,
        // falling back to the task's session_id field.
        let owner_turn = paracrine_id.as_ref().and_then(|pid| {
            self.sessions.iter().find_map(|(sid, state)| {
                state.active_turn.as_ref().and_then(|turn| {
                    turn.associated_paracrine_ids
                        .contains(pid)
                        .then(|| (sid.clone(), turn.turn_id.clone(), turn.chat_id.clone()))
                })
            })
        });
        let owner_thread = paracrine_id.as_ref().and_then(|pid| {
            self.sessions.iter().find_map(|(sid, state)| {
                state
                    .paracrine_threads
                    .iter()
                    .find(|thread| thread.id == pid.as_str())
                    .map(|thread| (sid.clone(), thread.origin_turn_id.clone()))
            })
        });
        let session_id = task
            .session_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| owner_turn.as_ref().map(|(sid, _, _)| sid.clone()))
            .or_else(|| owner_thread.as_ref().map(|(sid, _)| sid.clone()));

        match routing {
            ParacrineRouting::RawForward => {
                // Emit content directly to membrane — no model loop.
                let content = task.content.clone().unwrap_or_default();
                if let (Some(sid), Some(pid)) = (&session_id, &paracrine_id) {
                    if let Some(state) = self.sessions.get_mut(sid) {
                        state.close_paracrine_thread(
                            pid,
                            ParacrineThreadStatus::Completed,
                            Some(content.clone()),
                            Some("raw_forward".into()),
                        );
                    }
                    self.persist_session_checkpoint(sid).await?;
                }
                let node_id = local_node_id();
                let _ = self
                    .ipc_client
                    .send_request(IpcRequest::EmitTask {
                        target_node: node_id,
                        target_role: "membrane".into(),
                        target_guest_id: None,
                        task_json: serde_json::json!({
                            "action": "send_message",
                            "content": content,
                            "paracrine_id": paracrine_id,
                        })
                        .to_string(),
                    })
                    .await;
            }

            ParacrineRouting::ProgressUpdate => {
                // Emit a partial/ephemeral update to membrane without closing
                // or interrupting the active turn.
                let content = task.content.clone().unwrap_or_default();
                if let Some(sid) = &session_id {
                    if let Some(pid) = &paracrine_id {
                        if let Some(state) = self.sessions.get_mut(sid) {
                            state.signal_paracrine_thread(pid, content.clone());
                        }
                        self.persist_session_checkpoint(sid).await?;
                    }
                    let _ = self.emit_partial_reply(sid, content).await;
                }
            }

            ParacrineRouting::Heartbeat => {
                // No model involvement — just log and acknowledge.
                info!(
                    paracrine_id = paracrine_id.as_deref().unwrap_or("?"),
                    "paracrine heartbeat received"
                );
                if let (Some(sid), Some(pid)) = (&session_id, &paracrine_id) {
                    if let Some(state) = self.sessions.get_mut(sid) {
                        state.signal_paracrine_thread(pid, "heartbeat".into());
                    }
                    self.persist_session_checkpoint(sid).await?;
                }
            }

            ParacrineRouting::MemoryEnrichment => {
                // Push specialist content into the session memory window.
                // Falls through to CognitiveReEntry if no session found.
                if session_id.is_none() {
                    warn!(
                        paracrine_id = paracrine_id.as_deref().unwrap_or("?"),
                        "MemoryEnrichment: no session found, dropping"
                    );
                    return Ok(());
                }
                // Memory injection handled by model re-entry with enriched context.
                if let (Some(sid), Some(pid)) = (&session_id, &paracrine_id) {
                    if let Some(state) = self.sessions.get_mut(sid) {
                        state.close_paracrine_thread(
                            pid,
                            ParacrineThreadStatus::Completed,
                            task.content.clone(),
                            Some("memory_enrichment".into()),
                        );
                    }
                }
                self.handle_user_message(task, task_id).await?;
            }

            ParacrineRouting::DatasourceInjection => {
                // Structured retrieval — inject into session context and re-enter
                // the model so it can reason over the data.
                if session_id.is_none() {
                    warn!(
                        paracrine_id = paracrine_id.as_deref().unwrap_or("?"),
                        "DatasourceInjection: no session found, dropping"
                    );
                    return Ok(());
                }
                if let (Some(sid), Some(pid)) = (&session_id, &paracrine_id) {
                    if let Some(state) = self.sessions.get_mut(sid) {
                        state.close_paracrine_thread(
                            pid,
                            ParacrineThreadStatus::Completed,
                            task.content.clone(),
                            Some("datasource_injection".into()),
                        );
                    }
                }
                self.handle_user_message(task, task_id).await?;
            }

            ParacrineRouting::EnrichedToolResult => {
                // Replace the "paracrine dispatched" placeholder with the real
                // specialist response and re-enter the model as if the tool call
                // completed normally.
                let owner_turn_id = owner_turn.as_ref().map(|(_, tid, _)| tid.clone());
                let owner_chat_id = owner_turn.as_ref().map(|(_, _, cid)| cid.clone());
                if let (Some(sid), Some(pid)) = (&session_id, &paracrine_id) {
                    // Charge one delegation hop against the turn's cross-hop budget
                    // BEFORE closing the thread, so a breach can close it with the
                    // budget-exhausted disposition instead of Completed. The per-hop
                    // iteration reset (below) removes the only other bound on
                    // whisper->re-entry->whisper chains, so this is what stops a
                    // misbehaving orchestrator<->specialist pair from spinning forever.
                    let breach = if let Some(state) = self.sessions.get_mut(sid) {
                        let exec = state.settings.execution.clone();
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        state.active_turn.as_mut().and_then(|turn| {
                            let turn_id = turn.turn_id.clone();
                            match charge_paracrine_hop(turn, &exec, now) {
                                ParacrineBudgetOutcome::WithinBudget => None,
                                ParacrineBudgetOutcome::HopsExhausted { hops, budget } => Some((
                                    turn_id,
                                    format!(
                                        "This delegation chain exceeded its budget of {budget} hops \
                                         (reached {hops}) and was stopped. A specialist role kept \
                                         handing work back and forth without resolving. Please \
                                         restate the request or narrow the task."
                                    ),
                                )),
                                ParacrineBudgetOutcome::TimeExhausted {
                                    elapsed_secs,
                                    budget_secs,
                                } => Some((
                                    turn_id,
                                    format!(
                                        "This delegation chain exceeded its cumulative time budget \
                                         of {budget_secs}s (ran {elapsed_secs}s) and was stopped. \
                                         Delegation ran too long without resolving. Please restate \
                                         the request or narrow the task."
                                    ),
                                )),
                            }
                        })
                    } else {
                        None
                    };

                    if let Some((turn_id, notice)) = breach {
                        if let Some(state) = self.sessions.get_mut(sid) {
                            state.close_paracrine_thread(
                                pid,
                                ParacrineThreadStatus::BudgetExhausted,
                                task.content.clone(),
                                Some("budget_exhausted".into()),
                            );
                        }
                        warn!(
                            session_id = %sid,
                            paracrine_id = %pid,
                            "paracrine delegation chain exceeded its budget; failing turn"
                        );
                        // Turn-failure heal intake: budget breaches flow into
                        // the self-heal queue so recurring delegation loops
                        // surface as A3 work items.
                        self.push_heal_event("paracrine_budget_exhausted", &notice)
                            .await;
                        return self.fail_active_turn(sid.clone(), turn_id, notice).await;
                    }

                    if let Some(state) = self.sessions.get_mut(sid) {
                        state.close_paracrine_thread(
                            pid,
                            ParacrineThreadStatus::Completed,
                            task.content.clone(),
                            Some("enriched_tool_result".into()),
                        );
                        // Reset the iteration counter: the turn was waiting for an external
                        // response, not thinking. Give it a fresh budget to process the reply.
                        if let Some(turn) = state.active_turn.as_mut() {
                            turn.iteration = 0;
                        }
                    }
                }
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    tool_name: Some("delegate.whisper".into()),
                    content: task.content.clone(),
                    session_id: session_id.clone().or(task.session_id.clone()),
                    turn_id: owner_turn_id.or(task.turn_id.clone()),
                    chat_id: owner_chat_id.or(task.chat_id.clone()),
                    source: Some("paracrine".into()),
                    final_reply_to: task.final_reply_to.clone(),
                    final_reply_role: task.final_reply_role.clone(),
                    ..task
                })
                .await?;
            }

            ParacrineRouting::ReflectiveReEntry => {
                // Reflective path: feed brain's reply back into the orchestrator's
                // own paracrine layer. The exosome's paracrine_id is preserved so
                // the resulting turn gets paracrine_origin set, which auto-injects
                // delegate.merge. The orchestrator reasons about the reply and either
                // calls delegate.merge to surface it or completes silently to absorb.
                if let (Some(sid), Some(pid)) = (&session_id, &paracrine_id) {
                    if let Some(state) = self.sessions.get_mut(sid) {
                        state.close_paracrine_thread(
                            pid,
                            ParacrineThreadStatus::Completed,
                            task.content.clone(),
                            Some("reflective_re_entry".into()),
                        );
                    }
                }
                self.handle_user_message(task, task_id).await?;
            }

            ParacrineRouting::CognitiveReEntry => {
                // Standard path: feed into cognitive re-entry.
                // If there is an active turn, the re-entry will merge this
                // response into its context. If not, a new synthesis turn begins.
                if let (Some(sid), Some(pid)) = (&session_id, &paracrine_id) {
                    if let Some(state) = self.sessions.get_mut(sid) {
                        state.close_paracrine_thread(
                            pid,
                            ParacrineThreadStatus::Completed,
                            task.content.clone(),
                            Some("cognitive_re_entry".into()),
                        );
                    }
                }
                self.handle_user_message(task, task_id).await?;
            }

            ParacrineRouting::PriorityReEntry => {
                // Arbiter-promoted: prepend to the session queue so this task is
                // processed NEXT, ahead of any already-waiting messages.
                let session_id = task.session_id_or_default(&self.agent_id);
                if let Some(pid) = &paracrine_id {
                    if let Some(state) = self.sessions.get_mut(&session_id) {
                        state.close_paracrine_thread(
                            pid,
                            ParacrineThreadStatus::Completed,
                            task.content.clone(),
                            Some("priority_re_entry".into()),
                        );
                    }
                }
                if let Some(state) = self.sessions.get_mut(&session_id) {
                    if state.is_turn_active() {
                        info!(
                            session_id = %session_id,
                            "PriorityReEntry: prepending arbiter-promoted task to front of queue"
                        );
                        state.prepend_user_task(task_id, task);
                    } else {
                        // No active turn — dispatch immediately.
                        self.handle_user_message(task, task_id).await?;
                    }
                }
            }

            ParacrineRouting::ApprovalResolution => {
                // The operator role (e.g. membrane + human) has sent an approval decision
                // for a parked turn. Extract `decision` and optional `note` from the content
                // field (parsed as JSON), then synthesize a SlashCommand to reuse the existing
                // approval resolution path.
                let session_id =
                    session_id.unwrap_or_else(|| task.session_id_or_default(&self.agent_id));
                if let Some(pid) = &paracrine_id {
                    if let Some(state) = self.sessions.get_mut(&session_id) {
                        state.close_paracrine_thread(
                            pid,
                            ParacrineThreadStatus::Completed,
                            task.content.clone(),
                            Some("approval_resolution".into()),
                        );
                    }
                }
                // The sender encodes the decision as JSON in `content`, e.g.:
                //   {"decision": "approved", "note": "looks good"}
                let parsed_content = task
                    .content
                    .as_deref()
                    .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok());
                let decision = parsed_content
                    .as_ref()
                    .and_then(|p| p.get("decision"))
                    .and_then(|d| d.as_str())
                    .unwrap_or("approved");
                let note = parsed_content
                    .as_ref()
                    .and_then(|p| p.get("note"))
                    .and_then(|n| n.as_str())
                    .map(str::to_string);
                let command = if decision == "denied" {
                    SlashCommand::Deny { note }
                } else {
                    SlashCommand::Approve { note }
                };
                // Use inbound routing fields for the command reply; handle_approval_command
                // extracts the real turn values from the restored parked turn.
                let local_node = local_node_id();
                let cmd_chat_id = task.chat_id.clone().unwrap_or_default();
                let cmd_reply_to = task.final_reply_to.clone().unwrap_or(local_node);
                let cmd_reply_role = task
                    .final_reply_role
                    .clone()
                    .unwrap_or_else(|| "membrane".into());
                let cmd_reply_guest_id = task.final_reply_guest_id.clone();
                info!(
                    session_id = %session_id,
                    decision = %decision,
                    "paracrine ApprovalResolution: applying operator decision to parked turn"
                );
                self.handle_approval_command(
                    task_id,
                    session_id,
                    task.turn_id.clone().unwrap_or_default(),
                    cmd_chat_id,
                    cmd_reply_to,
                    cmd_reply_role,
                    cmd_reply_guest_id,
                    command,
                )
                .await?;
            }
        }

        Ok(())
    }

    /// Agent-local `delegate.whisper` tool: dispatch a prompt to a specialist
    /// role as a paracrine emission. Body moved verbatim from the
    /// `"delegate.whisper"` match arm of `execute_local_agent_tool`.
    pub(super) async fn execute_delegate_whisper_tool(
        &mut self,
        payload: ToolExecutionPayload,
    ) -> Result<()> {
        let args = &payload.arguments;
        let role = match args.get("role").and_then(|v| v.as_str()) {
            Some(r) => r.to_string(),
            None => {
                return self
                    .fail_active_turn(
                        payload.session_id,
                        payload.turn_id,
                        "delegate.whisper: missing required argument 'role'".into(),
                    )
                    .await;
            }
        };
        let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => {
                return self
                    .fail_active_turn(
                        payload.session_id,
                        payload.turn_id,
                        "delegate.whisper: missing required argument 'prompt'".into(),
                    )
                    .await;
            }
        };
        // Succinctness budget: whisper prompts are briefs, not context dumps.
        let raw_prompt_chars = prompt.chars().count();
        let prompt = truncate_for_wire(&prompt, PARACRINE_WHISPER_PROMPT_MAX_CHARS);
        if raw_prompt_chars > PARACRINE_WHISPER_PROMPT_MAX_CHARS {
            warn!(
                session_id = %payload.session_id,
                role = %role,
                prompt_chars = raw_prompt_chars,
                budget = PARACRINE_WHISPER_PROMPT_MAX_CHARS,
                "delegate.whisper: prompt exceeded succinctness budget; truncated"
            );
        }

        // `reply_to` controls where the specialist's response goes.
        // "self"     → back to this philote as paracrine_response
        // "membrane" → directly to the membrane role
        // "<node>/<role>" → explicit routing
        // default    → "self"
        let reply_to_str = args
            .get("reply_to")
            .and_then(|v| v.as_str())
            .unwrap_or("self");

        let node_id = local_node_id();
        let (reply_to_node, reply_to_role) = match reply_to_str {
            "membrane" => (node_id.clone(), "membrane".to_string()),
            "self" | "" => (node_id.clone(), "agent".to_string()),
            other => {
                if let Some((node, role_part)) = other.split_once('/') {
                    (node.to_string(), role_part.to_string())
                } else {
                    (node_id.clone(), other.to_string())
                }
            }
        };

        // Parse optional response_routing hint from arguments.
        // Defaults to ReflectiveReEntry if absent or unrecognised.
        let explicit_response_routing =
            args.get("routing").and_then(|v| v.as_str()).and_then(|s| {
                serde_json::from_value::<ParacrineRouting>(serde_json::Value::String(s.to_string()))
                    .ok()
            });
        let wait_requested = args
            .get("wait_for_response")
            .or_else(|| args.get("blocking"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let response_routing = if wait_requested && explicit_response_routing.is_none() {
            Some(ParacrineRouting::EnrichedToolResult)
        } else {
            explicit_response_routing
        };
        let wait_for_response = wait_requested
            || matches!(response_routing, Some(ParacrineRouting::EnrichedToolResult));
        let authority = args
            .get("authority")
            .and_then(|v| v.as_str())
            .unwrap_or("advice_only")
            .to_string();
        let tool_policy = args
            .get("tool_policy")
            .and_then(|v| v.as_str())
            .unwrap_or("role_default")
            .to_string();
        let approval_scope = args
            .get("approval_scope")
            .and_then(|v| v.as_str())
            .unwrap_or("originating_session")
            .to_string();

        // Always generate a paracrine_id — it threads through the full
        // thought graph and ties the response back to this turn.
        let paracrine_id = Uuid::new_v4().to_string();

        // Log the outbound exosome ID on the active turn so the routing
        // reflex can correlate the response when it arrives.
        // Also capture the current session_id and chat_id so the specialist's
        // response carries the right conversation context.
        let (source_session_id, source_chat_id, source_reply_guest_id) = {
            let mut sess_id = None;
            let mut chat_id = None;
            let mut reply_guest_id = None;
            if let Some(state) = self.sessions.get_mut(&payload.session_id) {
                if let Some(turn) = state.active_turn.as_mut() {
                    turn.associated_paracrine_ids.push(paracrine_id.clone());
                    sess_id = Some(state.session_id.clone());
                    if !turn.chat_id.is_empty() {
                        chat_id = Some(turn.chat_id.clone());
                    }
                    reply_guest_id = turn.final_reply_guest_id.clone();
                }
            }
            (sess_id, chat_id, reply_guest_id)
        };

        let exosome = Exosome {
            prompt: prompt.clone(),
            context: None,
            paracrine_id: Some(paracrine_id.clone()),
            response_routing: response_routing.clone(),
            source_session_id,
            source_chat_id,
        };

        // When reply_to="self", target this philote's own guest_id so the
        // specialist's paracrine_response routes back here specifically instead
        // of to the membrane seat (which has no paracrine_response handler).
        // Role incarnations use "{agent_id}:{role_name}"; default philotes use
        // "{agent_id}" directly.
        let effective_reply_guest_id = if matches!(reply_to_str, "self" | "") {
            Some(
                self.role_name
                    .as_ref()
                    .map(|rn| format!("{}:{}", self.agent_id, rn))
                    .unwrap_or_else(|| self.agent_id.clone()),
            )
        } else {
            source_reply_guest_id
        };

        let emit_result = self
            .ipc_client
            .send_request(IpcRequest::ParacrineEmit {
                role: role.clone(),
                exosome,
                reply_to_node,
                reply_to_role,
                reply_to_guest_id: effective_reply_guest_id,
                timeout_secs: None,
            })
            .await;

        if emit_result.is_ok() {
            if let Some(state) = self.sessions.get_mut(&payload.session_id) {
                state.open_paracrine_thread(
                    paracrine_id.clone(),
                    role.clone(),
                    prompt.clone(),
                    response_routing
                        .clone()
                        .unwrap_or(ParacrineRouting::ReflectiveReEntry),
                    authority,
                    tool_policy,
                    approval_scope,
                );
            }
        }

        let (content, tool_err) = match emit_result {
            Ok(_) if wait_for_response => {
                if let Some(state) = self.sessions.get(&payload.session_id) {
                    let checkpoint_memory_type = state.checkpoint_memory_type();
                    let checkpoint_json = state.checkpoint_json();
                    let index_state = state.clone();
                    self.ipc_client
                        .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
                        .await?;
                    self.sync_session_index(&index_state).await?;
                }
                let _ = self
                    .emit_turn_event(
                        &payload.session_id,
                        "waiting_paracrine",
                        Some(paracrine_id.clone()),
                    )
                    .await;
                return Ok(());
            }
            Ok(_) => (
                format!(
                    "Whisper sent to specialist (paracrine_id: {paracrine_id}). \
                     The specialist is processing asynchronously — their response \
                     will arrive separately. Do NOT call delegate.whisper again. \
                     Respond to the user now with a brief acknowledgment."
                ),
                None,
            ),
            Err(e) => {
                let err = TaskErrorPayload::transport_error(
                    "philote",
                    format!("delegate.whisper: IPC transport error — {e}"),
                );
                (err.display_message(), Some(err))
            }
        };

        self.handle_tool_result(InboundTaskPayload {
            action: Some("tool_result".into()),
            agent_action: None,
            handoff_bundle: None,
            source: Some("agent".into()),
            session_id: Some(payload.session_id),
            turn_id: Some(payload.turn_id),
            transport: None,
            chat_id: Some(payload.chat_id),
            thread_id: None,
            sender_id: None,
            sender_username: None,
            message_kind: None,
            content: Some(content),
            attachments: Vec::new(),
            command: None,
            callback_data: None,
            raw_transport_event: None,
            error: tool_err,
            tool_name: Some(payload.tool_name),
            arguments: None,
            final_reply_to: Some(payload.final_reply_to),
            final_reply_role: Some(payload.final_reply_role),
            final_reply_guest_id: payload.final_reply_guest_id,
            ..Default::default()
        })
        .await
    }

    // ── delegate.merge ───────────────────────────────────────────────
    // Explicit paracrine merge: emit a paracrine_response back to the
    // orchestrator immediately, without waiting for turn completion.
    // Sets paracrine_merge_completed on the turn so deliver_text_reply
    // does not auto-emit a duplicate response when the turn later closes.
    //
    // Call signature: { "content": "<response to send to orchestrator>" }
    // Available in specialist (paracrine) toolsets.
    pub(super) async fn execute_delegate_merge_tool(
        &mut self,
        payload: ToolExecutionPayload,
    ) -> Result<()> {
        let session_id = payload.session_id.clone();
        let turn_id = payload.turn_id.clone();
        let args = &payload.arguments;

        let content = match args.get("content").and_then(|v| v.as_str()) {
            Some(c) if !c.trim().is_empty() => c.to_string(),
            _ => {
                return self
                    .fail_active_turn(
                        session_id,
                        turn_id,
                        "delegate.merge: missing required argument 'content'".into(),
                    )
                    .await;
            }
        };
        // Succinctness budget: a merge is a distilled answer, not a transcript.
        let raw_merge_chars = content.chars().count();
        let content = truncate_for_wire(&content, PARACRINE_MERGE_CONTENT_MAX_CHARS);
        if raw_merge_chars > PARACRINE_MERGE_CONTENT_MAX_CHARS {
            warn!(
                session_id = %session_id,
                merge_chars = raw_merge_chars,
                budget = PARACRINE_MERGE_CONTENT_MAX_CHARS,
                "delegate.merge: content exceeded succinctness budget; truncated"
            );
        }

        // Capture routing info from the active turn before muting it.
        let (
            paracrine_id,
            reply_session_id,
            reply_chat_id,
            response_routing,
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
        ) = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                warn!("delegate.merge: unknown session {}", session_id);
                return Ok(());
            };
            let Some(turn) = state.active_turn.as_mut() else {
                warn!("delegate.merge: no active turn for session {}", session_id);
                return Ok(());
            };
            if turn.paracrine_origin.is_none() {
                return self
                    .fail_active_turn(
                        session_id,
                        turn_id,
                        "delegate.merge: not in a paracrine context — this tool is only available to specialist roles".into(),
                    )
                    .await;
            }
            let pid = turn.paracrine_origin.clone().unwrap();
            let rs = turn
                .paracrine_reply_session_id
                .clone()
                .unwrap_or_else(|| session_id.clone());
            let rc = turn
                .paracrine_reply_chat_id
                .clone()
                .unwrap_or_else(|| turn.chat_id.clone());
            let rr = turn.paracrine_response_routing.clone();
            let frt = turn.final_reply_to.clone();
            let frr = turn.final_reply_role.clone();
            let frg = turn.final_reply_guest_id.clone();
            // Mark merge as done so deliver_text_reply suppresses the auto-emit.
            turn.paracrine_merge_completed = true;
            (pid, rs, rc, rr, frt, frr, frg)
        };

        // Append role attribution tag (same as deliver_text_reply does).
        let attributed_content = if let Ok(role_name) = std::env::var("PHILOTIC_ROLE_NAME") {
            if !role_name.is_empty() {
                format!("{}\n\n@agent:{}", content, role_name)
            } else {
                content.clone()
            }
        } else {
            content.clone()
        };

        // Determine position in chain: if reply_session_id == session_id,
        // this is a top-of-chain reflection turn (Astrid surfacing brain's
        // reply). Otherwise it's a specialist turn (brain replying to Astrid).
        let is_top_of_chain = reply_session_id == session_id;

        if is_top_of_chain {
            // Reflective surface: emit send_reply directly to membrane so the
            // content goes to the user's Telegram chat.
            let surface_task = serde_json::json!({
                "action": "send_reply",
                "session_id": reply_session_id,
                "turn_id": turn_id,
                "chat_id": reply_chat_id,
                "content": attributed_content,
            });
            info!(
                session_id = %session_id,
                "delegate.merge: reflective surface — emitting send_reply to membrane"
            );
            let _ = self
                .ipc_client
                .send_request(IpcRequest::EmitTask {
                    target_node: final_reply_to,
                    target_role: final_reply_role,
                    target_guest_id: final_reply_guest_id,
                    task_json: surface_task.to_string(),
                })
                .await;
        } else {
            // Specialist merge: emit paracrine_response back to the orchestrator.
            let merge_task = serde_json::json!({
                "action": "paracrine_response",
                "session_id": reply_session_id,
                "turn_id": turn_id,
                "chat_id": reply_chat_id,
                "content": attributed_content,
                "exosome": {
                    "prompt": "",
                    "paracrine_id": paracrine_id,
                    "response_routing": response_routing,
                    "source_session_id": reply_session_id,
                    "source_chat_id": reply_chat_id,
                },
            });
            info!(
                session_id = %session_id,
                reply_session = %reply_session_id,
                "delegate.merge: emitting paracrine_response to orchestrator"
            );
            let _ = self
                .ipc_client
                .send_request(IpcRequest::EmitTask {
                    target_node: final_reply_to,
                    target_role: final_reply_role,
                    target_guest_id: final_reply_guest_id,
                    task_json: merge_task.to_string(),
                })
                .await;
        }

        // Return a tool result so the specialist's turn can continue or close.
        let result_content = format!(
            "Merge sent to orchestrator (paracrine_id: {}). Your response has been delivered to the main conversation. Complete your turn now.",
            paracrine_id
        );
        self.handle_tool_result(InboundTaskPayload {
            action: Some("tool_result".into()),
            agent_action: None,
            handoff_bundle: None,
            source: Some("agent".into()),
            session_id: Some(session_id),
            turn_id: Some(turn_id),
            transport: None,
            chat_id: Some(payload.chat_id),
            thread_id: None,
            sender_id: None,
            sender_username: None,
            message_kind: None,
            content: Some(result_content),
            attachments: Vec::new(),
            command: None,
            callback_data: None,
            raw_transport_event: None,
            error: None,
            tool_name: Some("delegate.merge".into()),
            arguments: None,
            final_reply_to: Some(payload.final_reply_to),
            final_reply_role: Some(payload.final_reply_role),
            final_reply_guest_id: payload.final_reply_guest_id,
            ..Default::default()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{def004_working_turn, run_recording_hotel};
    use super::AgentRuntime;
    use crate::r#loop::TurnPhase;
    use crate::protocol::{InboundTaskPayload, ToolExecutionPayload};
    use crate::session::PARACRINE_WHISPER_PROMPT_MAX_CHARS;
    use uuid::Uuid;

    /// Stub hotel like `run_recording_hotel`, but answers
    /// `ConsumeAutonomyAction` with a configurable grant decision and records
    /// the consultation alongside emitted tasks.
    async fn run_grant_hotel(
        listener: tokio::net::UnixListener,
        emitted: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        allow: bool,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut stream, _) = listener.accept().await.expect("accept");
        loop {
            let buf = match async {
                let mut len_buf = [0u8; 4];
                stream.read_exact(&mut len_buf).await?;
                let len = u32::from_be_bytes(len_buf) as usize;
                let mut buf = vec![0u8; len];
                stream.read_exact(&mut buf).await?;
                Ok::<_, std::io::Error>(buf)
            }
            .await
            {
                Ok(b) => b,
                Err(_) => return,
            };

            let req: philotic_client::IpcRequest =
                serde_json::from_slice(&buf).expect("decode request");
            let reply = match &req {
                philotic_client::IpcRequest::GetConfig { key } => {
                    serde_json::to_vec(&philotic_client::IpcResponse::ConfigData {
                        key: key.clone(),
                        value_json: None,
                    })
                    .unwrap()
                }
                philotic_client::IpcRequest::ConsumeAutonomyAction {
                    lane,
                    action_summary,
                    ..
                } => {
                    emitted.lock().unwrap().push(serde_json::json!({
                        "consume_autonomy_action": {
                            "lane": lane,
                            "action_summary": action_summary,
                        },
                    }));
                    let data = if allow {
                        serde_json::json!({
                            "allowed": true,
                            "posture": "auto_with_audit",
                            "audit_id": "autonomy_action:test",
                        })
                    } else {
                        serde_json::json!({
                            "allowed": false,
                            "posture": "confirm_first",
                            "reason": "posture_requires_confirmation",
                        })
                    };
                    serde_json::to_vec(&philotic_client::IpcResponse::success(
                        "consume_autonomy_action",
                        Some(data),
                    ))
                    .unwrap()
                }
                philotic_client::IpcRequest::EmitTask {
                    target_node,
                    target_role,
                    target_guest_id,
                    task_json,
                } => {
                    let task: serde_json::Value =
                        serde_json::from_str(task_json).unwrap_or(serde_json::Value::Null);
                    emitted.lock().unwrap().push(serde_json::json!({
                        "target_node": target_node,
                        "target_role": target_role,
                        "target_guest_id": target_guest_id,
                        "task": task,
                    }));
                    serde_json::to_vec(&philotic_client::IpcResponse::success("ok", None)).unwrap()
                }
                _ => {
                    serde_json::to_vec(&philotic_client::IpcResponse::success("ok", None)).unwrap()
                }
            };

            let len = u32::try_from(reply.len()).expect("frame length fits u32");
            stream
                .write_all(&len.to_be_bytes())
                .await
                .expect("write header");
            stream.write_all(&reply).await.expect("write payload");
        }
    }

    fn checkin_signal_json(confirmed_sil_entries: u32) -> serde_json::Value {
        serde_json::json!({
            "signal_id": "cron:steward-1:1000",
            "signal_type": "open_loop_staleness",
            "scope": "personal",
            "source_hotel": "test-hotel",
            "target_role_type": "attention-steward",
            "subject_refs": ["lifegraph:open_loop"],
            "cadence": "daily",
            "priority": "medium",
            "observed_at": "2026-06-04T20:00:00Z",
            "expires_at": null,
            "payload_summary": "Gentle nudge: the dentist form is still open.",
            "policy_tags": ["active_checkin"],
            "confirmed_sil_entries": confirmed_sil_entries,
            "sil_ref": "sil:confirmed:01jz-example",
        })
    }

    async fn steward_runtime(
        socket_path: &str,
    ) -> (
        AgentRuntime,
        tokio::task::JoinHandle<()>,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        let listener = tokio::net::UnixListener::bind(socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_grant_hotel(listener, emitted.clone(), true));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-steward".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-steward");
        // A telegram session whose id encodes the chat gives the check-in a
        // delivery route even with no active turn.
        runtime
            .ensure_session_loaded("telegram:555:jane", "telegram")
            .await
            .expect("session load");
        (runtime, server, emitted)
    }

    /// Gate open + AutoWithAudit grant: the check-in is pushed to the
    /// session's membrane via the OOB send_reply path, budget consultation
    /// goes through lane steward.active_checkins, and no life.observe write
    /// happens (the hotel-side audit record is the ledger).
    #[tokio::test]
    async fn earned_checkin_with_auto_grant_pushes_to_membrane() {
        let socket_path = format!("/tmp/philote-steward-a-{}.sock", Uuid::new_v4().simple());
        let (mut runtime, server, emitted) = steward_runtime(&socket_path).await;

        runtime
            .handle_paracrine_signal(
                InboundTaskPayload {
                    action: Some("paracrine_signal".into()),
                    paracrine_signal: Some(checkin_signal_json(6)),
                    ..Default::default()
                },
                Uuid::new_v4(),
            )
            .await
            .expect("handle signal");

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        let consult = emitted
            .iter()
            .find(|e| e.get("consume_autonomy_action").is_some())
            .expect("grant consulted");
        assert_eq!(
            consult["consume_autonomy_action"]["lane"],
            "steward.active_checkins"
        );
        let push = emitted
            .iter()
            .find(|e| e["task"]["action"] == "send_reply")
            .expect("check-in pushed to membrane");
        assert_eq!(push["target_role"], "membrane");
        assert_eq!(push["task"]["chat_id"], "555");
        assert_eq!(
            push["task"]["content"],
            "Gentle nudge: the dentist form is still open."
        );
        assert!(
            !emitted
                .iter()
                .any(|e| e["task"]["tool_name"] == "life.observe"),
            "delivered check-in must not also write life.observe: {:#?}",
            *emitted
        );
    }

    /// Gate open but the lane refuses (ConfirmFirst posture): no push —
    /// the check-in degrades to a life.observe proposed Signal tagged
    /// awaiting_operator_posture.
    #[tokio::test]
    async fn earned_checkin_below_auto_posture_degrades_to_observe() {
        let socket_path = format!("/tmp/philote-steward-b-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_grant_hotel(listener, emitted.clone(), false));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-steward".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-steward");
        runtime
            .ensure_session_loaded("telegram:555:jane", "telegram")
            .await
            .expect("session load");

        runtime
            .handle_paracrine_signal(
                InboundTaskPayload {
                    action: Some("paracrine_signal".into()),
                    paracrine_signal: Some(checkin_signal_json(6)),
                    ..Default::default()
                },
                Uuid::new_v4(),
            )
            .await
            .expect("handle signal");

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        assert!(
            emitted
                .iter()
                .any(|e| e.get("consume_autonomy_action").is_some()),
            "grant must be consulted"
        );
        assert!(
            !emitted.iter().any(|e| e["task"]["action"] == "send_reply"),
            "ConfirmFirst posture must not push: {:#?}",
            *emitted
        );
        let observe = emitted
            .iter()
            .find(|e| e["task"]["tool_name"] == "life.observe")
            .expect("degraded check-in writes life.observe");
        assert_eq!(observe["target_role"], "life-graph-runner");
        let claim_id = observe["task"]["arguments"]["evidence"]["claim_ref"]["id"]
            .as_str()
            .expect("claim id");
        assert!(
            claim_id.starts_with("checkin:proposed:"),
            "expected proposed check-in node, got {claim_id}"
        );
        let tags = observe["task"]["arguments"]["evidence"]["metadata"]["policy_tags"]
            .as_array()
            .expect("policy tags");
        assert!(tags.iter().any(|t| t == "awaiting_operator_posture"));
    }

    /// Gate closed (confirmed SIL entries below threshold): even with a
    /// would-allow grant, the policy degrades to a plain RecordObservation —
    /// the lane is never consulted and nothing is pushed.
    #[tokio::test]
    async fn checkin_below_sil_threshold_never_consults_lane() {
        let socket_path = format!("/tmp/philote-steward-c-{}.sock", Uuid::new_v4().simple());
        let (mut runtime, server, emitted) = steward_runtime(&socket_path).await;

        runtime
            .handle_paracrine_signal(
                InboundTaskPayload {
                    action: Some("paracrine_signal".into()),
                    paracrine_signal: Some(checkin_signal_json(4)),
                    ..Default::default()
                },
                Uuid::new_v4(),
            )
            .await
            .expect("handle signal");

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        assert!(
            !emitted
                .iter()
                .any(|e| e.get("consume_autonomy_action").is_some()),
            "closed gate must not consult the lane: {:#?}",
            *emitted
        );
        assert!(!emitted.iter().any(|e| e["task"]["action"] == "send_reply"));
        let observe = emitted
            .iter()
            .find(|e| e["task"]["tool_name"] == "life.observe")
            .expect("closed gate still records the observation");
        let claim_id = observe["task"]["arguments"]["evidence"]["claim_ref"]["id"]
            .as_str()
            .expect("claim id");
        assert!(
            claim_id.starts_with("signal:paracrine:"),
            "expected plain observation node, got {claim_id}"
        );
    }

    /// Succinctness budget: a `delegate.whisper` prompt longer than
    /// `PARACRINE_WHISPER_PROMPT_MAX_CHARS` must be truncated (with an explicit
    /// omission marker) before it is recorded on the paracrine thread and
    /// dispatched over the wire.
    #[tokio::test]
    async fn whisper_prompt_over_budget_is_truncated_before_dispatch() {
        let socket_path = format!("/tmp/philote-succinct-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-succinct".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-succinct");

        let session_id = "sess-succinct";
        let turn_id = "turn-succinct";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        let mut turn = def004_working_turn(turn_id, "delegate.whisper");
        turn.pending_tool_call = None;
        turn.phase = TurnPhase::WaitingTool;
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(turn);

        const OVERAGE: usize = 500;
        let huge_prompt = "p".repeat(PARACRINE_WHISPER_PROMPT_MAX_CHARS + OVERAGE);
        let payload = ToolExecutionPayload {
            action: "execute_tool",
            session_id: session_id.into(),
            turn_id: turn_id.into(),
            chat_id: "555".into(),
            tool_name: "delegate.whisper".into(),
            arguments: serde_json::json!({
                "role": "specialist",
                "prompt": huge_prompt,
                "wait_for_response": true,
            }),
            execution_mode: "local_agent".into(),
            agent_id: "agent-succinct".into(),
            user_id: None,
            runner_id: None,
            incarnation_id: None,
            hotel_id: None,
            environment_id: None,
            task_runner_kind: None,
            task_runner_config: None,
            selection_reason: None,
            workspace_ref: None,
            task_runner_overlay: None,
            return_route: None,
            reply_to: "node-1".into(),
            reply_role: "agent".into(),
            reply_guest_id: None,
            final_reply_to: "membrane-node-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: Some("membrane-seat-1".into()),
        };

        runtime
            .execute_delegate_whisper_tool(payload)
            .await
            .expect("whisper dispatch");

        let goal = runtime
            .sessions
            .get(session_id)
            .and_then(|s| s.paracrine_threads.first())
            .map(|t| t.goal.clone())
            .expect("paracrine thread opened");

        assert!(
            goal.chars().count() < PARACRINE_WHISPER_PROMPT_MAX_CHARS + 64,
            "dispatched prompt must be bounded, got {} chars",
            goal.chars().count()
        );
        assert!(
            goal.contains(&format!("[truncated: {OVERAGE} chars omitted]")),
            "expected omission marker in truncated prompt"
        );

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    /// Suppression-path guard: `paracrine_merge_completed` must only suppress the
    /// final reply for turns that actually have a paracrine origin. A plain
    /// transport turn with the flag set (e.g. stale checkpoint bits) must still
    /// surface its send_reply.
    #[tokio::test]
    async fn merge_completed_flag_does_not_suppress_plain_transport_reply() {
        let socket_path = format!("/tmp/philote-def004b-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-def004b".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-def004b");

        let session_id = "sess-def004b";
        let turn_id = "turn-def004b";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        let mut turn = def004_working_turn(turn_id, "hotel.status");
        turn.pending_tool_call = None;
        turn.phase = TurnPhase::WaitingModel;
        // No paracrine_origin, but the merge flag is (incorrectly) set.
        turn.paracrine_merge_completed = true;
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(turn);

        let final_text = "Plain transport reply.";
        runtime
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
            .expect("model respond");

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        let send_replies: Vec<_> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "send_reply")
            .collect();
        assert_eq!(
            send_replies.len(),
            1,
            "plain transport turn must always surface its reply: {:#?}",
            *emitted
        );
        assert_eq!(send_replies[0]["task"]["content"], final_text);
    }

    /// Cross-hop budget guard: when an `EnrichedToolResult` re-entry pushes the
    /// turn's delegation chain past its hop budget, the turn must fail with an
    /// operator-visible notice and the paracrine thread must close with the
    /// `BudgetExhausted` disposition (not `Completed`, and not re-entering the
    /// model). Without this, whisper->re-entry->whisper chains have no cross-hop
    /// bound because each hop resets `iteration` to 0.
    #[tokio::test]
    async fn enriched_tool_result_breaching_budget_fails_turn() {
        use crate::session::ParacrineThreadStatus;

        let socket_path = format!("/tmp/philote-budget-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-budget".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-budget");

        let session_id = "sess-budget";
        let turn_id = "turn-budget";
        let pid = "paracrine-budget-1";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        // Owner turn that has already traversed the full default hop budget (5).
        let mut turn = def004_working_turn(turn_id, "delegate.whisper");
        turn.pending_tool_call = None;
        turn.phase = TurnPhase::WaitingTool;
        turn.associated_paracrine_ids.push(pid.into());
        turn.paracrine_hop_count = 5; // default budget; this incoming hop is the 6th

        {
            let state = runtime
                .sessions
                .get_mut(session_id)
                .expect("session exists");
            state.start_turn(turn);
            state.open_paracrine_thread(
                pid.into(),
                "specialist".into(),
                "do the thing".into(),
                philotic_client::ParacrineRouting::EnrichedToolResult,
                "delegated".into(),
                "inherit".into(),
                "inherit".into(),
            );
        }

        let exosome = philotic_client::Exosome {
            prompt: String::new(),
            context: None,
            paracrine_id: Some(pid.into()),
            response_routing: Some(philotic_client::ParacrineRouting::EnrichedToolResult),
            source_session_id: Some(session_id.into()),
            source_chat_id: None,
        };

        runtime
            .handle_paracrine_response(
                InboundTaskPayload {
                    action: Some("paracrine_response".into()),
                    session_id: Some(session_id.into()),
                    turn_id: Some(turn_id.into()),
                    content: Some("specialist reply".into()),
                    exosome: Some(serde_json::to_value(&exosome).unwrap()),
                    ..Default::default()
                },
                Uuid::new_v4(),
            )
            .await
            .expect("handle paracrine response");

        // The paracrine thread must be closed as BudgetExhausted, not Completed.
        let thread_status = runtime
            .sessions
            .get(session_id)
            .and_then(|s| s.paracrine_threads.iter().find(|t| t.id == pid))
            .map(|t| t.status.clone())
            .expect("thread present");
        assert!(
            matches!(thread_status, ParacrineThreadStatus::BudgetExhausted),
            "thread should be BudgetExhausted, got {thread_status:?}"
        );

        // The active turn must have been failed (cleared).
        assert!(
            runtime
                .sessions
                .get(session_id)
                .and_then(|s| s.active_turn.as_ref())
                .is_none(),
            "active turn should be failed and cleared"
        );

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        // An operator-visible send_reply must carry the budget notice.
        let emitted = emitted.lock().unwrap();
        let budget_reply = emitted.iter().find(|e| {
            e["task"]["action"] == "send_reply"
                && e["task"]["content"]
                    .as_str()
                    .map(|c| c.contains("delegation chain exceeded its budget"))
                    .unwrap_or(false)
        });
        assert!(
            budget_reply.is_some(),
            "expected an operator-visible budget-exhaustion reply: {:#?}",
            *emitted
        );
    }

    /// Reflective re-entry, top-of-chain: when the orchestrator's reflection turn
    /// produces user-facing text but does NOT call delegate.merge, that text must be
    /// surfaced to the user (implicit merge) as a send_reply — not silently absorbed.
    /// This is the primary "I never see it work" regression guard.
    #[tokio::test]
    async fn reflective_reentry_nonempty_reply_surfaces_to_user() {
        use crate::r#loop::TurnPhase;

        let socket_path = format!(
            "/tmp/philote-reflect-surface-{}.sock",
            Uuid::new_v4().simple()
        );
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-reflect".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-reflect");

        let session_id = "sess-reflect";
        let turn_id = "turn-reflect";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        let mut turn = def004_working_turn(turn_id, "hotel.status");
        turn.pending_tool_call = None;
        turn.phase = TurnPhase::WaitingModel;
        // Top-of-chain reflection turn: paracrine_origin set and reply_session_id
        // loops back to this same session.
        turn.paracrine_origin = Some("pid-reflect".into());
        turn.paracrine_reply_session_id = Some(session_id.into());
        turn.paracrine_reply_chat_id = Some("555".into());
        turn.paracrine_merge_completed = false;
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(turn);

        let surfaced = "Here is the synthesized answer for the user.";
        runtime
            .handle_model_response(InboundTaskPayload {
                action: Some("model_response".into()),
                session_id: Some(session_id.into()),
                turn_id: Some(turn_id.into()),
                agent_action: Some(serde_json::json!({
                    "kind": "respond",
                    "content": surfaced,
                })),
                content: Some(surfaced.into()),
                ..Default::default()
            })
            .await
            .expect("model respond");

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        let send_replies: Vec<_> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "send_reply")
            .collect();
        assert_eq!(
            send_replies.len(),
            1,
            "reflection reply must be surfaced to the user exactly once: {:#?}",
            *emitted
        );
        assert!(
            send_replies[0]["task"]["content"]
                .as_str()
                .unwrap_or_default()
                .contains(surfaced),
            "surfaced reply must carry the model text: {:#?}",
            *emitted
        );
        assert_eq!(send_replies[0]["task"]["chat_id"], "555");
        assert_eq!(send_replies[0]["target_role"], "membrane");
        assert!(
            !emitted
                .iter()
                .any(|e| e["task"]["action"] == "paracrine_response"),
            "top-of-chain surface must not emit a paracrine_response: {:#?}",
            *emitted
        );
    }

    /// Reflective re-entry, top-of-chain, EMPTY content reaching deliver_text_reply:
    /// the defensive empty-guard must absorb rather than post a blank bubble. (An
    /// empty *model output* becomes a Fail action upstream, so this guard only fires
    /// for other paths that reach deliver_text_reply with no text.)
    #[tokio::test]
    async fn reflective_reentry_empty_content_is_absorbed_not_surfaced() {
        use crate::r#loop::TurnPhase;

        let socket_path = format!(
            "/tmp/philote-reflect-absorb-{}.sock",
            Uuid::new_v4().simple()
        );
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-absorb".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-absorb");

        let session_id = "sess-absorb";
        let turn_id = "turn-absorb";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        let mut turn = def004_working_turn(turn_id, "hotel.status");
        turn.pending_tool_call = None;
        turn.phase = TurnPhase::WaitingModel;
        turn.paracrine_origin = Some("pid-absorb".into());
        turn.paracrine_reply_session_id = Some(session_id.into());
        turn.paracrine_reply_chat_id = Some("555".into());
        turn.paracrine_merge_completed = false;
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(turn);

        // Drive deliver_text_reply directly with empty content to exercise the guard
        // (the normal model path turns empty output into a Fail before reaching here).
        runtime
            .deliver_text_reply(
                session_id.into(),
                turn_id.into(),
                "   ".into(),
                None,
                false,
                None,
                None,
            )
            .await
            .expect("deliver empty");

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        assert!(
            !emitted.iter().any(|e| e["task"]["action"] == "send_reply"),
            "empty reflection completion must not surface a reply: {:#?}",
            *emitted
        );
        assert!(
            !emitted
                .iter()
                .any(|e| e["task"]["action"] == "paracrine_response"),
            "empty reflection completion must not emit a paracrine_response: {:#?}",
            *emitted
        );
    }
}
