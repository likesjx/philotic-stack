//! Tool execution and approval-gate handling for [`AgentRuntime`]:
//! `route_tool_call_execution` (including the duplicate-call dedup guard),
//! `execute_bound_tool`, `execute_local_agent_tool`, and the approval gate
//! (`handle_approval_request`, `handle_approval_command`,
//! `should_defer_parked_approval_command`).
//!
//! Mechanically extracted from `runtime.rs` (declared there as a `#[path]`
//! child module so private `AgentRuntime` fields stay accessible). No
//! behavior change.

use super::*;

/// Parse the optional `fallback_tiers` tool argument (a JSON array of tier
/// role-name strings) into the `Option<Vec<String>>` the hotel's ConfigureRole
/// IPC expects. Returns `None` when the argument is absent/not an array —
/// callers treat that as "preserve whatever ladder is already configured",
/// never as "wipe it".
pub(super) fn parse_fallback_tiers_arg(value: &serde_json::Value) -> Option<Vec<String>> {
    value.as_array().map(|arr| {
        arr.iter()
            .filter_map(|t| t.as_str().map(str::to_string))
            .collect::<Vec<String>>()
    })
}

/// Structural anchoring for `life.observe` (LifeGraph auto-anchor Slice 1):
/// the LLM tool schema never exposes `edges`, and non-model write paths have
/// historically hardcoded `edges: vec![]` — so every observation lands as an
/// orphan node with nothing to traverse at recall time. Given the mutable
/// tool-call args (after `observed_by`/`observed_role` have already been
/// resolved/stamped onto them), append the server-side SCOPED_TO anchor edge
/// unless one is already present.
///
/// The anchor target is resolved from `observed_by` (the observing agent's
/// canonical identity) through `scoped_to_anchor_edge`, which routes through
/// the SAME agent -> domain -> seeded-Role-node map the auto-recall/
/// provenance lane uses — never from a slugged role-name string, which could
/// fork a parallel Role node. No-ops when `observed_by` is absent/non-string
/// or resolves to no canonical Role (never manufactures a junk Role node).
/// Idempotent — never doubles up the anchor.
pub(super) fn inject_scoped_to_anchor(args: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(agent_id) = args
        .get("observed_by")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    let observed_role = args
        .get("observed_role")
        .and_then(serde_json::Value::as_str);
    let Some(anchor) = data_memorygraphrag::cypher::scoped_to_anchor_edge(&agent_id, observed_role)
    else {
        return;
    };

    let edges = args
        .entry("edges")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    let Some(list) = edges.as_array_mut() else {
        return;
    };
    let already_anchored = list.iter().any(|edge| {
        edge.get("rel_type").and_then(serde_json::Value::as_str) == Some(anchor.rel_type.as_str())
            && edge.get("target_id").and_then(serde_json::Value::as_str)
                == Some(anchor.target_id.as_str())
    });
    if !already_anchored {
        list.push(serde_json::json!({
            "rel_type": anchor.rel_type,
            "target_id": anchor.target_id,
            "upsert_target": anchor.upsert_target,
        }));
    }
}

/// Parses the `model_bindings` tool argument (Layer 1 per-agent model NAME
/// binding): a JSON object mapping provider role (e.g. `"model.openrouter"`)
/// to model id (e.g. `"z-ai/glm-5.2"`). Non-string values are dropped rather
/// than erroring the whole call — mirrors `parse_fallback_tiers_arg`'s
/// permissive-filter shape.
pub(super) fn parse_model_bindings_arg(
    value: &serde_json::Value,
) -> Option<std::collections::BTreeMap<String, String>> {
    value.as_object().map(|obj| {
        obj.iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect::<std::collections::BTreeMap<String, String>>()
    })
}

