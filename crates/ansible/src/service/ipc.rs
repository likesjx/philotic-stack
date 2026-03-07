use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
use ansible_mesh_core::storage::{
    GraphStorage, SessionEventRecord, SessionParticipantRecord, SessionRecord, SessionTurnRecord,
};
use philotic_client::{IpcRequest, IpcResponse};
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};
use uuid::Uuid;
use crate::LedgerCommand;
use std::collections::HashMap;
use std::sync::Arc;

type InboxRegistry = Arc<Mutex<HashMap<String, Vec<RoleSubscriber>>>>;

#[derive(Clone)]
struct RoleSubscriber {
    conn_id: Uuid,
    guest_id: String,
    supported_tools: Vec<String>,
    tx: mpsc::UnboundedSender<IpcResponse>,
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolRunnerRegistryEntry {
    guest_id: String,
    supported_tools: Vec<String>,
    last_seen_at: u64,
}

pub struct IpcServer {
    socket_path: String,
    dispatcher_tx: mpsc::Sender<LedgerCommand>,
    graph: Arc<dyn GraphStorage>,
    inboxes: InboxRegistry,
}

impl IpcServer {
    async fn write_frame<W: AsyncWriteExt + Unpin>(
        writer: &mut W,
        payload: &[u8],
    ) -> std::io::Result<()> {
        let len = u32::try_from(payload.len())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large"))?;
        writer.write_all(&len.to_be_bytes()).await?;
        writer.write_all(payload).await?;
        Ok(())
    }

    async fn read_frame<R: AsyncReadExt + Unpin>(
        reader: &mut R,
    ) -> std::io::Result<Option<Vec<u8>>> {
        let mut len_buf = [0u8; 4];
        match reader.read_exact(&mut len_buf).await {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(err) => return Err(err),
        }

        let len = u32::from_be_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).await?;
        Ok(Some(buf))
    }

    pub fn new(socket_path: impl Into<String>, dispatcher_tx: mpsc::Sender<LedgerCommand>, graph: Arc<dyn GraphStorage>) -> Self {
        Self {
            socket_path: socket_path.into(),
            dispatcher_tx,
            graph,
            inboxes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let path = Path::new(&self.socket_path);
        
        if path.exists() {
            std::fs::remove_file(path)?;
        }

        let listener = UnixListener::bind(path)?;
        info!("Hotel Front Desk (UDS) listening on: {}", self.socket_path);

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let dispatcher = self.dispatcher_tx.clone();
                    let graph = self.graph.clone();
                    let inboxes = self.inboxes.clone();
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_client(stream, dispatcher, graph, inboxes).await {
                            error!("IPC client connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    error!("IPC listener accept error: {}", e);
                }
            }
        }
    }

