//! Session-activity recording: the shared session/ledger bookkeeping used by
//! the PublishMessage / CreateTask / UpdateTask / CompleteTask / FailTask /
//! EmitTask handlers (`record_session_activity_from_value` plus the explicit
//! approval/reflex session-event appenders), apartment memory-checkpoint
//! recording (`record_apartment_checkpoint`), session-envelope extraction
//! (`extract_session_envelope` / [`SessionEnvelope`]), turn-status merging,
//! and the RepairStaleSessionTurns zombie-turn watchdog handler.
//!
//! The IPC dispatch match arms remain in `ipc.rs` and delegate here via `Self::`.
//!
//! Extracted verbatim from `ipc.rs` — no behavior change.

use super::ipc::{
    IpcServer, compose_tool_assembly, infer_marker_strength, infer_placement_risk_level, unix_ts,
};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::storage::{
    SessionEventRecord, SessionParticipantRecord, SessionRecord, SessionTurnRecord,
};
use philotic_client::IpcResponse;
use tracing::{error, info, warn};
use uuid::Uuid;

#[derive(Default)]
struct SessionEnvelope {
    session_id: Option<String>,
    turn_id: Option<String>,
    primary_agent_id: Option<String>,
    source: Option<String>,
    chat_id: Option<String>,
    action: Option<String>,
    content: Option<String>,
}

impl IpcServer {
    pub(super) fn record_apartment_checkpoint(
        graph: &GraphDomain,
        agent_id: &str,
        memory_type: &str,
        content_json: &serde_json::Value,
    ) {
        if memory_type == "short" && content_json.get("active_sessions").is_some() {
            return;
        }

        let Some(session_id) = content_json
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return;
        };

        let now = unix_ts();
        let mut session = graph
            .get_session(session_id)
            .ok()
            .flatten()
            .unwrap_or(SessionRecord {
                session_id: session_id.to_string(),
                session_kind: "conversation".into(),
                primary_agent_id: Some(agent_id.to_string()),
                active_incarnation_id: None,
                channel_kind: None,
                channel_session_key: None,
                status: "active".into(),
                lease_owner_component_id: Some(agent_id.to_string()),
                lease_expires_at: None,
                summary_json: serde_json::json!({}),
                created_at: now,
                updated_at: now,
            });

        session.primary_agent_id = Some(agent_id.to_string());
        session.updated_at = now;
        let mut summary_json = session.summary_json.clone();
        if !summary_json.is_object() {
            summary_json = serde_json::json!({});
        }
        summary_json["memory_checkpoint"] = serde_json::json!({
            "memory_type": memory_type,
            "checkpoint": content_json,
        });
        session.summary_json = summary_json;
        let _ = graph.upsert_session(&session);

