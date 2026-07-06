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
        {
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
            repaired += 1;
        }
        if repaired > 0 {
            info!(
                repaired,
                min_age_secs, "RepairStaleSessionTurns: repaired zombie turns"
            );
        }
        IpcResponse::success("repair", Some(serde_json::json!({"repaired": repaired})))
    }
}

fn merge_turn_status(current: &str, incoming: Option<&str>) -> Option<String> {
    let incoming = incoming?;
    if matches!(current, "completed" | "failed") && !matches!(incoming, "completed" | "failed") {
        return Some(current.to_string());
    }
    Some(incoming.to_string())
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
}
