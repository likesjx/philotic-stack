use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
use philotic_client::{IpcRequest, IpcResponse};
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};
use uuid::Uuid;
use crate::LedgerCommand;
use ansible_mesh_core::storage::GraphStorage;
use std::collections::HashMap;
use std::sync::Arc;

type InboxRegistry = Arc<Mutex<HashMap<String, Vec<RoleSubscriber>>>>;

#[derive(Clone)]
struct RoleSubscriber {
    conn_id: Uuid,
    tx: mpsc::UnboundedSender<IpcResponse>,
}

pub struct IpcServer {
    socket_path: String,
    dispatcher_tx: mpsc::Sender<LedgerCommand>,
    graph: Arc<dyn GraphStorage>,
    inboxes: InboxRegistry,
}

impl IpcServer {
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
                if let Err(e) = writer.write_all(&res_bytes).await {
                    return Err(e);
                }
            }
            Ok::<(), std::io::Error>(())
        });

        let mut buf = vec![0u8; 65536];
        let mut subscribed_roles = Vec::new();
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => {
                    Self::remove_subscriptions(&inboxes, conn_id, &subscribed_roles).await;
                    let _ = write_task.await;
                    return Ok(());
                }
                Ok(n) => {
                    match serde_json::from_slice::<IpcRequest>(&buf[..n]) {
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
        tx: &mpsc::UnboundedSender<IpcResponse>,
        subscribed_roles: &mut Vec<String>,
    ) {
        let mut guard = inboxes.lock().await;
        let entry = guard.entry(role.to_string()).or_default();
        if !entry.iter().any(|subscriber| subscriber.conn_id == conn_id) {
            entry.push(RoleSubscriber {
                conn_id,
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
                Self::add_subscription(inboxes, &identity.role, conn_id, outbound_tx, subscribed_roles).await;
                IpcResponse::success("reg", None)
            }
            IpcRequest::GetConfig { key } => {
                info!("GetConfig requested: {}", key);
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
                Self::add_subscription(inboxes, &role, conn_id, outbound_tx, subscribed_roles).await;
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
                let task_id = Uuid::new_v4();
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::storage::{GuestRecord, HotelRecord};
    use ansible_mesh_core::NodeCapabilities;
    use philotic_client::{GuestIdentity, PhiloticClient};
    use std::path::Path;

    #[derive(Default)]
    struct TestGraphStorage;

    impl GraphStorage for TestGraphStorage {
        fn load_node_capabilities(&self) -> anyhow::Result<Option<NodeCapabilities>> { Ok(None) }
        fn save_node_capabilities(&self, _caps: &NodeCapabilities) -> anyhow::Result<()> { Ok(()) }
        fn get_config_value(&self, _key: &str) -> anyhow::Result<Option<String>> { Ok(None) }
        fn get_hotel(&self, _hotel_name: &str) -> anyhow::Result<Option<HotelRecord>> { Ok(None) }
        fn upsert_hotel(&self, _hotel: &HotelRecord) -> anyhow::Result<()> { Ok(()) }
        fn set_hotel_pid(&self, _hotel_name: &str, _pid: Option<&str>) -> anyhow::Result<()> { Ok(()) }
        fn list_guests(&self, _hotel_name: &str, _active_only: bool) -> anyhow::Result<Vec<GuestRecord>> { Ok(vec![]) }
        fn set_guest_pid(&self, _hotel_name: &str, _guest_id: &str, _pid: Option<&str>) -> anyhow::Result<()> { Ok(()) }
        fn seed_guests(&self, _hotel_name: &str, _guests: &[GuestRecord]) -> anyhow::Result<()> { Ok(()) }
        fn sync_apartment(&self, _agent_id: &str, _memory_type: &str, _content_json: &serde_json::Value) -> anyhow::Result<()> { Ok(()) }
    }

    fn test_socket_path() -> String {
        format!("/tmp/ipc-e2e-{}.sock", Uuid::new_v4().simple())
    }

    #[tokio::test]
    async fn emit_task_is_delivered_to_registered_local_role() {
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
        };
        let hegemon_identity = GuestIdentity {
            guest_id: "hegemon-local".into(),
            role: "hegemon".into(),
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
}
