use crate::session::{TaskRunnerBaseConfig, ToolDefinition};
use philotic_client::{HandoffBundle, ReturnRoute, TaskErrorPayload};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TransportAttachment {
    pub kind: String,
    pub file_id: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
    #[serde(default)]
    pub telegram_file_path: Option<String>,
    #[serde(default)]
    pub blob_id: Option<String>,
    #[serde(default)]
    pub blob_download_url: Option<String>,
    #[serde(default)]
    pub transport_error: Option<String>,
    /// Inline audio payload — base64-encoded i16 LE PCM samples.
    /// Set by Discord voice bridge (voice.dialogue) to bypass blob store.
    #[serde(default)]
    pub inline_audio_b64: Option<String>,
    #[serde(default)]
    pub inline_audio_sample_rate: Option<u32>,
    #[serde(default)]
    pub inline_audio_channels: Option<u16>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InboundTaskPayload {
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub agent_action: Option<serde_json::Value>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub sender_id: Option<String>,
    #[serde(default)]
    pub sender_username: Option<String>,
    /// The Telegram chat type: "private", "group", "supergroup", or "channel".
    #[serde(default)]
    pub chat_type: Option<String>,
    /// The sender's first name (display name for group chat attribution).
    #[serde(default)]
    pub sender_first_name: Option<String>,
    #[serde(default)]
    pub message_kind: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub handoff_bundle: Option<HandoffBundle>,
    #[serde(default)]
    pub attachments: Vec<TransportAttachment>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub callback_data: Option<String>,
    #[serde(default)]
    pub raw_transport_event: Option<serde_json::Value>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub arguments: Option<serde_json::Value>,
    #[serde(default)]
    pub final_reply_to: Option<String>,
    #[serde(default)]
    pub final_reply_role: Option<String>,
    #[serde(default)]
    pub final_reply_guest_id: Option<String>,
    #[serde(default)]
    pub error: Option<TaskErrorPayload>,
    /// Result payload from a datasource_response task (graph-datasource etc.).
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    /// Capability identifier echoed back on datasource_response tasks.
    #[serde(default)]
    pub capability: Option<String>,
    /// Inline PCM audio from Discord voice bridge (voice.dialogue tasks).
    #[serde(default)]
    pub pcm_b64: Option<String>,
    #[serde(default)]
    pub sample_rate: Option<u32>,
    #[serde(default)]
    pub speaker_ssrc: Option<u32>,
    /// The [`Exosome`] envelope present on `paracrine_request` and
    /// `paracrine_response` tasks. Carries the paracrine_id and routing hint.
    #[serde(default)]
    pub exosome: Option<serde_json::Value>,
    /// Typed low-agency signal delivered by the paracrine loop, often from a
    /// cron-backed heartbeat. These are observed by role-type subscribers and
    /// should not automatically enter the normal conversational model path.
    #[serde(default)]
    pub paracrine_signal: Option<serde_json::Value>,
    /// When true (set by membrane variants with per-route approval gates,
    /// e.g. membrane-mcp), the philote must park this turn as WaitingApproval
    /// before model invocation and emit `approval_required` back to the sender.
    #[serde(default)]
    pub requires_approval: bool,
}

// Transitional note: older emitters may still carry failures in
// `agent_action.model_result.error`, but the preferred cross-component contract is the
// top-level `error` envelope on inbound tasks.

#[derive(Debug, Clone, Serialize)]
pub struct LigandEnvelope {
    /// Semantic intent of the routed envelope, e.g. `tool_planning`.
    pub signal_type: String,
    /// Human-readable purpose text for the specialist receptor path.
    pub purpose: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visible_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visible_tool_classes: Vec<String>,
    /// Flexible posture metadata for the router / approval layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_posture: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelRequestPayload {
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_class: Option<String>,
    pub session_id: String,
    pub turn_id: String,
    pub prompt: String,
    pub user_content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_projection: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affordances: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<TransportAttachment>,
    pub tools_for_model: Vec<ToolDefinition>,
    /// Forwarded verbatim to the model controller as `response_contract`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_contract: Option<serde_json::Value>,
    /// Forwarded verbatim to the model controller as `response_route`.
    /// This is the canonical route decision carried by the model-call envelope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_route: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ligand: Option<LigandEnvelope>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub provider_options: serde_json::Map<String, serde_json::Value>,
    pub chat_id: String,
    pub reply_to: String,
    pub reply_role: String,
    pub final_reply_to: String,
    pub final_reply_role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_reply_guest_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FinalReplyPayload {
    pub action: &'static str,
    pub session_id: String,
    pub turn_id: String,
    pub chat_id: String,
    pub content: String,
    /// Serialized `AudioArtifact` JSON from a voice synthesis response, if TTS is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_artifact: Option<String>,
    /// When true and `audio_artifact` is present, the transport should also deliver the text
    /// content alongside the audio (e.g. as a Telegram caption).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub send_text_caption: bool,
    /// Optional transport-level reply markup (e.g. Telegram inline keyboard JSON).
    /// Transports that don't understand it should ignore it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_markup: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PartialReplyPayload {
    pub action: &'static str,
    pub session_id: String,
    pub turn_id: String,
    pub chat_id: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TurnEventPayload {
    pub action: &'static str,
    /// Value of `TurnPhase::as_str()` for the new phase.
    pub event: String,
    pub session_id: String,
    pub turn_id: String,
    pub chat_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial_content: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TurnStatusPayload {
    pub action: &'static str,
    pub session_id: String,
    pub turn_id: String,
    pub chat_id: String,
    /// Human-readable status, e.g. "Searching the web..."
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskRunnerOverlay {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_read_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_search_results: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolExecutionPayload {
    pub action: &'static str,
    pub session_id: String,
    pub turn_id: String,
    pub chat_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub execution_mode: String,
    /// The agent that issued this tool call — used by tool-runners that need
    /// to scope operations to the agent's identity (e.g. memory vault routing).
    pub agent_id: String,
    /// The human user associated with this session, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incarnation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hotel_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_runner_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_runner_config: Option<TaskRunnerBaseConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_runner_overlay: Option<TaskRunnerOverlay>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_route: Option<ReturnRoute>,
    pub reply_to: String,
    pub reply_role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_guest_id: Option<String>,
    pub final_reply_to: String,
    pub final_reply_role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_reply_guest_id: Option<String>,
}

impl InboundTaskPayload {
    pub fn is_model_response(&self) -> bool {
        self.action.as_deref() == Some("model_response")
    }

    pub fn is_tool_result(&self) -> bool {
        self.action.as_deref() == Some("tool_result")
    }

    pub fn session_id_or_default(&self, agent_id: &str) -> String {
        if let Some(session_id) = self.session_id.as_ref().filter(|s| !s.is_empty()) {
            return session_id.clone();
        }

        let source = self
            .transport
            .as_deref()
            .or(self.source.as_deref())
            .unwrap_or("unknown");
        let chat_id = self.chat_id.as_deref().unwrap_or("ephemeral");
        if let Some(thread_id) = self.thread_id.as_deref().filter(|id| !id.is_empty()) {
            return format!("{source}:{chat_id}:{thread_id}:{agent_id}");
        }

        format!("{source}:{chat_id}:{agent_id}")
    }
}

#[cfg(test)]
mod tests {
    use super::{InboundTaskPayload, TaskRunnerOverlay, ToolExecutionPayload, TransportAttachment};
    use crate::session::TaskRunnerBaseConfig;
    use philotic_client::ReturnRoute;

    #[test]
    fn session_id_defaults_from_source_and_chat() {
        let payload = InboundTaskPayload {
            action: None,
            agent_action: None,
            source: Some("telegram".into()),
            session_id: None,
            turn_id: None,
            transport: Some("telegram".into()),
            chat_id: Some("1234".into()),
            thread_id: None,
            sender_id: None,
            sender_username: None,
            message_kind: None,
            content: Some("hello".into()),
            attachments: Vec::new(),
            command: None,
            callback_data: None,
            raw_transport_event: None,
            tool_name: None,
            arguments: None,
            final_reply_to: None,
            final_reply_role: None,
            final_reply_guest_id: None,
            handoff_bundle: None,
            error: None,
            ..Default::default()
        };

        assert_eq!(
            payload.session_id_or_default("agent-jane-01"),
            "telegram:1234:agent-jane-01"
        );
    }

    #[test]
    fn model_response_detection_is_action_based() {
        let payload = InboundTaskPayload {
            action: Some("model_response".into()),
            agent_action: None,
            source: None,
            session_id: None,
            turn_id: None,
            transport: None,
            chat_id: None,
            thread_id: None,
            sender_id: None,
            sender_username: None,
            message_kind: None,
            content: None,
            attachments: Vec::new(),
            command: None,
            callback_data: None,
            raw_transport_event: None,
            tool_name: None,
            arguments: None,
            final_reply_to: None,
            final_reply_role: None,
            final_reply_guest_id: None,
            handoff_bundle: None,
            error: None,
            ..Default::default()
        };

        assert!(payload.is_model_response());
    }

    #[test]
    fn tool_result_detection_is_action_based() {
        let payload = InboundTaskPayload {
            action: Some("tool_result".into()),
            agent_action: None,
            source: None,
            session_id: None,
            turn_id: None,
            transport: None,
            chat_id: None,
            thread_id: None,
            sender_id: None,
            sender_username: None,
            message_kind: None,
            content: None,
            attachments: Vec::new(),
            command: None,
            callback_data: None,
            raw_transport_event: None,
            tool_name: Some("echo".into()),
            arguments: None,
            final_reply_to: None,
            final_reply_role: None,
            final_reply_guest_id: None,
            handoff_bundle: None,
            error: None,
            ..Default::default()
        };

        assert!(payload.is_tool_result());
    }

    #[test]
    fn transport_envelope_fields_deserialize() {
        let payload: InboundTaskPayload = serde_json::from_value(serde_json::json!({
            "transport": "telegram",
            "source": "telegram",
            "session_id": "telegram:123:agent-jane-01",
            "turn_id": "telegram-update-1",
            "chat_id": "123",
            "thread_id": "77",
            "sender_id": "999",
            "sender_username": "jared",
            "message_kind": "voice",
            "content": "voice transcript",
            "command": "/status",
            "callback_data": "approve:123",
            "attachments": [{
                "kind": "voice",
                "file_id": "file-1",
                "mime_type": "audio/ogg"
            }],
            "raw_transport_event": {
                "update_id": 1
            }
        }))
        .expect("payload should deserialize");

        assert_eq!(payload.transport.as_deref(), Some("telegram"));
        assert_eq!(payload.thread_id.as_deref(), Some("77"));
        assert_eq!(payload.sender_id.as_deref(), Some("999"));
        assert_eq!(payload.sender_username.as_deref(), Some("jared"));
        assert_eq!(payload.message_kind.as_deref(), Some("voice"));
        assert_eq!(payload.command.as_deref(), Some("/status"));
        assert_eq!(payload.callback_data.as_deref(), Some("approve:123"));
        assert_eq!(
            payload.attachments,
            vec![TransportAttachment {
                kind: "voice".into(),
                file_id: "file-1".into(),
                mime_type: Some("audio/ogg".into()),
                file_name: None,
                file_size: None,
                telegram_file_path: None,
                blob_id: None,
                blob_download_url: None,
                transport_error: None,
                ..Default::default()
            }]
        );
        assert_eq!(
            payload.raw_transport_event,
            Some(serde_json::json!({ "update_id": 1 }))
        );
    }

    #[test]
    fn paracrine_signal_field_deserializes() {
        let payload: InboundTaskPayload = serde_json::from_value(serde_json::json!({
            "action": "paracrine_signal",
            "transport": "cron",
            "paracrine_signal": {
                "signal_id": "cron:job-1:1234",
                "signal_type": "life_graph.attention_scan",
                "scope": "life_graph",
                "target_role_type": "attention-steward"
            }
        }))
        .expect("payload should deserialize");

        assert_eq!(payload.action.as_deref(), Some("paracrine_signal"));
        assert_eq!(
            payload.paracrine_signal.as_ref().and_then(|signal| {
                signal
                    .get("target_role_type")
                    .and_then(serde_json::Value::as_str)
            }),
            Some("attention-steward")
        );
    }

    #[test]
    fn session_id_defaults_include_thread_when_present() {
        let payload = InboundTaskPayload {
            action: None,
            agent_action: None,
            source: Some("telegram".into()),
            session_id: None,
            turn_id: None,
            transport: Some("telegram".into()),
            chat_id: Some("1234".into()),
            thread_id: Some("42".into()),
            sender_id: None,
            sender_username: None,
            message_kind: Some("text".into()),
            content: Some("hello".into()),
            attachments: Vec::new(),
            command: Some("/status".into()),
            callback_data: None,
            raw_transport_event: None,
            tool_name: None,
            arguments: None,
            final_reply_to: None,
            final_reply_role: None,
            final_reply_guest_id: None,
            handoff_bundle: None,
            error: None,
            ..Default::default()
        };

        assert_eq!(
            payload.session_id_or_default("agent-jane-01"),
            "telegram:1234:42:agent-jane-01"
        );
    }

    #[test]
    fn tool_execution_payload_can_carry_task_runner_overlay() {
        let payload = ToolExecutionPayload {
            action: "execute_tool",
            session_id: "sess-1".into(),
            turn_id: "turn-1".into(),
            chat_id: "123".into(),
            tool_name: "workspace.read".into(),
            arguments: serde_json::json!({ "path": "README.md" }),
            execution_mode: "pinned".into(),
            runner_id: Some("task-runner-workspace-01".into()),
            incarnation_id: Some("task-runner-workspace-01".into()),
            hotel_id: Some("local-aiua-01".into()),
            environment_id: Some("workspace://main".into()),
            task_runner_kind: Some("workspace".into()),
            task_runner_config: Some(TaskRunnerBaseConfig {
                default_workspace_ref: Some("workspace://main".into()),
                allowed_tools: Some(vec!["workspace.read".into(), "workspace.search".into()]),
                max_read_bytes: Some(8192),
                max_search_results: Some(25),
            }),
            selection_reason: Some("preferred_environment_live".into()),
            workspace_ref: Some("workspace://main".into()),
            task_runner_overlay: Some(TaskRunnerOverlay {
                workspace_ref: Some("workspace://main".into()),
                allowed_tools: Some(vec!["workspace.read".into(), "workspace.search".into()]),
                max_read_bytes: Some(4096),
                max_search_results: Some(25),
            }),
            agent_id: "agent-jane-01".into(),
            user_id: None,
            return_route: Some(ReturnRoute {
                node: "local-aiua-01".into(),
                role: "agent".into(),
                guest_id: Some("agent-jane-01".into()),
                session_id: Some("sess-1".into()),
                turn_id: Some("turn-1".into()),
                correlation_id: None,
            }),
            reply_to: "local-aiua-01".into(),
            reply_role: "agent".into(),
            reply_guest_id: Some("agent-jane-01".into()),
            final_reply_to: "local-aiua-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
        };

        let json = serde_json::to_value(&payload).expect("serialize payload");
        assert_eq!(json["task_runner_kind"], "workspace");
        assert_eq!(
            json["task_runner_config"]["default_workspace_ref"],
            "workspace://main"
        );
        assert_eq!(json["task_runner_config"]["max_read_bytes"], 8192);
        assert_eq!(
            json["task_runner_overlay"]["workspace_ref"],
            "workspace://main"
        );
        assert_eq!(json["task_runner_overlay"]["max_read_bytes"], 4096);
        assert_eq!(json["task_runner_overlay"]["max_search_results"], 25);
        assert_eq!(json["return_route"]["guest_id"], "agent-jane-01");
        assert_eq!(json["reply_guest_id"], "agent-jane-01");
    }
}