impl AgentRuntime {
    /// Classify a tool call against the set of gates that ALWAYS require live
    /// operator approval and can never be preapproved or bypassed by policy
    /// (including `auto_approve_all`). Returns a stable gate-kind slug when the
    /// call is one of these, or `None` otherwise.
    ///
    /// This is the single source of truth used by `route_tool_call_execution`;
    /// keeping it a pure function lets the routing decision be unit-tested
    /// without constructing a full runtime. Callers still apply the
    /// `bypass_approval` guard themselves so the post-approval resume can run.
    pub(super) fn unconditional_approval_gate(
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Option<&'static str> {
        match tool_name {
            // Admin role creation permanently grants elevated authority.
            "role.configure"
                if arguments
                    .get("is_admin")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false) =>
            {
                Some("admin_role_creation")
            }
            // Rules durably and permanently affect agent behavior.
            "rule.propose" => Some("rule_propose"),
            // Routing policy proposals are stored durably and influence future routing.
            "routing.policy.propose" => Some("routing_policy_propose"),
            // Skill registration writes an abstract skill into the graph that can
            // later project tools onto agents — must not be silently accepted.
            "skill.register" => Some("skill_register"),
            _ => None,
        }
    }

    pub(super) async fn handle_approval_request(
        &mut self,
        session_id: String,
        turn_id: String,
        approval: ApprovalRequest,
        always_require_human: bool,
    ) -> Result<()> {
        let approval = Self::normalize_approval_request(approval);
        // `always_require_human` bypasses the approval policy entirely — the human operator
        // must approve in this session. Used for admin role creation, which cannot be
        // preapproved or bypassed by `auto_approve_all`.
        let preapproved = if always_require_human {
            false
        } else {
            self.sessions
                .get(&session_id)
                .map(|state| {
                    let tool = state
                        .active_turn
                        .as_ref()
                        .and_then(|t| t.pending_tool_call.as_ref());
                    state.approval_policy_allows(&approval, tool)
                })
                .unwrap_or(false)
        };

        let (
            task_id,
            chat_id,
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            checkpoint_memory_type,
            checkpoint_json,
            index_state,
            approval_active_plan,
            approval_pending_tool_call,
        ) = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                warn!(
                    "Received approval request for unknown session {}",
                    session_id
                );
                return Ok(());
            };
            let Some(active_turn) = state.active_turn.as_ref() else {
                warn!(
                    "Received approval request for session {} with no active turn",
                    session_id
                );
                return Ok(());
            };
            let task_id = active_turn.task_id;
            let chat_id = active_turn.chat_id.clone();
            let final_reply_to = active_turn.final_reply_to.clone();
            let final_reply_role = active_turn.final_reply_role.clone();
            let final_reply_guest_id = active_turn.final_reply_guest_id.clone();
            let approval_active_plan = active_turn.active_plan.clone();
            let approval_pending_tool_call = active_turn.pending_tool_call.clone();
            if preapproved {
                state.clear_pending_approval();
                state.set_active_turn_phase(TurnPhase::Thinking);
            } else {
                state.set_pending_approval(approval.clone());
                state.set_active_turn_phase(TurnPhase::WaitingApproval);
                // Park the turn so this session can accept new work while the operator
                // decides. active_turn becomes None; parked_approval_turn holds the state.
                state.park_active_turn_for_approval();
            }
            (
                task_id,
                chat_id,
                final_reply_to,
                final_reply_role,
                final_reply_guest_id,
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
                approval_active_plan,
                approval_pending_tool_call,
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        if preapproved {
            let pending_tool_call = self
                .sessions
                .get(&session_id)
                .and_then(|state| state.active_turn.as_ref())
                .and_then(|turn| turn.pending_tool_call.clone());
            let _ = self
                .ipc_client
                .send_request(IpcRequest::UpdateTask {
                    task_id,
                    state: "approval_preapproved".into(),
                    payload: serde_json::json!({
                        "session_id": session_id,
                        "turn_id": turn_id,
                        "chat_id": chat_id,
                        "approval_request": {
                            "approval_id": approval.approval_id,
                            "reason": approval.reason,
                            "approved_response": approval.approved_response,
                        },
                        "approval_resolution": {
                            "approval_id": approval.approval_id,
                            "decision": "approved",
                            "reason": approval.reason,
                            "resolution_mode": "preapproved",
                        }
                    }),
                })
                .await?;

            let reply_payload = FinalReplyPayload {
                action: "send_reply",
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
                chat_id,
                content: approval.approved_response.clone(),
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

            if let Some(tool_call) = pending_tool_call {
                // bypass_approval=true: preapproved path, no re-gate needed.
                return self
                    .route_tool_call_execution(session_id, turn_id, tool_call, true)
                    .await;
            }

            return self
                .complete_agent_response(
                    session_id,
                    turn_id,
                    approval.approved_response,
                    None,
                    None,
                    None,
                    None,
                )
                .await;
        }

        let _ = self
            .ipc_client
            .send_request(IpcRequest::UpdateTask {
                task_id,
                state: "waiting_approval".into(),
                payload: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": chat_id,
                    "approval_request": {
                        "approval_id": approval.approval_id,
                        "reason": approval.reason,
                        "approved_response": approval.approved_response,
                    }
                }),
            })
            .await?;

        let _ = self
            .emit_turn_event(
                &session_id,
                "waiting_approval",
                Some(approval.reason.clone()),
            )
            .await;

        let approval_keyboard = serde_json::json!({
            "inline_keyboard": [
                [
                    {"text": "✅ Approve", "callback_data": "approve"},
                    {"text": "❌ Deny", "callback_data": "deny"}
                ],
                [
                    {"text": "🔓 Trust for session", "callback_data": "trust"}
                ]
            ]
        });

        let reply_payload = FinalReplyPayload {
            action: "send_reply",
            session_id,
            turn_id,
            chat_id,
            content: Self::format_approval_message(
                &approval,
                approval_active_plan.as_ref(),
                approval_pending_tool_call.as_ref(),
            ),
            audio_artifact: None,
            send_text_caption: false,
            reply_markup: Some(approval_keyboard),
        };

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: final_reply_to,
                target_role: final_reply_role,
                target_guest_id: final_reply_guest_id,
                task_json: serde_json::to_string(&reply_payload)?,
            })
            .await?;

        Ok(())
    }

    pub(super) fn route_tool_call_execution(
        &mut self,
        session_id: String,
        turn_id: String,
        mut tool_call: ToolCall,
        // When `true`, the approval gate is skipped entirely — the caller has already
        // obtained a manual or preapproved resolution and must not re-gate the tool.
        bypass_approval: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // Per-agent provenance: life.observe writes must record WHO observed
            // (canonical agent id), not just the membrane transport. Stamp the
            // runtime's identity (and active role, if any) unless already set.
            if tool_call.tool_name == "life.observe" {
                if let Some(args) = tool_call.arguments.as_object_mut() {
                    let has_observed_by = args
                        .get("observed_by")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|v| !v.trim().is_empty());
                    if !has_observed_by {
                        args.insert(
                            "observed_by".into(),
                            serde_json::Value::String(self.agent_id.clone()),
                        );
                    }
                    if args.get("observed_role").is_none_or(|v| v.is_null()) {
                        let observed_role = self
                            .sessions
                            .get(&session_id)
                            .and_then(|state| {
                                state
                                    .role_activation
                                    .as_ref()
                                    .map(|activation| activation.role_name.clone())
                            })
                            .or_else(|| self.role_name.clone());
                        if let Some(role) = observed_role {
                            args.insert("observed_role".into(), serde_json::Value::String(role));
                        }
                    }
                    // Structural anchoring: the LLM tool schema never exposes
                    // `edges`, and non-model write paths have historically
                    // hardcoded `edges: vec![]` — so every observation lands
                    // as an orphan node with nothing to traverse at recall
                    // time. Server-side (zero model burden), attach a
                    // SCOPED_TO anchor resolved from the canonical
                    // observed_by identity, unless one is already present.
                    // Idempotent — never doubles up.
                    inject_scoped_to_anchor(args);
                }
            }
            // Agent-level approval enforcement: if the tool's policy annotation marks it as
            // requiring approval, and the current approval policy does not preapprove it,
            // synthesize an ApprovalRequest before executing. This runs independently of
            // whether the model itself requested approval — it is the agent's safety gate.
            // Skipped when bypass_approval is true (i.e. we are resuming after a resolution).
            let force_approval = if bypass_approval {
                false
            } else {
                self.sessions
                    .get(&session_id)
                    .map(|state| {
                        let requires = state
                            .tool_assembly
                            .policy_annotations
                            .get(&tool_call.tool_name)
                            .map(|a| a.approval_required)
                            .unwrap_or(false);
                        if requires {
                            let synthetic = ApprovalRequest {
                                approval_id: None,
                                reason: format!(
                                    "Tool '{}' requires approval before execution.",
                                    tool_call.tool_name
                                ),
                                approved_response: format!("Executing {}.", tool_call.tool_name),
                            };
                            !state.approval_policy_allows(&synthetic, Some(&tool_call))
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false)
            };

            // Non-bypassable gates: these tools ALWAYS require live operator approval
            // regardless of any approval policy (including `auto_approve_all`). The gate
            // runs before the normal force_approval path so it can set always_require_human.
            // Skipped when bypass_approval is true (already resolved by the operator on the
            // post-approval resume). See `unconditional_approval_gate` for the classifier.
            let unconditional_gate = if bypass_approval {
                None
            } else {
                Self::unconditional_approval_gate(&tool_call.tool_name, &tool_call.arguments)
            };
            let is_admin_role_creation = unconditional_gate == Some("admin_role_creation");
            let is_rule_propose = unconditional_gate == Some("rule_propose");
            let is_routing_policy_propose = unconditional_gate == Some("routing_policy_propose");
            let is_skill_register = unconditional_gate == Some("skill_register");

            if unconditional_gate.is_some() || force_approval {
                // Set pending_tool_call so the approval handler can read it for class lookup.
                if let Some(state) = self.sessions.get_mut(&session_id) {
                    state.set_pending_tool_call(tool_call.clone());
                }
                let role_name_hint = if is_admin_role_creation {
                    tool_call
                        .arguments
                        .get("role_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string()
                } else {
                    String::new()
                };
                let (reason, approved_response) = if is_admin_role_creation {
                    (
                        format!(
                            "Admin role '{}' creation requires your explicit live approval. \
                         This cannot be preapproved or bypassed by policy.",
                            role_name_hint
                        ),
                        format!("Admin role '{}' approved.", role_name_hint),
                    )
                } else if is_rule_propose {
                    (
                        "Rule proposal requires your explicit live approval.".to_string(),
                        "Rule proposal approved.".to_string(),
                    )
                } else if is_routing_policy_propose {
                    (
                        "Routing policy proposal requires your explicit live approval.".to_string(),
                        "Routing policy proposal approved.".to_string(),
                    )
                } else if is_skill_register {
                    (
                        "Skill registration requires your explicit live approval. \
                         This cannot be preapproved or bypassed by policy."
                            .to_string(),
                        "Skill registration approved.".to_string(),
                    )
                } else {
                    (
                        format!(
                            "Tool '{}' requires approval before execution.",
                            tool_call.tool_name
                        ),
                        format!("Executing {}.", tool_call.tool_name),
                    )
                };
                let synthetic = ApprovalRequest {
                    approval_id: Some(uuid::Uuid::new_v4().to_string()),
                    reason,
                    approved_response,
                };
                return self
                    .handle_approval_request(
                        session_id,
                        turn_id,
                        synthetic,
                        is_admin_role_creation
                            || is_rule_propose
                            || is_routing_policy_propose
                            || is_skill_register,
                    )
                    .await;
            }

            // Dedup guard: if (tool_name, canonical_args) already appears in this
            // turn's history with a non-error result, inject a correction note and
            // re-enter the model without dispatching the tool again. This prevents
            // spin loops where the model calls an idempotent tool (e.g. role.create_or_update)
            // repeatedly after it already succeeded.
            let canonical_args = serde_json::to_string(&tool_call.arguments).unwrap_or_default();
            let (
                already_succeeded,
                dedup_chat_id,
                dedup_reply_to,
                dedup_reply_role,
                dedup_reply_guest_id,
            ) = {
                let state = self.sessions.get(&session_id);
                let prev_success = state
                    .and_then(|s| s.active_turn.as_ref())
                    .map(|turn| {
                        turn.working_tool_history
                            .iter()
                            .any(|(prev_call, prev_result)| {
                                prev_call.tool_name == tool_call.tool_name
                                    && serde_json::to_string(&prev_call.arguments)
                                        .unwrap_or_default()
                                        == canonical_args
                                    && !prev_result.content.to_lowercase().contains("error")
                                    && !prev_result.content.to_lowercase().contains("failed")
                            })
                    })
                    .unwrap_or(false);
                let (chat_id, reply_to, reply_role, reply_guest) = state
                    .and_then(|s| s.active_turn.as_ref())
                    .map(|t| {
                        (
                            t.chat_id.clone(),
                            t.final_reply_to.clone(),
                            t.final_reply_role.clone(),
                            t.final_reply_guest_id.clone(),
                        )
                    })
                    .unwrap_or_default();
                (prev_success, chat_id, reply_to, reply_role, reply_guest)
            };

            if already_succeeded {
                warn!(
                    "Session [{}] dedup guard: `{}` already succeeded this turn; \
                     injecting correction note instead of re-dispatching.",
                    session_id, tool_call.tool_name
                );
                if let Some(state) = self.sessions.get_mut(&session_id) {
                    state.set_provider_repair_note(format!(
                        "`{}` with these arguments already succeeded earlier in this turn. \
                         Do not call it again. Review the tool history and either proceed \
                         to the next plan step or deliver your final response.",
                        tool_call.tool_name
                    ));
                }
                return self
                    .handle_tool_result(InboundTaskPayload {
                        action: Some("tool_result".into()),
                        source: Some("agent".into()),
                        session_id: Some(session_id),
                        turn_id: Some(turn_id),
                        chat_id: Some(dedup_chat_id),
                        content: Some(format!(
                            "[Duplicate call skipped] `{}` already ran and succeeded \
                             earlier in this turn with these arguments.",
                            tool_call.tool_name
                        )),
                        tool_name: Some(tool_call.tool_name),
                        final_reply_to: Some(dedup_reply_to),
                        final_reply_role: Some(dedup_reply_role),
                        final_reply_guest_id: dedup_reply_guest_id,
                        ..Default::default()
                    })
                    .await;
            }

            // Emit step_started if streaming is enabled.
            let stream_events = self
                .sessions
                .get(&session_id)
                .map(|s| s.settings.execution.stream_tool_events)
                .unwrap_or(true);
            if stream_events {
                let step_info = Some(tool_call.tool_name.clone());
                let _ = self
                    .emit_turn_event(&session_id, "step_started", step_info)
                    .await;
            }
            let (
                chat_id,
                final_reply_to,
                final_reply_role,
                final_reply_guest_id,
                workspace_ref,
                route,
                session_user_id,
            ) = {
                let Some(state) = self.sessions.get(&session_id) else {
                    warn!(
                        "Tool execution requested for unknown session {}",
                        session_id
                    );
                    return Ok(());
                };
                let route = match Self::execute_bound_tool(state, &tool_call) {
                    Ok(route) => route.clone(),
                    Err(err) => {
                        return self
                            .fail_active_turn(session_id, turn_id, err.to_string())
                            .await;
                    }
                };
                let Some(active_turn) = state.active_turn.as_ref() else {
                    warn!(
                        "Dropping tool execution routing for session {} turn {} after active turn disappeared",
                        session_id, turn_id
                    );
                    return Ok(());
                };
                (
                    active_turn.chat_id.clone(),
                    active_turn.final_reply_to.clone(),
                    active_turn.final_reply_role.clone(),
                    active_turn.final_reply_guest_id.clone(),
                    state.bindings.effective_workspace_ref.clone(),
                    route,
                    state.source.clone(),
                )
            };

            // Store the tool call on the active turn BEFORE dispatching so that when the
            // result returns, handle_tool_result can recover the full (name + arguments)
            // pair for the working_tool_history. Without this, the fallback uses empty args.
            let pending_checkpoint = if let Some(state) = self.sessions.get_mut(&session_id) {
                state.set_pending_tool_call(tool_call.clone());
                state.set_active_turn_phase(TurnPhase::WaitingTool);
                Some((
                    state.checkpoint_memory_type(),
                    state.checkpoint_json(),
                    state.clone(),
                ))
            } else {
                None
            };
            if let Some((checkpoint_memory_type, checkpoint_json, index_state)) = pending_checkpoint
            {
                if let Err(e) = tokio::time::timeout(
                    Duration::from_secs(15),
                    self.ipc_client.sync_apartment(
                        &self.agent_id,
                        &checkpoint_memory_type,
                        checkpoint_json,
                    ),
                )
                .await
                .map_err(|_| anyhow::anyhow!("sync_apartment: ipc ack timeout after 15s"))
                .and_then(|r| r)
                {
                    warn!("route_tool_call_execution: sync_apartment failed: {e}; continuing");
                }
                if let Err(e) = tokio::time::timeout(
                    Duration::from_secs(15),
                    self.sync_session_index(&index_state),
                )
                .await
                .map_err(|_| anyhow::anyhow!("sync_session_index: ipc ack timeout after 15s"))
                .and_then(|r| r)
                {
                    warn!("route_tool_call_execution: sync_session_index failed: {e}; continuing");
                }
            }

            // Emit a status message to membrane so the user sees what's happening.
            let status_label = tool_status_label(&tool_call.tool_name);
            let _ = tokio::time::timeout(
                Duration::from_secs(10),
                self.emit_turn_status(&session_id, status_label),
            )
            .await;

            let tool_req = ToolExecutionPayload {
                action: "execute_tool",
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
                chat_id,
                tool_name: tool_call.tool_name,
                arguments: tool_call.arguments,
                execution_mode: route.execution_mode.clone(),
                agent_id: self.agent_id.clone(),
                user_id: Some(session_user_id),
                runner_id: route.runner_id.clone(),
                incarnation_id: route.incarnation_id.clone(),
                hotel_id: route.hotel_id.clone(),
                environment_id: route.environment_id.clone(),
                task_runner_kind: route.task_runner_kind.clone(),
                task_runner_config: route.task_runner_config.clone(),
                selection_reason: route.selection_reason.clone(),
                workspace_ref: workspace_ref.clone(),
                task_runner_overlay: route.task_runner_kind.as_deref().map(|kind| {
                    TaskRunnerOverlay {
                        workspace_ref: if kind == "workspace" {
                            workspace_ref
                        } else {
                            None
                        },
                        allowed_tools: None,
                        max_read_bytes: None,
                        max_search_results: None,
                    }
                }),
                return_route: Some(philotic_client::ReturnRoute {
                    node: local_node_id(),
                    role: "agent".into(),
                    guest_id: Some(self.agent_id.clone()),
                    session_id: Some(session_id.clone()),
                    turn_id: Some(turn_id.clone()),
                    correlation_id: None,
                }),
                reply_to: local_node_id(),
                reply_role: "agent".into(),
                reply_guest_id: Some(self.agent_id.clone()),
                final_reply_to,
                final_reply_role,
                final_reply_guest_id,
            };

            if route.execution_mode == "local_agent" {
                return self.execute_local_agent_tool(tool_req).await;
            }

            // Capability primitives (image.*, audio.*, etc.) use the synchronous hotel router.
            if route.execution_mode == "capability_invoke" {
                let capability_req = build_capability_request(
                    &tool_req.tool_name,
                    &tool_req.arguments,
                    &tool_req.session_id,
                    &tool_req.agent_id,
                );
                let ipc_resp = self
                    .ipc_client
                    .send_request(IpcRequest::CapabilityInvoke {
                        request: capability_req,
                    })
                    .await?;
                let content = match ipc_resp {
                    IpcResponse::Standard {
                        ok: true,
                        data: Some(result),
                        ..
                    } => {
                        serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string())
                    }
                    IpcResponse::Standard {
                        ok: false, message, ..
                    } => format!("Capability call failed: {message}"),
                    other => format!(
                        "Unexpected hotel response for capability '{}': {other:?}",
                        tool_req.tool_name
                    ),
                };
                return self
                    .handle_tool_result(crate::protocol::InboundTaskPayload {
                        action: Some("tool_result".into()),
                        session_id: Some(tool_req.session_id),
                        turn_id: Some(tool_req.turn_id),
                        chat_id: Some(tool_req.chat_id),
                        tool_name: Some(tool_req.tool_name),
                        content: Some(content),
                        ..Default::default()
                    })
                    .await;
            }

            self.ipc_client
                .send_request_with_timeout(
                    IpcRequest::EmitTask {
                        target_node: route.target_node,
                        target_role: route.target_role,
                        target_guest_id: route.incarnation_id.clone(),
                        task_json: serde_json::to_string(&tool_req)?,
                    },
                    Duration::from_secs(30),
                )
                .await
                .context("tool dispatch: ipc ack failed or timed out after 30s")?;

            Ok(())
        })
    }

    pub(super) async fn handle_approval_command(
        &mut self,
        command_task_id: Uuid,
        session_id: String,
        command_turn_id: String,
        command_chat_id: String,
        command_reply_to: String,
        command_reply_role: String,
        command_reply_guest_id: Option<String>,
        command: SlashCommand,
    ) -> Result<()> {
        // Approval turns are parked in `parked_approval_turn` while the session stays free.
        // Restore the parked turn into active_turn so the resolution logic proceeds normally.
        if let Some(state) = self.sessions.get_mut(&session_id) {
            if state.has_parked_approval_turn() && !state.is_turn_active() {
                state.restore_parked_approval_turn();
            }
        }

        let pending = self
            .sessions
            .get(&session_id)
            .and_then(|state| state.active_turn.as_ref())
            .and_then(|turn| {
                if turn.phase == TurnPhase::WaitingApproval {
                    turn.pending_approval.clone().map(|approval| {
                        (
                            turn.task_id,
                            turn.turn_id.clone(),
                            turn.chat_id.clone(),
                            turn.final_reply_to.clone(),
                            turn.final_reply_role.clone(),
                            turn.final_reply_guest_id.clone(),
                            turn.pending_tool_call.clone(),
                            approval,
                        )
                    })
                } else {
                    None
                }
            });

        let Some((
            original_task_id,
            original_turn_id,
            original_chat_id,
            original_reply_to,
            original_reply_role,
            original_reply_guest_id,
            original_pending_tool_call,
            approval,
        )) = pending
        else {
            let _ = self
                .ipc_client
                .send_request(IpcRequest::CompleteTask {
                    task_id: command_task_id,
                    result: serde_json::json!({
                        "session_id": session_id,
                        "turn_id": command_turn_id,
                        "chat_id": command_chat_id,
                        "content": "No approval pending."
                    }),
                })
                .await?;
            let reply_payload = FinalReplyPayload {
                action: "send_reply",
                session_id,
                turn_id: command_turn_id,
                chat_id: command_chat_id,
                content: "No approval pending.".into(),
                audio_artifact: None,
                send_text_caption: false,
                reply_markup: None,
            };
            self.ipc_client
                .send_request(IpcRequest::EmitTask {
                    target_node: command_reply_to,
                    target_role: command_reply_role,
                    target_guest_id: command_reply_guest_id,
                    task_json: serde_json::to_string(&reply_payload)?,
                })
                .await?;
            return Ok(());
        };

        let command_has_steering = command.steering_note().is_some();
        let (checkpoint_memory_type, checkpoint_json, index_state) = {
            let state = self
                .sessions
                .get_mut(&session_id)
                .expect("session should exist while resolving approval");
            state.clear_pending_approval();
            match command {
                SlashCommand::Approve { .. } => state.set_active_turn_phase(TurnPhase::Thinking),
                SlashCommand::Deny { .. } => {
                    if command_has_steering {
                        state.set_active_turn_phase(TurnPhase::Thinking);
                    } else {
                        state.set_active_turn_phase(TurnPhase::Failed);
                    }
                }
                SlashCommand::Ping
                | SlashCommand::Status
                | SlashCommand::Context
                | SlashCommand::Pause
                | SlashCommand::Resume
                | SlashCommand::Role { .. }
                | SlashCommand::Roles
                | SlashCommand::Back
                | SlashCommand::ToolsAdd { .. }
                | SlashCommand::ToolsClear
                | SlashCommand::SkillsAdd { .. }
                | SlashCommand::SkillsClear
                | SlashCommand::WorkspaceSet { .. }
                | SlashCommand::WorkspaceClear
                | SlashCommand::PreapproveThisSession
                | SlashCommand::Preapprove { .. }
                | SlashCommand::ApprovalStatus
                | SlashCommand::ApprovalReset
                | SlashCommand::ApprovalClear { .. }
                | SlashCommand::Abandon { .. }
                | SlashCommand::Tts { .. }
                | SlashCommand::Voice { .. }
                | SlashCommand::Model { .. }
                | SlashCommand::ModelPreset { .. }
                | SlashCommand::Dirty
                | SlashCommand::Sfw
                | SlashCommand::Correct { .. }
                | SlashCommand::Plan { .. } => {}
            }
            (
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
                task_id: command_task_id,
                result: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": command_turn_id,
                    "chat_id": command_chat_id,
                    "content": command.reply_text().unwrap_or("ok"),
                }),
            })
            .await?;

        match command {
            SlashCommand::Approve { note } => {
                let _ = self
                    .ipc_client
                    .send_request(IpcRequest::UpdateTask {
                        task_id: original_task_id,
                        state: "resuming".into(),
                        payload: serde_json::json!({
                            "session_id": session_id,
                            "turn_id": original_turn_id,
                            "chat_id": original_chat_id,
                            "approval_resolution": {
                                "approval_id": approval.approval_id,
                                "decision": "approved",
                                "reason": approval.reason,
                                "resolution_mode": "manual",
                                "note": note,
                            }
                        }),
                    })
                    .await?;
                if let Some(note) = note {
                    return self
                        .resume_turn_with_steering(
                            session_id,
                            original_turn_id,
                            original_chat_id,
                            note,
                            "resuming_with_steering",
                            "[User approval steering]",
                        )
                        .await;
                }
                let _ = self
                    .ipc_client
                    .send_request(IpcRequest::CompleteTask {
                        task_id: original_task_id,
                        result: serde_json::json!({
                            "session_id": session_id,
                            "turn_id": original_turn_id,
                            "chat_id": original_chat_id,
                            "content": approval.approved_response,
                        }),
                    })
                    .await?;

                let reply_payload = FinalReplyPayload {
                    action: "send_reply",
                    session_id: session_id.clone(),
                    turn_id: original_turn_id.clone(),
                    chat_id: original_chat_id,
                    content: approval.approved_response.clone(),
                    audio_artifact: None,
                    send_text_caption: false,
                    reply_markup: None,
                };

                self.ipc_client
                    .send_request(IpcRequest::EmitTask {
                        target_node: original_reply_to,
                        target_role: original_reply_role,
                        target_guest_id: original_reply_guest_id.clone(),
                        task_json: serde_json::to_string(&reply_payload)?,
                    })
                    .await?;

                // Scripted gate: record approval as the current step's output and
                // let the executor drive what comes next.
                if approval
                    .approval_id
                    .as_deref()
                    .map(|id| id.starts_with("scripted_gate:"))
                    .unwrap_or(false)
                {
                    if let Some(state) = self.sessions.get_mut(&session_id) {
                        state.with_scripted_executor_mut(|exec| {
                            exec.record_step_output(serde_json::json!({"approved": true}));
                        });
                    }
                    return self
                        .scripted_dispatch_after_advance(
                            session_id,
                            original_turn_id,
                            None,
                            None,
                            None,
                        )
                        .await;
                }

                if let Some(tool_call) = original_pending_tool_call {
                    // bypass_approval=true: this tool was already manually approved above.
                    return self
                        .route_tool_call_execution(session_id, original_turn_id, tool_call, true)
                        .await;
                }
            }
            SlashCommand::Deny { note } => {
                let _ = self
                    .ipc_client
                    .send_request(IpcRequest::UpdateTask {
                        task_id: original_task_id,
                        state: "approval_denied".into(),
                        payload: serde_json::json!({
                            "session_id": session_id,
                            "turn_id": original_turn_id,
                            "chat_id": original_chat_id,
                            "approval_resolution": {
                                "approval_id": approval.approval_id,
                                "decision": "denied",
                                "reason": approval.reason,
                                "resolution_mode": "manual",
                                "note": note,
                            }
                        }),
                    })
                    .await?;
                if let Some(note) = note {
                    return self
                        .resume_turn_with_steering(
                            session_id,
                            original_turn_id,
                            original_chat_id,
                            note,
                            "redirecting_after_denial",
                            "[User denied the proposed action. Do this instead]",
                        )
                        .await;
                }
                let _ = self
                    .ipc_client
                    .send_request(IpcRequest::FailTask {
                        task_id: original_task_id,
                        error_code: "APPROVAL_DENIED".into(),
                        reason: approval.reason.clone(),
                        session_id: None,
                        turn_id: None,
                    })
                    .await?;

                let reply_payload = FinalReplyPayload {
                    action: "send_reply",
                    session_id: session_id.clone(),
                    turn_id: original_turn_id.clone(),
                    chat_id: original_chat_id,
                    content: format!("Denied: {}", approval.reason),
                    audio_artifact: None,
                    send_text_caption: false,
                    reply_markup: None,
                };

                self.ipc_client
                    .send_request(IpcRequest::EmitTask {
                        target_node: original_reply_to,
                        target_role: original_reply_role,
                        target_guest_id: original_reply_guest_id,
                        task_json: serde_json::to_string(&reply_payload)?,
                    })
                    .await?;

                // Scripted gate denial: fail the turn (default reject_action = fail_turn).
                if approval
                    .approval_id
                    .as_deref()
                    .map(|id| id.starts_with("scripted_gate:"))
                    .unwrap_or(false)
                {
                    return self
                        .fail_active_turn(
                            session_id,
                            original_turn_id,
                            format!("Plan rejected: {}", approval.reason),
                        )
                        .await;
                }
            }
            SlashCommand::Ping
            | SlashCommand::Status
            | SlashCommand::Context
            | SlashCommand::Pause
            | SlashCommand::Resume
            | SlashCommand::Role { .. }
            | SlashCommand::Roles
            | SlashCommand::Back
            | SlashCommand::ToolsAdd { .. }
            | SlashCommand::ToolsClear
            | SlashCommand::SkillsAdd { .. }
            | SlashCommand::SkillsClear
            | SlashCommand::WorkspaceSet { .. }
            | SlashCommand::WorkspaceClear
            | SlashCommand::PreapproveThisSession
            | SlashCommand::Preapprove { .. }
            | SlashCommand::ApprovalStatus
            | SlashCommand::ApprovalReset
            | SlashCommand::ApprovalClear { .. }
            | SlashCommand::Abandon { .. }
            | SlashCommand::Tts { .. }
            | SlashCommand::Voice { .. }
            | SlashCommand::Model { .. }
            | SlashCommand::ModelPreset { .. }
            | SlashCommand::Dirty
            | SlashCommand::Sfw
            | SlashCommand::Correct { .. }
            | SlashCommand::Plan { .. } => {}
        }

        Ok(())
    }

    pub(super) fn should_defer_parked_approval_command(state: &SessionState) -> bool {
        state.has_parked_approval_turn()
            && state
                .active_turn
                .as_ref()
                .map(|turn| turn.phase != TurnPhase::WaitingApproval)
                .unwrap_or(false)
    }

    pub(super) fn execute_bound_tool<'a>(
        state: &'a SessionState,
        tool_call: &ToolCall,
    ) -> Result<&'a ToolExecutionRoute> {
        if !state.tool_is_enabled(&tool_call.tool_name) {
            anyhow::bail!(
                "Tool {} is not enabled for this session",
                tool_call.tool_name
            );
        }
        state
            .resolve_tool_route(&tool_call.tool_name)
            .and_then(|route| {
                if route.execution_mode != "local_agent" && route.availability_state != "live" {
                    None
                } else {
                    Some(route)
                }
            })
            .ok_or_else(|| {
                if let Some(route) = state.resolve_tool_route(&tool_call.tool_name) {
                    anyhow::anyhow!(
                        "Tool {} requires runner materialization (availability: {}, runner: {})",
                        tool_call.tool_name,
                        route.availability_state,
                        route.runner_id.as_deref().unwrap_or("unknown")
                    )
                } else {
                    anyhow::anyhow!(
                        "Tool {} has no assembled execution route",
                        tool_call.tool_name
                    )
                }
            })
    }

    pub(super) async fn execute_local_agent_tool(
        &mut self,
        payload: ToolExecutionPayload,
    ) -> Result<()> {
        match payload.tool_name.as_str() {
            "session.status" => {
                let content = self
                    .sessions
                    .get(&payload.session_id)
                    .map(SessionState::session_status_text)
                    .unwrap_or_else(|| "Session status unavailable.".into());

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
                    error: None,
                    tool_name: Some(payload.tool_name),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }
            "hotel.status" => {
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::GetHotelStatus)
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let text = serde_json::to_string_pretty(&data)
                            .unwrap_or_else(|_| data.to_string());
                        (text, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("Hotel status unavailable.".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("hotel.status: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }
            "hotel.perimeter.status" => {
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::GetPerimeterStatus)
                    .await
                {
                    Ok(IpcResponse::PerimeterStatus { snapshot_json }) => {
                        let text = serde_json::from_str::<serde_json::Value>(&snapshot_json)
                            .ok()
                            .and_then(|v| serde_json::to_string_pretty(&v).ok())
                            .unwrap_or(snapshot_json);
                        (text, None)
                    }
                    Ok(_) => ("Perimeter status unavailable.".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("hotel.perimeter.status: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }
            "hotel.perimeter.refresh" => {
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::RefreshPerimeter)
                    .await
                {
                    Ok(IpcResponse::PerimeterStatus { snapshot_json }) => {
                        let text = serde_json::from_str::<serde_json::Value>(&snapshot_json)
                            .ok()
                            .and_then(|v| serde_json::to_string_pretty(&v).ok())
                            .unwrap_or(snapshot_json);
                        (format!("Perimeter refreshed.\n{text}"), None)
                    }
                    Ok(_) => ("Perimeter refresh unavailable.".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("hotel.perimeter.refresh: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }
            "hotel.egress.check" => {
                let target_url = payload
                    .arguments
                    .get("target_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let method = payload
                    .arguments
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("GET")
                    .to_string();
                let agent_id = payload
                    .arguments
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::CheckEgress {
                        agent_id,
                        target_url,
                        method,
                    })
                    .await
                {
                    Ok(IpcResponse::EgressGrant {
                        allowed,
                        audit,
                        deny_reason,
                        inject_headers,
                    }) => {
                        let text = serde_json::to_string_pretty(&serde_json::json!({
                            "allowed": allowed,
                            "audit": audit,
                            "deny_reason": deny_reason,
                            "inject_headers": inject_headers,
                        }))
                        .unwrap_or_else(|_| format!("allowed={allowed}"));
                        (text, None)
                    }
                    Ok(_) => ("Egress check unavailable.".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("hotel.egress.check: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }
            "hotel.logs" => {
                let lines = payload
                    .arguments
                    .get("lines")
                    .and_then(|v| v.as_u64())
                    .map(|v| v.min(500) as u32)
                    .unwrap_or(50);
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::GetHotelLogs { lines })
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let log = data
                            .get("log")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        (log, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("Hotel logs unavailable.".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("hotel.logs: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }
            "hotel.best_place_to_run" => {
                let args = &payload.arguments;
                let agent_id = args
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let role_name = args
                    .get("role_name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let tool_name = args
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let required_markers = args
                    .get("required_markers")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let prefer_locality = args
                    .get("prefer_locality")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::BestPlaceToRun {
                        agent_id,
                        role_name,
                        tool_name,
                        required_markers,
                        prefer_locality,
                    })
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let hotel = data
                            .get("recommended_hotel")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let node = data
                            .get("recommended_node_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let reason = data
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("no reason recorded");
                        let candidate_lines = data
                            .get("candidates")
                            .and_then(|v| v.as_array())
                            .map(|items| {
                                items
                                    .iter()
                                    .take(5)
                                    .map(|c| {
                                        let h = c
                                            .get("hotel_name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown");
                                        let n = c
                                            .get("node_id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown");
                                        let s =
                                            c.get("score").and_then(|v| v.as_i64()).unwrap_or(0);
                                        format!("- {} ({}) score={}", h, n, s)
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .unwrap_or_default();
                        let msg = if candidate_lines.is_empty() {
                            format!("Best placement: {} ({}).\nReason: {}", hotel, node, reason)
                        } else {
                            format!(
                                "Best placement: {} ({}).\nReason: {}\nCandidates:\n{}",
                                hotel, node, reason, candidate_lines
                            )
                        };
                        (msg, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("Placement recommendation unavailable.".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("hotel.best_place_to_run: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }
            "router.stats" => {
                let window_secs = payload
                    .arguments
                    .get("window_hours")
                    .and_then(|v| v.as_f64())
                    .map(|h| (h * 3600.0) as u64);
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::GetRouterStats { window_secs })
                    .await
                {
                    Ok(IpcResponse::RouterStats {
                        stats,
                        generated_at,
                    }) => {
                        let text = serde_json::to_string_pretty(&serde_json::json!({
                            "generated_at": generated_at,
                            "stats": stats
                        }))
                        .unwrap_or_else(|_| "Router stats unavailable.".into());
                        (text, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("Router stats unavailable.".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("router.stats: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }
            "agent.configure" => {
                let args = &payload.arguments;
                let config_path = match args.get("config_path").and_then(|v| v.as_str()) {
                    Some(p) => p.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "agent.configure: missing required argument 'config_path'".into(),
                            )
                            .await;
                    }
                };
                let value = match args.get("value") {
                    Some(v) => v.clone(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "agent.configure: missing required argument 'value'".into(),
                            )
                            .await;
                    }
                };
                let operation = args
                    .get("operation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("set")
                    .to_string();

                let configure_result = {
                    let Some(state) = self.sessions.get_mut(&payload.session_id) else {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "agent.configure: session not found".into(),
                            )
                            .await;
                    };
                    let bindings_before = state.bindings.clone();
                    match state.apply_configure(&config_path, &value, &operation) {
                        Ok(msg) => {
                            let changed = state.bindings != bindings_before;
                            Ok((msg, changed))
                        }
                        Err(err) => Err(err),
                    }
                };
                let (content, bindings_changed) = match configure_result {
                    Ok(result) => result,
                    Err(err) => {
                        return self
                            .fail_active_turn(payload.session_id, payload.turn_id, err)
                            .await;
                    }
                };

                // Rebuild tool assembly if bindings changed so the new toolset takes effect
                // immediately within the same session.
                if bindings_changed {
                    if let Some(state) = self.sessions.get_mut(&payload.session_id) {
                        state.rebuild_default_tool_assembly();
                    }
                }

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
                    error: None,
                    tool_name: Some(payload.tool_name),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }
            "agent.migrate_to" => {
                let dest_hotel = match payload.arguments.get("dest_hotel").and_then(|v| v.as_str())
                {
                    Some(h) => h.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "agent.migrate_to: missing required argument 'dest_hotel'".into(),
                            )
                            .await;
                    }
                };
                let result = self
                    .ipc_client
                    .send_request(IpcRequest::AgentMigrateToHotel {
                        agent_id: self.agent_id.clone(),
                        dest_hotel: dest_hotel.clone(),
                    })
                    .await;
                let content = match result {
                    Ok(IpcResponse::Standard {
                        ok: true, message, ..
                    }) => {
                        if message.is_empty() {
                            format!("Migration to '{}' dispatched.", dest_hotel)
                        } else {
                            message
                        }
                    }
                    Ok(IpcResponse::Standard {
                        ok: false, message, ..
                    }) => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                if message.is_empty() {
                                    format!(
                                        "agent.migrate_to: migration to '{}' failed",
                                        dest_hotel
                                    )
                                } else {
                                    message
                                },
                            )
                            .await;
                    }
                    Ok(other) => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                format!("agent.migrate_to: unexpected response: {other:?}"),
                            )
                            .await;
                    }
                    Err(e) => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                format!("agent.migrate_to: IPC error — {e}"),
                            )
                            .await;
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
                    error: None,
                    tool_name: Some(payload.tool_name),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }
            "role.configure" => {
                let args = &payload.arguments;

                macro_rules! require_str_arg {
                    ($key:literal) => {
                        match args.get($key).and_then(|v| v.as_str()) {
                            Some(s) => s.to_string(),
                            None => {
                                return self
                                    .fail_active_turn(
                                        payload.session_id,
                                        payload.turn_id,
                                        format!(
                                            "role.configure: missing required argument '{}'",
                                            $key
                                        ),
                                    )
                                    .await;
                            }
                        }
                    };
                }

                let role_name = require_str_arg!("role_name");
                let toolset_profile = require_str_arg!("toolset_profile");

                if let None = args.get("reasoning").and_then(|v| v.as_object()) {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "role.configure: missing required object argument 'reasoning'".into(),
                        )
                        .await;
                }

                let role_identity_addendum = args
                    .get("role_identity_addendum")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let role_manifest = args
                    .get("role_manifest")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let is_admin = args
                    .get("is_admin")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let inactive_ttl_seconds =
                    args.get("inactive_ttl_seconds").and_then(|v| v.as_u64());
                let iteration_cap = args
                    .get("iteration_cap")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                let approval_policy = args
                    .get("approval_policy")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let model_profile = args
                    .get("model_profile")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let context_window_policy = args
                    .get("context_window_policy")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let fallback_tiers = args
                    .get("fallback_tiers")
                    .and_then(parse_fallback_tiers_arg);
                let model_bindings = args
                    .get("model_bindings")
                    .and_then(parse_model_bindings_arg);
                let content_policy = args
                    .get("content_policy")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                // Read the active persona role from session state to pass as calling authority.
                // Falls back to "orchestrator" when no role is active (default persona).
                let calling_role = self
                    .sessions
                    .get(&payload.session_id)
                    .and_then(|s| s.role_activation.as_ref())
                    .map(|r| r.role_name.clone())
                    .unwrap_or_else(|| "orchestrator".to_string());

                let req = IpcRequest::ConfigureRole {
                    agent_id: self.agent_id.clone(),
                    role_name: role_name.clone(),
                    guest_id: format!("{}:{}", self.agent_id, role_name),
                    calling_role,
                    toolset_profile,
                    role_identity_addendum,
                    role_manifest,
                    is_admin,
                    inactive_ttl_seconds,
                    iteration_cap,
                    approval_policy,
                    model_profile,
                    context_window_policy,
                    fallback_tiers: fallback_tiers.clone(),
                    model_bindings: model_bindings.clone(),
                    content_policy,
                };

                let (content, tool_err) = match self.ipc_client.send_request(req).await {
                    Ok(IpcResponse::ConfigureRoleOk { role_name: name }) => {
                        // Mirror the hotel's preserve-on-None semantics locally: when
                        // this call didn't set an explicit ladder, keep whatever this
                        // process already had cached for the role rather than
                        // collapsing it to empty (which would desync the cache from
                        // the DB until the next restart/reconfigure).
                        let effective_fallback_tiers = fallback_tiers.unwrap_or_else(|| {
                            self.configured_roles
                                .get(&name)
                                .map(|c| c.turn_loop_config.fallback_tiers.clone())
                                .unwrap_or_default()
                        });
                        // Same preserve-on-None mirroring for model_bindings
                        // (Layer 1): an omitted argument keeps whatever this
                        // process already had cached rather than collapsing
                        // it to empty.
                        let effective_model_bindings = model_bindings.unwrap_or_else(|| {
                            self.configured_roles
                                .get(&name)
                                .map(|c| c.turn_loop_config.model_bindings.clone())
                                .unwrap_or_default()
                        });
                        // Same preserve-on-None mirroring for content_policy: an
                        // omitted argument keeps whatever this process already had
                        // cached (or "standard" for a brand-new role) rather than
                        // resetting an operator-set "unrestricted" policy.
                        let effective_content_policy = args
                            .get("content_policy")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                self.configured_roles
                                    .get(&name)
                                    .map(|c| c.content_policy.clone())
                                    .unwrap_or_else(|| "standard".to_string())
                            });
                        self.configured_roles.insert(
                            name.clone(),
                            CachedRoleConfig {
                                toolset_profile: args
                                    .get("toolset_profile")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("default")
                                    .to_string(),
                                role_identity_addendum: args
                                    .get("role_identity_addendum")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                                role_manifest: args
                                    .get("role_manifest")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                                iteration_cap: args
                                    .get("iteration_cap")
                                    .and_then(|v| v.as_u64())
                                    .map(|v| v as u32),
                                approval_policy: args
                                    .get("approval_policy")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                                turn_loop_config: args
                                    .get("turn_loop_config")
                                    .and_then(|v| {
                                        serde_json::from_value::<
                                            ansible_mesh_core::graph::TurnLoopConfig,
                                        >(v.clone())
                                        .ok()
                                    })
                                    .map(|mut tlc| {
                                        tlc.fallback_tiers = effective_fallback_tiers.clone();
                                        tlc.model_bindings = effective_model_bindings.clone();
                                        tlc
                                    })
                                    .unwrap_or(ansible_mesh_core::graph::TurnLoopConfig {
                                        fallback_tiers: effective_fallback_tiers,
                                        model_bindings: effective_model_bindings,
                                        ..Default::default()
                                    }),
                                content_policy: effective_content_policy,
                            },
                        );
                        // Refresh the delegation roster so new/updated roles appear
                        // in the system prompt for subsequent sessions without a restart.
                        self.fetch_role_names().await;
                        (
                            format!("Successfully configured role incarnation for '{}'.", name),
                            None,
                        )
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "IPC_ERROR", msg);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "role.configure: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("role.configure: IPC transport error — {e}"),
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
            // role.create_or_update is the governed workflow surface for role authoring.
            // It validates the same required fields as role.configure and resolves through
            // the same IpcRequest::ConfigureRole hotel path — no external subscriber needed.
            "role.create_or_update" => {
                let args = &payload.arguments;

                macro_rules! require_str_arg {
                    ($key:literal) => {
                        match args.get($key).and_then(|v| v.as_str()) {
                            Some(s) => s.to_string(),
                            None => {
                                return self
                                    .fail_active_turn(
                                        payload.session_id,
                                        payload.turn_id,
                                        format!(
                                            "role.create_or_update: missing required argument '{}'",
                                            $key
                                        ),
                                    )
                                    .await;
                            }
                        }
                    };
                }

                let role_name = require_str_arg!("role_name");
                let toolset_profile = require_str_arg!("toolset_profile");

                if args.get("reasoning").and_then(|v| v.as_object()).is_none() {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "role.create_or_update: missing required object argument 'reasoning'"
                                .into(),
                        )
                        .await;
                }

                let role_identity_addendum = args
                    .get("role_identity_addendum")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let role_manifest = args
                    .get("role_manifest")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let is_admin = args
                    .get("is_admin")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let inactive_ttl_seconds =
                    args.get("inactive_ttl_seconds").and_then(|v| v.as_u64());
                let iteration_cap = args
                    .get("iteration_cap")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                let approval_policy = args
                    .get("approval_policy")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let model_profile = args
                    .get("model_profile")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let context_window_policy = args
                    .get("context_window_policy")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let fallback_tiers = args
                    .get("fallback_tiers")
                    .and_then(parse_fallback_tiers_arg);
                let model_bindings = args
                    .get("model_bindings")
                    .and_then(parse_model_bindings_arg);
                let content_policy = args
                    .get("content_policy")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let calling_role = self
                    .sessions
                    .get(&payload.session_id)
                    .and_then(|s| s.role_activation.as_ref())
                    .map(|r| r.role_name.clone())
                    .unwrap_or_else(|| "orchestrator".to_string());

                let req = IpcRequest::ConfigureRole {
                    agent_id: self.agent_id.clone(),
                    role_name: role_name.clone(),
                    guest_id: format!("{}:{}", self.agent_id, role_name),
                    calling_role,
                    toolset_profile,
                    role_identity_addendum,
                    role_manifest,
                    is_admin,
                    inactive_ttl_seconds,
                    iteration_cap,
                    approval_policy,
                    model_profile,
                    context_window_policy,
                    fallback_tiers: fallback_tiers.clone(),
                    model_bindings: model_bindings.clone(),
                    content_policy,
                };

                let (content, tool_err) = match self.ipc_client.send_request(req).await {
                    Ok(IpcResponse::ConfigureRoleOk { role_name: name }) => {
                        // See role.configure above: mirror the hotel's preserve-on-None
                        // semantics in the local cache so it doesn't desync from the DB.
                        let effective_fallback_tiers = fallback_tiers.unwrap_or_else(|| {
                            self.configured_roles
                                .get(&name)
                                .map(|c| c.turn_loop_config.fallback_tiers.clone())
                                .unwrap_or_default()
                        });
                        // Same preserve-on-None mirroring for model_bindings
                        // (Layer 1): an omitted argument keeps whatever this
                        // process already had cached rather than collapsing
                        // it to empty.
                        let effective_model_bindings = model_bindings.unwrap_or_else(|| {
                            self.configured_roles
                                .get(&name)
                                .map(|c| c.turn_loop_config.model_bindings.clone())
                                .unwrap_or_default()
                        });
                        let effective_content_policy = args
                            .get("content_policy")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                self.configured_roles
                                    .get(&name)
                                    .map(|c| c.content_policy.clone())
                                    .unwrap_or_else(|| "standard".to_string())
                            });
                        self.configured_roles.insert(
                            name.clone(),
                            CachedRoleConfig {
                                toolset_profile: args
                                    .get("toolset_profile")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("default")
                                    .to_string(),
                                role_identity_addendum: args
                                    .get("role_identity_addendum")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                                role_manifest: args
                                    .get("role_manifest")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                                iteration_cap: args
                                    .get("iteration_cap")
                                    .and_then(|v| v.as_u64())
                                    .map(|v| v as u32),
                                approval_policy: args
                                    .get("approval_policy")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                                turn_loop_config: args
                                    .get("turn_loop_config")
                                    .and_then(|v| {
                                        serde_json::from_value::<
                                            ansible_mesh_core::graph::TurnLoopConfig,
                                        >(v.clone())
                                        .ok()
                                    })
                                    .map(|mut tlc| {
                                        tlc.fallback_tiers = effective_fallback_tiers.clone();
                                        tlc.model_bindings = effective_model_bindings.clone();
                                        tlc
                                    })
                                    .unwrap_or(ansible_mesh_core::graph::TurnLoopConfig {
                                        fallback_tiers: effective_fallback_tiers,
                                        model_bindings: effective_model_bindings,
                                        ..Default::default()
                                    }),
                                content_policy: effective_content_policy,
                            },
                        );
                        self.fetch_role_names().await;
                        (
                            format!("Role '{}' created/updated successfully.", name),
                            None,
                        )
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "IPC_ERROR", msg);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "role.create_or_update: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("role.create_or_update: IPC transport error — {e}"),
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
            "skill.register" => {
                let args = &payload.arguments;

                macro_rules! require_str_arg {
                    ($key:literal) => {
                        match args.get($key).and_then(|v| v.as_str()) {
                            Some(s) => s.to_string(),
                            None => {
                                return self
                                    .fail_active_turn(
                                        payload.session_id,
                                        payload.turn_id,
                                        format!(
                                            "skill.register: missing required argument '{}'",
                                            $key
                                        ),
                                    )
                                    .await;
                            }
                        }
                    };
                }

                let skill_name = require_str_arg!("skill_name");
                let description = require_str_arg!("description");
                let subagent_kind = require_str_arg!("subagent_kind");
                let goal = require_str_arg!("goal");

                let str_vec = |key: &str| -> Vec<String> {
                    args.get(key)
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let allowed_tools = str_vec("allowed_tools");
                let allowed_classes = str_vec("allowed_classes");

                let response = self
                    .ipc_client
                    .send_request(IpcRequest::RegisterSkill {
                        skill_name: skill_name.clone(),
                        description,
                        subagent_kind,
                        goal,
                        allowed_tools,
                        allowed_classes,
                        hook_subscriptions: vec![],
                        completion_route: Default::default(),
                        failure_route: Default::default(),
                        idle_behavior: Default::default(),
                        lease_terms: Default::default(),
                    })
                    .await;

                let (content, tool_err) = match response {
                    Ok(IpcResponse::SkillRegistered {
                        skill_name: name,
                        validation_state,
                        validation_errors,
                    }) => {
                        let msg = if validation_errors.is_empty() {
                            format!("Skill '{}' registered (state: {}).", name, validation_state)
                        } else {
                            format!(
                                "Skill '{}' registered with state '{}'. Validation issues:\n{}",
                                name,
                                validation_state,
                                validation_errors
                                    .iter()
                                    .map(|e| format!("- {e}"))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            )
                        };
                        (msg, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "IPC_ERROR", msg);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "skill.register: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("skill.register: IPC transport error — {e}"),
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

            "skill.list" => {
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::ListSkills {})
                    .await
                {
                    Ok(IpcResponse::SkillList { skills }) => {
                        let msg = if skills.is_empty() {
                            "No skills registered.".to_string()
                        } else {
                            let lines: Vec<String> = skills
                                .iter()
                                .map(|s| {
                                    let name =
                                        s.get("skill_name").and_then(|v| v.as_str()).unwrap_or("?");
                                    let state = s
                                        .get("validation_state")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("?");
                                    let desc =
                                        s.get("description").and_then(|v| v.as_str()).unwrap_or("");
                                    let brief: String = desc.chars().take(80).collect();
                                    format!("- {} [{}] — {}", name, state, brief)
                                })
                                .collect();
                            format!("Registered skills:\n{}", lines.join("\n"))
                        };
                        (msg, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "IPC_ERROR", msg);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "skill.list: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("skill.list: IPC transport error — {e}"),
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

            "skill.assign" | "skill.revoke" => {
                let args = &payload.arguments;
                let op = payload.tool_name.as_str();

                let role_name = match args.get("role_name").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                format!("{op}: missing required argument 'role_name'"),
                            )
                            .await;
                    }
                };
                let skill_name = match args.get("skill_name").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                format!("{op}: missing required argument 'skill_name'"),
                            )
                            .await;
                    }
                };

                let req = if op == "skill.assign" {
                    IpcRequest::AssignSkill {
                        agent_id: self.agent_id.clone(),
                        role_name: role_name.clone(),
                        skill_name: skill_name.clone(),
                    }
                } else {
                    IpcRequest::RevokeSkill {
                        agent_id: self.agent_id.clone(),
                        role_name: role_name.clone(),
                        skill_name: skill_name.clone(),
                    }
                };

                let (content, tool_err) = match self.ipc_client.send_request(req).await {
                    Ok(IpcResponse::SkillAssigned {
                        role_name: rn,
                        skill_name: sn,
                        operation,
                    }) => (format!("Skill '{}' {} role '{}'.", sn, operation, rn), None),
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "IPC_ERROR", msg);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            &format!("{op}: unexpected hotel response"),
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("{op}: IPC transport error — {e}"),
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

            "subagent.spawn" => {
                let args = &payload.arguments;

                let goal = match args.get("goal").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "subagent.spawn: missing required argument 'goal'".into(),
                            )
                            .await;
                    }
                };
                let subagent_kind = args
                    .get("subagent_kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("philote-worker")
                    .to_string();
                let context_summary = args
                    .get("context_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let allowed_tools: Vec<String> = args
                    .get("allowed_tools")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let iteration_budget = args
                    .get("iteration_budget")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32);

                let delegation = philotic_client::SubagentDelegation {
                    parent_agent_id: self.agent_id.clone(),
                    parent_role: "agent".to_string(),
                    subagent_kind,
                    goal,
                    context_packet: philotic_client::SubagentContextPacket {
                        summary: context_summary,
                        ..Default::default()
                    },
                    allowed_tools,
                    iteration_budget,
                    ..Default::default()
                };

                let response = self
                    .ipc_client
                    .send_request(IpcRequest::SpawnSubagent {
                        session_id: payload.session_id.clone(),
                        delegation,
                    })
                    .await;

                let (content, tool_err) = match response {
                    Ok(IpcResponse::SpawnSubagentOk {
                        subagent_guest_id,
                        confirmed_lease,
                    }) => (
                        format!(
                            "Subagent spawned.\nGuest ID: {}\nLease expires at: {} (epoch {})",
                            subagent_guest_id,
                            confirmed_lease.lease_expires_at,
                            confirmed_lease.lease_epoch,
                        ),
                        None,
                    ),
                    Ok(IpcResponse::SpawnSubagentProposal {
                        subagent_guest_id,
                        confirmed_lease,
                        delta,
                    }) => (
                        format!(
                            "Subagent spawned (TTL adjusted: {}s → {}s).\nGuest ID: {}\nLease expires at: {}",
                            delta.requested_ttl,
                            delta.confirmed_ttl,
                            subagent_guest_id,
                            confirmed_lease.lease_expires_at,
                        ),
                        None,
                    ),
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "IPC_ERROR", msg);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "subagent.spawn: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("subagent.spawn: IPC transport error — {e}"),
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

            "role.set_home" => {
                let args = payload.arguments.as_object();
                let role_name = args
                    .and_then(|a| a.get("role_name"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let target_hotel = args
                    .and_then(|a| a.get("target_hotel"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty() && s.to_lowercase() != "null")
                    .map(str::to_string);
                let reason = args
                    .and_then(|a| a.get("reason"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let Some(role_name) = role_name else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "role.set_home: missing required argument 'role_name'".into(),
                        )
                        .await;
                };
                let Some(reason) = reason else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "role.set_home: missing required argument 'reason'".into(),
                        )
                        .await;
                };

                // Resolve the calling role from active session state.
                let calling_role = self
                    .sessions
                    .get(&payload.session_id)
                    .and_then(|s| s.role_activation.as_ref())
                    .map(|r| r.role_name.clone())
                    .unwrap_or_else(|| "orchestrator".into());

                let _ = reason; // recorded for operator visibility in approval surface
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::SetRoleHome {
                        agent_id: self.agent_id.clone(),
                        role_name: role_name.clone(),
                        calling_role,
                        target_hotel: target_hotel.clone(),
                    })
                    .await
                {
                    Ok(IpcResponse::RoleHomeSet {
                        role_name: name,
                        home_node,
                    }) => {
                        let msg = match home_node {
                            Some(ref node) => format!(
                                "Role '{name}' pinned to hotel '{node}'. Next handoff.to_role will route there."
                            ),
                            None => {
                                format!("Role '{name}' home cleared — will run on authority hotel.")
                            }
                        };
                        (msg, None)
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::tool_execution(
                            "role.set_home",
                            msg,
                            Some("SET_ROLE_HOME_REJECTED"),
                        );
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "role.set_home: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("role.set_home: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                if let Some(err) = tool_err {
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
                        error: Some(err),
                        tool_name: Some(payload.tool_name),
                        arguments: None,
                        final_reply_to: Some(payload.final_reply_to),
                        final_reply_role: Some(payload.final_reply_role),
                        final_reply_guest_id: payload.final_reply_guest_id,
                        ..Default::default()
                    })
                    .await
                } else {
                    self.complete_local_command(payload.session_id, payload.turn_id, content)
                        .await
                }
            }

            "transport.set_home" => {
                let args = payload.arguments.as_object();
                let transport = args
                    .and_then(|a| a.get("transport"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let resource_ref = args
                    .and_then(|a| a.get("resource_ref"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let target_hotel = args
                    .and_then(|a| a.get("target_hotel"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let standby_hotels = args
                    .and_then(|a| a.get("standby_hotels"))
                    .and_then(|v| v.as_array())
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|value| value.as_str())
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let reason = args
                    .and_then(|a| a.get("reason"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let Some(transport) = transport else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "transport.set_home: missing required argument 'transport'".into(),
                        )
                        .await;
                };
                let Some(resource_ref) = resource_ref else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "transport.set_home: missing required argument 'resource_ref'".into(),
                        )
                        .await;
                };
                let Some(target_hotel) = target_hotel else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "transport.set_home: missing required argument 'target_hotel'".into(),
                        )
                        .await;
                };
                let Some(reason) = reason else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "transport.set_home: missing required argument 'reason'".into(),
                        )
                        .await;
                };

                let calling_role = self
                    .sessions
                    .get(&payload.session_id)
                    .and_then(|s| s.role_activation.as_ref())
                    .map(|r| r.role_name.clone())
                    .unwrap_or_else(|| "orchestrator".into());

                let _ = reason;
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::SetTransportHome {
                        agent_id: self.agent_id.clone(),
                        transport: transport.clone(),
                        resource_ref: resource_ref.clone(),
                        calling_role,
                        target_hotel: target_hotel.clone(),
                        standby_hotels,
                    })
                    .await
                {
                    Ok(IpcResponse::TransportHomeSet {
                        transport,
                        resource_ref,
                        active_home_hotel,
                        standby_hotels,
                        ..
                    }) => {
                        let standby = if standby_hotels.is_empty() {
                            "no standby hotels".to_string()
                        } else {
                            format!("standby hotels: {}", standby_hotels.join(", "))
                        };
                        (
                            format!(
                                "Transport '{transport}' resource '{resource_ref}' now homes on hotel '{active_home_hotel}' ({standby})."
                            ),
                            None,
                        )
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::tool_execution(
                            "transport.set_home",
                            msg,
                            Some("SET_TRANSPORT_HOME_REJECTED"),
                        );
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "transport.set_home: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("transport.set_home: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                if let Some(err) = tool_err {
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
                        error: Some(err),
                        tool_name: Some(payload.tool_name),
                        arguments: None,
                        final_reply_to: Some(payload.final_reply_to),
                        final_reply_role: Some(payload.final_reply_role),
                        final_reply_guest_id: payload.final_reply_guest_id,
                        ..Default::default()
                    })
                    .await
                } else {
                    self.complete_local_command(payload.session_id, payload.turn_id, content)
                        .await
                }
            }

            "role.list" => {
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::ListRoleIncarnations {
                        agent_id: self.agent_id.clone(),
                    })
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let roles = data
                            .get("roles")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default();
                        if roles.is_empty() {
                            ("No roles configured for this agent.".into(), None)
                        } else {
                            let mut lines = vec![format!(
                                "Role roster for {} ({} roles):",
                                self.agent_id,
                                roles.len()
                            )];
                            for role in &roles {
                                let name = role
                                    .get("role_name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("?");
                                let profile = role
                                    .get("toolset_profile")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("?");
                                let state = role
                                    .get("readiness_state")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown");
                                let home = role
                                    .get("home_node")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("(authority hotel)");
                                lines.push(format!(
                                    "  {name}  profile={profile}  state={state}  home={home}"
                                ));
                            }
                            (lines.join("\n"), None)
                        }
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("Role list unavailable.".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("role.list: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "training.list" => {
                use ansible_mesh_core::whisper_training::TrainingFilter;
                use philotic_client::IpcRequest;
                let args = payload.arguments.as_object();
                let limit = args
                    .and_then(|a| a.get("limit"))
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or(20);
                let filter_str = args
                    .and_then(|a| a.get("filter"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("all");
                let filter = match filter_str {
                    "uncorrected" => TrainingFilter::Uncorrected,
                    "eligible" => TrainingFilter::Eligible,
                    "exported" => TrainingFilter::Exported,
                    _ => TrainingFilter::All,
                };
                let agent_id_filter = args
                    .and_then(|a| a.get("agent_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::ListTrainingSamples {
                        agent_id: agent_id_filter,
                        limit,
                        filter,
                    })
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let count = data.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
                        let samples = data
                            .get("samples")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default();
                        if samples.is_empty() {
                            (
                                format!("No training samples found (filter: {filter_str})."),
                                None,
                            )
                        } else {
                            let lines: Vec<String> = std::iter::once(format!(
                                "{count} training sample(s) (filter: {filter_str}):"
                            ))
                            .chain(samples.iter().map(|s| {
                                let sid =
                                    s.get("sample_id").and_then(|v| v.as_str()).unwrap_or("?");
                                let turn = s.get("turn_id").and_then(|v| v.as_str()).unwrap_or("?");
                                let raw = s
                                    .get("raw_transcript")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let corrected =
                                    s.get("corrected_transcript").and_then(|v| v.as_str());
                                let eligible = s
                                    .get("training_eligible")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                let exported =
                                    s.get("exported_at").and_then(|v| v.as_u64()).is_some();
                                let state = if exported {
                                    "exported"
                                } else if eligible {
                                    "eligible"
                                } else if corrected.is_some() {
                                    "corrected"
                                } else {
                                    "uncorrected"
                                };
                                let transcript = corrected.unwrap_or(raw);
                                format!(
                                    "  [{state}] {sid}  turn={turn}  transcript={transcript:.80}"
                                )
                            }))
                            .collect();
                            (lines.join("\n"), None)
                        }
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("training.list: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("training.list: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "training.correct" => {
                use philotic_client::IpcRequest;
                let args = payload.arguments.as_object();
                let turn_id = args
                    .and_then(|a| a.get("turn_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let corrected_transcript = args
                    .and_then(|a| a.get("corrected_transcript"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let (Some(turn_id), Some(corrected_transcript)) = (turn_id, corrected_transcript)
                else {
                    let err = TaskErrorPayload::ipc_failure(
                        "philote",
                        "MISSING_ARGS",
                        "training.correct requires 'turn_id' and 'corrected_transcript'",
                    );
                    return self
                        .handle_tool_result(InboundTaskPayload {
                            action: Some("tool_result".into()),
                            source: Some("agent".into()),
                            session_id: Some(payload.session_id),
                            turn_id: Some(payload.turn_id),
                            chat_id: Some(payload.chat_id),
                            content: Some(err.display_message()),
                            error: Some(err),
                            tool_name: Some(payload.tool_name),
                            final_reply_to: Some(payload.final_reply_to),
                            final_reply_role: Some(payload.final_reply_role),
                            final_reply_guest_id: payload.final_reply_guest_id,
                            ..Default::default()
                        })
                        .await;
                };
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::CorrectTrainingSample {
                        turn_id: turn_id.clone(),
                        corrected_transcript,
                    })
                    .await
                {
                    Ok(IpcResponse::Standard { ok: true, .. }) => (
                        format!(
                            "Correction applied to turn '{turn_id}'. Sample marked training_eligible."
                        ),
                        None,
                    ),
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("training.correct: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("training.correct: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "training.export" => {
                use ansible_mesh_core::whisper_training::TrainingExportFormat;
                use philotic_client::IpcRequest;
                let args = payload.arguments.as_object();
                let format_str = args
                    .and_then(|a| a.get("format"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("huggingface");
                let output_path = args
                    .and_then(|a| a.get("output_path"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let limit = args
                    .and_then(|a| a.get("limit"))
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);
                let Some(output_path) = output_path else {
                    let err = TaskErrorPayload::ipc_failure(
                        "philote",
                        "MISSING_ARGS",
                        "training.export requires 'format' and 'output_path'",
                    );
                    return self
                        .handle_tool_result(InboundTaskPayload {
                            action: Some("tool_result".into()),
                            source: Some("agent".into()),
                            session_id: Some(payload.session_id),
                            turn_id: Some(payload.turn_id),
                            chat_id: Some(payload.chat_id),
                            content: Some(err.display_message()),
                            error: Some(err),
                            tool_name: Some(payload.tool_name),
                            final_reply_to: Some(payload.final_reply_to),
                            final_reply_role: Some(payload.final_reply_role),
                            final_reply_guest_id: payload.final_reply_guest_id,
                            ..Default::default()
                        })
                        .await;
                };
                let format = if format_str == "nemo" {
                    TrainingExportFormat::Nemo
                } else {
                    TrainingExportFormat::HuggingFace
                };
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::ExportTrainingSamples {
                        format,
                        output_path: output_path.clone(),
                        limit,
                    })
                    .await
                {
                    Ok(IpcResponse::Standard { ok: true, data, .. }) => {
                        let count = data
                            .as_ref()
                            .and_then(|d| d.get("exported_count"))
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        (
                            format!(
                                "Exported {count} sample(s) ({format_str} format) → {output_path}"
                            ),
                            None,
                        )
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("training.export: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("training.export: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "training.status" => {
                use philotic_client::IpcRequest;
                let args = payload.arguments.as_object();
                let agent_id_filter = args
                    .and_then(|a| a.get("agent_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::GetTrainingStatus {
                        agent_id: agent_id_filter,
                    })
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let status = data.get("status").cloned().unwrap_or_default();
                        let total = status.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
                        let uncorrected = status
                            .get("uncorrected")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let eligible = status.get("eligible").and_then(|v| v.as_u64()).unwrap_or(0);
                        let exported = status.get("exported").and_then(|v| v.as_u64()).unwrap_or(0);
                        let content = format!(
                            "Training data status:\n  total captured: {total}\n  uncorrected: {uncorrected}\n  eligible for export: {eligible}\n  exported: {exported}"
                        );
                        (content, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("training.status: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("training.status: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "asr.setup" => {
                use philotic_client::IpcRequest;
                let args = payload.arguments.as_object();
                let python_path = args
                    .and_then(|a| a.get("python_path"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let model_name = args
                    .and_then(|a| a.get("model_name"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let auto_install = args
                    .and_then(|a| a.get("auto_install"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::AsrSetup {
                        python_path,
                        model_name,
                        auto_install,
                    })
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let msg = data
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("ASR provider configured.")
                            .to_string();
                        (msg, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("asr.setup: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("asr.setup: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "asr.status" => {
                use philotic_client::IpcRequest;
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::AsrStatus {})
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let registered = data
                            .get("registered")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let active = data
                            .get("active")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let pid = data.get("pid").and_then(|v| v.as_str()).unwrap_or("none");
                        let nemo = data
                            .get("nemo_available")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let guest_id = data.get("guest_id").and_then(|v| v.as_str()).unwrap_or("?");
                        let content = format!(
                            "Parakeet ASR status:\n  guest: {guest_id}\n  registered: {registered}\n  active: {active}\n  pid: {pid}\n  nemo_available: {nemo}"
                        );
                        (content, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("asr.status: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("asr.status: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "vision.setup" => {
                use philotic_client::IpcRequest;
                let repo_id = payload
                    .arguments
                    .get("repo_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::VisionSetup { repo_id })
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let msg = data
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Vision provider configured.")
                            .to_string();
                        (msg, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("vision.setup: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("vision.setup: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "vision.status" => {
                use philotic_client::IpcRequest;
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::VisionStatus {})
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let registered = data
                            .get("registered")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let active = data
                            .get("active")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let pid = data.get("pid").and_then(|v| v.as_str()).unwrap_or("none");
                        let libs = data
                            .get("libs_available")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let guest_id = data.get("guest_id").and_then(|v| v.as_str()).unwrap_or("?");
                        let profile_status = data
                            .get("model_profile_status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("not registered");
                        let content = format!(
                            "Vision provider status:\n  guest: {guest_id}\n  registered: {registered}\n  active: {active}\n  pid: {pid}\n  libs_available: {libs}\n  model_profile: {profile_status}"
                        );
                        (content, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("vision.status: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("vision.status: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "cron.list" => {
                use philotic_client::IpcRequest;
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::ListCronJobs)
                    .await
                {
                    Ok(IpcResponse::CronJobList { jobs }) => {
                        if jobs.is_empty() {
                            ("No cron jobs registered on this hotel.".into(), None)
                        } else {
                            let lines: Vec<String> = jobs
                                .iter()
                                .map(|j| {
                                    format!(
                                        "- id={} role={} schedule={} enabled={} next_fire={}",
                                        j.id, j.target_role, j.schedule, j.enabled, j.next_fire_at,
                                    )
                                })
                                .collect();
                            (
                                format!("Cron jobs ({}):\n{}", jobs.len(), lines.join("\n")),
                                None,
                            )
                        }
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("cron.list: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("cron.list: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "cron.register" => {
                use ansible_mesh_core::cron::{CronJob, CronJobSource};
                use philotic_client::IpcRequest;
                let args = &payload.arguments;
                let schedule = match args.get("schedule").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "cron.register: missing required argument 'schedule'".into(),
                            )
                            .await;
                    }
                };
                let target_role = match args.get("target_role").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "cron.register: missing required argument 'target_role'".into(),
                            )
                            .await;
                    }
                };
                let payload_str = match args.get("payload").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "cron.register: missing required argument 'payload'".into(),
                            )
                            .await;
                    }
                };
                let guaranteed = args
                    .get("guaranteed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let silent_ok = args
                    .get("silent_ok")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);

                let next_fire = match ansible_mesh_core::cron::next_fire_after(&schedule, now_ms) {
                    Ok(t) => t,
                    Err(e) => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                format!("cron.register: invalid schedule — {e}"),
                            )
                            .await;
                    }
                };

                let job = CronJob {
                    id: uuid::Uuid::new_v4().to_string(),
                    schedule,
                    target_role,
                    target_node_id: None,
                    payload: payload_str,
                    guaranteed,
                    enabled: true,
                    last_fired_epoch: None,
                    next_fire_at: next_fire,
                    created_at: now_ms,
                    created_by: CronJobSource::Guest(self.agent_id.clone()),
                    silent_ok,
                    // Newly registered jobs always want isolated cron sessions;
                    // the `RegisterCronJob` IPC handler re-asserts this
                    // regardless, but setting it here too keeps the
                    // constructed value honest.
                    session_target: ansible_mesh_core::cron::CronSessionTarget::Isolated,
                };
                let job_id = job.id.clone();

                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::RegisterCronJob { job })
                    .await
                {
                    Ok(IpcResponse::Standard { ok: true, .. }) => {
                        (format!("Cron job registered. id={job_id}"), None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("cron.register: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("cron.register: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "cron.enable" => {
                use philotic_client::IpcRequest;
                let job_id = match payload.arguments.get("job_id").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "cron.enable: missing required argument 'job_id'".into(),
                            )
                            .await;
                    }
                };
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::EnableCronJob {
                        job_id: job_id.clone(),
                    })
                    .await
                {
                    Ok(IpcResponse::Standard { ok: true, .. }) => {
                        (format!("Cron job {job_id} enabled."), None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("cron.enable: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("cron.enable: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "cron.disable" => {
                use philotic_client::IpcRequest;
                let job_id = match payload.arguments.get("job_id").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "cron.disable: missing required argument 'job_id'".into(),
                            )
                            .await;
                    }
                };
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::DisableCronJob {
                        job_id: job_id.clone(),
                    })
                    .await
                {
                    Ok(IpcResponse::Standard { ok: true, .. }) => {
                        (format!("Cron job {job_id} disabled."), None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("cron.disable: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("cron.disable: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "cron.remove" => {
                use philotic_client::IpcRequest;
                let job_id = match payload.arguments.get("job_id").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "cron.remove: missing required argument 'job_id'".into(),
                            )
                            .await;
                    }
                };
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::RemoveCronJob {
                        job_id: job_id.clone(),
                    })
                    .await
                {
                    Ok(IpcResponse::Standard { ok: true, .. }) => {
                        (format!("Cron job {job_id} removed."), None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("cron.remove: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("cron.remove: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "handoff.to_role" => {
                let args = payload.arguments.as_object();
                let role_name = args
                    .and_then(|a| a.get("role_name"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let reason = args
                    .and_then(|a| a.get("reason"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let Some(role_name) = role_name else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "handoff.to_role: missing required argument 'role_name'".into(),
                        )
                        .await;
                };
                let Some(reason) = reason else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "handoff.to_role: missing required argument 'reason'".into(),
                        )
                        .await;
                };

                let active_goal = args
                    .and_then(|a| a.get("active_goal"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                // Succinctness budget: handoff excerpts orient the target role;
                // the durable context stays in the session checkpoint.
                let context_summary = truncate_for_wire(
                    args.and_then(|a| a.get("context_summary"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                    HANDOFF_CONTEXT_EXCERPT_MAX_CHARS,
                );

                let target_focus_framing = args
                    .and_then(|a| a.get("target_focus_framing"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let Some(target_focus_framing) = target_focus_framing else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "handoff.to_role: missing required argument 'target_focus_framing'"
                                .into(),
                        )
                        .await;
                };

                let active_goal = active_goal
                    .map(|g| format!("{}\n\nTarget Focus Framing:\n{}", g, target_focus_framing));

                let expected_return_mode = args
                    .and_then(|a| a.get("expected_return_mode"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let cleanup_actions: Vec<String> = args
                    .and_then(|a| a.get("cleanup_actions"))
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();

                let from_role = self
                    .sessions
                    .get(&payload.session_id)
                    .and_then(|s| s.role_activation.as_ref())
                    .map(|r| r.role_name.clone())
                    .or_else(|| Some("orchestrator".into()));

                let handoff_bundle = HandoffBundle {
                    goal: active_goal.clone().unwrap_or_else(|| reason.clone()),
                    context_excerpt: context_summary,
                    session_id: payload.session_id.clone(),
                    initiating_turn_id: payload.turn_id.clone(),
                    handoff_reason: Some(reason),
                    from_role,
                    to_role: Some(role_name.clone()),
                    active_goal,
                    expected_return_mode,
                    cleanup_actions,
                    ..Default::default()
                };

                // Retry HandoffPending up to ~3 seconds while the target role materializes.
                const HANDOFF_MAX_RETRIES: u32 = 12;
                const HANDOFF_DEFAULT_WAIT_MS: u64 = 250;
                let handoff_req = IpcRequest::HandoffToRole {
                    session_id: payload.session_id.clone(),
                    role_name: role_name.clone(),
                    handoff_bundle,
                };
                let mut handoff_attempt = 0u32;
                let (content, tool_err) = loop {
                    let resp = self.ipc_client.send_request(handoff_req.clone()).await;
                    match resp {
                        Ok(IpcResponse::HandoffAck {
                            handoff_guest_id, ..
                        }) => {
                            break (
                                format!(
                                    "Handed off to role '{role_name}' (guest {handoff_guest_id})."
                                ),
                                None,
                            );
                        }
                        Ok(IpcResponse::HandoffPending { retry_after_ms, .. }) => {
                            handoff_attempt += 1;
                            if handoff_attempt >= HANDOFF_MAX_RETRIES {
                                let e = TaskErrorPayload::tool_execution(
                                    "handoff.to_role",
                                    format!(
                                        "Role '{role_name}' did not become live after {HANDOFF_MAX_RETRIES} retries"
                                    ),
                                    Some("HANDOFF_TIMEOUT"),
                                );
                                break (e.display_message(), Some(e));
                            }
                            let wait_ms = retry_after_ms.unwrap_or(HANDOFF_DEFAULT_WAIT_MS);
                            tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
                        }
                        Ok(IpcResponse::Error(msg)) => {
                            let e = TaskErrorPayload::tool_execution(
                                "handoff.to_role",
                                msg,
                                Some("HANDOFF_REJECTED"),
                            );
                            break (e.display_message(), Some(e));
                        }
                        Ok(_) => {
                            let e = TaskErrorPayload::ipc_failure(
                                "aiua",
                                "UNEXPECTED_RESPONSE",
                                "handoff.to_role: unexpected hotel response",
                            );
                            break (e.display_message(), Some(e));
                        }
                        Err(e) => {
                            let err = TaskErrorPayload::transport_error(
                                "philote",
                                format!("handoff.to_role: IPC transport error — {e}"),
                            );
                            break (err.display_message(), Some(err));
                        }
                    }
                };

                if let Some(err) = tool_err {
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
                        error: Some(err),
                        tool_name: Some(payload.tool_name),
                        arguments: None,
                        final_reply_to: Some(payload.final_reply_to),
                        final_reply_role: Some(payload.final_reply_role),
                        final_reply_guest_id: payload.final_reply_guest_id,
                        ..Default::default()
                    })
                    .await
                } else {
                    self.complete_local_command(payload.session_id, payload.turn_id, content)
                        .await
                }
            }

            "handoff.back" => {
                let args = payload.arguments.as_object();
                let summary = args
                    .and_then(|a| a.get("summary"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let Some(summary) = summary else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "handoff.back: missing required argument 'summary'".into(),
                        )
                        .await;
                };

                let return_to = args
                    .and_then(|a| a.get("return_to"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                const HANDOFF_BACK_MAX_RETRIES: u32 = 12;
                const HANDOFF_BACK_DEFAULT_WAIT_MS: u64 = 250;
                let handoff_back_req = IpcRequest::HandoffBack {
                    session_id: payload.session_id.clone(),
                    summary: summary.clone(),
                    return_to,
                };
                let mut handoff_back_attempt = 0u32;
                let (content, tool_err) = loop {
                    match self.ipc_client.send_request(handoff_back_req.clone()).await {
                        Ok(IpcResponse::HandoffBackAck {
                            return_guest_id, ..
                        }) => {
                            break (
                                format!(
                                    "Returned control (to guest {return_guest_id}). Summary: {summary}"
                                ),
                                None,
                            );
                        }
                        Ok(IpcResponse::HandoffPending { retry_after_ms, .. }) => {
                            handoff_back_attempt += 1;
                            if handoff_back_attempt >= HANDOFF_BACK_MAX_RETRIES {
                                let e = TaskErrorPayload::tool_execution(
                                    "handoff.back",
                                    format!(
                                        "Return role did not become live after {HANDOFF_BACK_MAX_RETRIES} retries"
                                    ),
                                    Some("HANDOFF_BACK_TIMEOUT"),
                                );
                                break (e.display_message(), Some(e));
                            }
                            let wait_ms = retry_after_ms.unwrap_or(HANDOFF_BACK_DEFAULT_WAIT_MS);
                            tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
                        }
                        Ok(IpcResponse::Error(msg)) => {
                            let e = TaskErrorPayload::tool_execution(
                                "handoff.back",
                                msg,
                                Some("HANDOFF_BACK_REJECTED"),
                            );
                            break (e.display_message(), Some(e));
                        }
                        Ok(_) => {
                            let e = TaskErrorPayload::ipc_failure(
                                "aiua",
                                "UNEXPECTED_RESPONSE",
                                "handoff.back: unexpected hotel response",
                            );
                            break (e.display_message(), Some(e));
                        }
                        Err(e) => {
                            let err = TaskErrorPayload::transport_error(
                                "philote",
                                format!("handoff.back: IPC transport error — {e}"),
                            );
                            break (err.display_message(), Some(err));
                        }
                    }
                };

                if let Some(err) = tool_err {
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
                        error: Some(err),
                        tool_name: Some(payload.tool_name),
                        arguments: None,
                        final_reply_to: Some(payload.final_reply_to),
                        final_reply_role: Some(payload.final_reply_role),
                        final_reply_guest_id: payload.final_reply_guest_id,
                        ..Default::default()
                    })
                    .await
                } else {
                    self.complete_local_command(payload.session_id, payload.turn_id, content)
                        .await
                }
            }

            "delegate.to_peer" => {
                let args = payload.arguments.as_object();
                let target_agent_id = args
                    .and_then(|a| a.get("target_agent_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let task_description = args
                    .and_then(|a| a.get("task_description"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let context_package = args
                    .and_then(|a| a.get("context_package"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let Some(target_agent_id) = target_agent_id else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "delegate.to_peer: missing required argument 'target_agent_id'".into(),
                        )
                        .await;
                };
                let Some(task_description) = task_description else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "delegate.to_peer: missing required argument 'task_description'".into(),
                        )
                        .await;
                };
                let Some(context_package) = context_package else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "delegate.to_peer: missing required argument 'context_package'".into(),
                        )
                        .await;
                };

                let expected_artifacts: Vec<String> = args
                    .and_then(|a| a.get("expected_artifacts"))
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let timeout_secs = args
                    .and_then(|a| a.get("timeout_secs"))
                    .and_then(|v| v.as_u64());

                let _ = self
                    .emit_partial_reply(
                        &payload.session_id,
                        format!(
                            "Let me hand you over to {} to help with this...",
                            target_agent_id
                        ),
                    )
                    .await;

                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::DelegateToPeer {
                        target_agent_id: target_agent_id.clone(),
                        task_description,
                        context_package,
                        chat_id: payload.chat_id.clone(),
                        source: Some("peer".into()),
                        expected_artifacts,
                        timeout_secs,
                    })
                    .await
                {
                    Ok(IpcResponse::DelegationAck {
                        delegation_id,
                        status,
                    }) => (
                        format!(
                            "Delegated task to peer '{target_agent_id}' (delegation {delegation_id}, status: {status})."
                        ),
                        None,
                    ),
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::tool_execution(
                            "delegate.to_peer",
                            msg,
                            Some("DELEGATION_REJECTED"),
                        );
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "delegate.to_peer: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("delegate.to_peer: IPC transport error — {e}"),
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

            "delegate.to_external_cognitive_peer" => {
                let args = payload.arguments.as_object();
                let target_peer_type = args
                    .and_then(|a| a.get("target_peer_type"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let task_description = args
                    .and_then(|a| a.get("task_description"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let context_package = args
                    .and_then(|a| a.get("context_package"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let Some(target_peer_type) = target_peer_type else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "delegate.to_external_cognitive_peer: missing required argument 'target_peer_type'".into(),
                        )
                        .await;
                };
                let Some(task_description) = task_description else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "delegate.to_external_cognitive_peer: missing required argument 'task_description'".into(),
                        )
                        .await;
                };
                let Some(context_package) = context_package else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "delegate.to_external_cognitive_peer: missing required argument 'context_package'".into(),
                        )
                        .await;
                };

                let expected_artifacts: Vec<String> = args
                    .and_then(|a| a.get("expected_artifacts"))
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();

                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::DelegateToExternalPeer {
                        target_peer_type: target_peer_type.clone(),
                        task_description,
                        context_package,
                        expected_artifacts,
                    })
                    .await
                {
                    Ok(IpcResponse::DelegationAck {
                        delegation_id,
                        status,
                    }) => (
                        format!(
                            "Delegated task to external peer type '{target_peer_type}' (delegation {delegation_id}, status: {status})."
                        ),
                        None,
                    ),
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::tool_execution(
                            "delegate.to_external_cognitive_peer",
                            msg,
                            Some("DELEGATION_REJECTED"),
                        );
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "delegate.to_external_cognitive_peer: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!(
                                "delegate.to_external_cognitive_peer: IPC transport error — {e}"
                            ),
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

            "bash.exec" => {
                let args = &payload.arguments;

                let command = match args.get("command").and_then(|v| v.as_str()) {
                    Some(c) => c.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "bash.exec: missing required argument 'command'".into(),
                            )
                            .await;
                    }
                };

                let working_dir = args
                    .get("working_dir")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| {
                        // Fall back to the agent session's import_workspace path if set.
                        self.sessions
                            .get(&payload.session_id)
                            .and_then(|s| s.agent_profile.import_workspace.as_deref())
                            .map(str::to_string)
                    });

                let timeout_secs = args
                    .get("timeout_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(30);

                let exec_result = self
                    .execute_bash_command(command, working_dir, timeout_secs)
                    .await;

                let (content, tool_err) = match exec_result {
                    Ok(json) => (json.to_string(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::tool_execution(
                            "bash.exec",
                            e.to_string(),
                            Some("EXEC_ERROR"),
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

            "memory.recall" => self.execute_memory_recall_tool(payload).await,

            "memory.remember" => self.execute_memory_remember_tool(payload).await,

            "memory.cultivate" => self.execute_memory_cultivate_tool(payload).await,

            "memory.true_up" => self.execute_memory_true_up_tool(payload).await,

            "memory.promote_candidate" => self.execute_memory_promote_candidate_tool(payload).await,

            "memory.status" => self.execute_memory_status_tool(payload).await,

            "memory.fix" => self.execute_memory_fix_tool(payload).await,

            "rule.propose" => {
                let description = payload
                    .arguments
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let rationale = payload
                    .arguments
                    .get("rationale")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if description.is_empty() {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "rule.propose: 'description' is required.".into(),
                        )
                        .await;
                }

                let agent_id = self.agent_id.clone();
                let result_text = match self
                    .ipc_client
                    .send_request(IpcRequest::ProposeRule {
                        agent_id: agent_id.clone(),
                        description: description.clone(),
                        rationale: rationale.clone(),
                    })
                    .await
                {
                    Ok(IpcResponse::RuleProposed { rule_id }) => {
                        // Optimistically push the new rule into session state so it is visible
                        // in the next turn's context without requiring a restart.
                        if let Some(state) = self.sessions.get_mut(&payload.session_id) {
                            state.rules.push(serde_json::json!({
                                "rule_id": rule_id,
                                "description": description,
                                "rationale": rationale,
                            }));
                        }
                        format!(
                            "Rule stored permanently (id: {rule_id}). It will be injected into every future cognitive turn."
                        )
                    }
                    Ok(IpcResponse::Standard {
                        ok: true, message, ..
                    }) => message,
                    Ok(IpcResponse::Standard {
                        ok: false, message, ..
                    }) => {
                        format!("rule.propose: hotel rejected — {message}")
                    }
                    Ok(_) => "rule.propose: unexpected response from hotel.".into(),
                    Err(e) => format!("rule.propose: IPC error — {e}"),
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
                    content: Some(result_text),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: None,
                    tool_name: Some(payload.tool_name),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "delegate.whisper" => self.execute_delegate_whisper_tool(payload).await,

            // ── delegate.merge ── implementation in paracrine.rs ────────────
            "delegate.merge" => self.execute_delegate_merge_tool(payload).await,

            "approval.request_standing" => {
                let session_id = payload.session_id.clone();
                let turn_id = payload.turn_id.clone();
                let tool_name = payload
                    .arguments
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let required_successes = payload
                    .arguments
                    .get("required_successes")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(3) as u32;

                if tool_name.is_empty() {
                    return self
                        .fail_active_turn(
                            session_id,
                            turn_id,
                            "approval.request_standing: missing required argument 'tool_name'"
                                .into(),
                        )
                        .await;
                }

                let content = if let Some(state) = self.sessions.get_mut(&session_id) {
                    state.register_standing_preapproval(&tool_name, required_successes);
                    let current_streak = *state.tool_success_streak.get(&tool_name).unwrap_or(&0);
                    if state.approval_policy.preapproved_tools.contains(&tool_name) {
                        format!(
                            "Standing approval granted immediately for '{}' — \
                             current streak ({}) already meets the threshold ({}).",
                            tool_name, current_streak, required_successes
                        )
                    } else {
                        format!(
                            "Standing approval registered for '{}'. \
                             It will be auto-granted after {} successive successes \
                             (current streak: {}).",
                            tool_name, required_successes, current_streak
                        )
                    }
                } else {
                    "Session not found.".into()
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(session_id),
                    turn_id: Some(turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    tool_name: Some("approval.request_standing".into()),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── mcp.provision ────────────────────────────────────────────────
            "mcp.provision" => {
                let session_id = payload.session_id.clone();
                let turn_id = payload.turn_id.clone();
                let args = &payload.arguments;

                let endpoint_id = match args.get("endpoint_id").and_then(|v| v.as_str()) {
                    Some(s) if !s.trim().is_empty() => s.to_string(),
                    _ => {
                        return self
                            .fail_active_turn(
                                session_id,
                                turn_id,
                                "mcp.provision: missing required argument 'endpoint_id'".into(),
                            )
                            .await;
                    }
                };
                let port = match args.get("port").and_then(|v| v.as_u64()) {
                    Some(p) if p > 0 && p < 65536 => p as u16,
                    _ => {
                        return self
                            .fail_active_turn(
                                session_id,
                                turn_id,
                                "mcp.provision: missing or invalid 'port' (must be 1–65535)".into(),
                            )
                            .await;
                    }
                };

                let tools_raw = args.get("tools").cloned().unwrap_or(serde_json::json!([]));
                let tools: Vec<ansible_mesh_core::mcp_endpoint::McpToolSpec> =
                    match serde_json::from_value(tools_raw) {
                        Ok(t) => t,
                        Err(e) => {
                            return self
                                .fail_active_turn(
                                    session_id,
                                    turn_id,
                                    format!("mcp.provision: invalid 'tools' shape — {e}"),
                                )
                                .await;
                        }
                    };

                let preapproval_rules: Vec<ansible_mesh_core::mcp_endpoint::McpPreapprovalRule> =
                    args.get("preapproval_rules")
                        .and_then(|v| {
                            serde_json::from_value::<
                                Vec<ansible_mesh_core::mcp_endpoint::McpPreapprovalRule>,
                            >(v.clone())
                            .ok()
                        })
                        .unwrap_or_default()
                        .into_iter()
                        .map(
                            |mut rule: ansible_mesh_core::mcp_endpoint::McpPreapprovalRule| {
                                rule.approved_by_turn = turn_id.clone();
                                rule.approved_at = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0);
                                rule
                            },
                        )
                        .collect();

                let updated_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                let exposure = args
                    .get("exposure")
                    .and_then(|v| {
                        serde_json::from_value::<ansible_mesh_core::ExposureTier>(v.clone()).ok()
                    })
                    .unwrap_or(ansible_mesh_core::ExposureTier::Local);

                let config = ansible_mesh_core::mcp_endpoint::McpEndpointConfig {
                    endpoint_id: endpoint_id.clone(),
                    owner_agent_id: self.agent_id.clone(),
                    port,
                    path: None,
                    exposure,
                    tools,
                    preapproval_rules,
                    updated_at,
                };

                let response = self
                    .ipc_client
                    .send_request(IpcRequest::ProvisionMcpEndpoint { config })
                    .await;

                let (content, tool_err) = match response {
                    Ok(IpcResponse::McpEndpointProvisioned {
                        endpoint_id: ref eid,
                        port: p,
                        materialized,
                    }) => {
                        let status = if materialized {
                            "spawned a new membrane-mcp guest"
                        } else {
                            "updated config on existing membrane-mcp guest"
                        };
                        (
                            format!(
                                "MCP endpoint provisioned.\n\
                                 Endpoint ID: {eid}\n\
                                 Port: {p}\n\
                                 Status: {status}\n\
                                 Pre-approval rules for this endpoint are now active."
                            ),
                            None,
                        )
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e =
                            philotic_client::TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = philotic_client::TaskErrorPayload::ipc_failure(
                            "aiua",
                            "IPC_ERROR",
                            msg,
                        );
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = philotic_client::TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "mcp.provision: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = philotic_client::TaskErrorPayload::transport_error(
                            "philote",
                            format!("mcp.provision: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

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
                    content: Some(content),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: tool_err,
                    tool_name: Some("mcp.provision".into()),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── mcp.revoke ───────────────────────────────────────────────────
            "mcp.revoke" => {
                let session_id = payload.session_id.clone();
                let turn_id = payload.turn_id.clone();

                let endpoint_id = match payload
                    .arguments
                    .get("endpoint_id")
                    .and_then(|v| v.as_str())
                {
                    Some(s) if !s.trim().is_empty() => s.to_string(),
                    _ => {
                        return self
                            .fail_active_turn(
                                session_id,
                                turn_id,
                                "mcp.revoke: missing required argument 'endpoint_id'".into(),
                            )
                            .await;
                    }
                };

                let response = self
                    .ipc_client
                    .send_request(IpcRequest::RevokeMcpEndpoint {
                        endpoint_id: endpoint_id.clone(),
                        owner_agent_id: self.agent_id.clone(),
                    })
                    .await;

                let (content, tool_err) = match response {
                    Ok(IpcResponse::McpEndpointProvisioned {
                        endpoint_id: ref eid,
                        ..
                    }) => (
                        format!(
                            "MCP endpoint '{eid}' revoked. The membrane-mcp guest has been signalled to shut down."
                        ),
                        None,
                    ),
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e =
                            philotic_client::TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = philotic_client::TaskErrorPayload::ipc_failure(
                            "aiua",
                            "IPC_ERROR",
                            msg,
                        );
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = philotic_client::TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "mcp.revoke: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = philotic_client::TaskErrorPayload::transport_error(
                            "philote",
                            format!("mcp.revoke: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

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
                    content: Some(content),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: tool_err,
                    tool_name: Some("mcp.revoke".into()),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── mcp.status ───────────────────────────────────────────────────
            "mcp.status" => {
                let endpoint_id = match payload
                    .arguments
                    .get("endpoint_id")
                    .and_then(|v| v.as_str())
                {
                    Some(s) if !s.trim().is_empty() => s.to_string(),
                    _ => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "mcp.status: missing required argument 'endpoint_id'".into(),
                            )
                            .await;
                    }
                };
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::GetMcpEndpointStatus {
                        endpoint_id: endpoint_id.clone(),
                    })
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let text = serde_json::to_string_pretty(&data)
                            .unwrap_or_else(|_| data.to_string());
                        (text, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("MCP endpoint status unavailable.".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("mcp.status: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── table.add_listener ───────────────────────────────────────────
            "table.add_listener" => {
                let args = payload.arguments.as_object();

                let event_kind = match args
                    .and_then(|a| a.get("event_kind"))
                    .and_then(|v| v.as_str())
                {
                    Some(s) if !s.is_empty() => s.to_string(),
                    _ => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "table.add_listener: missing required argument 'event_kind'".into(),
                            )
                            .await;
                    }
                };

                let table_name = match args
                    .and_then(|a| a.get("table_name"))
                    .and_then(|v| v.as_str())
                {
                    Some(s) if !s.is_empty() => s.to_string(),
                    _ => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "table.add_listener: missing required argument 'table_name'".into(),
                            )
                            .await;
                    }
                };

                let schema_map = args
                    .and_then(|a| a.get("schema_map"))
                    .and_then(|v| v.as_object())
                    .cloned();
                let filter_keys = args
                    .and_then(|a| a.get("filter_keys"))
                    .and_then(|v| v.as_object())
                    .cloned();
                let adapter_script = args
                    .and_then(|a| a.get("adapter_script"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let target_role = args
                    .and_then(|a| a.get("target_role"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("table-datasource")
                    .to_string();

                // Read current listener config.
                let mut config: serde_json::Value = match self
                    .ipc_client
                    .send_request(IpcRequest::GetConfig {
                        key: "router_listener.config".into(),
                    })
                    .await
                {
                    Ok(IpcResponse::ConfigData {
                        value_json: Some(raw),
                        ..
                    }) => serde_json::from_str(&raw).unwrap_or(serde_json::json!({
                        "filter_keys": {},
                        "event_kinds": {}
                    })),
                    _ => serde_json::json!({ "filter_keys": {}, "event_kinds": {} }),
                };

                // Merge filter_keys if provided.
                if let Some(fk) = filter_keys {
                    if let Some(obj) = config
                        .get_mut("filter_keys")
                        .and_then(|v| v.as_object_mut())
                    {
                        obj.extend(fk);
                    }
                }

                // Ensure event_kinds map exists.
                if config.get("event_kinds").is_none() {
                    config["event_kinds"] = serde_json::json!({});
                }

                // Build and insert the event handler.
                let mut handler = serde_json::json!({
                    "mode": "table_insert",
                    "table_name": table_name,
                    "target_role": target_role,
                });
                if let Some(sm) = schema_map {
                    handler["schema_map"] = serde_json::Value::Object(sm);
                }
                if let Some(script) = adapter_script {
                    handler["adapter_script"] = serde_json::Value::String(script);
                }

                if let Some(kinds) = config
                    .get_mut("event_kinds")
                    .and_then(|v| v.as_object_mut())
                {
                    kinds.insert(event_kind.clone(), handler);
                }

                // Write back.
                let (content_str, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::SetConfig {
                        key: "router_listener.config".into(),
                        value_json: config.to_string(),
                    })
                    .await
                {
                    Ok(IpcResponse::Standard { ok: true, .. }) => (
                        format!(
                            "Listener registered: event_kind='{event_kind}' → table='{table_name}'. \
                             The router-listener applies this on its next reconnect. \
                             Next: call graph.query to CREATE a (TableConfig {{id:'table_config:{table_name}', \
                             name:'{table_name}'}}) node in your partition so this table appears in \
                             your cognitive envelope on future sessions."
                        ),
                        None,
                    ),
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e =
                            philotic_client::TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("table.add_listener: unexpected hotel response".into(), None),
                    Err(e) => {
                        let err = philotic_client::TaskErrorPayload::transport_error(
                            "philote",
                            format!("table.add_listener: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content_str),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── routing.policy.propose ───────────────────────────────────────
            "routing.policy.propose" => {
                let args = payload.arguments.as_object();
                let problem = args
                    .and_then(|a| a.get("problem"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let proposed_change = args
                    .and_then(|a| a.get("proposed_change"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let evidence = args
                    .and_then(|a| a.get("evidence"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let affected_stage = args
                    .and_then(|a| a.get("affected_stage"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let affected_capability = args
                    .and_then(|a| a.get("affected_capability"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let learned_reflex_preference_key = args
                    .and_then(|a| a.get("learned_reflex_preference_key"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                if problem.is_empty() || proposed_change.is_empty() {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "routing.policy.propose: 'problem' and 'proposed_change' are required."
                                .into(),
                        )
                        .await;
                }

                let result_text = match self
                    .ipc_client
                    .send_request(IpcRequest::RecordRoutingPolicyProposal {
                        agent_id: self.agent_id.clone(),
                        problem: problem.clone(),
                        proposed_change,
                        evidence,
                        affected_stage,
                        affected_capability,
                        learned_reflex_preference_key,
                    })
                    .await
                {
                    Ok(IpcResponse::RoutingPolicyRecorded { proposal_id }) => {
                        format!(
                            "Routing policy proposal recorded (id: {proposal_id}). \
                             An operator will review and approve or reject the proposed change."
                        )
                    }
                    Ok(IpcResponse::Standard {
                        ok: true, message, ..
                    }) => message,
                    Ok(IpcResponse::Standard {
                        ok: false, message, ..
                    }) => {
                        format!("routing.policy.propose: hotel rejected — {message}")
                    }
                    Ok(_) => "routing.policy.propose: unexpected response from hotel.".into(),
                    Err(e) => format!("routing.policy.propose: IPC error — {e}"),
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
                    content: Some(result_text),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: None,
                    tool_name: Some("routing.policy.propose".into()),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── routing.reflex.set ───────────────────────────────────────────
            "routing.reflex.set" => {
                let args = payload.arguments.as_object();
                let preference_key = args
                    .and_then(|a| a.get("preference_key"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("generation_capability_preference")
                    .to_string();
                let reflexes_json = args
                    .and_then(|a| a.get("reflexes"))
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                let reason = args
                    .and_then(|a| a.get("reason"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let result_text = match self
                    .ipc_client
                    .send_request(IpcRequest::UpsertAgentReflexPreference {
                        agent_id: self.agent_id.clone(),
                        preference_key: preference_key.clone(),
                        precedence: 70,
                        reflexes_json,
                        config_json: serde_json::json!({ "reason": reason }),
                    })
                    .await
                {
                    Ok(IpcResponse::Standard { ok: true, .. }) => {
                        format!(
                            "Routing reflex '{preference_key}' stored. Takes effect on the next turn."
                        )
                    }
                    Ok(IpcResponse::Standard {
                        ok: false, message, ..
                    }) => {
                        format!("routing.reflex.set: hotel rejected — {message}")
                    }
                    Ok(_) => "routing.reflex.set: unexpected response from hotel.".into(),
                    Err(e) => format!("routing.reflex.set: IPC error — {e}"),
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
                    content: Some(result_text),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: None,
                    tool_name: Some("routing.reflex.set".into()),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── routing.reflex.get ───────────────────────────────────────────
            "routing.reflex.get" => {
                let args = payload.arguments.as_object();
                let filter_key = args
                    .and_then(|a| a.get("preference_key"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let result_text = match self
                    .ipc_client
                    .send_request(IpcRequest::GetAgentReflexPreferences {
                        agent_id: self.agent_id.clone(),
                        preference_key: filter_key,
                    })
                    .await
                {
                    Ok(IpcResponse::AgentReflexPreferences { rows }) => {
                        serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".into())
                    }
                    Ok(IpcResponse::Standard {
                        ok: true, message, ..
                    }) => message,
                    Ok(_) => "routing.reflex.get: unexpected response from hotel.".into(),
                    Err(e) => format!("routing.reflex.get: IPC error — {e}"),
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
                    content: Some(result_text),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: None,
                    tool_name: Some("routing.reflex.get".into()),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── routing.pipeline.set ────────────────────────────────────────
            "routing.pipeline.set" => {
                let args = payload.arguments.as_object();
                let rule_id = args
                    .and_then(|a| a.get("rule_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("default")
                    .to_string();
                let rule_json = payload.arguments.clone();

                let result_text = match self
                    .ipc_client
                    .send_request(IpcRequest::UpsertRoutingPipelineRule {
                        agent_id: self.agent_id.clone(),
                        rule_id: rule_id.clone(),
                        rule_json,
                    })
                    .await
                {
                    Ok(IpcResponse::Standard { ok: true, .. }) => {
                        format!(
                            "Pipeline rule '{rule_id}' stored. Takes effect on the next inbound turn."
                        )
                    }
                    Ok(IpcResponse::Standard {
                        ok: false, message, ..
                    }) => {
                        format!("routing.pipeline.set: hotel rejected — {message}")
                    }
                    Ok(_) => "routing.pipeline.set: unexpected response from hotel.".into(),
                    Err(e) => format!("routing.pipeline.set: IPC error — {e}"),
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
                    content: Some(result_text),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: None,
                    tool_name: Some("routing.pipeline.set".into()),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── routing.pipeline.remove ──────────────────────────────────────
            "routing.pipeline.remove" => {
                let args = payload.arguments.as_object();
                let rule_id = args
                    .and_then(|a| a.get("rule_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let result_text = match self
                    .ipc_client
                    .send_request(IpcRequest::RemoveRoutingPipelineRule {
                        agent_id: self.agent_id.clone(),
                        rule_id: rule_id.clone(),
                    })
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true, message, ..
                    }) => message,
                    Ok(IpcResponse::Standard {
                        ok: false, message, ..
                    }) => {
                        format!("routing.pipeline.remove: hotel rejected — {message}")
                    }
                    Ok(_) => "routing.pipeline.remove: unexpected response from hotel.".into(),
                    Err(e) => format!("routing.pipeline.remove: IPC error — {e}"),
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
                    content: Some(result_text),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: None,
                    tool_name: Some("routing.pipeline.remove".into()),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── routing.pipeline.get ─────────────────────────────────────────
            "routing.pipeline.get" => {
                let args = payload.arguments.as_object();
                let filter_id = args
                    .and_then(|a| a.get("rule_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let result_text = match self
                    .ipc_client
                    .send_request(IpcRequest::GetRoutingPipelineRules {
                        agent_id: self.agent_id.clone(),
                        rule_id: filter_id,
                    })
                    .await
                {
                    Ok(IpcResponse::RoutingPipelineRules { pipeline_rules }) => {
                        serde_json::to_string_pretty(&pipeline_rules)
                            .unwrap_or_else(|_| "[]".into())
                    }
                    Ok(IpcResponse::Standard {
                        ok: true, message, ..
                    }) => message,
                    Ok(_) => "routing.pipeline.get: unexpected response from hotel.".into(),
                    Err(e) => format!("routing.pipeline.get: IPC error — {e}"),
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
                    content: Some(result_text),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: None,
                    tool_name: Some("routing.pipeline.get".into()),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── desktop.observe ──────────────────────────────────────────────
            "desktop.observe" => {
                // Observe-only: returns desktop runner metadata. No screenshot or interaction.
                // A real desktop guest is not required — this tool describes what would be observed.
                let result_text = serde_json::json!({
                    "status": "no_desktop_guest",
                    "message": "No desktop guest is currently materialised on this hotel. \
                                Desktop observation requires a desktop runner guest to be active.",
                    "tool": "desktop.observe",
                })
                .to_string();

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
                    content: Some(result_text),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: None,
                    tool_name: Some("desktop.observe".into()),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            other => {
                self.fail_active_turn(
                    payload.session_id,
                    payload.turn_id,
                    format!("Agent-local tool {} is not implemented", other),
                )
                .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_working_turn;
    use super::*;
    use crate::r#loop::{ToolCall, TurnPhase};

    #[test]
    fn inject_scoped_to_anchor_appends_edge_when_agent_resolves_and_no_edges() {
        // aria/architect is the discriminating case: the domain slug
        // "architect" does NOT match its role_node_id suffix
        // ("ai_architect"). A regression to slug-of-role/agent reconstruction
        // would produce "life:role:architect" here instead.
        let mut args = serde_json::json!({
            "observed_by": "agent-aria-01",
            "observed_role": "architect"
        })
        .as_object()
        .unwrap()
        .clone();

        inject_scoped_to_anchor(&mut args);

        let edges = args
            .get("edges")
            .and_then(serde_json::Value::as_array)
            .expect("edges array must be created");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0]["rel_type"], "SCOPED_TO");
        assert_eq!(edges[0]["target_id"], "life:role:ai_architect");
        assert_eq!(edges[0]["upsert_target"], true);
    }

    #[test]
    fn inject_scoped_to_anchor_is_idempotent_when_already_anchored() {
        let mut args = serde_json::json!({
            "observed_by": "agent-aria-01",
            "observed_role": "architect",
            "edges": [
                { "rel_type": "SCOPED_TO", "target_id": "life:role:ai_architect", "upsert_target": true }
            ]
        })
        .as_object()
        .unwrap()
        .clone();

        inject_scoped_to_anchor(&mut args);

        let edges = args
            .get("edges")
            .and_then(serde_json::Value::as_array)
            .expect("edges array must survive");
        assert_eq!(edges.len(), 1, "anchor must not be duplicated");
    }

    #[test]
    fn inject_scoped_to_anchor_preserves_existing_domain_edges() {
        let mut args = serde_json::json!({
            "observed_by": "agent-aria-01",
            "observed_role": "architect",
            "edges": [
                { "rel_type": "RELATES_TO", "target_id": "life:role:musician", "upsert_target": false }
            ]
        })
        .as_object()
        .unwrap()
        .clone();

        inject_scoped_to_anchor(&mut args);

        let edges = args
            .get("edges")
            .and_then(serde_json::Value::as_array)
            .expect("edges array must survive");
        assert_eq!(
            edges.len(),
            2,
            "anchor must append alongside existing edges"
        );
        assert_eq!(edges[0]["rel_type"], "RELATES_TO");
        assert_eq!(edges[1]["rel_type"], "SCOPED_TO");
    }

    #[test]
    fn inject_scoped_to_anchor_resolves_via_observed_role_fallback_for_unknown_agent() {
        let mut args = serde_json::json!({
            "observed_by": "agent-unknown-01",
            "observed_role": "chief_of_staff"
        })
        .as_object()
        .unwrap()
        .clone();

        inject_scoped_to_anchor(&mut args);

        let edges = args
            .get("edges")
            .and_then(serde_json::Value::as_array)
            .expect("edges array must be created via observed_role fallback");
        assert_eq!(edges[0]["target_id"], "life:role:chief-of-staff");
    }

    #[test]
    fn inject_scoped_to_anchor_noop_when_observed_by_absent() {
        let mut args = serde_json::json!({ "observed_role": "architect" })
            .as_object()
            .unwrap()
            .clone();

        inject_scoped_to_anchor(&mut args);

        assert!(args.get("edges").is_none());
    }

    #[test]
    fn inject_scoped_to_anchor_noop_when_agent_and_role_both_unresolvable() {
        let mut args = serde_json::json!({
            "observed_by": "agent-unknown-01",
            "observed_role": "not_a_domain"
        })
        .as_object()
        .unwrap()
        .clone();

        inject_scoped_to_anchor(&mut args);

        assert!(args.get("edges").is_none());
    }

    #[test]
    fn skill_register_is_an_unconditional_approval_gate() {
        // skill.register must ALWAYS require live operator approval — it can project
        // tools onto agents, so it joins admin role creation / rule.propose /
        // routing.policy.propose in the non-bypassable gate set.
        assert_eq!(
            super::AgentRuntime::unconditional_approval_gate(
                "skill.register",
                &serde_json::json!({ "skill_name": "x" })
            ),
            Some("skill_register")
        );
    }

    #[test]
    fn unconditional_gate_classifies_known_gates_and_ignores_others() {
        use super::AgentRuntime as R;
        assert_eq!(
            R::unconditional_approval_gate("rule.propose", &serde_json::json!({})),
            Some("rule_propose")
        );
        assert_eq!(
            R::unconditional_approval_gate("routing.policy.propose", &serde_json::json!({})),
            Some("routing_policy_propose")
        );
        assert_eq!(
            R::unconditional_approval_gate(
                "role.configure",
                &serde_json::json!({ "is_admin": true })
            ),
            Some("admin_role_creation")
        );
        // Non-admin role.configure is NOT an unconditional gate.
        assert_eq!(
            R::unconditional_approval_gate(
                "role.configure",
                &serde_json::json!({ "is_admin": false })
            ),
            None
        );
        // Ordinary tools are never force-gated here.
        assert_eq!(
            R::unconditional_approval_gate("echo", &serde_json::json!({})),
            None
        );
    }

    #[test]
    fn bound_tool_execution_allows_listed_tools() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.add_tool_binding("echo");
        let route = super::AgentRuntime::execute_bound_tool(
            &state,
            &ToolCall {
                tool_name: "echo".into(),
                arguments: serde_json::json!({ "text": "hello" }),
            },
        )
        .expect("echo tool should be allowed");
        assert_eq!(route.target_role, "tool.echo");
    }

    #[test]
    fn parked_approval_command_defers_behind_active_turn() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.parked_approval_turn = Some(test_working_turn(TurnPhase::WaitingApproval));
        state.start_turn(test_working_turn(TurnPhase::WaitingModel));

        assert!(super::AgentRuntime::should_defer_parked_approval_command(
            &state
        ));
    }

    #[test]
    fn parked_approval_command_does_not_defer_when_session_is_free() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.parked_approval_turn = Some(test_working_turn(TurnPhase::WaitingApproval));

        assert!(!super::AgentRuntime::should_defer_parked_approval_command(
            &state
        ));
    }

    #[test]
    fn bound_tool_execution_rejects_unlisted_tools() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.add_tool_binding("echo");

        let err = super::AgentRuntime::execute_bound_tool(
            &state,
            &ToolCall {
                tool_name: "workspace.read".into(),
                arguments: serde_json::json!({}),
            },
        )
        .expect_err("tool should be blocked");
        assert!(err.to_string().contains("not enabled"));
    }

    #[test]
    fn bound_tool_execution_requires_live_route() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.add_tool_binding("echo");
        if let Some(route) = state.tool_assembly.execution_routes.get_mut("echo") {
            route.availability_state = "materialization_required".into();
        }

        let err = super::AgentRuntime::execute_bound_tool(
            &state,
            &ToolCall {
                tool_name: "echo".into(),
                arguments: serde_json::json!({ "text": "hello" }),
            },
        )
        .expect_err("dormant route should not execute");
        assert!(err.to_string().contains("requires runner materialization"));
    }

    #[test]
    fn local_agent_route_executes_without_external_runner_liveness() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.clear_tool_bindings();
        state.add_tool_binding("session.status");

        let route = super::AgentRuntime::execute_bound_tool(
            &state,
            &ToolCall {
                tool_name: "session.status".into(),
                arguments: serde_json::json!({}),
            },
        )
        .expect("local agent tools should not require an external runner");
        assert_eq!(route.execution_mode, "local_agent");
    }

    #[test]
    fn parse_fallback_tiers_arg_reads_string_array() {
        assert_eq!(
            super::parse_fallback_tiers_arg(&serde_json::json!(["model", "model.openrouter"])),
            Some(vec!["model".to_string(), "model.openrouter".to_string()])
        );
        assert_eq!(
            super::parse_fallback_tiers_arg(&serde_json::json!(null)),
            None
        );
        assert_eq!(
            super::parse_fallback_tiers_arg(&serde_json::json!("not-an-array")),
            None
        );
    }

    fn fallback_tiers_test_payload(arguments: serde_json::Value) -> ToolExecutionPayload {
        ToolExecutionPayload {
            action: "execute_tool",
            session_id: "sess-fallback".into(),
            turn_id: "turn-fallback".into(),
            chat_id: "555".into(),
            tool_name: "role.configure".into(),
            arguments,
            execution_mode: "local_agent".into(),
            agent_id: "agent-fallback-tiers".into(),
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
        }
    }

    /// `execute_local_agent_tool` is one large hand-written match over every
    /// tool name; its generated async state machine is big enough that
    /// running it under `#[tokio::test]`'s default test-thread stack
    /// overflows even for a single call. Run these tests on a dedicated
    /// thread with a generous stack instead of touching the production
    /// function's shape just to make it test-friendly.
    fn run_with_big_stack<F, Fut>(f: F)
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

    /// Philote passthrough: `role.configure` with an explicit `fallback_tiers`
    /// argument must reach the hotel's ConfigureRole IPC and land in the
    /// locally cached `TurnLoopConfig`.
    #[test]
    fn role_configure_passthrough_sets_fallback_tiers() {
        run_with_big_stack(|| async {
            use super::super::tests::run_recording_hotel;

            let socket_path = format!(
                "/tmp/philote-fallback-tiers-set-{}.sock",
                Uuid::new_v4().simple()
            );
            let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
            let emitted =
                std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
            let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

            let identity = philotic_client::GuestIdentity {
                guest_id: "agent-fallback-tiers".into(),
                role: "agent".into(),
                supported_tools: Vec::new(),
            };
            let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
                .await
                .expect("connect to stub hotel");
            let mut runtime = AgentRuntime::new(client, "agent-fallback-tiers");

            let payload = fallback_tiers_test_payload(serde_json::json!({
                "role_name": "developer",
                "toolset_profile": "developer",
                "fallback_tiers": ["model", "model.openrouter"],
                "reasoning": {
                    "purpose": "p",
                    "toolset_rationale": "r",
                    "handoff_posture_and_limits": "h",
                },
            }));
            runtime
                .execute_local_agent_tool(payload)
                .await
                .expect("role.configure executes");

            let cached = runtime
                .configured_roles
                .get("developer")
                .expect("role cached after call");
            assert_eq!(
                cached.turn_loop_config.fallback_tiers,
                vec!["model".to_string(), "model.openrouter".to_string()]
            );

            drop(runtime);
            let _ = server.await;
            let _ = std::fs::remove_file(&socket_path);

            let emitted = emitted.lock().unwrap();
            assert_eq!(emitted.len(), 1);
            assert_eq!(
                emitted[0]["configure_role"]["fallback_tiers"],
                serde_json::json!(["model", "model.openrouter"])
            );
        });
    }

    /// The other half of the passthrough contract: a `role.configure` call
    /// that OMITS `fallback_tiers` must send `None` over IPC and must PRESERVE
    /// whatever ladder is already cached locally, mirroring the hotel-side
    /// preserve-on-None fix so the DB and in-process caches can't desync.
    #[test]
    fn role_configure_passthrough_preserves_cached_fallback_tiers_when_omitted() {
        run_with_big_stack(|| async {
            use super::super::tests::run_recording_hotel;

            let socket_path = format!(
                "/tmp/philote-fallback-tiers-preserve-{}.sock",
                Uuid::new_v4().simple()
            );
            let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
            let emitted =
                std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
            let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

            let identity = philotic_client::GuestIdentity {
                guest_id: "agent-fallback-tiers".into(),
                role: "agent".into(),
                supported_tools: Vec::new(),
            };
            let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
                .await
                .expect("connect to stub hotel");
            let mut runtime = AgentRuntime::new(client, "agent-fallback-tiers");

            // Pre-seed the cache as if an earlier call had set a custom ladder.
            runtime.configured_roles.insert(
                "developer".to_string(),
                CachedRoleConfig {
                    toolset_profile: "developer".into(),
                    role_identity_addendum: None,
                    role_manifest: None,
                    iteration_cap: None,
                    approval_policy: None,
                    turn_loop_config: ansible_mesh_core::graph::TurnLoopConfig {
                        fallback_tiers: vec!["model".to_string(), "model.openrouter".to_string()],
                        ..Default::default()
                    },
                    content_policy: "standard".into(),
                },
            );

            let payload = fallback_tiers_test_payload(serde_json::json!({
                "role_name": "developer",
                "toolset_profile": "developer",
                "iteration_cap": 7,
                "reasoning": {
                    "purpose": "p",
                    "toolset_rationale": "r",
                    "handoff_posture_and_limits": "h",
                },
            }));
            runtime
                .execute_local_agent_tool(payload)
                .await
                .expect("role.configure executes");

            let cached_after = runtime
                .configured_roles
                .get("developer")
                .expect("role still cached after call");
            assert_eq!(
                cached_after.turn_loop_config.fallback_tiers,
                vec!["model".to_string(), "model.openrouter".to_string()],
                "omitting fallback_tiers must preserve the previously cached ladder"
            );

            drop(runtime);
            let _ = server.await;
            let _ = std::fs::remove_file(&socket_path);

            let emitted = emitted.lock().unwrap();
            assert_eq!(emitted.len(), 1);
            assert_eq!(
                emitted[0]["configure_role"]["fallback_tiers"],
                serde_json::Value::Null
            );
        });
    }
}
