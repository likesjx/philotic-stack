//! Philote-authored surfaces — runtime side (doc:desktop-generative-surfaces,
//! seams `surface-render-tools` + native lowering).
//!
//! `ui.surface.create|update|delete` build A2UI v0.9 messages, store them on the
//! hotel (`ApplySurfaceMessages`: the hotel mints the surface and action ids and
//! enforces ownership), then deliver to the turn's channel. Delivery reports an
//! honest outcome the model must repeat (say-do): `displayed` only when the
//! operator was actually shown the surface, `rejected` with the reason
//! otherwise. A tap on a lowered button (`sa:<action_id>`) comes back as a
//! structured surface-action turn.

use super::*;
use ansible_mesh_core::surface::lower::{LoweredSurface, lower_to_chat};
use ansible_mesh_core::surface::record::{SurfaceRecord, base_agent_id};
use ansible_mesh_core::surface::{A2UI_VERSION, ACTION_ID_KEY, CATALOG_ID};

/// Channels that can show a lowered surface as a message + inline keyboard.
fn transport_lowers_to_chat(transport: &str) -> bool {
    transport == "telegram"
}

/// What happened when the surface was delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SurfaceOutcome {
    Displayed { detail: String },
    Rejected { reason: String },
    Deleted,
}

impl SurfaceOutcome {
    fn label(&self) -> &'static str {
        match self {
            Self::Displayed { .. } => "displayed",
            Self::Rejected { .. } => "rejected",
            Self::Deleted => "deleted",
        }
    }
}

