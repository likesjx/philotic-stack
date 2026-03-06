use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
use philotic_client::{IpcRequest, IpcResponse};
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use uuid::Uuid;
use crate::LedgerCommand;
use ansible_mesh_core::storage::GraphStorage;
use std::sync::Arc;

pub struct IpcServer {
    socket_path: String,
    dispatcher_tx: mpsc::Sender<LedgerCommand>,
    graph: Arc<dyn GraphStorage>,
}

impl IpcServer {
    pub fn new(socket_path: impl Into<String>, dispatcher_tx: mpsc::Sender<LedgerCommand>, graph: Arc<dyn GraphStorage>) -> Self {
        Self {
            socket_path: socket_path.into(),
            dispatcher_tx,
            graph,
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
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_client(stream, dispatcher, graph).await {
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

    async fn handle_client(mut stream: UnixStream, dispatcher_tx: mpsc::Sender<LedgerCommand>, graph: Arc<dyn GraphStorage>) -> anyhow::Result<()> {
        let mut buf = vec![0u8; 65536];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => return Ok(()), // Client disconnected
                Ok(n) => {
                    match serde_json::from_slice::<IpcRequest>(&buf[..n]) {
                        Ok(req) => {
                            let response = Self::process_request(req, &dispatcher_tx, graph.as_ref()).await;
                            let res_bytes = serde_json::to_vec(&response)?;
                            // Could append a newline if guests are using line-delimited reading
                            // res_bytes.push(b'\n');
                            stream.write_all(&res_bytes).await?;
                        }
                        Err(e) => {
                            warn!("Malformed IPC request payload: {}", e);
                            let err_res = IpcResponse::error("unknown", "MALFORMED_PAYLOAD", e.to_string());
                            let res_bytes = serde_json::to_vec(&err_res)?;
                            stream.write_all(&res_bytes).await?;
                        }
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    async fn process_request(req: IpcRequest, dispatcher_tx: &mpsc::Sender<LedgerCommand>, graph: &dyn GraphStorage) -> IpcResponse {
        match req {
            IpcRequest::Register(identity) => {
                info!("Guest registered over UDS: [{}] Role: {}", identity.guest_id, identity.role);
                IpcResponse::success("reg", None)
            }
            IpcRequest::GetConfig { key } => {
                info!("GetConfig requested: {}", key);
                // In full implementation, we pass an sqlite handle or channel to context graph
                IpcResponse::success("config", None)
            }
            IpcRequest::PublishMessage { target_role, payload } => {
                info!("PublishMessage for role: {}", target_role);
                let env = EventEnvelope {
                    event_id: Uuid::new_v4(),
                    seq: 0, // Set by the sequence manager in PORT-BP-003
                    source_node_id: "local-ansible-01".into(),
                    source_agent_id: "unknown".into(), // Will be pulled from connection context
                    target_agent_id: Some(target_role),
                    kind: EventKind::TaskInvoke,
                    corr_id: "pub".into(),
                    attempt: 0,
                    created_at: 0,
                    expires_at: None,
                    payload: EventPayload::Inline { data: payload.to_string() },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
                IpcResponse::success("pub", None)
            }
            IpcRequest::CreateTask { target_role, payload } => {
                info!("CreateTask for role: {}", target_role);
                let task_id = Uuid::new_v4();
                let env = EventEnvelope {
                    event_id: task_id,
                    seq: 0,
                    source_node_id: "local-ansible-01".into(),
                    source_agent_id: "unknown".into(),
                    target_agent_id: Some(target_role),
                    kind: EventKind::TaskInvoke,
                    corr_id: "create".into(),
                    attempt: 0,
                    created_at: 0,
                    expires_at: None,
                    payload: EventPayload::Inline { data: payload.to_string() },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
                IpcResponse::success("create", Some(serde_json::json!({ "task_id": task_id.to_string() })))
            }
            IpcRequest::AckEvent { event_id } => {
                info!("AckEvent for: {}", event_id);
                IpcResponse::success("ack", None)
            }
            IpcRequest::UpdateTask { task_id, state, payload } => {
                info!("UpdateTask for: {} to state: {}", task_id, state);
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
                IpcResponse::success("sub", None)
            }
            IpcRequest::SyncApartment { agent_id, memory_type, content_json } => {
                info!("SyncApartment for: {} ({})", agent_id, memory_type);
                if let Err(e) = graph.sync_apartment(&agent_id, &memory_type, &content_json) {
                    error!("Failed to sync memory apartment: {}", e);
                    return IpcResponse::error("sync", "SYNC_ERROR", e.to_string());
                }
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
                let env = EventEnvelope {
                    event_id: Uuid::new_v4(),
                    seq: 0,
                    source_node_id: "local-ansible-01".into(),
                    source_agent_id: "unknown".into(),
                    target_agent_id: Some(target_role),
                    kind: EventKind::TaskInvoke,
                    corr_id: "emit".into(),
                    attempt: 0,
                    created_at: 0,
                    expires_at: None,
                    payload: EventPayload::Inline { data: task_json },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
                IpcResponse::success("emit", None)
            }
        }
    }
}
