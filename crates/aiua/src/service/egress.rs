use std::collections::HashMap;
use std::sync::Arc;

use ansible_mesh_core::ExposureTier;
use ansible_mesh_core::domain::GraphDomain;
use async_trait::async_trait;
use perimeter_core::egress::{
    EgressCredential, EgressDecision, EgressGateway, EgressPolicy, EgressRequest,
    evaluate_egress_policy, host_from_url,
};
use tracing::{debug, warn};

use crate::vault::{SecretAccess, resolve_secret};

/// Concrete hotel-level EgressGateway.
///
/// Holds a per-tier policy table (loaded from config) and resolves vault-backed
/// credentials via the hotel's graph domain.
pub struct HotelEgressGateway {
    /// Policies keyed by tier. If no policy exists for a tier, defaults to Allow.
    policies: HashMap<ExposureTier, EgressPolicy>,
    graph: Arc<GraphDomain>,
}

impl HotelEgressGateway {
    pub fn new(policies: Vec<EgressPolicy>, graph: Arc<GraphDomain>) -> Self {
        let policies = policies.into_iter().map(|p| (p.tier, p)).collect();
        Self { policies, graph }
    }

    fn credential_for_host<'a>(
        policy: &'a EgressPolicy,
        host: &str,
    ) -> Option<&'a EgressCredential> {
        policy.credentials.get(host)
    }
}

#[async_trait]
impl EgressGateway for HotelEgressGateway {
    fn check(&self, request: &EgressRequest) -> EgressDecision {
        evaluate_egress_policy(&self.policies, request)
    }

    async fn inject_credentials(&self, request: &mut EgressRequest) -> anyhow::Result<()> {
        let policy = match self.policies.get(&request.tier) {
            Some(p) => p,
            None => return Ok(()),
        };

        let host = match host_from_url(&request.target_url) {
            Some(h) => h.to_string(),
            None => return Ok(()),
        };

        let cred = match Self::credential_for_host(policy, &host) {
            Some(c) => c.clone(),
            None => return Ok(()),
        };

        let access = SecretAccess {
            role: "hotel".into(),
            guest_id: "hotel-egress-gateway".into(),
        };

        match resolve_secret(&self.graph, &cred.vault_key, &access) {
            Ok(Some(value)) => {
                let header_value = cred.format.replace("{}", &value);
                debug!(host, header = %cred.header, "injecting egress credential");
                request.headers.insert(cred.header.clone(), header_value);
            }
            Ok(None) => {
                warn!(host, vault_key = %cred.vault_key, "egress credential vault_key not found");
            }
            Err(e) => {
                warn!(host, vault_key = %cred.vault_key, err = %e, "egress credential resolve failed");
            }
        }

        Ok(())
    }
}