/// Turn the tool arguments into an A2UI message batch.
pub(super) fn surface_messages_for(
    tool_name: &str,
    args: &serde_json::Value,
) -> std::result::Result<(Option<String>, Option<String>, Vec<serde_json::Value>), String> {
    let surface_id = args
        .get("surface_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    // The hotel normalises every surfaceId, so a placeholder is fine here.
    let sid = surface_id.clone().unwrap_or_else(|| "new".into());
    let mut messages = Vec::new();
    match tool_name {
        "ui.surface.create" => {
            if surface_id.is_some() {
                return Err("ui.surface.create takes no surface_id; use ui.surface.update".into());
            }
            messages.push(serde_json::json!({"version": A2UI_VERSION,
                "createSurface": {"surfaceId": sid, "catalogId": CATALOG_ID}}));
            let components = args
                .get("components")
                .and_then(|v| v.as_array())
                .filter(|c| !c.is_empty())
                .ok_or(
                    "ui.surface.create needs a non-empty 'components' array including id 'root'",
                )?;
            messages.push(serde_json::json!({"version": A2UI_VERSION,
                "updateComponents": {"surfaceId": sid, "components": components}}));
            if let Some(data) = args.get("data_model").filter(|v| !v.is_null()) {
                messages.push(serde_json::json!({"version": A2UI_VERSION,
                    "updateDataModel": {"surfaceId": sid, "value": data}}));
            }
        }
        "ui.surface.update" => {
            if surface_id.is_none() {
                return Err("ui.surface.update needs 'surface_id'".into());
            }
            if let Some(components) = args.get("components").and_then(|v| v.as_array()) {
                if !components.is_empty() {
                    messages.push(serde_json::json!({"version": A2UI_VERSION,
                        "updateComponents": {"surfaceId": sid, "components": components}}));
                }
            }
            if let Some(data) = args.get("data_model").filter(|v| !v.is_null()) {
                messages.push(serde_json::json!({"version": A2UI_VERSION,
                    "updateDataModel": {"surfaceId": sid, "value": data}}));
            }
            if let Some(path) = args.get("data_path").and_then(|v| v.as_str()) {
                let mut body = serde_json::json!({"surfaceId": sid, "path": path});
                if let Some(value) = args.get("data_value") {
                    body["value"] = value.clone();
                }
                messages
                    .push(serde_json::json!({"version": A2UI_VERSION, "updateDataModel": body}));
            }
            if messages.is_empty() {
                return Err(
                    "ui.surface.update needs 'components', 'data_model', or 'data_path'".into(),
                );
            }
        }
        "ui.surface.delete" => {
            if surface_id.is_none() {
                return Err("ui.surface.delete needs 'surface_id'".into());
            }
            messages.push(serde_json::json!({"version": A2UI_VERSION,
                "deleteSurface": {"surfaceId": sid}}));
        }
        other => return Err(format!("unknown surface tool {other}")),
    }
    Ok((surface_id, title, messages))
}

/// The text the model sees. It states plainly what the operator saw.
pub(super) fn surface_tool_result_text(record: &SurfaceRecord, outcome: &SurfaceOutcome) -> String {
    let title = record.title.as_deref().unwrap_or("(untitled)");
    let actions: Vec<String> = record
        .actions()
        .iter()
        .map(|a| format!("{} ({})", a.event_name, a.component_id))
        .collect();
    let head = format!(
        "surface_id={} title=\"{}\" outcome={}",
        record.surface_id,
        title,
        outcome.label()
    );
    match outcome {
        SurfaceOutcome::Displayed { detail } => format!(
            "{head}\n{detail}\nButtons the operator can press: {}. A press arrives as a [surface action] message.",
            if actions.is_empty() {
                "none".into()
            } else {
                actions.join(", ")
            }
        ),
        SurfaceOutcome::Rejected { reason } => format!(
            "{head}\nThe surface is stored but the operator has NOT seen it: {reason}\n\
             Do not say you showed it. Either simplify it (Text, Column, Row, List, Card, \
             Divider, Button only) or describe the content in your reply instead."
        ),
        SurfaceOutcome::Deleted => {
            format!("{head}\nThe surface is deleted; its buttons no longer work.")
        }
    }
}

/// Observation text for a pressed surface button.
pub(super) fn surface_action_observation_text(
    record: &SurfaceRecord,
    action_id: &str,
) -> Option<String> {
    let action = record.find_action(action_id)?;
    let component = record.state.components.get(&action.component_id)?;
    let label = component
        .get("child")
        .and_then(|c| c.as_str())
        .and_then(|child| record.state.components.get(child))
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or(&action.event_name)
        .to_string();
    let mut context = component["action"]["event"]
        .get("context")
        .and_then(|c| c.as_object())
        .cloned()
        .unwrap_or_default();
    context.remove(ACTION_ID_KEY);
    // Resolve absolute data bindings so the philote sees values, not paths.
    for value in context.values_mut() {
        if let Some(path) = value.get("path").and_then(|p| p.as_str()) {
            if path.starts_with('/') {
                *value = record
                    .state
                    .data_model
                    .pointer(path)
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
            }
        }
    }
    let event = serde_json::json!({
        "surface_id": record.surface_id,
        "title": record.title,
        "action": action.event_name,
        "component": action.component_id,
        "label": label,
        "context": context,
    });
    Some(format!(
        "[surface action] The operator pressed \"{label}\" on your surface \"{}\". {event}\n\
         Handle this action now.",
        record.title.as_deref().unwrap_or(&record.surface_id)
    ))
}

impl AgentRuntime {
    /// `ui.surface.create|update|delete`.
    pub(super) async fn handle_surface_tool(
        &mut self,
        payload: ToolExecutionPayload,
    ) -> Result<()> {
        let (surface_id, title, messages) =
            match surface_messages_for(&payload.tool_name, &payload.arguments) {
                Ok(parts) => parts,
                Err(msg) => {
                    let err = TaskErrorPayload::ipc_failure("philote", "SURFACE_ARGS", msg.clone());
                    return self
                        .deliver_procedure_tool_result(payload, msg, Some(err))
                        .await;
                }
            };
        let transport = self
            .sessions
            .get(&payload.session_id)
            .map(|s| s.source.clone())
            .unwrap_or_default();
        let response = self
            .ipc_client
            .send_request(IpcRequest::ApplySurfaceMessages {
                surface_id,
                messages,
                title,
                session_id: Some(payload.session_id.clone()),
                chat_id: Some(payload.chat_id.clone()).filter(|c| !c.is_empty()),
                transport: Some(transport.clone()).filter(|t| !t.is_empty()),
            })
            .await;
        let record = match response {
            Ok(IpcResponse::Standard {
                ok: true,
                data: Some(data),
                ..
            }) => match serde_json::from_value::<SurfaceRecord>(data) {
                Ok(record) => record,
                Err(e) => {
                    let err = TaskErrorPayload::ipc_failure(
                        "aiua",
                        "SURFACE_DECODE",
                        format!("{}: malformed surface record — {e}", payload.tool_name),
                    );
                    return self
                        .deliver_procedure_tool_result(payload, err.display_message(), Some(err))
                        .await;
                }
            },
            Ok(IpcResponse::Standard {
                ok: false,
                code,
                message,
                ..
            }) => {
                // Validation refusals carry the exact catalog reason; the model
                // can fix the surface and retry.
                let err = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                return self
                    .deliver_procedure_tool_result(payload, err.display_message(), Some(err))
                    .await;
            }
            Ok(other) => {
                let err = TaskErrorPayload::ipc_failure(
                    "aiua",
                    "UNEXPECTED_RESPONSE",
                    format!("{}: unexpected hotel response {other:?}", payload.tool_name),
                );
                return self
                    .deliver_procedure_tool_result(payload, err.display_message(), Some(err))
                    .await;
            }
            Err(e) => {
                let err = TaskErrorPayload::transport_error(
                    "philote",
                    format!("{}: IPC transport error — {e}", payload.tool_name),
                );
                return self
                    .deliver_procedure_tool_result(payload, err.display_message(), Some(err))
                    .await;
            }
        };

        let outcome = if record.status == ansible_mesh_core::surface::record::SurfaceStatus::Deleted
        {
            SurfaceOutcome::Deleted
        } else {
            self.deliver_surface(&payload, &transport, &record).await
        };
        info!(
            session_id = %payload.session_id,
            surface_id = %record.surface_id,
            outcome = outcome.label(),
            "surface tool applied"
        );
        let content = surface_tool_result_text(&record, &outcome);
        self.deliver_procedure_tool_result(payload, content, None)
            .await
    }

    /// Show the surface on the turn's channel, or say honestly why not.
    async fn deliver_surface(
        &mut self,
        payload: &ToolExecutionPayload,
        transport: &str,
        record: &SurfaceRecord,
    ) -> SurfaceOutcome {
        if !transport_lowers_to_chat(transport) {
            return SurfaceOutcome::Rejected {
                reason: format!(
                    "the '{}' channel has no surface renderer yet (the web renderer, Telegram Mini \
                     App and Apple view are later slices)",
                    if transport.is_empty() {
                        "unknown"
                    } else {
                        transport
                    }
                ),
            };
        }
        let lowered: LoweredSurface = match lower_to_chat(&record.state) {
            Ok(lowered) => lowered,
            Err(reason) => return SurfaceOutcome::Rejected { reason: reason.0 },
        };
        let buttons: usize = lowered.button_rows.iter().map(Vec::len).sum();
        let reply = FinalReplyPayload {
            action: "send_reply",
            session_id: payload.session_id.clone(),
            turn_id: payload.turn_id.clone(),
            chat_id: payload.chat_id.clone(),
            content: lowered.text.clone(),
            audio_artifact: None,
            send_text_caption: false,
            reply_markup: lowered.inline_keyboard(),
        };
        let task_json = match serde_json::to_string(&reply) {
            Ok(json) => json,
            Err(e) => {
                return SurfaceOutcome::Rejected {
                    reason: format!("could not encode the chat message: {e}"),
                };
            }
        };
        match self
            .ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: payload.final_reply_to.clone(),
                target_role: payload.final_reply_role.clone(),
                target_guest_id: payload.final_reply_guest_id.clone(),
                task_json,
            })
            .await
        {
            Ok(IpcResponse::Standard {
                ok: false, message, ..
            }) => SurfaceOutcome::Rejected {
                reason: format!("the hotel refused to deliver it: {message}"),
            },
            Ok(IpcResponse::Error(message)) => SurfaceOutcome::Rejected {
                reason: format!("the hotel refused to deliver it: {message}"),
            },
            Err(e) => SurfaceOutcome::Rejected {
                reason: format!("delivery failed: {e}"),
            },
            Ok(_) => SurfaceOutcome::Displayed {
                detail: format!(
                    "Sent to {transport} as a message with {buttons} button{}.",
                    if buttons == 1 { "" } else { "s" }
                ),
            },
        }
    }

    /// Resolve a pressed lowered button (`sa:<action_id>`) into the observation
    /// text the philote handles as the user's turn. `None` when no live surface
    /// of this agent in this session carries the action.
    pub(super) async fn surface_action_observation(
        &mut self,
        session_id: &str,
        action_id: &str,
    ) -> Option<String> {
        let response = self
            .ipc_client
            .send_request(IpcRequest::ListSurfaces {
                owner_agent_id: Some(base_agent_id(&self.agent_id).to_string()),
                session_id: Some(session_id.to_string()),
                include_deleted: false,
                limit: Some(50),
            })
            .await;
        let Ok(IpcResponse::Standard {
            ok: true,
            data: Some(data),
            ..
        }) = response
        else {
            warn!(session_id = %session_id, "surface action: could not list surfaces");
            return None;
        };
        let records: Vec<SurfaceRecord> =
            serde_json::from_value(data.get("surfaces").cloned().unwrap_or_default())
                .unwrap_or_default();
        records
            .iter()
            .find_map(|record| surface_action_observation_text(record, action_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::surface::record::{ApplyContext, apply_surface_messages};

    fn record() -> SurfaceRecord {
        let args = serde_json::json!({
            "title": "Guests",
            "components": [
                {"id": "root", "component": "Column", "children": ["t", "go"]},
                {"id": "t", "component": "Text", "text": {"path": "/guest"}},
                {"id": "l", "component": "Text", "text": "Restart"},
                {"id": "go", "component": "Button", "child": "l",
                 "action": {"event": {"name": "restart_guest",
                    "context": {"guest": {"path": "/guest"}, "action_id": "forged"}}}}
            ],
            "data_model": {"guest": "beacon"}
        });
        let (sid, title, messages) = surface_messages_for("ui.surface.create", &args).unwrap();
        assert!(sid.is_none());
        let ctx = ApplyContext {
            caller_guest_id: "agent-beacon".into(),
            source_hotel: "vps-jane".into(),
            title,
            session_id: Some("sess-1".into()),
            chat_id: Some("555".into()),
            transport: Some("telegram".into()),
            now: 1,
        };
        apply_surface_messages(
            None,
            || "s1".into(),
            &ctx,
            &messages,
            || "a000000000001".into(),
        )
        .unwrap()
    }

    #[test]
    fn create_args_become_a_valid_batch() {
        let r = record();
        assert_eq!(r.title.as_deref(), Some("Guests"));
        assert_eq!(r.state.data_model["guest"], "beacon");
        assert_eq!(r.actions()[0].action_id, "a000000000001");
    }

    #[test]
    fn tool_argument_errors_are_explained() {
        assert!(surface_messages_for("ui.surface.create", &serde_json::json!({})).is_err());
        assert!(
            surface_messages_for(
                "ui.surface.update",
                &serde_json::json!({"surface_id": "s1"})
            )
            .is_err()
        );
        assert!(surface_messages_for("ui.surface.delete", &serde_json::json!({})).is_err());
        let (_, _, update) = surface_messages_for(
            "ui.surface.update",
            &serde_json::json!({"surface_id": "s1", "data_path": "/guest", "data_value": "jane"}),
        )
        .unwrap();
        assert_eq!(update[0]["updateDataModel"]["path"], "/guest");
    }

    #[test]
    fn result_text_never_claims_display_on_rejection() {
        let r = record();
        let rejected = surface_tool_result_text(
            &r,
            &SurfaceOutcome::Rejected {
                reason: "TextField needs the web renderer".into(),
            },
        );
        assert!(rejected.contains("outcome=rejected"));
        assert!(rejected.contains("has NOT seen it"));
        let shown = surface_tool_result_text(
            &r,
            &SurfaceOutcome::Displayed {
                detail: "Sent to telegram as a message with 1 button.".into(),
            },
        );
        assert!(shown.contains("outcome=displayed"));
        assert!(shown.contains("restart_guest (go)"));
    }

    #[test]
    fn pressed_button_becomes_a_resolved_observation() {
        let r = record();
        let text = surface_action_observation_text(&r, "a000000000001").unwrap();
        assert!(text.starts_with("[surface action]"));
        assert!(text.contains("\"Restart\""));
        assert!(text.contains("\"guest\":\"beacon\""), "{text}");
        assert!(
            !text.contains("a000000000001"),
            "action id must not leak into the prompt"
        );
        assert!(surface_action_observation_text(&r, "a999").is_none());
    }

    #[test]
    fn only_telegram_lowers_today() {
        assert!(transport_lowers_to_chat("telegram"));
        assert!(!transport_lowers_to_chat("discord"));
        assert!(!transport_lowers_to_chat("mcp"));
    }
}