        let _ = graph.append_session_event(&SessionEventRecord {
            event_id: Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            turn_id: content_json
                .get("active_turn")
                .and_then(|t| t.get("turn_id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            component_id: agent_id.to_string(),
            kind: "apartment_checkpoint".into(),
            payload_json: serde_json::json!({
                "memory_type": memory_type,
            }),
            created_at: now,
        });
    }

    /// Close a turn whose task was accepted but then dropped undelivered.
    ///
    /// `EmitTask` records the turn `running` before it dispatches. When local
    /// delivery finds no subscriber the task is discarded permanently —
    /// `SubscribeInbox` does not replay, so a guest that subscribes later never
    /// sees it — and nothing was left to move that turn off `running`. It sat
    /// until `RepairStaleSessionTurns` failed it 300s later as
    /// `ZOMBIE_TURN_REPAIR`, which reads as a timeout and hides the real cause.
    /// A missing `egress-http-runner` presented that way for over a week.
    ///
    /// Recording the true reason at the moment of the drop turns a phantom
    /// ~315s "stuck turn" into an immediate, correctly-attributed failure.
    pub(super) fn fail_undelivered_session_turn(
        graph: &GraphDomain,
        payload: &serde_json::Value,
        error_code: &str,
        reason: &str,
    ) {
        let envelope = Self::extract_session_envelope(payload);
        let (Some(session_id), Some(turn_id)) = (envelope.session_id, envelope.turn_id) else {
            return;
        };
        let Ok(Some(mut turn)) = graph.get_session_turn(&session_id, &turn_id) else {
            return;
        };
        // Only rescue a turn still in flight. A turn that already reached a
        // terminal state must not be rewritten by a late drop notice.
        if turn.status != "running" && turn.status != "queued" {
            return;
        }
        turn.status = "failed".into();
        turn.error_json = Some(serde_json::json!({"error": error_code, "reason": reason}));
        turn.completed_at = Some(unix_ts());
        if let Err(e) = graph.upsert_session_turn(&turn) {
            warn!("fail_undelivered_session_turn: {session_id}:{turn_id}: {e}");
        }
    }

    pub(super) fn record_session_activity_from_value(
        graph: &GraphDomain,
        payload: &serde_json::Value,
        request_event_id: Option<Uuid>,
        turn_status: Option<&str>,
        participant_role: Option<&str>,
        event_kind: &str,
    ) {
        let envelope = Self::extract_session_envelope(payload);
        let Some(session_id) = envelope.session_id.clone() else {
            return;
        };

        let now = unix_ts();
        let mut session = graph
            .get_session(&session_id)
            .ok()
            .flatten()
            .unwrap_or(SessionRecord {
                session_id: session_id.clone(),
                session_kind: "conversation".into(),
                primary_agent_id: envelope.primary_agent_id.clone(),
                active_incarnation_id: None,
                channel_kind: envelope.source.clone(),
                channel_session_key: envelope.chat_id.clone(),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({}),
                created_at: now,
                updated_at: now,
            });

        if session.primary_agent_id.is_none() {
            session.primary_agent_id = envelope.primary_agent_id.clone();
        }
        if session.channel_kind.is_none() {
            session.channel_kind = envelope.source.clone();
        }
        if session.channel_session_key.is_none() {
            session.channel_session_key = envelope.chat_id.clone();
        }
        if let Some(session_status) = payload
            .get("session_status")
            .and_then(serde_json::Value::as_str)
        {
            session.status = session_status.to_string();
        }
        if let Some(approval_policy) = payload.get("approval_policy") {
            let mut summary_json = session.summary_json.clone();
            if !summary_json.is_object() {
                summary_json = serde_json::json!({});
            }
            summary_json["approval_policy"] = approval_policy.clone();
            session.summary_json = summary_json;
        }
        if let Some(bindings) = payload.get("bindings") {
            let mut summary_json = session.summary_json.clone();
            if !summary_json.is_object() {
                summary_json = serde_json::json!({});
            }
            summary_json["bindings"] = bindings.clone();
            if payload.get("tool_assembly").is_none() {
                summary_json["tool_assembly"] =
                    compose_tool_assembly(bindings, &[], &[], &[], "local-aiua-01");
            }
            session.summary_json = summary_json;
        }
        if let Some(tool_assembly) = payload.get("tool_assembly") {
            let mut summary_json = session.summary_json.clone();
            if !summary_json.is_object() {
                summary_json = serde_json::json!({});
            }
            summary_json["tool_assembly"] = tool_assembly.clone();
            session.summary_json = summary_json;
        }
        if let Some(reflex_overrides) = payload.get("reflex_overrides") {
            let mut summary_json = session.summary_json.clone();
            if !summary_json.is_object() {
                summary_json = serde_json::json!({});
            }
            summary_json["reflex_overrides"] = reflex_overrides.clone();
            session.summary_json = summary_json;
        }
        if let Some(reflex_evaluations) = payload.get("reflex_evaluations") {
            let mut summary_json = session.summary_json.clone();
            if !summary_json.is_object() {
                summary_json = serde_json::json!({});
            }
            summary_json["reflex_evaluations"] = reflex_evaluations.clone();
            session.summary_json = summary_json;
        }
        if let Some(reflex_policy_records) = payload.get("reflex_policy_records") {
            let mut summary_json = session.summary_json.clone();
            if !summary_json.is_object() {
                summary_json = serde_json::json!({});
            }
            summary_json["reflex_policy_records"] = reflex_policy_records.clone();
            session.summary_json = summary_json;
        }
        // Placement-provenance gate (2026-07-06 parked-tool-result incident): EmitTask
        // records EVERY dispatch through here, including tool/datasource invokes (e.g.
        // a life.* invoke addressed to vps-jane:life-graph-runner). Persisting a tool
        // dispatch's delivery context as `agent_runtime_provenance` made the RUNNER the
        // session's "persisted local delivery guest", so the runner's own RESULT was
        // parked for the runner (resolve_agent_route provenance-hint park) and the
        // agent's turn died at the watchdog — 6/6 life.* turns in one session. Only
        // genuine agent-turn deliveries — a dispatch whose target role is "agent" or a
        // "role:{agent_id}:{role_name}" incarnation routing key — may update placement
        // provenance. This is a forced, explicit decision at the recording site:
        // tool/datasource/gateway/model dispatches and status-only updates (no dispatch
        // role) must leave placement provenance untouched.
        let dispatch_is_agent_turn_delivery = matches!(
            participant_role,
            Some(role) if role == "agent" || role.starts_with("role:")
        );
        if dispatch_is_agent_turn_delivery {
            let marker_kind = payload
                .get("placement_marker_kind")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    payload
                        .get("transport")
                        .and_then(serde_json::Value::as_str)
                        .map(|_| "transport_continuity".to_string())
                })
                .or_else(|| {
                    payload
                        .get("action")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|action| match action {
                            "handoff_bundle" | "handoff_return" => Some("role_handoff".to_string()),
                            _ => None,
                        })
                })
                .or_else(|| {
                    payload
                        .get("source")
                        .and_then(serde_json::Value::as_str)
                        .map(|_| "receptor_ingress".to_string())
                });
            let marker_source = payload
                .get("placement_marker_source")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    payload
                        .get("transport")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .or_else(|| {
                    payload
                        .get("action")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|action| match action {
                            "handoff_bundle" | "handoff_return" => Some(action.to_string()),
                            _ => None,
                        })
                })
                .or_else(|| {
                    payload
                        .get("source")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .or_else(|| Some(event_kind.to_string()));
            let marker_strength = payload
                .get("placement_marker_strength")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    infer_marker_strength(None, marker_kind.as_deref()).map(str::to_string)
                });
            let placement_risk_level = infer_placement_risk_level(
                marker_kind.as_deref(),
                marker_source.as_deref(),
                marker_strength.as_deref(),
            );
            let agent_id = payload
                .get("agent_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| envelope.primary_agent_id.clone());
            let authority_hotel = payload
                .get("authority_hotel")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let delivery_hotel = payload
                .get("delivery_hotel")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let delivery_node_id = payload
                .get("delivery_node_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let delivery_target_role = payload
                .get("delivery_target_role")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let delivery_target_guest_id = payload
                .get("delivery_target_guest_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let transport = payload
                .get("transport")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);

            if agent_id.is_some()
                || authority_hotel.is_some()
                || delivery_hotel.is_some()
                || delivery_node_id.is_some()
                || delivery_target_role.is_some()
                || delivery_target_guest_id.is_some()
                || transport.is_some()
                || marker_kind.is_some()
                || marker_source.is_some()
                || marker_strength.is_some()
            {
                let mut summary_json = session.summary_json.clone();
                if !summary_json.is_object() {
                    summary_json = serde_json::json!({});
                }
                summary_json["agent_runtime_provenance"] = serde_json::json!({
                    "agent_id": agent_id,
                    "authority_hotel": authority_hotel,
                    "delivery_hotel": delivery_hotel,
                    "delivery_node_id": delivery_node_id,
                    "delivery_target_role": delivery_target_role,
                    "delivery_target_guest_id": delivery_target_guest_id,
                    "transport": transport,
                    "marker_kind": marker_kind,
                    "marker_source": marker_source,
                    "marker_strength": marker_strength,
                    "placement_risk_level": placement_risk_level,
                    "updated_at": now,
                });
                session.summary_json = summary_json;
            }
        }
        session.updated_at = now;
        let _ = graph.upsert_session(&session);

        if let Some(role) = participant_role {
            // The participant's `component_id` must identify the concrete guest that
            // fills `role`, not the role label itself. Prefer the explicit
            // delivery-target guest id when the payload carries one; only fall back to
            // the role string when no concrete component id is known (e.g. a raw
            // publish/create payload recorded before delivery context is attached).
            let component_id = payload
                .get("delivery_target_guest_id")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(role);
            let _ = graph.upsert_session_participant(&SessionParticipantRecord {
                session_id: session_id.clone(),
                component_id: component_id.to_string(),
                role: role.to_string(),
                joined_at: now,
                last_seen_at: now,
            });
        }

        if let Some(turn_id) = envelope.turn_id.clone() {
            let existing = graph.get_session_turn(&session_id, &turn_id).ok().flatten();
            // A turn-tail action (a response, result, or progress ping) CARRIES the
            // id of a turn owned by the session's hotel — it never starts one. On a
            // routing hotel (e.g. the datasource host forwarding its own
            // `datasource_response` back cross-mesh) no local turn exists, and
            // fabricating one here creates an orphan that no code path will ever
            // close: the real turn completes on the owning hotel, the shadow row
            // stays `running` until the zombie reaper marks it failed, and the
            // session_silent detector then counts a phantom consecutive failure
            // for a session that is actually healthy. Found live 2026-08-10 as
            // Aria's LifeGraph replies "hanging": 3 mbp turns completed in 5-8s
            // while 3 vps shadows of them died at the 660s wall. Update-only for
            // tails; the session/participant/event records above still land.
            let fabricating_from_tail =
                existing.is_none() && is_turn_tail_action(envelope.action.as_deref());
            if !fabricating_from_tail {
                let mut turn = existing.unwrap_or(SessionTurnRecord {
                    turn_id: turn_id.clone(),
                    session_id: session_id.clone(),
                    request_event_id: request_event_id.map(|id| id.to_string()),
                    user_message_json: serde_json::json!({}),
                    status: turn_status.unwrap_or("queued").to_string(),
                    response_json: None,
                    error_json: None,
                    started_at: Some(now),
                    completed_at: None,
                });

                if let Some(event_id) = request_event_id {
                    turn.request_event_id = Some(event_id.to_string());
                }
                if turn.user_message_json == serde_json::json!({}) {
                    turn.user_message_json = serde_json::json!({
                        "source": envelope.source,
                        "chat_id": envelope.chat_id,
                        "content": envelope.content,
                        "action": envelope.action,
                    });
                }
                if let Some(status) = merge_turn_status(&turn.status, turn_status) {
                    turn.status = status.clone();
                    if matches!(status.as_str(), "completed" | "failed") {
                        turn.completed_at = Some(now);
                    }
                }
                if envelope.action.as_deref() == Some("model_response")
                    || envelope.action.as_deref() == Some("send_reply")
                {
                    turn.response_json = Some(payload.clone());
                }
                let _ = graph.upsert_session_turn(&turn);
            }
        }

        let turn_id = envelope.turn_id.clone();
        let _ = graph.append_session_event(&SessionEventRecord {
            event_id: Uuid::new_v4().to_string(),
            session_id,
            turn_id: turn_id.clone(),
            component_id: participant_role.unwrap_or("system").to_string(),
            kind: event_kind.to_string(),
            payload_json: payload.clone(),
            created_at: now,
        });

        Self::append_explicit_approval_events(
            graph,
            &session.session_id,
            turn_id.as_deref(),
            participant_role.unwrap_or("system"),
            payload,
            now,
        );
        Self::append_explicit_reflex_events(
            graph,
            &session.session_id,
            turn_id.as_deref(),
            participant_role.unwrap_or("system"),
            payload,
            now,
        );
    }

    fn append_explicit_reflex_events(
        graph: &GraphDomain,
        session_id: &str,
        turn_id: Option<&str>,
        component_id: &str,
        payload: &serde_json::Value,
        now: u64,
    ) {
        if let Some(reflex_overrides) = payload.get("reflex_overrides") {
            let _ = graph.append_session_event(&SessionEventRecord {
                event_id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.map(str::to_string),
                component_id: component_id.to_string(),
                kind: "reflex_overrides_updated".into(),
                payload_json: reflex_overrides.clone(),
                created_at: now,
            });
        }
        if let Some(reflex_evaluations) = payload.get("reflex_evaluations") {
            let _ = graph.append_session_event(&SessionEventRecord {
                event_id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.map(str::to_string),
                component_id: component_id.to_string(),
                kind: "reflex_evaluations_recorded".into(),
                payload_json: reflex_evaluations.clone(),
                created_at: now,
            });
        }
        if let Some(reflex_policy_records) = payload.get("reflex_policy_records") {
            let _ = graph.append_session_event(&SessionEventRecord {
                event_id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.map(str::to_string),
                component_id: component_id.to_string(),
                kind: "reflex_policy_records_updated".into(),
                payload_json: reflex_policy_records.clone(),
                created_at: now,
            });
        }
    }

    fn append_explicit_approval_events(
        graph: &GraphDomain,
        session_id: &str,
        turn_id: Option<&str>,
        component_id: &str,
        payload: &serde_json::Value,
        now: u64,
    ) {
        if let Some(approval_request) = payload.get("approval_request") {
            let _ = graph.append_session_event(&SessionEventRecord {
                event_id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.map(str::to_string),
                component_id: component_id.to_string(),
                kind: "approval_requested".into(),
                payload_json: approval_request.clone(),
                created_at: now,
            });
        }

        if let Some(approval_resolution) = payload.get("approval_resolution") {
            let event_kind = match approval_resolution
                .get("decision")
                .and_then(serde_json::Value::as_str)
            {
                Some("approved") => "approval_resolved",
                Some("denied") => "approval_denied",
                _ => "approval_resolved",
            };
            let _ = graph.append_session_event(&SessionEventRecord {
                event_id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.map(str::to_string),
                component_id: component_id.to_string(),
                kind: event_kind.into(),
                payload_json: approval_resolution.clone(),
                created_at: now,
            });
        }

        if let Some(approval_policy) = payload.get("approval_policy") {
            let _ = graph.append_session_event(&SessionEventRecord {
                event_id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.map(str::to_string),
                component_id: component_id.to_string(),
                kind: "approval_policy_changed".into(),
                payload_json: approval_policy.clone(),
                created_at: now,
            });
        }

        if let Some(session_status) = payload.get("session_status") {
            let _ = graph.append_session_event(&SessionEventRecord {
                event_id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.map(str::to_string),
                component_id: component_id.to_string(),
                kind: "session_status_changed".into(),
                payload_json: session_status.clone(),
                created_at: now,
            });
        }

        if let Some(bindings) = payload.get("bindings") {
            let _ = graph.append_session_event(&SessionEventRecord {
                event_id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.map(str::to_string),
                component_id: component_id.to_string(),
                kind: "session_bindings_updated".into(),
                payload_json: bindings.clone(),
                created_at: now,
            });
        }

        if let Some(tool_assembly) = payload.get("tool_assembly").cloned().or_else(|| {
            payload
                .get("bindings")
                .map(|bindings| compose_tool_assembly(bindings, &[], &[], &[], "local-aiua-01"))
        }) {
            let _ = graph.append_session_event(&SessionEventRecord {
                event_id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.map(str::to_string),
                component_id: component_id.to_string(),
                kind: "tool_assembly_updated".into(),
                payload_json: tool_assembly,
                created_at: now,
            });
        }
    }

    fn extract_session_envelope(payload: &serde_json::Value) -> SessionEnvelope {
        SessionEnvelope {
            session_id: payload
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    let source = payload.get("source").and_then(serde_json::Value::as_str)?;
                    let chat_id = payload.get("chat_id")?.as_str()?;
                    let agent_id = payload
                        .get("primary_agent_id")
                        .or_else(|| payload.get("agent_id"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("agent-jane-01");
                    Some(format!("{source}:{chat_id}:{agent_id}"))
                }),
            turn_id: payload
                .get("turn_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            primary_agent_id: payload
                .get("primary_agent_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    // agent_id is the canonical field in EmitTask payloads (e.g. operator_chat
                    // tasks dispatched with agent_id but no primary_agent_id).
                    payload
                        .get("agent_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                }),
            source: payload
                .get("source")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            chat_id: payload
                .get("chat_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            action: payload
                .get("action")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            content: payload
                .get("content")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        }
    }

    pub(super) fn handle_repair_stale_session_turns(
        graph: &GraphDomain,
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
        min_age_secs: u64,
    ) -> IpcResponse {
        let now_secs = unix_ts();
        let max_started_at = now_secs.saturating_sub(min_age_secs);
        let zombie_turns = match graph.list_zombie_session_turns(max_started_at) {
            Ok(turns) => turns,
            Err(e) => {
                error!("RepairStaleSessionTurns: query failed: {e}");
                return IpcResponse::error("repair", "QUERY_ERROR", e.to_string());
            }
        };
        let mut repaired: u32 = 0;
        for mut turn in zombie_turns {
            let sid = turn.session_id.clone();
            let tid = turn.turn_id.clone();
            turn.status = "failed".into();
            turn.error_json = Some(
                serde_json::json!({"error": "ZOMBIE_TURN_REPAIR", "reason": "hotel watchdog: stale running turn"}),
            );
            turn.completed_at = Some(now_secs);
            if let Err(e) = graph.upsert_session_turn(&turn) {
                warn!("RepairStaleSessionTurns: mark failed {sid}:{tid}: {e}");
                continue;
            }
            // Null the owning session's checkpoint active_turn.
            if let Ok(Some(mut session)) = graph.get_session(&sid) {
                let has_active = session
                    .summary_json
                    .pointer("/memory_checkpoint/checkpoint/active_turn")
                    .map(|v| !v.is_null())
                    .unwrap_or(false);
                if has_active {
                    if let Some(cp) = session
                        .summary_json
                        .pointer_mut("/memory_checkpoint/checkpoint")
                        .and_then(|v| v.as_object_mut())
                    {
                        cp.insert("active_turn".into(), serde_json::Value::Null);
                    }
                    let _ = graph.upsert_session(&session);
                }
            }
            if let Some(hq) = heal_queue {
                let _ = hq.push_error(
                    &sid,
                    &format!(
                        "zombie turn {sid}:{tid} repaired by hotel watchdog (age >{min_age_secs}s)"
                    ),
                );
            }

            // DEF-069: a session pinned to a role incarnation that is not
            // actually serving goes SILENTLY deaf — turns are created, never
            // dispatched, and reaped here 300s later with no operator-visible
            // error. agent-jane lost 31h to exactly this, was recovered by a
            // manual DB patch, then lost another 17h to the same pin two days
            // later. The reap is not a repair; it only tidies up after the loss.
            //
            // Demote the session back to its base agent — always materialized,
            // and the incarnation that served every completed turn in both
            // incidents — so a permanent silent outage degrades into at most a
            // couple of lost turns.
            Self::demote_stuck_role_incarnation(graph, heal_queue, &sid);

            repaired += 1;
        }
        if repaired > 0 {
            info!(
                repaired,
                min_age_secs, "RepairStaleSessionTurns: repaired zombie turns"
            );
        }

        // S6 (FLEET_SUPERVISION_PROPOSAL): silence is a signal. Both July
        // outages were only ever noticed by a human wondering why it had gone
        // quiet — agent-jane for 31h, then again for 17h. Nothing watched for
        // "this session is being talked to and has stopped answering", so the
        // system stayed confident while the operator lost every message.
        //
        // Runs on the zombie sweep's cadence because it needs no extra
        // scheduling and the two are the same question one level apart: the
        // reaper asks "did this turn die?", this asks "has this whole
        // conversation died?".
        let silent = Self::scan_silent_sessions(graph, heal_queue, now_secs);

        IpcResponse::success(
            "repair",
            Some(serde_json::json!({"repaired": repaired, "silent_sessions": silent})),
        )
    }

    /// Flag active sessions that are receiving messages and answering none.
    /// Returns how many were flagged. See [`detect_session_silence`].
    fn scan_silent_sessions(
        graph: &GraphDomain,
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
        now_secs: u64,
    ) -> u32 {
        let Ok(sessions) = graph.list_sessions() else {
            return 0;
        };

        // One outstanding entry per silent session, not one per sweep.
        //
        // The first cut relied on `push_classified`'s 60s flood window, on the
        // assumption this sweep ran every few minutes. It runs roughly every
        // 90s, so every silent session cleared the window and re-filed
        // ~40 times an hour, accumulating as `pending` — which feeds
        // `heal.queue-depth` (warn >100, critical >500) and would have drowned
        // the real backlog signal within hours. A persistent fault should be
        // one aging row, not a stream of them.
        let already_open: std::collections::HashSet<String> = heal_queue
            .and_then(|hq| hq.pending_errors(SILENCE_PENDING_SCAN).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|row| row.pattern_tag.as_deref() == Some("session_silent"))
            .map(|row| row.guest_id)
            .collect();

        let mut flagged = 0u32;
        for session in sessions.iter().filter(|s| s.status == "active") {
            if already_open.contains(&session.session_id) {
                continue;
            }
            let turns = graph
                .list_session_turns(&session.session_id, SILENCE_TURN_SCAN)
                .unwrap_or_default();
            let Some(silence) = detect_session_silence(&turns, now_secs) else {
                continue;
            };

            let incarnation = session.active_incarnation_id.as_deref().unwrap_or("<none>");
            let minutes = silence.silent_secs / 60;
            warn!(
                session_id = session.session_id.as_str(),
                incarnation,
                failed_turns = silence.failed_turns,
                silent_secs = silence.silent_secs,
                "silence-as-signal: session is receiving turns and answering none"
            );
            if let Some(hq) = heal_queue {
                let message = format!(
                    "[session_silent] session [{}] has failed {} consecutive turn(s) over {}m \
                     with no successful reply (active incarnation [{}]). The session is being \
                     talked to and is answering nothing — this is the shape of a silent outage, \
                     not an idle conversation.",
                    session.session_id, silence.failed_turns, minutes, incarnation
                );
                // Re-files each sweep while the silence persists. That is
                // deliberate: DEF-070 made repeated escalate-only entries
                // legible (`escalated`, not `resolved`) and the
                // `heal.escalated-unrepaired` doctor check aggregates them by
                // age, so a burning fault gets louder instead of scrolling past.
                let _ = hq.push_classified(&session.session_id, &message, "high", "session_silent");
            }
            flagged += 1;
        }
        flagged
    }

    /// Re-point a session away from a role incarnation that has stopped
    /// serving, back to its base agent. See the call site for why (DEF-069).
    fn demote_stuck_role_incarnation(
        graph: &GraphDomain,
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
        session_id: &str,
    ) {
        let Ok(Some(mut session)) = graph.get_session(session_id) else {
            return;
        };
        let turns = graph
            .list_session_turns(session_id, ZOMBIE_DEMOTE_TURN_SCAN)
            .unwrap_or_default();
        let Some(base_agent_id) = decide_role_incarnation_demotion(
            session.active_incarnation_id.as_deref(),
            session.primary_agent_id.as_deref(),
            &turns,
        ) else {
            return;
        };

        let stuck = session
            .active_incarnation_id
            .clone()
            .unwrap_or_else(|| "<none>".into());
        session.active_incarnation_id = Some(base_agent_id.clone());
        if let Err(e) = graph.upsert_session(&session) {
            warn!(
                session_id,
                stuck_incarnation = %stuck,
                error = %e,
                "zombie-demotion: failed to re-point session to base agent"
            );
            return;
        }

        warn!(
            session_id,
            stuck_incarnation = %stuck,
            base_agent_id = %base_agent_id,
            "zombie-demotion: role incarnation stopped serving — session re-pointed to its base \
             agent so it stops losing turns silently"
        );
        if let Some(hq) = heal_queue {
            let message = format!(
                "[role_incarnation_not_serving] session [{session_id}] was pinned to [{stuck}], \
                 which let {ZOMBIE_DEMOTE_THRESHOLD} consecutive turns die as zombies without \
                 ever dispatching; re-pointed to base agent [{base_agent_id}]. That incarnation \
                 is registered but not serving — re-issue the role command only after confirming \
                 its guest actually consumes tasks."
            );
            let _ =
                hq.push_classified(session_id, &message, "high", "role_incarnation_not_serving");
        }
    }
}

/// Consecutive zombie-reaped turns (no completed turn in between) that a role
/// incarnation is allowed before its session is demoted to the base agent.
///
/// Two, not one: a single zombie is routinely a transient provider or network
/// failure — mbp-jane lost a turn exactly that way during a Telegram/openrouter
/// outage on 2026-07-28 — and that must not knock a session out of a role the
/// operator deliberately chose.
const ZOMBIE_DEMOTE_THRESHOLD: usize = 2;

/// How long a session may go without a single successful turn — while still
/// receiving turns — before it is flagged as silently broken.
///
/// One hour. Both July outages ran for 17h and 31h before a human noticed;
/// anything under a few hours is a decisive improvement, and an hour is long
/// enough that a slow model, a provider retry ladder, or a brief network
/// outage cannot manufacture a false positive.
const SESSION_SILENCE_SECS: u64 = 3600;

/// Minimum consecutive failures before silence is credible. One failure is a
/// blip; the mbp-jane Telegram/openrouter outage of 2026-07-28 produced exactly
/// one.
const SESSION_SILENCE_MIN_FAILURES: usize = 2;

/// How recent the *newest* failure must be for the silence to count as live.
///
/// The upper bound the first cut of this detector was missing. A session that
/// failed twice and was then abandoned — `ops-glm-verify-3`,
/// `smoke:life-graph:life.observe`, `mcp-perplexity-uat` — otherwise stays
/// "silent" forever and re-alerts on every sweep. That shipped, and produced
/// 264 alerts on mbp-jane inside an hour, every one of them a dead session
/// whose last failure was 26 days old.
///
/// Six hours: long enough to cover an operator who steps away mid-outage and
/// returns, short enough that a genuinely abandoned session falls out quickly.
const SESSION_SILENCE_STILL_ACTIVE_SECS: u64 = 6 * 3600;

/// How many recent turns to scan per session when measuring silence.
const SILENCE_TURN_SCAN: usize = 20;

/// How many pending heal rows to scan when checking for an already-open
/// `session_silent` entry. Generous: the check exists to stop this sweep from
/// re-filing, so missing an open row would defeat it.
const SILENCE_PENDING_SCAN: usize = 500;

/// What a silent session looks like once detected.
pub(super) struct SessionSilence {
    /// Consecutive failed turns since the last success (or since the start).
    pub failed_turns: usize,
    /// How long the session has been failing, newest failure to oldest.
    pub silent_secs: u64,
}

/// Decide whether a session is *silently broken* rather than merely idle.
///
/// The distinction is the whole point. An idle conversation has no recent
/// turns at all and must never alert — the operator simply has not written.
/// A silent outage has turns arriving and **none** succeeding, which is exactly
/// what agent-jane looked like for 31h and then 17h while every existing
/// detector stayed quiet.
///
/// Returns `None` unless: the newest turn is a failure, at least
/// [`SESSION_SILENCE_MIN_FAILURES`] consecutive turns have failed with no
/// success among them, and the oldest of those failures is at least
/// [`SESSION_SILENCE_SECS`] old.
///
/// `turns` may arrive in any order; it is sorted newest-first internally
/// because the graph listing is not time-ordered.
pub(super) fn detect_session_silence(
    turns: &[SessionTurnRecord],
    now_secs: u64,
) -> Option<SessionSilence> {
    let mut ordered: Vec<&SessionTurnRecord> = turns.iter().collect();
    ordered.sort_by_key(|t| std::cmp::Reverse(t.started_at));

    let mut failed_turns = 0usize;
    let mut oldest_failure_at: Option<u64> = None;
    let mut newest_failure_at: Option<u64> = None;

    for turn in ordered {
        match turn.status.as_str() {
            // A success anywhere in the recent streak means the session is
            // answering — whatever else is wrong, it is not silent.
            "completed" => break,
            "failed" => {
                failed_turns += 1;
                if let Some(started) = turn.started_at {
                    oldest_failure_at = Some(started);
                    newest_failure_at.get_or_insert(started);
                }
            }
            // `running` is the turn currently in flight, and anything else is
            // inconclusive. Neither proves life nor death — skip without
            // breaking the streak.
            _ => {}
        }
    }

    if failed_turns < SESSION_SILENCE_MIN_FAILURES {
        return None;
    }

    // Silence must be bounded at BOTH ends. Without an upper bound, a one-off
    // session that failed twice and was then abandoned — an ops check, a smoke
    // run, a UAT probe — looks identical forever to a conversation failing
    // right now. Shipping only the lower bound produced 264 alerts on mbp-jane
    // within an hour, every one of them a dead `ops-*` / `smoke:*` session
    // whose last failure was 26 days old.
    //
    // An ongoing outage keeps receiving turns; an abandoned session does not.
    // So the newest failure has to be recent for this to be live.
    let newest = newest_failure_at?;
    if now_secs.saturating_sub(newest) > SESSION_SILENCE_STILL_ACTIVE_SECS {
        return None;
    }

    let oldest = oldest_failure_at?;
    let silent_secs = now_secs.saturating_sub(oldest);
    if silent_secs < SESSION_SILENCE_SECS {
        return None;
    }
    Some(SessionSilence {
        failed_turns,
        silent_secs,
    })
}

/// How many recent turns to scan when measuring the consecutive zombie streak.
const ZOMBIE_DEMOTE_TURN_SCAN: usize = 20;

/// Pure decision rule for [`IpcServer::demote_stuck_role_incarnation`], split
/// out so the policy is testable without a graph.
///
/// Returns `Some(base_agent_id)` when the session should be re-pointed.
/// `turns` may arrive in any order; it is sorted newest-first internally.
fn decide_role_incarnation_demotion(
    active_incarnation_id: Option<&str>,
    primary_agent_id: Option<&str>,
    turns: &[SessionTurnRecord],
) -> Option<String> {
    // Only role incarnations ("{agent_id}:{role}") can be demoted; a session
    // already on its base agent has nowhere safer to go.
    let active = active_incarnation_id?;
    let (base_agent_id, _role) = active.split_once(':')?;

    // Never re-point at an agent this session does not belong to. Without this
    // guard a malformed incarnation id could silently hand the session to a
    // different agent — a worse failure than the one being repaired.
    if primary_agent_id != Some(base_agent_id) {
        return None;
    }

    let mut ordered: Vec<&SessionTurnRecord> = turns.iter().collect();
    ordered.sort_by_key(|t| std::cmp::Reverse(t.started_at));

    let mut streak = 0usize;
    for turn in ordered {
        if is_zombie_repaired_turn(turn) {
            streak += 1;
            if streak >= ZOMBIE_DEMOTE_THRESHOLD {
                return Some(base_agent_id.to_string());
            }
        } else if turn.status == "completed" {
            // A success more recent than the streak means the incarnation is
            // alive; this is not the stuck-pin failure.
            break;
        }
        // Any other status (running, unknown) is inconclusive — keep scanning
        // rather than counting it or breaking on it.
    }
    None
}

fn is_zombie_repaired_turn(turn: &SessionTurnRecord) -> bool {
    turn.status == "failed"
        && turn
            .error_json
            .as_ref()
            .and_then(|e| e.get("error"))
            .and_then(serde_json::Value::as_str)
            == Some("ZOMBIE_TURN_REPAIR")
}

/// Actions that ride an EXISTING turn rather than starting one: responses,
/// results, and progress pings all carry the turn id of a turn owned by the
/// session's hotel. `record_session_activity_from_value` must never create a
/// turn record from one of these — on a routing hotel the referenced turn is
/// not local, and a fabricated copy has no closer (see the comment at the
/// creation site). `tool_progress` matters as much as the responses: the
/// datasource keepalive pings every 20s, so one slow tool call would fabricate
/// and refresh a shadow turn for its whole duration.
fn is_turn_tail_action(action: Option<&str>) -> bool {
    matches!(
        action,
        Some(
            "datasource_response"
                | "model_response"
                | "send_reply"
                | "tool_result"
                | "tool_progress"
                | "task_error"
        )
    )
}

fn merge_turn_status(current: &str, incoming: Option<&str>) -> Option<String> {
    let incoming = incoming?;
    if matches!(current, "completed" | "failed") && !matches!(incoming, "completed" | "failed") {
        return Some(current.to_string());
    }
    Some(incoming.to_string())
}

#[cfg(test)]
mod demotion_policy_tests {
    use super::*;

    fn turn(turn_id: &str, status: &str, started_at: u64, zombie: bool) -> SessionTurnRecord {
        SessionTurnRecord {
            turn_id: turn_id.into(),
            session_id: "telegram:1:agent-jane".into(),
            request_event_id: None,
            user_message_json: serde_json::json!({}),
            status: status.into(),
            response_json: None,
            error_json: zombie.then(|| {
                serde_json::json!({
                    "error": "ZOMBIE_TURN_REPAIR",
                    "reason": "hotel watchdog: stale running turn"
                })
            }),
            started_at: Some(started_at),
            completed_at: Some(started_at + 300),
        }
    }

    const JANE: Option<&str> = Some("agent-jane");

    /// The exact shape of both agent-jane outages: session pinned to
    /// `agent-jane:orchestrator`, consecutive turns reaped as zombies.
    #[test]
    fn demotes_after_consecutive_zombies_on_a_role_incarnation() {
        let turns = [
            turn("t1", "completed", 100, false),
            turn("t2", "failed", 200, true),
            turn("t3", "failed", 300, true),
        ];
        assert_eq!(
            decide_role_incarnation_demotion(Some("agent-jane:orchestrator"), JANE, &turns),
            Some("agent-jane".to_string())
        );
    }

    /// A single zombie is routinely a transient provider/network failure — the
    /// 2026-07-28 Telegram+openrouter outage produced exactly one. It must not
    /// knock a session out of a role the operator chose.
    #[test]
    fn one_zombie_is_a_blip_not_a_stuck_pin() {
        let turns = [
            turn("t1", "completed", 100, false),
            turn("t2", "failed", 200, true),
        ];
        assert_eq!(
            decide_role_incarnation_demotion(Some("agent-jane:vixen"), JANE, &turns),
            None
        );
    }

    /// A success newer than the zombies means the incarnation is alive.
    #[test]
    fn a_completed_turn_breaks_the_streak() {
        let turns = [
            turn("t1", "failed", 100, true),
            turn("t2", "failed", 200, true),
            turn("t3", "completed", 300, false),
        ];
        assert_eq!(
            decide_role_incarnation_demotion(Some("agent-jane:vixen"), JANE, &turns),
            None
        );
    }

    /// A session already on its base agent has nowhere safer to go — demoting
    /// it would be a no-op at best and a misroute at worst.
    #[test]
    fn base_agent_sessions_are_never_demoted() {
        let turns = [
            turn("t1", "failed", 100, true),
            turn("t2", "failed", 200, true),
        ];
        assert_eq!(
            decide_role_incarnation_demotion(Some("agent-jane"), JANE, &turns),
            None
        );
        assert_eq!(decide_role_incarnation_demotion(None, JANE, &turns), None);
    }

    /// Never hand a session to an agent it does not belong to.
    #[test]
    fn refuses_to_demote_across_agents() {
        let turns = [
            turn("t1", "failed", 100, true),
            turn("t2", "failed", 200, true),
        ];
        assert_eq!(
            decide_role_incarnation_demotion(Some("agent-astrid:orchestrator"), JANE, &turns),
            None
        );
        assert_eq!(
            decide_role_incarnation_demotion(Some("agent-jane:orchestrator"), None, &turns),
            None
        );
    }

    /// Ordering is derived from `started_at`, not from list order — the graph
    /// listing is not time-sorted.
    #[test]
    fn unordered_input_is_evaluated_newest_first() {
        let scrambled = [
            turn("t3", "failed", 300, true),
            turn("t1", "completed", 100, false),
            turn("t2", "failed", 200, true),
        ];
        assert_eq!(
            decide_role_incarnation_demotion(Some("agent-jane:orchestrator"), JANE, &scrambled),
            Some("agent-jane".to_string())
        );

        // Same records, but the newest is a success → alive, no demotion.
        let alive = [
            turn("t2", "failed", 200, true),
            turn("t3", "completed", 300, false),
            turn("t1", "failed", 100, true),
        ];
        assert_eq!(
            decide_role_incarnation_demotion(Some("agent-jane:orchestrator"), JANE, &alive),
            None
        );
    }

    /// Ordinary failures are not zombies; only watchdog-reaped turns count.
    #[test]
    fn non_zombie_failures_do_not_trigger_demotion() {
        let turns = [
            turn("t1", "failed", 100, false),
            turn("t2", "failed", 200, false),
        ];
        assert_eq!(
            decide_role_incarnation_demotion(Some("agent-jane:orchestrator"), JANE, &turns),
            None
        );
    }
}

#[cfg(test)]
mod silence_signal_tests {
    use super::*;

    const NOW: u64 = 1_000_000;
    const HOUR: u64 = 3600;

    fn t(status: &str, started_at: u64) -> SessionTurnRecord {
        SessionTurnRecord {
            turn_id: format!("turn-{started_at}"),
            session_id: "telegram:1:agent-jane".into(),
            request_event_id: None,
            user_message_json: serde_json::json!({}),
            status: status.into(),
            response_json: None,
            error_json: None,
            started_at: Some(started_at),
            completed_at: Some(started_at + 300),
        }
    }

    /// The agent-jane shape: turns keep arriving, none succeed, for hours.
    #[test]
    fn flags_a_session_that_receives_turns_and_answers_none() {
        let turns = [
            t("completed", NOW - 8 * HOUR),
            t("failed", NOW - 5 * HOUR),
            t("failed", NOW - 3 * HOUR),
            t("failed", NOW - 2 * HOUR),
        ];
        let s = detect_session_silence(&turns, NOW).expect("should flag");
        assert_eq!(s.failed_turns, 3);
        assert_eq!(s.silent_secs, 5 * HOUR, "measured from the OLDEST failure");
    }

    /// The false positive that would make this feature unusable: an operator
    /// who simply has not written in days must never page anyone.
    #[test]
    fn an_idle_conversation_is_never_flagged() {
        // Silent for a week, but everything that did arrive succeeded.
        let idle = [
            t("completed", NOW - 7 * 24 * HOUR),
            t("completed", NOW - 6 * 24 * HOUR),
        ];
        assert!(detect_session_silence(&idle, NOW).is_none());
        // And a session with no turns at all.
        assert!(detect_session_silence(&[], NOW).is_none());
    }

    /// A success newer than the failures means it is answering.
    #[test]
    fn a_recent_success_clears_the_signal() {
        let turns = [
            t("failed", NOW - 5 * HOUR),
            t("failed", NOW - 4 * HOUR),
            t("completed", NOW - 30 * 60),
        ];
        assert!(detect_session_silence(&turns, NOW).is_none());
    }

    /// One failure is a blip — mbp-jane produced exactly one during the
    /// 2026-07-28 Telegram/openrouter outage. Two is a pattern.
    #[test]
    fn a_single_failure_is_a_blip() {
        let one = [t("completed", NOW - 9 * HOUR), t("failed", NOW - 5 * HOUR)];
        assert!(detect_session_silence(&one, NOW).is_none());

        let two = [
            t("completed", NOW - 9 * HOUR),
            t("failed", NOW - 5 * HOUR),
            t("failed", NOW - 4 * HOUR),
        ];
        assert!(detect_session_silence(&two, NOW).is_some());
    }

    /// Failures inside the window are still in progress, not yet an outage —
    /// a provider retry ladder can burn two turns in minutes.
    #[test]
    fn recent_failures_are_below_the_time_threshold() {
        let fresh = [t("failed", NOW - 10 * 60), t("failed", NOW - 5 * 60)];
        assert!(detect_session_silence(&fresh, NOW).is_none());

        // Exactly at the threshold fires; the boundary is inclusive.
        let at_threshold = [
            t("failed", NOW - SESSION_SILENCE_SECS),
            t("failed", NOW - SESSION_SILENCE_SECS + 60),
        ];
        assert!(detect_session_silence(&at_threshold, NOW).is_some());
    }

    /// `running` is the turn in flight — it neither proves life nor breaks the
    /// streak, or a wedged session would hide behind its own stuck turn.
    #[test]
    fn an_in_flight_turn_does_not_mask_silence() {
        let turns = [
            t("completed", NOW - 9 * HOUR),
            t("failed", NOW - 5 * HOUR),
            t("failed", NOW - 4 * HOUR),
            t("running", NOW - 60),
        ];
        let s = detect_session_silence(&turns, NOW).expect("running must not mask the streak");
        assert_eq!(s.failed_turns, 2);
    }

    /// Replays the real agent-jane timeline from
    /// `session:telegram:7898847424:agent-jane` (unix epochs straight out of
    /// the mbp-jane graph) and asserts the detector fires one hour in.
    ///
    /// The outage actually ran **31 hours** before a human noticed the silence.
    /// This is the regression that keeps that from being possible again.
    #[test]
    fn would_have_caught_the_real_agent_jane_outage_in_one_hour() {
        // 2026-07-25 15:16:14Z — last successful turn ("/sfw").
        let last_success = 1_784_999_774;
        // 15:17:00, 15:18:59, 15:19:27, 15:52:33 — every turn after it died.
        let failures = [1_784_999_820, 1_784_999_939, 1_784_999_967, 1_785_001_953];
        let mut turns = vec![t("completed", last_success)];
        turns.extend(failures.iter().map(|&at| t("failed", at)));

        // 15:47 — half an hour in. Real, but not yet conclusive.
        assert!(
            detect_session_silence(&turns, last_success + 1860).is_none(),
            "must not fire before the silence window elapses"
        );

        // 16:17:00 — exactly one hour after the first failure.
        let one_hour_in = failures[0] + SESSION_SILENCE_SECS;
        let s = detect_session_silence(&turns, one_hour_in)
            .expect("one hour of unanswered turns must page");
        assert_eq!(s.failed_turns, 4);
        assert_eq!(s.silent_secs, SESSION_SILENCE_SECS);

        // The operator kept writing — 16:26:37, 16:26:53, 16:26:54 all died
        // too. While messages keep arriving it keeps paging.
        let kept_trying = [1_785_004_e3 as u64 + 597, 1_785_004_e3 as u64 + 613];
        let mut still_going = turns.clone();
        still_going.extend(kept_trying.iter().map(|&at| t("failed", at)));
        assert!(
            detect_session_silence(&still_going, kept_trying[1] + 1800).is_some(),
            "must keep paging while unanswered turns keep arriving"
        );

        // …and once the operator gives up for longer than the active window,
        // it goes quiet. That is correct: it already paged at hour one, and a
        // conversation nobody is writing to is no longer an ongoing outage.
        assert!(
            detect_session_silence(&still_going, kept_trying[1] + 7 * 3600).is_none(),
            "stops paging once nothing new arrives for longer than the active window"
        );

        // For scale: the operator's "Are you there?" landed ~26 hours after
        // this point and was itself swallowed. The alert this test pins would
        // have arrived 30 hours before that message was ever typed.
    }

    /// The false positive that actually shipped. `ops-glm-verify-3`,
    /// `smoke:life-graph:life.observe`, `mcp-perplexity-uat` and friends are
    /// one-off sessions that failed twice and were then abandoned. With only a
    /// lower bound they stayed "silent" forever and re-alerted every sweep —
    /// 264 entries on mbp-jane inside an hour, last failure 26 days old.
    ///
    /// An abandoned session is not an outage. Nobody is waiting on it.
    #[test]
    fn an_abandoned_session_stops_alerting() {
        // A realistic epoch, because 26 days does not fit under the small NOW.
        let now = 1_786_000_000u64;
        let abandoned_at = now - 26 * 24 * HOUR;
        let dead = [t("failed", abandoned_at - HOUR), t("failed", abandoned_at)];
        assert!(
            detect_session_silence(&dead, now).is_none(),
            "a session whose last failure is 26 days old is abandoned, not silent"
        );

        // The same session, checked while it was actually failing, still pages.
        assert!(
            detect_session_silence(&dead, abandoned_at + 600).is_some(),
            "it must still have alerted at the time it was genuinely failing"
        );
    }

    /// The boundary between "still failing" and "abandoned".
    #[test]
    fn silence_expires_once_the_newest_failure_goes_stale() {
        let turns = [
            t("failed", NOW - SESSION_SILENCE_STILL_ACTIVE_SECS - 2 * HOUR),
            t("failed", NOW - SESSION_SILENCE_STILL_ACTIVE_SECS + HOUR),
        ];
        // Newest failure is inside the active window → still an outage.
        assert!(detect_session_silence(&turns, NOW).is_some());

        // Roll time forward past the window with no new turns → falls silent.
        let later = NOW + 2 * SESSION_SILENCE_STILL_ACTIVE_SECS;
        assert!(detect_session_silence(&turns, later).is_none());
    }

    /// The graph listing is not time-ordered.
    #[test]
    fn unordered_input_is_evaluated_newest_first() {
        let scrambled = [
            t("failed", NOW - 3 * HOUR),
            t("completed", NOW - 30 * 60), // newest → session is alive
            t("failed", NOW - 4 * HOUR),
        ];
        assert!(detect_session_silence(&scrambled, NOW).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::ipc::test_dispatcher_channel;
    use crate::service::ipc::tests::{ipc_env_guard, test_socket_path};
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
    use philotic_client::{GuestIdentity, IpcRequest, PhiloticClient};
    use std::path::Path;
    use std::sync::Arc;

    #[tokio::test]
    async fn update_task_with_approval_policy_updates_session_summary_and_event_log() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = test_dispatcher_channel();
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        agent
            .send_request(IpcRequest::UpdateTask {
                task_id: Uuid::new_v4(),
                state: "session_policy_updated".into(),
                payload: serde_json::json!({
                    "session_id": "sess-policy-events",
                    "turn_id": "turn-policy-1",
                    "chat_id": "123",
                    "approval_policy": {
                        "auto_approve_all": true,
                        "preapproved_tools": [],
                        "preapproved_classes": []
                    },
                    "action": "approval_policy_update"
                }),
            })
            .await
            .expect("update task should succeed");

        let session = graph
            .get_session("sess-policy-events")
            .expect("session lookup should work")
            .expect("session should exist");
        assert_eq!(
            session.summary_json["approval_policy"]["auto_approve_all"],
            true
        );

        let events = graph
            .list_session_events("sess-policy-events", 20)
            .expect("event listing should work");
        assert!(
            events
                .iter()
                .any(|event| event.kind == "approval_policy_changed")
        );

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn update_task_with_reflex_governance_updates_session_summary_and_event_log() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = test_dispatcher_channel();
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        agent
            .send_request(IpcRequest::UpdateTask {
                task_id: Uuid::new_v4(),
                state: "session_reflexes_updated".into(),
                payload: serde_json::json!({
                    "session_id": "sess-reflex-events",
                    "turn_id": "turn-reflex-1",
                    "chat_id": "123",
                    "reflex_overrides": {
                        "remote_tool_reflex": "allow",
                        "credential_scope_reflex": "mesh_scoped"
                    },
                    "reflex_evaluations": [{
                        "reflex_name": "remote_tool_reflex",
                        "decision": "operator_override",
                        "reason": "trusted operator session"
                    }]
                }),
            })
            .await
            .expect("update task should succeed");

        let session = graph
            .get_session("sess-reflex-events")
            .expect("session lookup should work")
            .expect("session should exist");
        assert_eq!(
            session.summary_json["reflex_overrides"]["remote_tool_reflex"],
            "allow"
        );
        assert_eq!(
            session.summary_json["reflex_evaluations"][0]["decision"],
            "operator_override"
        );

        let events = graph
            .list_session_events("sess-reflex-events", 20)
            .expect("event listing should work");
        assert!(
            events
                .iter()
                .any(|event| event.kind == "reflex_overrides_updated")
        );
        assert!(
            events
                .iter()
                .any(|event| event.kind == "reflex_evaluations_recorded")
        );

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn update_task_with_session_status_and_bindings_updates_session_summary_and_event_log() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = test_dispatcher_channel();
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        agent
            .send_request(IpcRequest::UpdateTask {
                task_id: Uuid::new_v4(),
                state: "session_status_updated".into(),
                payload: serde_json::json!({
                    "session_id": "sess-lifecycle",
                    "turn_id": "turn-lifecycle-1",
                    "chat_id": "123",
                    "session_status": "paused",
                    "bindings": {
                        "effective_toolset": ["echo"],
                        "effective_skillset": ["planning"],
                        "effective_workspace_ref": "workspace://main",
                        "effective_model_controller": "gemini-flash"
                    },
                    "action": "session_status_update"
                }),
            })
            .await
            .expect("update task should succeed");

        let session = graph
            .get_session("sess-lifecycle")
            .expect("session lookup should work")
            .expect("session should exist");
        assert_eq!(session.status, "paused");
        assert_eq!(
            session.summary_json["bindings"]["effective_toolset"][0],
            "echo"
        );
        assert!(session.summary_json["tool_assembly"]["execution_routes"]["echo"].is_null());

        let events = graph
            .list_session_events("sess-lifecycle", 20)
            .expect("event listing should work");
        assert!(
            events
                .iter()
                .any(|event| event.kind == "session_status_changed")
        );
        assert!(
            events
                .iter()
                .any(|event| event.kind == "session_bindings_updated")
        );
        assert!(
            events
                .iter()
                .any(|event| event.kind == "tool_assembly_updated")
        );

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn update_task_with_reflex_policy_records_updates_session_summary_and_event_log() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = test_dispatcher_channel();
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-reflex-policy-events".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: Some("agent-jane:orchestrator".into()),
                channel_kind: Some("operator".into()),
                channel_session_key: Some("chat-1".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({}),
                created_at: 1,
                updated_at: 1,
            })
            .expect("seed session");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        agent
            .send_request(IpcRequest::UpdateTask {
                task_id: Uuid::new_v4(),
                state: "session_reflex_policy_updated".into(),
                payload: serde_json::json!({
                    "session_id": "sess-reflex-policy-events",
                    "turn_id": "turn-reflex-policy-1",
                    "chat_id": "123",
                    "reflex_policy_records": [{
                        "policy_scope": "session_override",
                        "policy_source": "operator_override",
                        "precedence": 90,
                        "reason": "trusted operator session",
                        "reflexes": {
                            "remote_tool_reflex": "allow",
                            "credential_scope_reflex": "mesh_scoped"
                        }
                    }]
                }),
            })
            .await
            .expect("update task should succeed");

        let session = graph
            .get_session("sess-reflex-policy-events")
            .expect("session lookup should work")
            .expect("session should exist");
        assert_eq!(
            session.summary_json["reflex_policy_records"][0]["policy_scope"],
            "session_override"
        );
        assert_eq!(
            session.summary_json["reflex_policy_records"][0]["reflexes"]["remote_tool_reflex"],
            "allow"
        );

        let events = graph
            .list_session_events("sess-reflex-policy-events", 20)
            .expect("event listing should work");
        assert!(
            events
                .iter()
                .any(|event| event.kind == "reflex_policy_records_updated")
        );

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    /// The live 2026-08-10 shape: a datasource host emits its own
    /// `datasource_response` back toward a session owned by ANOTHER hotel. The
    /// EmitTask handler records activity with turn_status "running" and the
    /// payload carries the ORIGINAL turn's id — which does not exist locally.
    /// Fabricating a local turn from it creates an orphan no code path closes:
    /// it can only be reaped as ZOMBIE_TURN_REPAIR, which then feeds phantom
    /// "consecutive failed turns" into the session_silent detector for a
    /// session that is actually healthy.
    #[test]
    fn tail_action_never_fabricates_a_turn_on_a_routing_hotel() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));

        for action in [
            "datasource_response",
            "model_response",
            "send_reply",
            "tool_result",
            "tool_progress",
            "task_error",
        ] {
            let session_id = format!("telegram:7898847424:agent-aria-{action}");
            let turn_id = format!("turn-owned-elsewhere-{action}");
            IpcServer::record_session_activity_from_value(
                &graph,
                &serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "action": action,
                    "chat_id": "7898847424",
                }),
                Some(Uuid::new_v4()),
                Some("running"),
                Some("agent"),
                "emit_task",
            );
            assert!(
                graph
                    .get_session_turn(&session_id, &turn_id)
                    .expect("turn lookup should work")
                    .is_none(),
                "a `{action}` must not fabricate a local turn for an id it only references"
            );
            // The rest of the recording still lands — this is a narrow guard on
            // turn CREATION, not a bypass of activity tracking.
            assert!(
                graph
                    .get_session(&session_id)
                    .expect("session lookup should work")
                    .is_some(),
                "session record must still be upserted for `{action}`"
            );
            assert_eq!(
                graph
                    .list_session_events(&session_id, 10)
                    .expect("event list should work")
                    .len(),
                1,
                "the session event must still be appended for `{action}`"
            );
        }
    }

    /// The guard must be creation-only: on the session-OWNING hotel the turn
    /// exists (created when the request was recorded), and a tail arriving
    /// there must keep updating it — response_json lands, status merges.
    #[test]
    fn tail_action_still_updates_an_existing_turn() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        let session_id = "telegram:7898847424:agent-aria";
        let turn_id = "turn-local-1";

        // The request leg creates the turn (no action field — an inbound user
        // message, not a tail).
        IpcServer::record_session_activity_from_value(
            &graph,
            &serde_json::json!({
                "session_id": session_id,
                "turn_id": turn_id,
                "content": "what's on my to-do list?",
            }),
            Some(Uuid::new_v4()),
            Some("running"),
            Some("agent"),
            "emit_task",
        );
        assert!(
            graph
                .get_session_turn(session_id, turn_id)
                .expect("turn lookup should work")
                .is_some(),
            "a non-tail dispatch must still create the turn (over-blocking guard)"
        );

        // The response tail must update that same turn, not be dropped.
        IpcServer::record_session_activity_from_value(
            &graph,
            &serde_json::json!({
                "session_id": session_id,
                "turn_id": turn_id,
                "action": "model_response",
                "content": "here it is",
            }),
            Some(Uuid::new_v4()),
            Some("running"),
            Some("agent"),
            "emit_task",
        );
        let turn = graph
            .get_session_turn(session_id, turn_id)
            .expect("turn lookup should work")
            .expect("turn must still exist");
        assert!(
            turn.response_json.is_some(),
            "a tail arriving on the owning hotel must still record response_json"
        );
        assert_eq!(turn.status, "running");
    }

    #[test]
    fn record_session_activity_participant_component_id_is_distinct_from_role() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));

        // Payload carrying the concrete guest filling the target role.
        let payload = serde_json::json!({
            "session_id": "sess-participant-1",
            "turn_id": "turn-participant-1",
            "delivery_target_guest_id": "philote-orchestrator-guest-01",
        });
        IpcServer::record_session_activity_from_value(
            &graph,
            &payload,
            None,
            None,
            Some("orchestrator"),
            "publish_message",
        );

        let participants = graph
            .list_session_participants("sess-participant-1")
            .expect("list participants should work");
        assert_eq!(participants.len(), 1, "exactly one participant recorded");
        let participant = &participants[0];
        // Regression: component_id must be the real guest id, not the role string.
        assert_eq!(participant.component_id, "philote-orchestrator-guest-01");
        assert_eq!(participant.role, "orchestrator");
        assert_ne!(
            participant.component_id, participant.role,
            "component_id must not collapse to the role label"
        );
    }

    #[test]
    fn record_session_activity_participant_falls_back_to_role_without_target_guest() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));

        // No delivery_target_guest_id: no concrete component id is known, so the role
        // label is the documented fallback for component_id.
        let payload = serde_json::json!({
            "session_id": "sess-participant-2",
            "turn_id": "turn-participant-2",
        });
        IpcServer::record_session_activity_from_value(
            &graph,
            &payload,
            None,
            None,
            Some("gateway"),
            "publish_message",
        );

        let participants = graph
            .list_session_participants("sess-participant-2")
            .expect("list participants should work");
        assert_eq!(participants.len(), 1);
        assert_eq!(participants[0].component_id, "gateway");
        assert_eq!(participants[0].role, "gateway");
    }

    // Regression for the 2026-07-06 parked-tool-result incident: recording a TOOL
    // dispatch (delivery context stamped with the runner as target guest) must NOT
    // update the session's agent_runtime_provenance. Before the fix, the life.*
    // invoke to vps-jane:life-graph-runner became the session's "persisted local
    // delivery guest", so the runner's own RESULT was parked for the runner and the
    // agent turn died at the watchdog (6/6 life.* turns in one session).
    #[test]
    fn tool_dispatch_recording_does_not_poison_agent_runtime_provenance() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));

        // 1. Genuine agent-turn delivery establishes placement provenance.
        let agent_delivery = serde_json::json!({
            "session_id": "sess-tool-poison",
            "turn_id": "turn-1",
            "agent_id": "agent-beacon",
            "delivery_hotel": "vps-jane",
            "delivery_node_id": "vps-jane-aiua-01",
            "delivery_target_role": "agent",
            "delivery_target_guest_id": "agent-beacon",
            "transport": "operator_chat",
        });
        IpcServer::record_session_activity_from_value(
            &graph,
            &agent_delivery,
            None,
            Some("running"),
            Some("agent"),
            "emit_task",
        );
        let session = graph
            .get_session("sess-tool-poison")
            .expect("session lookup")
            .expect("session exists");
        assert_eq!(
            session.summary_json["agent_runtime_provenance"]["delivery_target_guest_id"],
            "agent-beacon",
            "agent-turn delivery must establish placement provenance"
        );

        // 2. Tool dispatch to the life-graph-runner — delivery context names the RUNNER.
        let tool_dispatch = serde_json::json!({
            "session_id": "sess-tool-poison",
            "turn_id": "turn-1",
            "action": "tool_invoke",
            "tool_name": "life.observe",
            "agent_id": "agent-beacon",
            "delivery_hotel": "vps-jane",
            "delivery_node_id": "vps-jane-aiua-01",
            "delivery_target_role": "life-graph-runner",
            "delivery_target_guest_id": "vps-jane:life-graph-runner",
        });
        IpcServer::record_session_activity_from_value(
            &graph,
            &tool_dispatch,
            None,
            Some("running"),
            Some("life-graph-runner"),
            "emit_task",
        );
        let session = graph
            .get_session("sess-tool-poison")
            .expect("session lookup")
            .expect("session exists");
        assert_eq!(
            session.summary_json["agent_runtime_provenance"]["delivery_target_guest_id"],
            "agent-beacon",
            "tool dispatch must not overwrite agent placement provenance with the runner"
        );
        assert_eq!(
            session.summary_json["agent_runtime_provenance"]["delivery_target_role"],
            "agent"
        );
    }

    // Companion to the poisoning regression: a session whose FIRST recorded dispatch is
    // a tool invoke must not gain agent placement provenance at all, while a
    // role-incarnation routing-key delivery ("role:{agent}:{role}") still counts as a
    // genuine agent-turn delivery.
    #[test]
    fn provenance_gate_skips_tool_dispatch_but_accepts_role_routing_key() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));

        let tool_dispatch = serde_json::json!({
            "session_id": "sess-tool-first",
            "turn_id": "turn-1",
            "delivery_hotel": "vps-jane",
            "delivery_target_role": "life-graph-runner",
            "delivery_target_guest_id": "vps-jane:life-graph-runner",
        });
        IpcServer::record_session_activity_from_value(
            &graph,
            &tool_dispatch,
            None,
            Some("running"),
            Some("life-graph-runner"),
            "emit_task",
        );
        let session = graph
            .get_session("sess-tool-first")
            .expect("session lookup")
            .expect("session exists");
        assert!(
            session
                .summary_json
                .get("agent_runtime_provenance")
                .is_none(),
            "tool dispatch must not create agent placement provenance"
        );

        let role_delivery = serde_json::json!({
            "session_id": "sess-tool-first",
            "turn_id": "turn-2",
            "delivery_hotel": "vps-jane",
            "delivery_target_role": "role:agent-beacon:orchestrator",
            "delivery_target_guest_id": "agent-beacon:orchestrator",
        });
        IpcServer::record_session_activity_from_value(
            &graph,
            &role_delivery,
            None,
            Some("running"),
            Some("role:agent-beacon:orchestrator"),
            "emit_task",
        );
        let session = graph
            .get_session("sess-tool-first")
            .expect("session lookup")
            .expect("session exists");
        assert_eq!(
            session.summary_json["agent_runtime_provenance"]["delivery_target_guest_id"],
            "agent-beacon:orchestrator",
            "role-incarnation routing-key delivery must update placement provenance"
        );
    }
}
