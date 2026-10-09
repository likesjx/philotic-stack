//! Protected RPC adapter. No request JSON, Register ACK or raw reply establishes
//! trust. The launch owner supplies the expected hotel's kernel PID and UID.
use crate::{IpcRequest, IpcResponse, PhiloticClient};
use ansible_mesh_core::privacy::{
    AuthenticatedAgent, ProcessingOperation, ResourcePolicy, ServerAuthenticatedIdentity,
};
use ansible_mesh_core::privacy_local::LocalTaskEnvelope;
use ansible_mesh_core::privacy_rpc::{
    ProtectedAuthorityOutcome, ProtectedAuthorityReply, ProtectedAuthorityRequest,
};
use ansible_mesh_core::privacy_storage::{
    PolicySnapshot, ServerPolicySnapshotAuthority, capture_payload_digest,
};
use anyhow::{Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use uuid::Uuid;

/// Server launch metadata, not caller JSON or environment PID strings. No serde.
#[derive(Clone, Copy)]
pub struct TrustedHotelPeer {
    pub pid: u32,
    pub uid: u32,
}
/// A snapshot for ONE immediate dispatch attempt. Never cache across retries,
/// sentences, reconnects or publication. Runtime cancellation still needs fences.
pub struct VerifiedLocalResolution {
    pub actor: AuthenticatedAgent,
    pub policies: PolicySnapshot,
    pub sources: Vec<String>,
    pub payload_digest: String,
    pub consumer_incarnation: Uuid,
}
struct ServerContext {
    actor: String,
    roles: BTreeSet<String>,
    revision: u64,
    policies: BTreeMap<String, ResourcePolicy>,
}
impl ServerAuthenticatedIdentity for ServerContext {
    fn stable_agent_id(&self) -> &str {
        &self.actor
    }
    fn roles(&self) -> BTreeSet<String> {
        self.roles.clone()
    }
}
impl ServerPolicySnapshotAuthority for ServerContext {
    fn revision(&self) -> u64 {
        self.revision
    }
    fn policies(&self) -> BTreeMap<String, ResourcePolicy> {
        self.policies.clone()
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum LocalCancellation {
    RevokedPendingQuiescence,
}
impl PhiloticClient {
    fn verify_hotel_peer(&mut self, expected: TrustedHotelPeer) -> Result<()> {
        self.ensure_connected()?;
        let actual = self
            .stream
            .as_ref()
            .expect("checked connected")
            .peer_cred()?;
        if expected.pid == 0
            || actual.pid().and_then(|p| u32::try_from(p).ok()) != Some(expected.pid)
            || actual.uid() != expected.uid
        {
            self.disconnect();
            bail!("untrusted hotel kernel peer");
        }
        Ok(())
    }
    async fn protected_rpc(
        &mut self,
        peer: TrustedHotelPeer,
        request: ProtectedAuthorityRequest,
        timeout: Duration,
    ) -> Result<ProtectedAuthorityReply> {
        self.verify_hotel_peer(peer)?;
        match self
            .send_request_with_timeout(IpcRequest::ProtectedAuthority(request), timeout)
            .await?
        {
            IpcResponse::ProtectedAuthorityReply {
                protected_authority,
            } => Ok(protected_authority),
            _ => {
                self.disconnect();
                bail!("missing protected RPC response");
            }
        }
    }
    pub async fn resolve_local_authority(
        &mut self,
        peer: TrustedHotelPeer,
        envelope: &LocalTaskEnvelope,
        endpoint: &str,
        operation: ProcessingOperation,
        timeout: Duration,
    ) -> Result<VerifiedLocalResolution> {
        let request = ProtectedAuthorityRequest::Resolve {
            request_id: Uuid::new_v4(),
            guest: self._identity.guest_id.clone(),
            envelope: envelope.clone(),
            endpoint: endpoint.into(),
            operation,
        };
        let reply = self.protected_rpc(peer, request, timeout).await?;
        if let ProtectedAuthorityOutcome::Resolved {
            task_id,
            authority_handle,
            payload_digest,
            consumer_incarnation,
            policy_revision,
            sources,
            actor_id,
            roles,
            policies,
        } = reply.outcome
        {
            if task_id != envelope.task_id
                || authority_handle != envelope.authority_handle
                || payload_digest != capture_payload_digest(&envelope.payload)
                || consumer_incarnation.is_nil()
                || sources.is_empty()
                || sources.len() > 256
            {
                self.disconnect();
                bail!("protected response binding mismatch");
            }
            let context = ServerContext {
                actor: actor_id,
                roles,
                revision: policy_revision,
                policies,
            };
            let actor = AuthenticatedAgent::from_server(&context)
                .map_err(|_| anyhow::anyhow!("invalid server principal"))?;
            let policies = PolicySnapshot::from_server(&context)?;
            for source in &sources {
                ansible_mesh_core::privacy::authorize_read(&policies, Some(&actor), source)
                    .map_err(|_| anyhow::anyhow!("invalid server source closure"))?;
            }
            Ok(VerifiedLocalResolution {
                actor,
                policies,
                sources,
                payload_digest,
                consumer_incarnation,
            })
        } else {
            bail!("protected resolution denied");
        }
    }
    pub async fn cancel_local_authority(
        &mut self,
        peer: TrustedHotelPeer,
        envelope: &LocalTaskEnvelope,
        timeout: Duration,
    ) -> Result<LocalCancellation> {
        let request = ProtectedAuthorityRequest::Cancel {
            request_id: Uuid::new_v4(),
            guest: self._identity.guest_id.clone(),
            envelope: envelope.clone(),
        };
        let reply = self.protected_rpc(peer, request, timeout).await?;
        match reply.outcome {
            ProtectedAuthorityOutcome::RevokedPendingQuiescence {
                task_id,
                authority_handle,
            } if task_id == envelope.task_id && authority_handle == envelope.authority_handle => {
                Ok(LocalCancellation::RevokedPendingQuiescence)
            }
            ProtectedAuthorityOutcome::Denied => bail!("protected cancellation denied"),
            _ => {
                self.disconnect();
                bail!("protected cancellation response mismatch");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GuestIdentity;
    use std::collections::VecDeque;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;
    fn fixture() -> (
        PhiloticClient,
        UnixStream,
        TrustedHotelPeer,
        LocalTaskEnvelope,
    ) {
        let (stream, server) = UnixStream::pair().unwrap();
        let credentials = stream.peer_cred().unwrap();
        let peer = TrustedHotelPeer {
            pid: u32::try_from(credentials.pid().unwrap()).unwrap(),
            uid: credentials.uid(),
        };
        let client = PhiloticClient {
            stream: Some(stream),
            _identity: GuestIdentity {
                guest_id: "synthetic-guest".into(),
                role: "untrusted-admin-claim".into(),
                supported_tools: vec![],
            },
            pending_push: VecDeque::new(),
            read_buf: vec![],
            pending_stale_responses: 0,
        };
        let envelope = LocalTaskEnvelope {
            task_id: Uuid::new_v4(),
            payload: "synthetic full task".into(),
            authority_handle: Uuid::new_v4(),
        };
        (client, server, peer, envelope)
    }
    async fn request(server: &mut UnixStream) -> ProtectedAuthorityRequest {
        let n = server.read_u32().await.unwrap();
        let mut bytes = vec![0; n as usize];
        server.read_exact(&mut bytes).await.unwrap();
        match serde_json::from_slice(&bytes).unwrap() {
            IpcRequest::ProtectedAuthority(request) => request,
            _ => panic!("wrong RPC"),
        }
    }
    async fn reply(server: &mut UnixStream, response: IpcResponse) {
        let bytes = serde_json::to_vec(&response).unwrap();
        server.write_u32(bytes.len() as u32).await.unwrap();
        server.write_all(&bytes).await.unwrap();
    }
    fn resolved(request: &ProtectedAuthorityRequest) -> IpcResponse {
        let envelope = request.envelope();
        IpcResponse::ProtectedAuthorityReply {
            protected_authority: ProtectedAuthorityReply {
                request_id: request.request_id(),
                outcome: ProtectedAuthorityOutcome::Resolved {
                    task_id: envelope.task_id,
                    authority_handle: envelope.authority_handle,
                    payload_digest: capture_payload_digest(&envelope.payload),
                    consumer_incarnation: Uuid::new_v4(),
                    policy_revision: 1,
                    sources: vec!["synthetic-source".into()],
                    actor_id: "synthetic-owner".into(),
                    roles: BTreeSet::new(),
                    policies: BTreeMap::from([(
                        "synthetic-source".into(),
                        ResourcePolicy::private("synthetic-owner".into(), "synthetic-owner".into()),
                    )]),
                },
            },
        }
    }
    #[tokio::test]
    async fn protected_rpc_resolution_and_cancel_use_distinct_correlated_replies() {
        let (mut client, mut server, peer, envelope) = fixture();
        let hotel = tokio::spawn(async move {
            let r = request(&mut server).await;
            reply(&mut server, IpcResponse::NetworkState { online: true }).await;
            reply(&mut server, resolved(&r)).await;
            let c = request(&mut server).await;
            reply(
                &mut server,
                IpcResponse::ProtectedAuthorityReply {
                    protected_authority: ProtectedAuthorityReply {
                        request_id: c.request_id(),
                        outcome: ProtectedAuthorityOutcome::RevokedPendingQuiescence {
                            task_id: c.envelope().task_id,
                            authority_handle: c.envelope().authority_handle,
                        },
                    },
                },
            )
            .await;
        });
        let context = client
            .resolve_local_authority(
                peer,
                &envelope,
                "local",
                ProcessingOperation::Inference,
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert_eq!(context.actor.stable_agent_id(), "synthetic-owner");
        assert!(
            ansible_mesh_core::privacy::authorize_processing(
                &context.policies,
                Some(&context.actor),
                &context.sources,
                ProcessingOperation::Inference,
                ansible_mesh_core::privacy::ProviderBoundary::External
            )
            .is_err()
        );
        assert_eq!(
            client
                .cancel_local_authority(peer, &envelope, Duration::from_secs(1))
                .await
                .unwrap(),
            LocalCancellation::RevokedPendingQuiescence
        );
        hotel.await.unwrap();
    }
    #[tokio::test]
    async fn protected_rpc_wrong_hotel_pid_denies_before_any_write() {
        let (mut client, mut server, mut peer, envelope) = fixture();
        peer.pid = 0;
        assert!(
            client
                .resolve_local_authority(
                    peer,
                    &envelope,
                    "local",
                    ProcessingOperation::Inference,
                    Duration::from_secs(1)
                )
                .await
                .is_err()
        );
        assert!(client.stream.is_none());
        let mut byte = [0];
        assert_eq!(server.read(&mut byte).await.unwrap(), 0);
    }
    #[tokio::test]
    async fn protected_rpc_generic_ack_unknown_id_and_modified_digest_disconnect() {
        for kind in 0..3 {
            let (mut client, mut server, peer, envelope) = fixture();
            let hotel = tokio::spawn(async move {
                let r = request(&mut server).await;
                let mut response = resolved(&r);
                match kind {
                    0 => {
                        response = IpcResponse::Ack {
                            req_id: r.request_id().to_string(),
                        }
                    }
                    1 => {
                        if let IpcResponse::ProtectedAuthorityReply {
                            protected_authority,
                        } = &mut response
                        {
                            protected_authority.request_id = Uuid::new_v4();
                        }
                    }
                    _ => {
                        if let IpcResponse::ProtectedAuthorityReply {
                            protected_authority,
                        } = &mut response
                            && let ProtectedAuthorityOutcome::Resolved { payload_digest, .. } =
                                &mut protected_authority.outcome
                        {
                            payload_digest.push('x');
                        }
                    }
                }
                reply(&mut server, response).await;
            });
            assert!(
                client
                    .resolve_local_authority(
                        peer,
                        &envelope,
                        "local",
                        ProcessingOperation::Inference,
                        Duration::from_secs(1)
                    )
                    .await
                    .is_err()
            );
            assert!(client.stream.is_none());
            hotel.await.unwrap();
        }
    }
    #[tokio::test]
    async fn protected_rpc_timeout_closes_connection_and_unsolicited_reply_denies() {
        let (mut client, mut server, peer, envelope) = fixture();
        assert!(
            client
                .resolve_local_authority(
                    peer,
                    &envelope,
                    "local",
                    ProcessingOperation::Inference,
                    Duration::from_millis(10)
                )
                .await
                .is_err()
        );
        assert!(client.stream.is_none());
        let r = request(&mut server).await;
        let (mut client, mut server, _, _) = fixture();
        reply(&mut server, resolved(&r)).await;
        assert!(client.recv_task().await.is_err());
        assert!(client.stream.is_none());
    }
}
