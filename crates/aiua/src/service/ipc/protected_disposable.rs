//! Opt-in synthetic single-operation endpoint. Never installed in production startup.
//! This is not a grant issuer, task scheduler, or Philote restart implementation.
pub mod inspection;
#[cfg(test)]
mod normal_runtime_tests;
#[cfg(test)]
mod recovery_tests;
use ansible_mesh_core::{
    domain::GraphDomain,
    privacy_local::{LocalLaunchRegistry, VerifiedLocalSession},
};
use anyhow::{Result, ensure};
use philotic_client::{IpcRequest, IpcResponse};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::{io::AsyncReadExt, net::UnixStream};

/// Only the hotel supplies this contract; no wire deserialization or delegation.
/// The operation JSON is exact: no generic routing or caller-selected action.
pub struct DisposableContract {
    pub hotel: String,
    pub guest: String,
    pub agent: String,
    pub role: String,
    pub destination_role: String,
    pub destination_guest: String,
    pub operation_json: String,
    pub checkpoint_key: String,
    pub receipt_key: String,
    pub expires_at: Instant,
}

pub struct ProtectedDisposable {
    inspection: Option<Arc<inspection::Inspection>>,
    launches: Arc<LocalLaunchRegistry>,
    contract: DisposableContract,
    // Serialize all adapter writes/revocation, then take launch lock, then graph.
    // No caller acquires these in reverse order or invokes callbacks under them.
    revoked: Mutex<bool>,
    #[cfg(test)]
    recovery: Option<Arc<recovery_tests::RecoveryFixture>>,
}
impl ProtectedDisposable {
    pub fn new(launches: Arc<LocalLaunchRegistry>, contract: DisposableContract) -> Result<Self> {
        ensure!(
            contract.checkpoint_key != contract.receipt_key,
            "receipt is not a checkpoint"
        );
        ensure!(
            !contract.agent.is_empty() && !contract.guest.is_empty(),
            "missing identity"
        );
        ensure!(
            contract.operation_json.len() <= 65536,
            "operation too large"
        );
        serde_json::from_str::<serde_json::Value>(&contract.operation_json)?;
        Ok(Self {
            inspection: None,
            launches,
            contract,
            revoked: Mutex::new(false),
            #[cfg(test)]
            recovery: None,
        })
    }
    /// Revocation returns only after any previously admitted synchronous commit.
    pub fn revoke(&self) -> Result<()> {
        *self
            .revoked
            .lock()
            .map_err(|_| anyhow::anyhow!("poisoned contract"))? = true;
        Ok(())
    }
    pub fn with_inspection(mut self, inspection: Arc<inspection::Inspection>) -> Self {
        self.inspection = Some(inspection);
        self
    }
    fn handle(
        &self,
        stream: &UnixStream,
        proof: &mut Option<Arc<VerifiedLocalSession>>,
        request: IpcRequest,
        graph: &GraphDomain,
    ) -> Result<IpcResponse> {
        let revoked = self
            .revoked
            .lock()
            .map_err(|_| anyhow::anyhow!("poisoned contract"))?;
        ensure!(
            !*revoked && Instant::now() < self.contract.expires_at,
            "inactive contract"
        );
        let c = &self.contract;
        if let IpcRequest::Register(identity) = &request {
            *proof = None;
            ensure!(
                identity.guest_id == c.guest
                    && identity.role == c.role
                    && identity.supported_tools.is_empty(),
                "registration mismatch"
            );
            let candidate = self.launches.authenticate(&c.hotel, stream, &c.guest)?;
            candidate.with_current_launch(|actor| {
                ensure!(
                    actor.stable_agent_id() == c.agent && actor.roles().contains(&c.role),
                    "principal mismatch"
                );
                Ok(())
            })?;
            *proof = Some(candidate);
            return Ok(IpcResponse::success("protected_register", None));
        }
        let proof = proof
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("registration required"))?;
        proof.with_current_launch(|actor| {
            ensure!(actor.stable_agent_id() == c.agent && actor.roles().contains(&c.role), "principal mismatch");
            ensure!(Instant::now() < c.expires_at, "expired contract");
            if let Some(inspection) = &self.inspection {
                return inspection.handle(actor.stable_agent_id(), request);
            }
            #[cfg(test)]
            if let Some(fixture) = &self.recovery {
                return fixture.handle(actor.stable_agent_id(), &proof.consumer(), request, c);
            }
            match request {
                IpcRequest::SyncApartment { agent_id, memory_type, content_json } => {
                    ensure!(agent_id == actor.stable_agent_id() && memory_type == c.checkpoint_key, "apartment mismatch");
                    ensure!(serde_json::to_vec(&content_json)?.len() <= 65536, "checkpoint too large");
                    graph.sync_apartment(actor.stable_agent_id(), &c.checkpoint_key, &content_json)?;
                    Ok(IpcResponse::success("protected_sync", None))
                }
                IpcRequest::EmitTask { target_node, target_role, target_guest_id, task_json } => {
                    ensure!(target_node == c.hotel && target_role == c.destination_role && target_guest_id.as_deref() == Some(c.destination_guest.as_str()) && task_json == c.operation_json, "operation scope mismatch");
                    // The durable receipt IS the synthetic effect. No external callback or
                    // side effect is hidden behind queue admission. Exact replay is a no-op.
                    let receipt = serde_json::json!({"operation": c.operation_json, "destination": c.destination_guest, "destination_role": c.destination_role, "hotel": c.hotel, "principal": c.agent});
                    match graph.get_apartment(&c.agent, &c.receipt_key)? {
                        Some(existing) => ensure!(existing == receipt, "unknown/conflicting receipt"),
                        None => graph.sync_apartment(&c.agent, &c.receipt_key, &receipt)?,
                    }
                    Ok(IpcResponse::success("protected_effect", None))
                }
                _ => anyhow::bail!("request outside disposable contract"),
            }
        })
    }
    pub(super) async fn serve(
        &self,
        mut stream: UnixStream,
        graph: Arc<GraphDomain>,
    ) -> Result<()> {
        let mut proof = None;
        loop {
            let mut length = [0u8; 4];
            match stream.read_exact(&mut length).await {
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e.into()),
            }
            let length = u32::from_be_bytes(length) as usize;
            ensure!(length <= 65536, "protected frame too large");
            let mut frame = vec![0; length];
            stream.read_exact(&mut frame).await?;
            let registering = matches!(
                serde_json::from_slice::<IpcRequest>(&frame),
                Ok(IpcRequest::Register(_))
            );
            #[cfg(test)]
            let committing = matches!(
                serde_json::from_slice::<IpcRequest>(&frame),
                Ok(IpcRequest::EmitTask { .. })
            );
            #[cfg(test)]
            let dispatching = matches!(serde_json::from_slice::<IpcRequest>(&frame),Ok(IpcRequest::GetConfig {key}) if key.ends_with(":dispatch"));
            #[cfg(test)]
            let bootstrapping = matches!(serde_json::from_slice::<IpcRequest>(&frame),Ok(IpcRequest::GetConfig {key}) if key == philotic_client::development_inspection::KEY);
            let result = serde_json::from_slice(&frame)
                .map_err(anyhow::Error::from)
                .and_then(|request| self.handle(&stream, &mut proof, request, &graph));
            #[cfg(test)]
            if let Some(fixture) = &self.recovery {
                if fixture
                    .hold_response
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
                {
                    fixture.release_response.notified().await;
                }
            }
            let response = result.unwrap_or_else(|_| {
                IpcResponse::error(
                    "protected",
                    "PROTECTED_DENIED",
                    "Disposable contract denied",
                )
            });
            #[cfg(test)]
            if let Some(i) = &self.inspection
                && ((bootstrapping && i.hold_bootstrap.load(std::sync::atomic::Ordering::SeqCst))
                    || (committing && i.hold.load(std::sync::atomic::Ordering::SeqCst))
                    || (dispatching && i.hold_dispatch.load(std::sync::atomic::Ordering::SeqCst)))
            {
                i.reached.notify_one();
                i.release.notified().await;
            }
            super::IpcServer::write_frame(&mut stream, &serde_json::to_vec(&response)?).await?;
            if registering
                && matches!(response, IpcResponse::Standard { ok: true, .. })
                && let Some(i) = &self.inspection
            {
                super::IpcServer::write_frame(&mut stream, &serde_json::to_vec(&i.envelope())?)
                    .await?;
                #[cfg(test)]
                super::IpcServer::write_frame(&mut stream, &serde_json::to_vec(&i.envelope())?)
                    .await?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::{
        privacy_local::{LaunchPrincipal, SupervisedProcess},
        sqlite_storage::SqliteGraphStorage,
    };
    use philotic_client::GuestIdentity;
    use std::time::Duration;
    // Unit fixture only: kernel credentials still come from the socket. Production
    // uses ProtectedLocalGuest's owned child, never a caller-supplied PID.
    struct Process(u32);
    impl SupervisedProcess for Process {
        fn pid(&self) -> Result<u32> {
            Ok(self.0)
        }
        fn alive(&self) -> Result<bool> {
            Ok(true)
        }
    }
    fn contract() -> DisposableContract {
        DisposableContract {
            hotel: "fixture".into(),
            guest: "guest".into(),
            agent: "agent".into(),
            role: "agent".into(),
            destination_role: "synthetic".into(),
            destination_guest: "sink".into(),
            operation_json: "{\"operation_id\":\"one\"}".into(),
            checkpoint_key: "fixture-checkpoint".into(),
            receipt_key: "fixture-receipt".into(),
            expires_at: Instant::now() + Duration::from_secs(30),
        }
    }
    fn register() -> IpcRequest {
        IpcRequest::Register(GuestIdentity {
            guest_id: "guest".into(),
            role: "agent".into(),
            supported_tools: vec![],
        })
    }
    fn emit() -> IpcRequest {
        let c = contract();
        IpcRequest::EmitTask {
            target_node: c.hotel,
            target_role: c.destination_role,
            target_guest_id: Some(c.destination_guest),
            task_json: c.operation_json,
        }
    }
    fn sync(agent: &str, key: &str) -> IpcRequest {
        IpcRequest::SyncApartment {
            agent_id: agent.into(),
            memory_type: key.into(),
            content_json: serde_json::json!({"phase":"prepared"}),
        }
    }
    fn graph() -> GraphDomain {
        let store = SqliteGraphStorage::open(":memory:").unwrap();
        GraphDomain::new(Arc::new(store.adapter()))
    }
    fn attach(registry: &LocalLaunchRegistry, pid: u32, uid: u32) -> uuid::Uuid {
        registry
            .attach(
                "guest",
                uid,
                LaunchPrincipal {
                    stable_agent_id: "agent".into(),
                    roles: ["agent".into()].into_iter().collect(),
                },
                Arc::new(Process(pid)),
            )
            .unwrap()
    }
    #[tokio::test]
    async fn protected_boundary_identity_scope_replay_and_retirement() {
        let (socket, _peer) = UnixStream::pair().unwrap();
        let registry = Arc::new(LocalLaunchRegistry::default());
        let generation = attach(
            &registry,
            std::process::id(),
            socket.peer_cred().unwrap().uid(),
        );
        let adapter = ProtectedDisposable::new(registry.clone(), contract()).unwrap();
        let graph = graph();
        let mut proof = None;
        assert!(adapter.handle(&socket, &mut proof, emit(), &graph).is_err());
        let mut wrong_registration = register();
        if let IpcRequest::Register(identity) = &mut wrong_registration {
            identity.role = "tool".into();
        }
        assert!(
            adapter
                .handle(&socket, &mut proof, wrong_registration, &graph)
                .is_err()
        );
        adapter
            .handle(&socket, &mut proof, register(), &graph)
            .unwrap();
        assert!(
            adapter
                .handle(
                    &socket,
                    &mut proof,
                    sync("other", "fixture-checkpoint"),
                    &graph
                )
                .is_err()
        );
        assert!(
            adapter
                .handle(
                    &socket,
                    &mut proof,
                    sync("agent", "fixture-receipt"),
                    &graph
                )
                .is_err()
        );
        assert!(
            adapter
                .handle(
                    &socket,
                    &mut proof,
                    IpcRequest::GetConfig {
                        key: "secret".into()
                    },
                    &graph
                )
                .is_err()
        );
        let mut wrong = emit();
        if let IpcRequest::EmitTask { target_node, .. } = &mut wrong {
            *target_node = "remote".into();
        }
        assert!(adapter.handle(&socket, &mut proof, wrong, &graph).is_err());
        adapter
            .handle(
                &socket,
                &mut proof,
                sync("agent", "fixture-checkpoint"),
                &graph,
            )
            .unwrap();
        adapter.handle(&socket, &mut proof, emit(), &graph).unwrap();
        let receipt = graph.get_apartment("agent", "fixture-receipt").unwrap();
        adapter.handle(&socket, &mut proof, emit(), &graph).unwrap();
        assert_eq!(
            receipt,
            graph.get_apartment("agent", "fixture-receipt").unwrap()
        );
        for change_hotel in [false, true] {
            let mut changed = contract();
            if change_hotel {
                changed.hotel = "different-hotel".into();
            } else {
                changed.destination_role = "different-role".into();
            }
            let request = IpcRequest::EmitTask {
                target_node: changed.hotel.clone(),
                target_role: changed.destination_role.clone(),
                target_guest_id: Some(changed.destination_guest.clone()),
                task_json: changed.operation_json.clone(),
            };
            let changed_adapter = ProtectedDisposable::new(registry.clone(), changed).unwrap();
            assert!(
                changed_adapter
                    .handle(&socket, &mut proof, request, &graph)
                    .is_err()
            );
            assert_eq!(
                receipt,
                graph.get_apartment("agent", "fixture-receipt").unwrap()
            );
        }
        graph
            .sync_apartment(
                "agent",
                "fixture-receipt",
                &serde_json::json!({"unknown": true}),
            )
            .unwrap();
        assert!(adapter.handle(&socket, &mut proof, emit(), &graph).is_err());
        assert_eq!(
            graph.get_apartment("agent", "fixture-receipt").unwrap(),
            Some(serde_json::json!({"unknown": true}))
        );
        registry.retire("guest", generation).unwrap();
        assert!(adapter.handle(&socket, &mut proof, emit(), &graph).is_err());
        attach(
            &registry,
            std::process::id(),
            socket.peer_cred().unwrap().uid(),
        );
        assert!(
            adapter
                .handle(
                    &socket,
                    &mut proof,
                    sync("agent", "fixture-checkpoint"),
                    &graph
                )
                .is_err()
        );
        adapter
            .handle(&socket, &mut proof, register(), &graph)
            .unwrap();
        adapter.revoke().unwrap();
        assert!(adapter.handle(&socket, &mut proof, emit(), &graph).is_err());
    }
    #[tokio::test]
    async fn protected_boundary_wrong_kernel_identity_and_expiry() {
        let (socket, _peer) = UnixStream::pair().unwrap();
        let registry = Arc::new(LocalLaunchRegistry::default());
        attach(
            &registry,
            std::process::id() + 1,
            socket.peer_cred().unwrap().uid(),
        );
        let graph = graph();
        let mut proof = None;
        let adapter = ProtectedDisposable::new(registry.clone(), contract()).unwrap();
        assert!(
            adapter
                .handle(&socket, &mut proof, register(), &graph)
                .is_err()
        );
        attach(
            &registry,
            std::process::id(),
            socket.peer_cred().unwrap().uid(),
        );
        let mut expired = contract();
        expired.expires_at = Instant::now();
        let adapter = ProtectedDisposable::new(registry, expired).unwrap();
        assert!(
            adapter
                .handle(&socket, &mut proof, register(), &graph)
                .is_err()
        );
    }
    #[tokio::test]
    async fn protected_boundary_socket_never_falls_back_to_generic_handlers() {
        let registry = Arc::new(LocalLaunchRegistry::default());
        attach(&registry, std::process::id(), unsafe { libc::geteuid() });
        let adapter = Arc::new(ProtectedDisposable::new(registry, contract()).unwrap());
        let graph = Arc::new(graph());
        let path = format!("/tmp/protected-disposable-{}.sock", uuid::Uuid::new_v4());
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let server = super::super::IpcServer::new(&path, "fixture", tx, graph.clone())
            .with_protected_disposable(adapter.clone());
        let task = tokio::spawn(async move { server.run().await.unwrap() });
        let mut stream = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(stream) = UnixStream::connect(&path).await {
                    break stream;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        async fn send(stream: &mut UnixStream, request: IpcRequest) -> IpcResponse {
            super::super::IpcServer::write_frame(stream, &serde_json::to_vec(&request).unwrap())
                .await
                .unwrap();
            let bytes = tokio::time::timeout(
                Duration::from_secs(2),
                super::super::IpcServer::read_frame(stream),
            )
            .await
            .unwrap()
            .unwrap()
            .unwrap();
            serde_json::from_slice(&bytes).unwrap()
        }
        assert!(matches!(
            send(&mut stream, register()).await,
            IpcResponse::Standard { ok: true, .. }
        ));
        assert!(matches!(
            send(&mut stream, emit()).await,
            IpcResponse::Standard { ok: true, .. }
        ));
        // QueryStatus succeeds in the generic handler, and must be denied here.
        let response = send(
            &mut stream,
            IpcRequest::QueryStatus {
                task_id: uuid::Uuid::new_v4(),
            },
        )
        .await;
        assert!(matches!(response, IpcResponse::Standard { ok: false, .. }));
        assert!(rx.try_recv().is_err()); // no ledger dispatch
        assert!(
            graph
                .get_apartment("agent", "fixture-receipt")
                .unwrap()
                .is_some()
        );
        adapter.revoke().unwrap();
        assert!(matches!(
            send(&mut stream, sync("agent", "fixture-checkpoint")).await,
            IpcResponse::Standard { ok: false, .. }
        ));
        assert!(
            graph
                .get_apartment("agent", "fixture-checkpoint")
                .unwrap()
                .is_none()
        );
        drop(stream);
        task.abort();
        let _ = task.await;
    }
}
