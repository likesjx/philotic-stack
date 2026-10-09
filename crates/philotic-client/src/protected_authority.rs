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
use ansible_mesh_core::route_composition::{CandidateIdentity, EffectiveRoute};
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
/// Expected catalog metadata used only to compare diagnostic replies. This
/// carries no admission, credentials or dispatch authority. The caller must
/// refresh it when the hotel's catalog changes; a mismatch fails closed.
pub struct RouteExplainCatalog {
    pub revision: u64,
    pub candidates: BTreeSet<CandidateIdentity>,
}

/// A correlated explanation for one bound payload, never a dispatch grant.
/// Every actual provider attempt still requires fresh canonical admission.
#[derive(Debug)]
pub struct BoundRouteExplanation {
    pub task_id: Uuid,
    pub payload_digest: String,
    pub catalog_revision: u64,
    pub policy_revision: u64,
    pub route: EffectiveRoute,
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
    /// Read-only route diagnostics through the kernel-peer-verified protected
    /// RPC. No legacy query fallback, provider invocation or policy write.
    pub async fn explain_local_model_route(
        &mut self,
        peer: TrustedHotelPeer,
        envelope: &LocalTaskEnvelope,
        catalog: &RouteExplainCatalog,
        timeout: Duration,
    ) -> Result<BoundRouteExplanation> {
        let request_id = Uuid::new_v4();
        let request = ProtectedAuthorityRequest::ExplainModelRoute {
            request_id,
            guest: self._identity.guest_id.clone(),
            envelope: envelope.clone(),
            catalog_revision: catalog.revision,
        };
        let reply = self.protected_rpc(peer, request, timeout).await?;
        if reply.request_id != request_id {
            self.disconnect();
            bail!("protected route request binding mismatch");
        }
        match reply.outcome {
            ProtectedAuthorityOutcome::ModelRoute {
                task_id,
                catalog_revision,
                policy_revision,
                payload_digest,
                route,
            } => {
                let identities: BTreeSet<_> = route.candidates.iter().cloned().collect();
                if task_id != envelope.task_id
                    || payload_digest != capture_payload_digest(&envelope.payload)
                    || catalog_revision != catalog.revision
                    || identities.len() != route.candidates.len()
                    || !identities.is_subset(&catalog.candidates)
                    || (route.strict_pin && route.candidates.len() > 1)
                {
                    self.disconnect();
                    bail!("protected route response binding mismatch");
                }
                Ok(BoundRouteExplanation {
                    task_id,
                    payload_digest,
                    catalog_revision,
                    policy_revision,
                    route,
                })
            }
            ProtectedAuthorityOutcome::Denied => bail!("protected route explanation denied"),
            _ => {
                self.disconnect();
                bail!("protected route response type mismatch");
            }
        }
    }

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
    fn explain_catalog() -> RouteExplainCatalog {
        RouteExplainCatalog {
            revision: 7,
            candidates: BTreeSet::from([CandidateIdentity {
                provider: "synthetic".into(),
                model: "synthetic".into(),
                endpoint: "opaque-endpoint".into(),
                credential_scope: "opaque-credential".into(),
                hotel: "synthetic-hotel".into(),
                incarnation: "synthetic-incarnation".into(),
                policy_scope: "synthetic-policy".into(),
            }]),
        }
    }
    fn explained(request: &ProtectedAuthorityRequest) -> IpcResponse {
        assert!(
            matches!(request, ProtectedAuthorityRequest::ExplainModelRoute { catalog_revision: 7, guest, .. } if guest == "synthetic-guest")
        );
        let catalog = explain_catalog();
        IpcResponse::ProtectedAuthorityReply {
            protected_authority: ProtectedAuthorityReply {
                request_id: request.request_id(),
                outcome: ProtectedAuthorityOutcome::ModelRoute {
                    task_id: request.envelope().task_id,
                    payload_digest: capture_payload_digest(&request.envelope().payload),
                    catalog_revision: 7,
                    policy_revision: 9,
                    route: EffectiveRoute {
                        candidates: catalog.candidates.into_iter().collect(),
                        diagnostics: vec![],
                        strict_pin: false,
                    },
                },
            },
        }
    }
    fn second_explain_identity() -> CandidateIdentity {
        let mut identity = explain_catalog().candidates.into_iter().next().unwrap();
        identity.endpoint.push_str("-second");
        identity
    }

