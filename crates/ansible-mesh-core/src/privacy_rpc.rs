//! Opt-in local authority RPC. Wire values alone never authenticate a caller.
use crate::privacy::{ProcessingOperation, ProviderBoundary, ResourcePolicy};
use crate::privacy_local::{
    LocalLaunchRegistry, LocalTaskAuthority, LocalTaskEnvelope, VerifiedLocalSession,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tokio::net::UnixStream;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", deny_unknown_fields, rename_all = "snake_case")]
pub enum ProtectedAuthorityRequest {
    Resolve {
        request_id: Uuid,
        guest: String,
        envelope: LocalTaskEnvelope,
        endpoint: String,
        operation: ProcessingOperation,
    },
    Cancel {
        request_id: Uuid,
        guest: String,
        envelope: LocalTaskEnvelope,
    },
}
impl ProtectedAuthorityRequest {
    pub fn request_id(&self) -> Uuid {
        match self {
            Self::Resolve { request_id, .. } | Self::Cancel { request_id, .. } => *request_id,
        }
    }
    pub fn guest(&self) -> &str {
        match self {
            Self::Resolve { guest, .. } | Self::Cancel { guest, .. } => guest,
        }
    }
    pub fn envelope(&self) -> &LocalTaskEnvelope {
        match self {
            Self::Resolve { envelope, .. } | Self::Cancel { envelope, .. } => envelope,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedAuthorityReply {
    pub request_id: Uuid,
    pub outcome: ProtectedAuthorityOutcome,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", deny_unknown_fields, rename_all = "snake_case")]
pub enum ProtectedAuthorityOutcome {
    Denied,
    Resolved {
        task_id: Uuid,
        authority_handle: Uuid,
        payload_digest: String,
        consumer_incarnation: Uuid,
        policy_revision: u64,
        sources: Vec<String>,
        actor_id: String,
        roles: BTreeSet<String>,
        policies: BTreeMap<String, ResourcePolicy>,
    },
    /// Admission is revoked. This is never a successful runtime stop response.
    RevokedPendingQuiescence {
        task_id: Uuid,
        authority_handle: Uuid,
    },
}
impl ProtectedAuthorityReply {
    pub fn denied(request_id: Uuid) -> Self {
        Self {
            request_id,
            outcome: ProtectedAuthorityOutcome::Denied,
        }
    }
}

/// Must share the canonical issuer and supervisor registry. Endpoint
/// classification is installed by the server, never supplied on wire.
pub struct LocalAuthorityRpc {
    hotel: String,
    launches: Arc<LocalLaunchRegistry>,
    authority: Arc<LocalTaskAuthority>,
    endpoints: BTreeMap<String, ProviderBoundary>,
}
impl LocalAuthorityRpc {
    pub fn new(
        hotel: String,
        launches: Arc<LocalLaunchRegistry>,
        authority: Arc<LocalTaskAuthority>,
        endpoints: BTreeMap<String, ProviderBoundary>,
    ) -> anyhow::Result<Self> {
        authority.check_registry(&hotel, &launches)?;
        Ok(Self {
            hotel,
            launches,
            authority,
            endpoints,
        })
    }
    pub fn authenticate(
        &self,
        stream: &UnixStream,
        request: &ProtectedAuthorityRequest,
    ) -> anyhow::Result<Arc<VerifiedLocalSession>> {
        self.launches
            .authenticate(&self.hotel, stream, request.guest())
    }
    /// Run on a blocking worker: resolution accesses the dedicated SQLite store.
    pub fn handle(
        &self,
        session: &VerifiedLocalSession,
        request: &ProtectedAuthorityRequest,
    ) -> ProtectedAuthorityReply {
        let outcome = (|| -> anyhow::Result<ProtectedAuthorityOutcome> {
            if !session.matches_guest(&self.hotel, request.guest()) {
                anyhow::bail!("RPC session/request mismatch");
            }
            match request {
                ProtectedAuthorityRequest::Resolve {
                    envelope,
                    endpoint,
                    operation,
                    ..
                } => {
                    let boundary = self.endpoints.get(endpoint).copied().unwrap_or_default();
                    let resolved = self
                        .authority
                        .resolve(envelope, session, *operation, boundary)?;
                    let policies = resolved.policies.source_closure(&resolved.sources)?;
                    Ok(ProtectedAuthorityOutcome::Resolved {
                        task_id: envelope.task_id,
                        authority_handle: envelope.authority_handle,
                        payload_digest: resolved.payload_digest,
                        consumer_incarnation: resolved.consumer_incarnation,
                        policy_revision: resolved.policies.revision(),
                        sources: resolved.sources,
                        actor_id: resolved.actor.stable_agent_id().into(),
                        roles: resolved.actor.roles(),
                        policies,
                    })
                }
                ProtectedAuthorityRequest::Cancel { envelope, .. } => {
                    self.authority.cancel_envelope(session, envelope)?;
                    Ok(ProtectedAuthorityOutcome::RevokedPendingQuiescence {
                        task_id: envelope.task_id,
                        authority_handle: envelope.authority_handle,
                    })
                }
            }
        })()
        .unwrap_or(ProtectedAuthorityOutcome::Denied);
        ProtectedAuthorityReply {
            request_id: request.request_id(),
            outcome,
        }
    }
}
