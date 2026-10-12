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
    ExplainModelRoute {
        request_id: Uuid,
        guest: String,
        envelope: LocalTaskEnvelope,
        catalog_revision: u64,
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
            Self::Resolve { request_id, .. }
            | Self::Cancel { request_id, .. }
            | Self::ExplainModelRoute { request_id, .. } => *request_id,
        }
    }
    pub fn guest(&self) -> &str {
        match self {
            Self::Resolve { guest, .. }
            | Self::Cancel { guest, .. }
            | Self::ExplainModelRoute { guest, .. } => guest,
        }
    }
    pub fn envelope(&self) -> &LocalTaskEnvelope {
        match self {
            Self::Resolve { envelope, .. }
            | Self::Cancel { envelope, .. }
            | Self::ExplainModelRoute { envelope, .. } => envelope,
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
    ModelRoute {
        task_id: Uuid,
        catalog_revision: u64,
        policy_revision: u64,
        payload_digest: String,
        route: crate::route_composition::EffectiveRoute,
    },
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
/// Installed by the hotel. Projection must cover the exact bound payload,
/// reject unsupported/native-live operations, and derive trust from server policy.
/// It cannot grant processing access; canonical authority checks every endpoint.
pub trait BoundRouteProjection: Send + Sync {
    fn project(
        &self,
        payload: &str,
    ) -> anyhow::Result<(crate::model_oracle::RouteNeed, ProcessingOperation)>;
}

/// Explicit installation opts this protected RPC into route composition.
/// Policies, aliases and endpoints are server-owned; none are accepted on wire.
pub struct HotelRouteCatalog {
    pub revision: u64,
    pub policies: BTreeMap<String, crate::route_composition::RoutePolicy>,
    pub waterfall: Vec<String>,
    pub candidates: BTreeMap<String, crate::route_composition::ResolvedCandidate>,
    pub projection: Arc<dyn BoundRouteProjection>,
    pub cooloff_secs: u64,
}

pub struct LocalAuthorityRpc {
    hotel: String,
    launches: Arc<LocalLaunchRegistry>,
    authority: Arc<LocalTaskAuthority>,
    endpoints: BTreeMap<String, ProviderBoundary>,
    routes: Option<HotelRouteCatalog>,
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
            routes: None,
        })
    }
    pub fn with_route_catalog(mut self, catalog: HotelRouteCatalog) -> Self {
        self.routes = Some(catalog);
        self
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
                ProtectedAuthorityRequest::ExplainModelRoute {
                    envelope,
                    catalog_revision,
                    ..
                } => {
                    use crate::route_composition::{compose_effective_route, Admission};
                    let catalog = self
                        .routes
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("route catalog not installed"))?;
                    anyhow::ensure!(*catalog_revision == catalog.revision, "stale route catalog");
                    let (need, operation) = catalog.projection.project(&envelope.payload)?;
                    let mut candidates = catalog.candidates.clone();
                    let mut binding = None;
                    for candidate in candidates.values_mut() {
                        // Catalog access denial remains a restriction, never a grant.
                        if candidate.admission != Admission::Allowed {
                            continue;
                        }
                        candidate.admission = Admission::PrivacyDenied;
                        let boundary = self
                            .endpoints
                            .get(&candidate.identity.endpoint)
                            .copied()
                            .unwrap_or_default();
                        if let Ok(resolved) = self
                            .authority
                            .resolve(envelope, session, operation, boundary)
                        {
                            let current = (
                                resolved.actor.stable_agent_id().to_owned(),
                                resolved.policies.revision(),
                                resolved.payload_digest,
                            );
                            if let Some(previous) = &binding {
                                anyhow::ensure!(
                                    previous == &current,
                                    "authority changed while composing route"
                                );
                            } else {
                                binding = Some(current);
                            }
                            candidate.admission = Admission::Allowed;
                        }
                    }
                    // No admitted endpoint means no authenticated policy projection.
                    let (actor, policy_revision, payload_digest) =
                        binding.ok_or_else(|| anyhow::anyhow!("no admitted endpoint"))?;
                    let policy = catalog
                        .policies
                        .get(&actor)
                        .ok_or_else(|| anyhow::anyhow!("route policy absent"))?;
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_secs();
                    let route = compose_effective_route(
                        policy,
                        &catalog.waterfall,
                        &candidates,
                        &need,
                        now,
                        catalog.cooloff_secs,
                    );
                    Ok(ProtectedAuthorityOutcome::ModelRoute {
                        task_id: envelope.task_id,
                        catalog_revision: catalog.revision,
                        policy_revision,
                        payload_digest,
                        route,
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

#[cfg(test)]
mod route_wire_tests {
    use super::*;

    #[test]
    fn route_request_preserves_authenticated_envelope_and_correlation() {
        let request_id = Uuid::new_v4();
        let envelope = LocalTaskEnvelope {
            task_id: Uuid::new_v4(),
            payload: "bound task".into(),
            authority_handle: Uuid::new_v4(),
        };
        let request = ProtectedAuthorityRequest::ExplainModelRoute {
            request_id,
            guest: "model-router".into(),
            envelope,
            catalog_revision: 7,
        };
        let encoded = serde_json::to_value(&request).unwrap();
        let decoded: ProtectedAuthorityRequest = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded.request_id(), request_id);
        assert_eq!(decoded.guest(), "model-router");
        assert_eq!(decoded.envelope().payload, "bound task");
        let mut injected = encoded;
        injected["policy"] = serde_json::json!({"version": "preferences_then_hotel_v2"});
        assert!(serde_json::from_value::<ProtectedAuthorityRequest>(injected).is_err());
    }

    #[test]
    fn effective_route_diagnostics_round_trip_without_profiles_or_grants() {
        use crate::route_composition::*;
        let route = EffectiveRoute {
            candidates: vec![],
            strict_pin: true,
            diagnostics: vec![RouteDiagnostic {
                candidate: "missing".into(),
                origin: RouteOrigin::DirectOverride,
                disposition: CandidateDisposition::UnknownCandidate,
            }],
        };
        let encoded = serde_json::to_value(&route).unwrap();
        let decoded: EffectiveRoute = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(route, decoded);
        assert!(encoded.get("policies").is_none());
        assert!(encoded.get("sources").is_none());
    }
}