    #[tokio::test]
    async fn protected_explain_returns_bound_diagnostics_and_empty_strict_plan() {
        for empty_strict in [false, true] {
            let (mut client, mut server, peer, envelope) = fixture();
            let hotel = tokio::spawn(async move {
                let request = request(&mut server).await;
                let mut response = explained(&request);
                if empty_strict
                    && let IpcResponse::ProtectedAuthorityReply {
                        protected_authority,
                    } = &mut response
                    && let ProtectedAuthorityOutcome::ModelRoute { route, .. } =
                        &mut protected_authority.outcome
                {
                    route.candidates.clear();
                    route.strict_pin = true;
                }
                reply(&mut server, response).await;
            });
            let explanation = client
                .explain_local_model_route(
                    peer,
                    &envelope,
                    &explain_catalog(),
                    Duration::from_secs(1),
                )
                .await
                .unwrap();
            assert_eq!(explanation.task_id, envelope.task_id);
            assert_eq!(
                explanation.payload_digest,
                capture_payload_digest(&envelope.payload)
            );
            assert_eq!(explanation.catalog_revision, 7);
            assert_eq!(explanation.policy_revision, 9);
            assert_eq!(explanation.route.strict_pin, empty_strict);
            assert_eq!(
                explanation.route.candidates.len(),
                usize::from(!empty_strict)
            );
            hotel.await.unwrap();
        }
    }

    #[tokio::test]
    async fn protected_explain_rejects_wrong_binding_identity_and_response_type() {
        // Request/task/digest/catalog, all seven identity dimensions, duplicate,
        // expanded strict pin, generic ACK, and a resolution-as-explanation.
        for kind in 0..15 {
            let (mut client, mut server, peer, envelope) = fixture();
            let hotel = tokio::spawn(async move {
                let request = request(&mut server).await;
                let mut response = explained(&request);
                if kind == 13 {
                    response = IpcResponse::Ack {
                        req_id: request.request_id().to_string(),
                    };
                } else if kind == 14 {
                    response = resolved(&request);
                } else if let IpcResponse::ProtectedAuthorityReply {
                    protected_authority,
                } = &mut response
                {
                    if kind == 0 {
                        protected_authority.request_id = Uuid::new_v4();
                    } else if let ProtectedAuthorityOutcome::ModelRoute {
                        task_id,
                        payload_digest,
                        catalog_revision,
                        route,
                        ..
                    } = &mut protected_authority.outcome
                    {
                        match kind {
                            1 => *task_id = Uuid::new_v4(),
                            2 => payload_digest.push('x'),
                            3 => *catalog_revision = 6,
                            4 => route.candidates[0].provider.push('x'),
                            5 => route.candidates[0].model.push('x'),
                            6 => route.candidates[0].endpoint.push('x'),
                            7 => route.candidates[0].credential_scope.push('x'),
                            8 => route.candidates[0].hotel.push('x'),
                            9 => route.candidates[0].incarnation.push('x'),
                            10 => route.candidates[0].policy_scope.push('x'),
                            11 => {
                                route.candidates.push(route.candidates[0].clone());
                            }
                            12 => {
                                route.candidates.push(second_explain_identity());
                                route.strict_pin = true;
                            }
                            _ => unreachable!(),
                        }
                    }
                }
                reply(&mut server, response).await;
            });
            let mut catalog = explain_catalog();
            if kind == 12 {
                // Both endpoints belong to the expected catalog. This case
                // isolates strict-pin expansion from identity/duplicate denial.
                catalog.candidates.insert(second_explain_identity());
            }
            assert!(
                client
                    .explain_local_model_route(peer, &envelope, &catalog, Duration::from_secs(1))
                    .await
                    .is_err(),
                "case {kind}"
            );
            assert!(client.stream.is_none(), "case {kind}");
            hotel.await.unwrap();
        }
    }