    async fn handle_client(stream: UnixStream, dispatcher_tx: mpsc::Sender<LedgerCommand>, graph: Arc<dyn GraphStorage>, inboxes: InboxRegistry) -> anyhow::Result<()> {
        let conn_id = Uuid::new_v4();
        let (mut reader, mut writer) = stream.into_split();
        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<IpcResponse>();
        let write_task = tokio::spawn(async move {
            while let Some(response) = outbound_rx.recv().await {
                let res_bytes = match serde_json::to_vec(&response) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        error!("Failed to serialize IPC response: {}", e);
                        continue;
                    }
                };
                if let Err(e) = Self::write_frame(&mut writer, &res_bytes).await {
                    return Err(e);
                }
            }
            Ok::<(), std::io::Error>(())
        });

        let mut subscribed_roles = Vec::new();
        loop {
            match Self::read_frame(&mut reader).await {
                Ok(None) => {
                    Self::remove_subscriptions(&inboxes, conn_id, &subscribed_roles).await;
                    let _ = write_task.await;
                    return Ok(());
                }
                Ok(Some(frame)) => {
                    match serde_json::from_slice::<IpcRequest>(&frame) {
                        Ok(req) => {
                            let response = Self::process_request(
                                req,
                                &dispatcher_tx,
                                graph.as_ref(),
                                &inboxes,
                                conn_id,
                                &outbound_tx,
                                &mut subscribed_roles,
                            )
                            .await;
                            let _ = outbound_tx.send(response);
                        }
                        Err(e) => {
                            warn!("Malformed IPC request payload: {}", e);
                            let _ = outbound_tx.send(IpcResponse::error("unknown", "MALFORMED_PAYLOAD", e.to_string()));
                        }
                    }
                }
                Err(e) => {
                    Self::remove_subscriptions(&inboxes, conn_id, &subscribed_roles).await;
                    let _ = write_task.await;
                    return Err(e.into());
                }
            }
        }
    }

    async fn add_subscription(
        inboxes: &InboxRegistry,
        role: &str,
        conn_id: Uuid,
        guest_id: &str,
        supported_tools: &[String],
        tx: &mpsc::UnboundedSender<IpcResponse>,
        subscribed_roles: &mut Vec<String>,
    ) {
        let mut guard = inboxes.lock().await;
        let entry = guard.entry(role.to_string()).or_default();
        if !entry.iter().any(|subscriber| subscriber.conn_id == conn_id) {
            entry.push(RoleSubscriber {
                conn_id,
                guest_id: guest_id.to_string(),
                supported_tools: supported_tools.to_vec(),
                tx: tx.clone(),
            });
        }
        if !subscribed_roles.iter().any(|existing| existing == role) {
            subscribed_roles.push(role.to_string());
        }
    }

    async fn remove_subscriptions(inboxes: &InboxRegistry, conn_id: Uuid, subscribed_roles: &[String]) {
        let mut guard = inboxes.lock().await;
        for role in subscribed_roles {
            if let Some(subscribers) = guard.get_mut(role) {
                subscribers.retain(|subscriber| subscriber.conn_id != conn_id);
            }
        }
        guard.retain(|_, subscribers| !subscribers.is_empty());
    }

    async fn deliver_inbound_task(inboxes: &InboxRegistry, target_role: &str, task_id: Uuid, task_json: String) {
        let subscribers = {
            let guard = inboxes.lock().await;
            guard.get(target_role).cloned().unwrap_or_default()
        };

        if subscribers.is_empty() {
            warn!("No local inbox subscribers for role '{}'; task {} stays ledger-only for now.", target_role, task_id);
            return;
        }

        let response = IpcResponse::InboundTask {
            source_node: "local-ansible-01".into(),
            task_id,
            task_json,
        };

        let mut stale = Vec::new();
        for subscriber in subscribers {
            if subscriber.tx.send(response.clone()).is_err() {
                stale.push(subscriber.conn_id);
            }
        }

        if !stale.is_empty() {
            let mut guard = inboxes.lock().await;
            if let Some(entries) = guard.get_mut(target_role) {
                entries.retain(|subscriber| !stale.contains(&subscriber.conn_id));
            }
        }
    }

    async fn process_request(
        req: IpcRequest,
        dispatcher_tx: &mpsc::Sender<LedgerCommand>,
        graph: &dyn GraphStorage,
        inboxes: &InboxRegistry,
        conn_id: Uuid,
        outbound_tx: &mpsc::UnboundedSender<IpcResponse>,
        subscribed_roles: &mut Vec<String>,
    ) -> IpcResponse {
        match req {
            IpcRequest::Register(identity) => {
                info!("Guest registered over UDS: [{}] Role: {}", identity.guest_id, identity.role);
                Self::add_subscription(
                    inboxes,
                    &identity.role,
                    conn_id,
                    &identity.guest_id,
                    &identity.supported_tools,
                    outbound_tx,
                    subscribed_roles,
                )
                .await;
                if identity.role == "tool" {
                    if let Err(err) = Self::upsert_tool_runner_registry_entry(graph, &identity) {
                        error!("Failed to persist tool runner registry entry: {}", err);
                    }
                }
                IpcResponse::success("reg", None)
            }
            IpcRequest::GetConfig { key } => {
                info!("GetConfig requested: {}", key);
                if let Some(session_id) = key.strip_prefix("__session_snapshot__:") {
                    match Self::compose_session_snapshot(graph, inboxes, session_id).await {
                        Ok(value) => {
                            return IpcResponse::ConfigData {
                                key,
                                value_json: value.map(|v| v.to_string()),
                            };
                        }
                        Err(e) => {
                            error!("Failed to compose session snapshot: {}", e);
                            return IpcResponse::error("config", "CONFIG_ERROR", e.to_string());
                        }
                    }
                }
                if let Some((agent_id, memory_type)) = key
                    .strip_prefix("__apartment__:")
                    .and_then(|rest| rest.split_once(':'))
                {
                    match graph.get_apartment(agent_id, memory_type) {
                        Ok(value) => {
                            return IpcResponse::ConfigData {
                                key,
                                value_json: value.map(|v| v.to_string()),
                            };
                        }
                        Err(e) => {
                            error!("Failed to load apartment from GraphStorage: {}", e);
                            return IpcResponse::error("config", "CONFIG_ERROR", e.to_string());
                        }
                    }
                }
                match graph.get_config_value(&key) {
                    Ok(value_json) => IpcResponse::ConfigData { key, value_json },
                    Err(e) => {
                        error!("Failed to load config key from GraphStorage: {}", e);
                        IpcResponse::error("config", "CONFIG_ERROR", e.to_string())
                    }
                }
            }
            IpcRequest::PublishMessage { target_role, payload } => {
                info!("PublishMessage for role: {}", target_role);
                let task_id = Uuid::new_v4();
                let payload_json = payload.to_string();
                Self::record_session_activity_from_value(
                    graph,
                    &payload,
                    Some(task_id),
                    None,
                    Some(&target_role),
                    "publish_message",
                );
                let env = EventEnvelope {
                    event_id: task_id,
                    seq: 0, // Set by the sequence manager in PORT-BP-003
                    source_node_id: "local-ansible-01".into(),
                    source_agent_id: "unknown".into(), // Will be pulled from connection context
                    target_agent_id: Some(target_role.clone()),
                    kind: EventKind::TaskInvoke,
                    corr_id: "pub".into(),
                    attempt: 0,
                    created_at: 0,
                    expires_at: None,
                    payload: EventPayload::Inline { data: payload_json.clone() },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
                Self::deliver_inbound_task(inboxes, &target_role, task_id, payload_json).await;
                IpcResponse::success("pub", None)
            }
            IpcRequest::CreateTask { target_role, payload } => {
                info!("CreateTask for role: {}", target_role);
                let task_id = Uuid::new_v4();
                let payload_json = payload.to_string();
                Self::record_session_activity_from_value(
                    graph,
                    &payload,
                    Some(task_id),
                    Some("queued"),
                    Some(&target_role),
                    "create_task",
                );
                let env = EventEnvelope {
                    event_id: task_id,
                    seq: 0,
                    source_node_id: "local-ansible-01".into(),
                    source_agent_id: "unknown".into(),
                    target_agent_id: Some(target_role.clone()),
                    kind: EventKind::TaskInvoke,
                    corr_id: "create".into(),
                    attempt: 0,
                    created_at: 0,
                    expires_at: None,
                    payload: EventPayload::Inline { data: payload_json.clone() },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
                Self::deliver_inbound_task(inboxes, &target_role, task_id, payload_json).await;
                IpcResponse::success("create", Some(serde_json::json!({ "task_id": task_id.to_string() })))
            }
            IpcRequest::AckEvent { event_id } => {
                info!("AckEvent for: {}", event_id);
                IpcResponse::success("ack", None)
            }
            IpcRequest::UpdateTask { task_id, state, payload } => {
                info!("UpdateTask for: {} to state: {}", task_id, state);
                Self::record_session_activity_from_value(
                    graph,
                    &payload,
                    None,
                    Some(&state),
                    None,
                    "update_task",
                );
                let env = EventEnvelope {
                    event_id: Uuid::new_v4(),
                    seq: 0,
                    source_node_id: "local-ansible-01".into(),
                    source_agent_id: "unknown".into(),
                    target_agent_id: None,
                    kind: EventKind::TaskInvoke, // Or potentially a new TaskUpdate kind if required
                    corr_id: task_id.to_string(),
                    attempt: 0,
                    created_at: 0,
                    expires_at: None,
                    payload: EventPayload::Inline { data: payload.to_string() },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
                IpcResponse::success("update", None)
            }
            IpcRequest::CompleteTask { task_id, result } => {
                info!("CompleteTask for: {}", task_id);
                Self::record_session_activity_from_value(
                    graph,
                    &result,
                    None,
                    Some("completed"),
                    None,
                    "complete_task",
                );
                let env = EventEnvelope {
                    event_id: Uuid::new_v4(),
                    seq: 0,
                    source_node_id: "local-ansible-01".into(),
                    source_agent_id: "unknown".into(),
                    target_agent_id: None,
                    kind: EventKind::TaskResult,
                    corr_id: task_id.to_string(),
                    attempt: 0,
                    created_at: 0,
                    expires_at: None,
                    payload: EventPayload::Inline { data: result.to_string() },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
                IpcResponse::success("complete", None)
            }
            IpcRequest::FailTask { task_id, error_code, reason } => {
                info!("FailTask for: {} ({}): {}", task_id, error_code, reason);
                Self::record_session_activity_from_value(
                    graph,
                    &serde_json::json!({
                        "error": error_code,
                        "reason": reason,
                    }),
                    None,
                    Some("failed"),
                    None,
                    "fail_task",
                );
                let env = EventEnvelope {
                    event_id: Uuid::new_v4(),
                    seq: 0,
                    source_node_id: "local-ansible-01".into(),
                    source_agent_id: "unknown".into(),
                    target_agent_id: None,
                    kind: EventKind::TaskResult, 
                    corr_id: task_id.to_string(),
                    attempt: 0,
                    created_at: 0,
                    expires_at: None,
                    payload: EventPayload::Inline { 
                        data: serde_json::json!({
                            "error": error_code,
                            "reason": reason
                        }).to_string() 
                    },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
                IpcResponse::success("fail", None)
            }
            IpcRequest::SubscribeInbox { role } => {
                info!("SubscribeInbox for role: {}", role);
                let guest = {
                    let guard = inboxes.lock().await;
                    guard
                        .values()
                        .flat_map(|subscribers| subscribers.iter())
                        .find(|subscriber| subscriber.conn_id == conn_id)
                        .cloned()
                };
                let guest_id = guest
                    .as_ref()
                    .map(|subscriber| subscriber.guest_id.as_str())
                    .unwrap_or("unknown");
                let supported_tools = guest
                    .as_ref()
                    .map(|subscriber| subscriber.supported_tools.as_slice())
                    .unwrap_or(&[]);
                Self::add_subscription(
                    inboxes,
                    &role,
                    conn_id,
                    guest_id,
                    supported_tools,
                    outbound_tx,
                    subscribed_roles,
                )
                .await;
                IpcResponse::success("sub", None)
            }
            IpcRequest::SyncApartment { agent_id, memory_type, content_json } => {
                info!("SyncApartment for: {} ({})", agent_id, memory_type);
                if let Err(e) = graph.sync_apartment(&agent_id, &memory_type, &content_json) {
                    error!("Failed to sync memory apartment: {}", e);
                    return IpcResponse::error("sync", "SYNC_ERROR", e.to_string());
                }
                Self::record_apartment_checkpoint(graph, &agent_id, &memory_type, &content_json);
                IpcResponse::success("sync", None)
            }
            IpcRequest::QueryStatus { task_id: _ } => {
                IpcResponse::success("query", None)
            }
            IpcRequest::QueryTimeline { task_id: _ } => {
                IpcResponse::success("timeline", None)
            }
            IpcRequest::EmitTask { target_node, target_role, task_json } => {
                info!("EmitTask mapped to TaskInvoke for {}/{}", target_node, target_role);
                let task_id = Uuid::new_v4();
                if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&task_json) {
                    Self::record_session_activity_from_value(
                        graph,
                        &payload,
                        Some(task_id),
                        Some("running"),
                        Some(&target_role),
                        "emit_task",
                    );
                }
                let env = EventEnvelope {
                    event_id: task_id,
                    seq: 0,
                    source_node_id: "local-ansible-01".into(),
                    source_agent_id: "unknown".into(),
                    target_agent_id: Some(target_role.clone()),
                    kind: EventKind::TaskInvoke,
                    corr_id: "emit".into(),
                    attempt: 0,
                    created_at: 0,
                    expires_at: None,
                    payload: EventPayload::Inline { data: task_json.clone() },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
                Self::deliver_inbound_task(inboxes, &target_role, task_id, task_json).await;
                IpcResponse::success("emit", None)
            }
        }
    }

    fn record_apartment_checkpoint(
        graph: &dyn GraphStorage,
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

    fn record_session_activity_from_value(
        graph: &dyn GraphStorage,
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
        if let Some(session_status) = payload.get("session_status").and_then(serde_json::Value::as_str)
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
                summary_json["tool_assembly"] = compose_tool_assembly(bindings, &[], &[]);
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
        session.updated_at = now;
        let _ = graph.upsert_session(&session);

        if let (Some(component_id), Some(role)) = (participant_role, participant_role) {
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
    }

    fn upsert_tool_runner_registry_entry(
        graph: &dyn GraphStorage,
        identity: &philotic_client::GuestIdentity,
    ) -> anyhow::Result<()> {
        let mut registry = load_tool_runner_registry(graph)?;
        registry.retain(|entry| entry.guest_id != identity.guest_id);
        registry.push(ToolRunnerRegistryEntry {
            guest_id: identity.guest_id.clone(),
            supported_tools: identity.supported_tools.clone(),
            last_seen_at: unix_ts(),
        });
        registry.sort_by(|a, b| a.guest_id.cmp(&b.guest_id));
        let registry_json = serde_json::Value::Array(
            registry
                .iter()
                .map(|entry| {
                    serde_json::json!({
                        "guest_id": entry.guest_id,
                        "supported_tools": entry.supported_tools,
                        "last_seen_at": entry.last_seen_at,
                    })
                })
                .collect(),
        );
        graph.set_config_value("tool_runner_registry", &registry_json.to_string())
    }

    fn append_explicit_approval_events(
        graph: &dyn GraphStorage,
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
                .map(|bindings| compose_tool_assembly(bindings, &[], &[]))
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
                    Some(format!("{source}:{chat_id}:agent-jane-01"))
                }),
            turn_id: payload
                .get("turn_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            primary_agent_id: payload
                .get("primary_agent_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| Some("agent-jane-01".to_string())),
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

    async fn compose_session_snapshot(
        graph: &dyn GraphStorage,
        inboxes: &InboxRegistry,
        session_id: &str,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let Some(session) = graph.get_session(session_id)? else {
            return Ok(None);
        };

        let turns = graph.list_session_turns(session_id, 8)?;
        let apartment_checkpoint = session
            .primary_agent_id
            .as_deref()
            .and_then(|agent_id| {
                let memory_type = format!("short_session:{session_id}");
                graph.get_apartment(agent_id, &memory_type).ok().flatten()
            });

        let session_index = session
            .primary_agent_id
            .as_deref()
            .and_then(|agent_id| graph.get_apartment(agent_id, "short").ok().flatten());

        let recent_turns = if let Some(checkpoint_turns) = apartment_checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.get("recent_turns"))
            .and_then(serde_json::Value::as_array)
        {
            checkpoint_turns.clone()
        } else {
            turns.iter()
                .map(|turn| {
                    serde_json::json!({
                        "turn_id": turn.turn_id,
                        "user_content": turn.user_message_json.get("content").and_then(serde_json::Value::as_str).unwrap_or_default(),
                        "assistant_content": turn.response_json.as_ref().and_then(|r| r.get("content")).and_then(serde_json::Value::as_str),
                    })
                })
                .collect::<Vec<_>>()
        };

        let active_turn = apartment_checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.get("active_turn"))
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let bindings = session
            .summary_json
            .get("bindings")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let registered_runners = load_tool_runner_registry(graph)?;
        let tool_runners = live_tool_runners(inboxes).await;
        let tool_assembly = compose_tool_assembly(&bindings, &registered_runners, &tool_runners);
        let tool_runner_registry = merge_tool_runners(&registered_runners, &tool_runners);

        Ok(Some(serde_json::json!({
            "session_id": session.session_id,
            "agent_id": session.primary_agent_id,
            "source": session.channel_kind,
            "status": session.status,
            "summary": session.summary_json,
            "approval_policy": session
                .summary_json
                .get("approval_policy")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
            "bindings": bindings,
            "tool_assembly": tool_assembly,
            "tool_runners": tool_runner_registry,
            "recent_turns": recent_turns,
            "active_turn": active_turn,
            "session_index": session_index,
        })))
    }
}

#[derive(Debug, Clone)]
struct LiveToolRunner {
    guest_id: String,
    supported_tools: Vec<String>,
}

async fn live_tool_runners(inboxes: &InboxRegistry) -> Vec<LiveToolRunner> {
    let guard = inboxes.lock().await;
    let mut runners = Vec::new();

    if let Some(subscribers) = guard.get("tool") {
        for subscriber in subscribers {
            if !runners
                .iter()
                .any(|existing: &LiveToolRunner| existing.guest_id == subscriber.guest_id)
            {
                runners.push(LiveToolRunner {
                    guest_id: subscriber.guest_id.clone(),
                    supported_tools: subscriber.supported_tools.clone(),
                });
            }
        }
    }

    runners
}

fn load_tool_runner_registry(
    graph: &dyn GraphStorage,
) -> anyhow::Result<Vec<ToolRunnerRegistryEntry>> {
    let Some(raw) = graph.get_config_value("tool_runner_registry")? else {
        return Ok(Vec::new());
    };
    let value = serde_json::from_str::<serde_json::Value>(&raw).unwrap_or_else(|_| serde_json::json!([]));
    let entries = value
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| {
            Some(ToolRunnerRegistryEntry {
                guest_id: entry.get("guest_id")?.as_str()?.to_string(),
                supported_tools: entry
                    .get("supported_tools")
                    .and_then(serde_json::Value::as_array)
                    .map(|tools| {
                        tools
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
                last_seen_at: entry
                    .get("last_seen_at")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            })
        })
        .collect::<Vec<_>>();
    Ok(entries)
}

fn merge_tool_runners(
    registered_runners: &[ToolRunnerRegistryEntry],
    live_runners: &[LiveToolRunner],
) -> serde_json::Value {
    let merged = registered_runners
        .iter()
        .map(|runner| {
            let is_connected = live_runners
                .iter()
                .any(|live| live.guest_id == runner.guest_id);
            serde_json::json!({
                "guest_id": runner.guest_id,
                "supported_tools": runner.supported_tools,
                "last_seen_at": runner.last_seen_at,
                "is_connected": is_connected,
            })
        })
        .collect::<Vec<_>>();
    serde_json::Value::Array(merged)
}

fn compose_tool_assembly(
    bindings: &serde_json::Value,
    registered_runners: &[ToolRunnerRegistryEntry],
    live_runners: &[LiveToolRunner],
) -> serde_json::Value {
    let mut toolset = bindings
        .get("effective_toolset")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if toolset.is_empty() {
        toolset.push(serde_json::json!("echo"));
    }

    let tools_for_model = toolset
        .iter()
        .filter_map(|tool| tool.as_str())
        .filter(|tool_name| {
            registered_runners.iter().any(|runner| {
                runner.supported_tools.is_empty()
                    || runner.supported_tools.iter().any(|supported| supported == *tool_name)
            })
        })
        .map(|tool_name| {
            serde_json::json!({
                "tool_name": tool_name,
                "description": format!("Execute the {} tool.", tool_name),
                "input_schema": {
                    "type": "object"
                }
            })
        })
        .collect::<Vec<_>>();

    let execution_routes = toolset
        .iter()
        .filter_map(|tool| tool.as_str())
        .filter_map(|tool_name| {
            let registered = registered_runners.iter().find(|runner| {
                runner.supported_tools.is_empty()
                    || runner.supported_tools.iter().any(|supported| supported == tool_name)
            })?;
            let live_runner = live_runners.iter().find(|runner| {
                runner.supported_tools.is_empty()
                    || runner.supported_tools.iter().any(|supported| supported == tool_name)
            });
            Some((tool_name, registered, live_runner))
        })
        .map(|(tool_name, registered, live_runner)| {
            (
                tool_name.to_string(),
                serde_json::json!({
                    "target_node": "local-ansible-01",
                    "target_role": format!("tool.{}", tool_name),
                    "runner_id": live_runner
                        .map(|runner| runner.guest_id.clone())
                        .unwrap_or_else(|| registered.guest_id.clone()),
                    "execution_mode": "ipc",
                    "availability_state": if live_runner.is_some() {
                        "live"
                    } else {
                        "materialization_required"
                    }
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();

    let policy_annotations = toolset
        .iter()
        .filter_map(|tool| tool.as_str())
        .map(|tool_name| {
            (
                tool_name.to_string(),
                serde_json::json!({
                    "policy_class": format!("tool:{tool_name}"),
                    "approval_required": false
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();

    serde_json::json!({
        "tools_for_model": tools_for_model,
        "execution_routes": execution_routes,
        "policy_annotations": policy_annotations,
    })
}

fn unix_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
    use ansible_mesh_core::storage::{
        GuestRecord, HotelRecord, SessionEventRecord, SessionParticipantRecord, SessionRecord,
        SessionTurnRecord,
    };
    use ansible_mesh_core::NodeCapabilities;
    use philotic_client::{GuestIdentity, PhiloticClient};
    use std::path::Path;
    use std::sync::{LazyLock, Mutex as StdMutex};

    #[derive(Default)]
    struct TestGraphStorage;

    impl GraphStorage for TestGraphStorage {
        fn load_node_capabilities(&self) -> anyhow::Result<Option<NodeCapabilities>> { Ok(None) }
        fn save_node_capabilities(&self, _caps: &NodeCapabilities) -> anyhow::Result<()> { Ok(()) }
        fn get_config_value(&self, _key: &str) -> anyhow::Result<Option<String>> { Ok(None) }
        fn set_config_value(&self, _key: &str, _value_json: &str) -> anyhow::Result<()> { Ok(()) }
        fn get_hotel(&self, _hotel_name: &str) -> anyhow::Result<Option<HotelRecord>> { Ok(None) }
        fn upsert_hotel(&self, _hotel: &HotelRecord) -> anyhow::Result<()> { Ok(()) }
        fn set_hotel_pid(&self, _hotel_name: &str, _pid: Option<&str>) -> anyhow::Result<()> { Ok(()) }
        fn list_guests(&self, _hotel_name: &str, _active_only: bool) -> anyhow::Result<Vec<GuestRecord>> { Ok(vec![]) }
        fn set_guest_pid(&self, _hotel_name: &str, _guest_id: &str, _pid: Option<&str>) -> anyhow::Result<()> { Ok(()) }
        fn seed_guests(&self, _hotel_name: &str, _guests: &[GuestRecord]) -> anyhow::Result<()> { Ok(()) }
        fn sync_apartment(&self, _agent_id: &str, _memory_type: &str, _content_json: &serde_json::Value) -> anyhow::Result<()> { Ok(()) }
        fn get_apartment(&self, _agent_id: &str, _memory_type: &str) -> anyhow::Result<Option<serde_json::Value>> { Ok(None) }
        fn upsert_session(&self, _session: &SessionRecord) -> anyhow::Result<()> { Ok(()) }
        fn get_session(&self, _session_id: &str) -> anyhow::Result<Option<SessionRecord>> { Ok(None) }
        fn upsert_session_participant(&self, _participant: &SessionParticipantRecord) -> anyhow::Result<()> { Ok(()) }
        fn list_session_participants(&self, _session_id: &str) -> anyhow::Result<Vec<SessionParticipantRecord>> { Ok(vec![]) }
        fn upsert_session_turn(&self, _turn: &SessionTurnRecord) -> anyhow::Result<()> { Ok(()) }
        fn get_session_turn(&self, _session_id: &str, _turn_id: &str) -> anyhow::Result<Option<SessionTurnRecord>> { Ok(None) }
        fn list_session_turns(&self, _session_id: &str, _limit: usize) -> anyhow::Result<Vec<SessionTurnRecord>> { Ok(vec![]) }
        fn append_session_event(&self, _event: &SessionEventRecord) -> anyhow::Result<()> { Ok(()) }
        fn list_session_events(&self, _session_id: &str, _limit: usize) -> anyhow::Result<Vec<SessionEventRecord>> { Ok(vec![]) }
    }

    fn test_socket_path() -> String {
        format!("/tmp/ipc-e2e-{}.sock", Uuid::new_v4().simple())
    }

    static IPC_TEST_ENV_LOCK: LazyLock<StdMutex<()>> = LazyLock::new(|| StdMutex::new(()));

    fn ipc_env_guard() -> std::sync::MutexGuard<'static, ()> {
        IPC_TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    #[tokio::test]
    async fn emit_task_is_delivered_to_registered_local_role() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, mut dispatcher_rx) = mpsc::channel(8);
        let graph: Arc<dyn GraphStorage> = Arc::new(TestGraphStorage);
        let server = IpcServer::new(socket_path.clone(), dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe { std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path); }

        let agent_identity = GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let hegemon_identity = GuestIdentity {
            guest_id: "hegemon-local".into(),
            role: "hegemon".into(),
            supported_tools: Vec::new(),
        };

        let mut agent = PhiloticClient::connect(agent_identity).await.expect("agent connect");
        let mut hegemon = PhiloticClient::connect(hegemon_identity).await.expect("hegemon connect");

        let task_payload = serde_json::json!({
            "source": "telegram",
            "chat_id": "12345",
            "content": "hello from telegram"
        })
        .to_string();

        let response = hegemon
            .send_request(IpcRequest::EmitTask {
                target_node: "local-ansible-01".into(),
                target_role: "agent".into(),
                task_json: task_payload.clone(),
            })
            .await
            .expect("emit task");

        assert!(matches!(response, IpcResponse::Standard { ok: true, .. }));

        let delivered = tokio::time::timeout(tokio::time::Duration::from_secs(1), agent.recv_task())
            .await
            .expect("agent should receive task before timeout")
            .expect("agent recv should succeed");

        match delivered {
            IpcResponse::InboundTask { source_node, task_json, .. } => {
                assert_eq!(source_node, "local-ansible-01");
                assert_eq!(task_json, task_payload);
            }
            other => panic!("unexpected inbound response: {other:?}"),
        }

        let ledger_msg = dispatcher_rx.recv().await.expect("ledger command should be emitted");
        assert!(matches!(ledger_msg, LedgerCommand::AppendLocal(_)));

        unsafe { std::env::remove_var("PHILOTIC_HOTEL_SOCKET"); }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn emit_task_persists_session_and_turn_metadata() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph: Arc<dyn GraphStorage> = Arc::new(graph_store.clone());
        let server = IpcServer::new(socket_path.clone(), dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe { std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path); }

        let hegemon_identity = GuestIdentity {
            guest_id: "hegemon-local".into(),
            role: "hegemon".into(),
            supported_tools: Vec::new(),
        };
        let mut hegemon = PhiloticClient::connect(hegemon_identity).await.expect("hegemon connect");

        hegemon
            .send_request(IpcRequest::EmitTask {
                target_node: "local-ansible-01".into(),
                target_role: "agent".into(),
                task_json: serde_json::json!({
                    "source": "telegram",
                    "session_id": "telegram:123:agent-jane-01",
                    "turn_id": "telegram-update-1",
                    "chat_id": "123",
                    "content": "hello from telegram"
                })
                .to_string(),
            })
            .await
            .expect("emit task");

        let session = graph_store
            .get_session("telegram:123:agent-jane-01")
            .expect("session lookup should work")
            .expect("session should exist");
        assert_eq!(session.channel_kind.as_deref(), Some("telegram"));

        let turns = graph_store
            .list_session_turns("telegram:123:agent-jane-01", 10)
            .expect("turn listing should work");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].turn_id, "telegram-update-1");
        assert_eq!(turns[0].user_message_json["content"], "hello from telegram");

        let events = graph_store
            .list_session_events("telegram:123:agent-jane-01", 10)
            .expect("event listing should work");
        assert!(!events.is_empty(), "session events should be recorded");

        unsafe { std::env::remove_var("PHILOTIC_HOTEL_SOCKET"); }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn get_config_can_return_canonical_session_snapshot() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph: Arc<dyn GraphStorage> = Arc::new(graph_store.clone());
        let server = IpcServer::new(socket_path.clone(), dispatcher_tx, graph);

        graph_store
            .upsert_session(&SessionRecord {
                session_id: "sess-1".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({"summary": "hello summary"}),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");
        graph_store
            .upsert_session_turn(&SessionTurnRecord {
                turn_id: "turn-1".into(),
                session_id: "sess-1".into(),
                request_event_id: Some("req-1".into()),
                user_message_json: serde_json::json!({"content": "hello"}),
                status: "completed".into(),
                response_json: Some(serde_json::json!({"content": "hi"})),
                error_json: None,
                started_at: Some(1),
                completed_at: Some(2),
            })
            .expect("turn should seed");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe { std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path); }

        let agent_identity = GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let mut agent = PhiloticClient::connect(agent_identity).await.expect("agent connect");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-1".into(),
            })
            .await
            .expect("snapshot request should succeed");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(snapshot["session_id"], "sess-1");
                assert_eq!(snapshot["source"], "telegram");
                assert_eq!(snapshot["recent_turns"][0]["user_content"], "hello");
                assert_eq!(snapshot["recent_turns"][0]["assistant_content"], "hi");
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe { std::env::remove_var("PHILOTIC_HOTEL_SOCKET"); }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn session_snapshot_includes_approval_policy_from_session_summary() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph: Arc<dyn GraphStorage> = Arc::new(graph_store.clone());
        let server = IpcServer::new(socket_path.clone(), dispatcher_tx, graph);

        graph_store
            .upsert_session(&SessionRecord {
                session_id: "sess-approval".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "approval_policy": {
                        "auto_approve_all": true
                    }
                }),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe { std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path); }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-approval".into(),
            })
            .await
            .expect("snapshot request should succeed");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(snapshot["approval_policy"]["auto_approve_all"], true);
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe { std::env::remove_var("PHILOTIC_HOTEL_SOCKET"); }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn session_snapshot_includes_bindings_and_status_from_session_summary() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph: Arc<dyn GraphStorage> = Arc::new(graph_store.clone());
        let server = IpcServer::new(socket_path.clone(), dispatcher_tx, graph);

        graph_store
            .upsert_session(&SessionRecord {
                session_id: "sess-bindings".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "paused".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "bindings": {
                        "effective_toolset": ["echo"],
                        "effective_skillset": ["planning"],
                        "effective_workspace_ref": "workspace://main",
                        "effective_model_controller": "gemini-flash"
                    }
                }),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe { std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path); }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");
        let mut tool = PhiloticClient::connect(GuestIdentity {
            guest_id: "tool-runner-local".into(),
            role: "tool".into(),
            supported_tools: vec!["echo".into()],
        })
        .await
        .expect("tool connect");
        tool
            .send_request(IpcRequest::SubscribeInbox {
                role: "tool.echo".into(),
            })
            .await
            .expect("tool subscribe");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-bindings".into(),
            })
            .await
            .expect("snapshot request should succeed");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(snapshot["status"], "paused");
                assert_eq!(snapshot["bindings"]["effective_toolset"][0], "echo");
                assert_eq!(snapshot["tool_assembly"]["tools_for_model"][0]["tool_name"], "echo");
                assert_eq!(snapshot["tool_assembly"]["execution_routes"]["echo"]["target_role"], "tool.echo");
                assert_eq!(snapshot["tool_runners"][0]["guest_id"], "tool-runner-local");
                assert_eq!(snapshot["tool_runners"][0]["is_connected"], true);
                assert_eq!(
                    snapshot["bindings"]["effective_workspace_ref"],
                    "workspace://main"
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe { std::env::remove_var("PHILOTIC_HOTEL_SOCKET"); }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn tool_runner_registration_persists_durable_registry() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph: Arc<dyn GraphStorage> = Arc::new(graph_store.clone());
        let server = IpcServer::new(socket_path.clone(), dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe { std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path); }

        let _tool = PhiloticClient::connect(GuestIdentity {
            guest_id: "tool-runner-local".into(),
            role: "tool".into(),
            supported_tools: vec!["echo".into()],
        })
        .await
        .expect("tool connect");

        let raw = graph_store
            .get_config_value("tool_runner_registry")
            .expect("registry lookup should work")
            .expect("registry should exist");
        let registry: serde_json::Value =
            serde_json::from_str(&raw).expect("registry should decode");
        assert_eq!(registry[0]["guest_id"], "tool-runner-local");
        assert_eq!(registry[0]["supported_tools"][0], "echo");

        unsafe { std::env::remove_var("PHILOTIC_HOTEL_SOCKET"); }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn session_snapshot_marks_registered_but_offline_tools_as_materialization_required() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph: Arc<dyn GraphStorage> = Arc::new(graph_store.clone());
        let server = IpcServer::new(socket_path.clone(), dispatcher_tx, graph);

        graph_store
            .upsert_session(&SessionRecord {
                session_id: "sess-dormant-runner".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "bindings": {
                        "effective_toolset": ["echo"]
                    }
                }),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");
        graph_store
            .set_config_value(
                "tool_runner_registry",
                &serde_json::json!([
                    {
                        "guest_id": "tool-runner-local",
                        "supported_tools": ["echo"],
                        "last_seen_at": 42
                    }
                ])
                .to_string(),
            )
            .expect("registry should seed");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe { std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path); }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-dormant-runner".into(),
            })
            .await
            .expect("snapshot request should succeed");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(snapshot["tool_assembly"]["tools_for_model"][0]["tool_name"], "echo");
                assert_eq!(
                    snapshot["tool_assembly"]["execution_routes"]["echo"]["availability_state"],
                    "materialization_required"
                );
                assert_eq!(snapshot["tool_runners"][0]["is_connected"], false);
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe { std::env::remove_var("PHILOTIC_HOTEL_SOCKET"); }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn session_snapshot_uses_per_session_checkpoint_when_agent_has_multiple_sessions() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph: Arc<dyn GraphStorage> = Arc::new(graph_store.clone());
        let server = IpcServer::new(socket_path.clone(), dispatcher_tx, graph);

        for session_id in ["sess-1", "sess-2"] {
            graph_store
                .upsert_session(&SessionRecord {
                    session_id: session_id.into(),
                    session_kind: "conversation".into(),
                    primary_agent_id: Some("agent-jane-01".into()),
                    channel_kind: Some("telegram".into()),
                    channel_session_key: Some(format!("chat-{session_id}")),
                    status: "active".into(),
                    lease_owner_component_id: None,
                    lease_expires_at: None,
                    summary_json: serde_json::json!({}),
                    created_at: 1,
                    updated_at: 2,
                })
                .expect("session should seed");
        }
        graph_store
            .raw_conn()
            .lock()
            .expect("sqlite lock")
            .execute(
                "INSERT INTO agent_identities (agent_id, persona_name, bundle_json) VALUES (?1, ?2, ?3)",
                rusqlite::params!["agent-jane-01", "Jane", "{}"],
            )
            .expect("agent identity should seed");

        graph_store
            .sync_apartment(
                "agent-jane-01",
                "short",
                &serde_json::json!({
                    "agent_id": "agent-jane-01",
                    "active_sessions": [
                        {"session_id": "sess-2", "updated_at": 200, "has_active_turn": false},
                        {"session_id": "sess-1", "updated_at": 100, "has_active_turn": true}
                    ]
                }),
            )
            .expect("session index should seed");
        graph_store
            .sync_apartment(
                "agent-jane-01",
                "short_session:sess-1",
                &serde_json::json!({
                    "session_id": "sess-1",
                    "agent_id": "agent-jane-01",
                    "source": "telegram",
                    "active_turn": {
                        "turn_id": "turn-1a",
                        "task_id": Uuid::nil().to_string(),
                        "chat_id": "chat-sess-1",
                        "user_content": "hello from sess-1",
                        "final_reply_to": "local-ansible-01",
                        "final_reply_role": "hegemon"
                    },
                    "recent_turns": [{
                        "turn_id": "turn-1z",
                        "user_content": "older sess-1",
                        "assistant_content": "older reply"
                    }]
                }),
            )
            .expect("session checkpoint should seed");
        graph_store
            .sync_apartment(
                "agent-jane-01",
                "short_session:sess-2",
                &serde_json::json!({
                    "session_id": "sess-2",
                    "agent_id": "agent-jane-01",
                    "source": "telegram",
                    "active_turn": null,
                    "recent_turns": [{
                        "turn_id": "turn-2z",
                        "user_content": "latest sess-2",
                        "assistant_content": "reply 2"
                    }]
                }),
            )
            .expect("other session checkpoint should seed");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe { std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path); }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-1".into(),
            })
            .await
            .expect("snapshot request should succeed");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(snapshot["session_id"], "sess-1");
                assert_eq!(snapshot["active_turn"]["turn_id"], "turn-1a");
                assert_eq!(snapshot["recent_turns"][0]["user_content"], "older sess-1");
                assert_eq!(snapshot["session_index"]["active_sessions"].as_array().unwrap().len(), 2);
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe { std::env::remove_var("PHILOTIC_HOTEL_SOCKET"); }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn update_task_with_approval_metadata_writes_explicit_approval_events() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph: Arc<dyn GraphStorage> = Arc::new(graph_store.clone());
        let server = IpcServer::new(socket_path.clone(), dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe { std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path); }

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
                state: "approval_preapproved".into(),
                payload: serde_json::json!({
                    "session_id": "sess-approval-events",
                    "turn_id": "turn-approval-1",
                    "chat_id": "123",
                    "approval_request": {
                        "approval_id": "appr-1",
                        "reason": "deploy the thing",
                        "approved_response": "Approved: deploy the thing"
                    },
                    "approval_resolution": {
                        "approval_id": "appr-1",
                        "decision": "approved",
                        "reason": "deploy the thing",
                        "resolution_mode": "preapproved"
                    }
                }),
            })
            .await
            .expect("update task should succeed");

        let events = graph_store
            .list_session_events("sess-approval-events", 20)
            .expect("event listing should work");
        assert!(events.iter().any(|event| event.kind == "approval_requested"));
        assert!(events.iter().any(|event| event.kind == "approval_resolved"));
        assert!(events.iter().any(|event| {
            event.kind == "approval_resolved"
                && event.payload_json["resolution_mode"] == "preapproved"
        }));

        unsafe { std::env::remove_var("PHILOTIC_HOTEL_SOCKET"); }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn update_task_with_approval_policy_updates_session_summary_and_event_log() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph: Arc<dyn GraphStorage> = Arc::new(graph_store.clone());
        let server = IpcServer::new(socket_path.clone(), dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe { std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path); }

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

        let session = graph_store
            .get_session("sess-policy-events")
            .expect("session lookup should work")
            .expect("session should exist");
        assert_eq!(session.summary_json["approval_policy"]["auto_approve_all"], true);

        let events = graph_store
            .list_session_events("sess-policy-events", 20)
            .expect("event listing should work");
        assert!(events
            .iter()
            .any(|event| event.kind == "approval_policy_changed"));

        unsafe { std::env::remove_var("PHILOTIC_HOTEL_SOCKET"); }
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
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph: Arc<dyn GraphStorage> = Arc::new(graph_store.clone());
        let server = IpcServer::new(socket_path.clone(), dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe { std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path); }

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

        let session = graph_store
            .get_session("sess-lifecycle")
            .expect("session lookup should work")
            .expect("session should exist");
        assert_eq!(session.status, "paused");
        assert_eq!(session.summary_json["bindings"]["effective_toolset"][0], "echo");
        assert!(session.summary_json["tool_assembly"]["execution_routes"]["echo"].is_null());

        let events = graph_store
            .list_session_events("sess-lifecycle", 20)
            .expect("event listing should work");
        assert!(events.iter().any(|event| event.kind == "session_status_changed"));
        assert!(events.iter().any(|event| event.kind == "session_bindings_updated"));
        assert!(events.iter().any(|event| event.kind == "tool_assembly_updated"));

        unsafe { std::env::remove_var("PHILOTIC_HOTEL_SOCKET"); }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn e2e_session_round_trip_persists_and_delivers_reply() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, mut dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph: Arc<dyn GraphStorage> = Arc::new(graph_store.clone());
        let server = IpcServer::new(socket_path.clone(), dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe { std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path); }

        let mut hegemon = PhiloticClient::connect(GuestIdentity {
            guest_id: "hegemon-local".into(),
            role: "hegemon".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("hegemon connect");
        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");
        let mut model = PhiloticClient::connect(GuestIdentity {
            guest_id: "model-local".into(),
            role: "model".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("model connect");

        let session_id = "telegram:123:agent-jane-01";
        let turn_id = "telegram-update-1";

        hegemon
            .send_request(IpcRequest::EmitTask {
                target_node: "local-ansible-01".into(),
                target_role: "agent".into(),
                task_json: serde_json::json!({
                    "source": "telegram",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "123",
                    "content": "hello from telegram",
                    "final_reply_to": "local-ansible-01",
                    "final_reply_role": "hegemon"
                })
                .to_string(),
            })
            .await
            .expect("emit user task");

        let inbound_to_agent = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            agent.recv_task(),
        )
        .await
        .expect("agent should receive task")
        .expect("agent recv should succeed");

        let task_id = match inbound_to_agent {
            IpcResponse::InboundTask { task_id, task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                assert_eq!(payload["session_id"], session_id);
                assert_eq!(payload["turn_id"], turn_id);
                task_id
            }
            other => panic!("unexpected inbound response to agent: {other:?}"),
        };

        agent
            .send_request(IpcRequest::UpdateTask {
                task_id,
                state: "waiting_model".into(),
                payload: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "123",
                    "content": "hello from telegram"
                }),
            })
            .await
            .expect("update task");
        agent
            .send_request(IpcRequest::EmitTask {
                target_node: "local-ansible-01".into(),
                target_role: "model".into(),
                task_json: serde_json::json!({
                    "action": "generate_text",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "prompt": "hello from telegram",
                    "chat_id": "123",
                    "reply_to": "local-ansible-01",
                    "reply_role": "agent",
                    "final_reply_to": "local-ansible-01",
                    "final_reply_role": "hegemon"
                })
                .to_string(),
            })
            .await
            .expect("emit model request");

        let inbound_to_model = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            model.recv_task(),
        )
        .await
        .expect("model should receive task")
        .expect("model recv should succeed");

        match inbound_to_model {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                assert_eq!(payload["reply_role"], "agent");
            }
            other => panic!("unexpected inbound response to model: {other:?}"),
        }

        model
            .send_request(IpcRequest::EmitTask {
                target_node: "local-ansible-01".into(),
                target_role: "agent".into(),
                task_json: serde_json::json!({
                    "action": "model_response",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "123",
                    "content": "hi back",
                    "final_reply_to": "local-ansible-01",
                    "final_reply_role": "hegemon"
                })
                .to_string(),
            })
            .await
            .expect("emit model response");

        let inbound_model_response = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            agent.recv_task(),
        )
        .await
        .expect("agent should receive model response")
        .expect("agent recv should succeed");

        match inbound_model_response {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                assert_eq!(payload["action"], "model_response");
                assert_eq!(payload["content"], "hi back");
            }
            other => panic!("unexpected model response to agent: {other:?}"),
        }

        agent
            .send_request(IpcRequest::CompleteTask {
                task_id,
                result: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "123",
                    "content": "hi back"
                }),
            })
            .await
            .expect("complete task");
        agent
            .send_request(IpcRequest::EmitTask {
                target_node: "local-ansible-01".into(),
                target_role: "hegemon".into(),
                task_json: serde_json::json!({
                    "action": "send_reply",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "123",
                    "content": "hi back"
                })
                .to_string(),
            })
            .await
            .expect("emit final reply");

        let final_reply = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            hegemon.recv_task(),
        )
        .await
        .expect("hegemon should receive final reply")
        .expect("hegemon recv should succeed");

        match final_reply {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                assert_eq!(payload["action"], "send_reply");
                assert_eq!(payload["content"], "hi back");
            }
            other => panic!("unexpected final response to hegemon: {other:?}"),
        }

        let turn = graph_store
            .get_session_turn(session_id, turn_id)
            .expect("turn lookup should work")
            .expect("turn should exist");
        assert_eq!(turn.status, "completed");
        assert_eq!(
            turn.response_json
                .as_ref()
                .and_then(|json| json.get("content"))
                .and_then(serde_json::Value::as_str),
            Some("hi back")
        );

        let mut ledger_count = 0usize;
        while tokio::time::timeout(
            tokio::time::Duration::from_millis(10),
            dispatcher_rx.recv(),
        )
        .await
        .ok()
        .flatten()
        .is_some()
        {
            ledger_count += 1;
            if ledger_count > 10 {
                break;
            }
        }
        assert!(ledger_count >= 4, "expected multiple ledger writes, got {ledger_count}");

        unsafe { std::env::remove_var("PHILOTIC_HOTEL_SOCKET"); }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn e2e_structured_tool_call_round_trip_persists_and_delivers_reply() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, mut dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph: Arc<dyn GraphStorage> = Arc::new(graph_store.clone());
        let server = IpcServer::new(socket_path.clone(), dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe { std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path); }

        let mut hegemon = PhiloticClient::connect(GuestIdentity {
            guest_id: "hegemon-local".into(),
            role: "hegemon".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("hegemon connect");
        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");
        let mut model = PhiloticClient::connect(GuestIdentity {
            guest_id: "model-local".into(),
            role: "model".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("model connect");

        let session_id = "telegram:456:agent-jane-01";
        let turn_id = "telegram-update-tool-1";

        hegemon
            .send_request(IpcRequest::EmitTask {
                target_node: "local-ansible-01".into(),
                target_role: "agent".into(),
                task_json: serde_json::json!({
                    "source": "telegram",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "456",
                    "content": "use echo hello structured tool",
                    "final_reply_to": "local-ansible-01",
                    "final_reply_role": "hegemon"
                })
                .to_string(),
            })
            .await
            .expect("emit user task");

        let inbound_to_agent = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            agent.recv_task(),
        )
        .await
        .expect("agent should receive task")
        .expect("agent recv should succeed");

        let task_id = match inbound_to_agent {
            IpcResponse::InboundTask { task_id, task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                assert_eq!(payload["session_id"], session_id);
                assert_eq!(payload["turn_id"], turn_id);
                task_id
            }
            other => panic!("unexpected inbound response to agent: {other:?}"),
        };

        agent
            .send_request(IpcRequest::UpdateTask {
                task_id,
                state: "waiting_model".into(),
                payload: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "456",
                    "content": "use echo hello structured tool"
                }),
            })
            .await
            .expect("update task");
        agent
            .send_request(IpcRequest::EmitTask {
                target_node: "local-ansible-01".into(),
                target_role: "model".into(),
                task_json: serde_json::json!({
                    "action": "generate_text",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "prompt": "use echo hello structured tool",
                    "user_content": "use echo hello structured tool",
                    "chat_id": "456",
                    "reply_to": "local-ansible-01",
                    "reply_role": "agent",
                    "final_reply_to": "local-ansible-01",
                    "final_reply_role": "hegemon"
                })
                .to_string(),
            })
            .await
            .expect("emit model request");

        let inbound_to_model = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            model.recv_task(),
        )
        .await
        .expect("model should receive task")
        .expect("model recv should succeed");

        match inbound_to_model {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                assert_eq!(payload["user_content"], "use echo hello structured tool");
            }
            other => panic!("unexpected inbound response to model: {other:?}"),
        }

        model
            .send_request(IpcRequest::EmitTask {
                target_node: "local-ansible-01".into(),
                target_role: "agent".into(),
                task_json: serde_json::json!({
                    "action": "model_response",
                    "agent_action": {
                        "kind": "tool_call",
                        "tool_name": "echo",
                        "arguments": {
                            "text": "hello structured tool"
                        }
                    },
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "456",
                    "content": "tool_call: echo hello structured tool",
                    "final_reply_to": "local-ansible-01",
                    "final_reply_role": "hegemon"
                })
                .to_string(),
            })
            .await
            .expect("emit model tool call response");

        let inbound_tool_response = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            agent.recv_task(),
        )
        .await
        .expect("agent should receive model response")
        .expect("agent recv should succeed");

        match inbound_tool_response {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                assert_eq!(payload["agent_action"]["kind"], "tool_call");
                assert_eq!(payload["agent_action"]["tool_name"], "echo");
            }
            other => panic!("unexpected model response to agent: {other:?}"),
        }

        agent
            .send_request(IpcRequest::CompleteTask {
                task_id,
                result: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "456",
                    "content": "Tool echo says: hello structured tool"
                }),
            })
            .await
            .expect("complete task");
        agent
            .send_request(IpcRequest::EmitTask {
                target_node: "local-ansible-01".into(),
                target_role: "hegemon".into(),
                task_json: serde_json::json!({
                    "action": "send_reply",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "456",
                    "content": "Tool echo says: hello structured tool"
                })
                .to_string(),
            })
            .await
            .expect("emit final reply");

        let final_reply = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            hegemon.recv_task(),
        )
        .await
        .expect("hegemon should receive final reply")
        .expect("hegemon recv should succeed");

        match final_reply {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                assert_eq!(payload["action"], "send_reply");
                assert_eq!(payload["content"], "Tool echo says: hello structured tool");
            }
            other => panic!("unexpected final response to hegemon: {other:?}"),
        }

        let turn = graph_store
            .get_session_turn(session_id, turn_id)
            .expect("turn lookup should work")
            .expect("turn should exist");
        assert_eq!(turn.status, "completed");
        assert_eq!(
            turn.response_json
                .as_ref()
                .and_then(|json| json.get("content"))
                .and_then(serde_json::Value::as_str),
            Some("Tool echo says: hello structured tool")
        );

        let events = graph_store
            .list_session_events(session_id, 20)
            .expect("event listing should work");
        assert!(
            events.iter().any(|event| event.payload_json.get("agent_action").is_some()),
            "expected structured agent action to be captured in session events"
        );

        let mut ledger_count = 0usize;
        while tokio::time::timeout(
            tokio::time::Duration::from_millis(10),
            dispatcher_rx.recv(),
        )
        .await
        .ok()
        .flatten()
        .is_some()
        {
            ledger_count += 1;
            if ledger_count > 10 {
                break;
            }
        }
        assert!(ledger_count >= 4, "expected multiple ledger writes, got {ledger_count}");

        unsafe { std::env::remove_var("PHILOTIC_HOTEL_SOCKET"); }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }
}