    #[tokio::test]
    async fn protected_explain_denial_does_not_fallback_or_disconnect() {
        let (mut client, mut server, peer, envelope) = fixture();
        let hotel = tokio::spawn(async move {
            let first = request(&mut server).await;
            reply(
                &mut server,
                IpcResponse::ProtectedAuthorityReply {
                    protected_authority: ProtectedAuthorityReply::denied(first.request_id()),
                },
            )
            .await;
            let second = request(&mut server).await;
            assert_ne!(first.request_id(), second.request_id());
            reply(&mut server, explained(&second)).await;
        });
        assert!(
            client
                .explain_local_model_route(
                    peer,
                    &envelope,
                    &explain_catalog(),
                    Duration::from_secs(1)
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("denied")
        );
        assert!(client.stream.is_some());
        assert!(
            client
                .explain_local_model_route(
                    peer,
                    &envelope,
                    &explain_catalog(),
                    Duration::from_secs(1)
                )
                .await
                .is_ok()
        );
        hotel.await.unwrap();
    }

    #[tokio::test]
    async fn protected_explain_replayed_reply_cannot_answer_next_request() {
        let (mut client, mut server, peer, envelope) = fixture();
        let hotel = tokio::spawn(async move {
            let first = request(&mut server).await;
            reply(&mut server, explained(&first)).await;
            let second = request(&mut server).await;
            assert_ne!(first.request_id(), second.request_id());
            reply(&mut server, explained(&first)).await;
        });
        assert!(
            client
                .explain_local_model_route(
                    peer,
                    &envelope,
                    &explain_catalog(),
                    Duration::from_secs(1)
                )
                .await
                .is_ok()
        );
        assert!(
            client
                .explain_local_model_route(
                    peer,
                    &envelope,
                    &explain_catalog(),
                    Duration::from_secs(1)
                )
                .await
                .is_err()
        );
        assert!(client.stream.is_none());
        hotel.await.unwrap();
    }

    #[tokio::test]
    async fn protected_explain_transport_failure_and_timeout_close_connection() {
        for truncated in [false, true] {
            let (mut client, mut server, peer, envelope) = fixture();
            let hotel = tokio::spawn(async move {
                let _ = request(&mut server).await;
                if truncated {
                    server.write_u32(32).await.unwrap();
                    server.write_all(b"incomplete").await.unwrap();
                }
            });
            assert!(
                client
                    .explain_local_model_route(
                        peer,
                        &envelope,
                        &explain_catalog(),
                        Duration::from_secs(1)
                    )
                    .await
                    .is_err()
            );
            assert!(client.stream.is_none());
            hotel.await.unwrap();
        }
        let (mut client, mut server, peer, mut envelope) = fixture();
        envelope.payload = "s".repeat(1_048_576);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            client.explain_local_model_route(
                peer,
                &envelope,
                &explain_catalog(),
                Duration::from_millis(10),
            ),
        )
        .await;
        assert!(
            result.is_ok(),
            "SDK deadline must include backpressured write"
        );
        assert!(result.unwrap().is_err());
        assert!(client.stream.is_none());
        let mut bytes = vec![];
        server.read_to_end(&mut bytes).await.unwrap();
        assert!(!bytes.is_empty());
        let (mut client, mut server, peer, envelope) = fixture();
        assert!(
            client
                .explain_local_model_route(
                    peer,
                    &envelope,
                    &explain_catalog(),
                    Duration::from_millis(10)
                )
                .await
                .is_err()
        );
        assert!(client.stream.is_none());
        let old = request(&mut server).await;
        let (mut client, mut server, _, _) = fixture();
        reply(&mut server, explained(&old)).await;
        assert!(client.recv_task().await.is_err());
        assert!(client.stream.is_none());
    }

    #[tokio::test]
    async fn protected_explain_wrong_hotel_peer_denies_before_write() {
        let (mut client, mut server, mut peer, envelope) = fixture();
        peer.pid = 0;
        assert!(
            client
                .explain_local_model_route(
                    peer,
                    &envelope,
                    &explain_catalog(),
                    Duration::from_secs(1)
                )
                .await
                .is_err()
        );
        assert!(client.stream.is_none());
        assert_eq!(server.read(&mut [0]).await.unwrap(), 0);
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
    #[tokio::test]
    async fn protected_cancel_never_promotes_resolution_or_claimed_stop_to_quiescence() {
        for claimed_stop in [false, true] {
            let (mut client, mut server, peer, envelope) = fixture();
            let hotel = tokio::spawn(async move {
                let r = request(&mut server).await;
                if claimed_stop {
                    let bytes = serde_json::to_vec(&serde_json::json!({
                        "protected_authority": { "request_id": r.request_id(), "outcome": { "status": "confirmed_stop" } }
                    })).unwrap();
                    server.write_u32(bytes.len() as u32).await.unwrap();
                    server.write_all(&bytes).await.unwrap();
                } else {
                    reply(&mut server, resolved(&r)).await;
                }
            });
            assert!(
                client
                    .cancel_local_authority(peer, &envelope, Duration::from_secs(1))
                    .await
                    .is_err()
            );
            assert!(client.stream.is_none());
            hotel.await.unwrap();
        }
    }

    #[tokio::test]
    async fn protected_rpc_deadline_includes_backpressured_partial_write() {
        let (mut client, mut server, peer, mut envelope) = fixture();
        envelope.payload = "s".repeat(1_048_576);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            client.resolve_local_authority(
                peer,
                &envelope,
                "local",
                ProcessingOperation::Inference,
                Duration::from_millis(20),
            ),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        assert!(client.stream.is_none());
        let mut bytes = vec![];
        tokio::time::timeout(Duration::from_secs(1), server.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert!(bytes.len() >= 4);
        let advertised = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize + 4;
        assert!(
            bytes.len() < advertised,
            "fixture must interrupt the write, not only its reply wait"
        );
    }
}
