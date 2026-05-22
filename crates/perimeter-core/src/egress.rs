use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use ansible_mesh_core::ExposureTier;

/// Per-tier egress policy — stored in mesh-config.json under `egress.tiers`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressPolicy {
    pub tier: ExposureTier,
    #[serde(default)]
    pub default_action: EgressDefaultAction,
    #[serde(default)]
    pub require_tls: bool,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    #[serde(default)]
    pub denied_hosts: Vec<String>,
    /// Vault-backed credentials injected for specific hosts.
    /// Key: hostname (e.g. "api.perplexity.ai"), Value: credential spec.
    #[serde(default)]
    pub credentials: HashMap<String, EgressCredential>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EgressDefaultAction {
    #[default]
    Allow,
    Deny,
    AllowWithAudit,
}

/// Vault-backed credential injected into an outbound request header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressCredential {
    /// Key to resolve from the vault.
    pub vault_key: String,
    /// HTTP header name to set (e.g. "Authorization").
    pub header: String,
    /// Format string with `{}` placeholder for the resolved value (e.g. "Bearer {}").
    pub format: String,
}

/// The outcome of an egress check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressDecision {
    Allow,
    AllowWithAudit,
    Deny { reason: String },
}

impl EgressDecision {
    pub fn is_allowed(&self) -> bool {
        !matches!(self, EgressDecision::Deny { .. })
    }
}

/// An outbound request submitted to the EgressGateway for policy evaluation.
#[derive(Debug, Clone)]
pub struct EgressRequest {
    pub agent_id: String,
    pub target_url: String,
    pub method: String,
    pub headers: HashMap<String, String>,
    /// The tier of the hotel listener the agent is operating on.
    pub tier: ExposureTier,
}

/// Trait implemented by the hotel's EgressGateway.
/// Agents submit outbound requests here for policy check and optional credential injection.
#[async_trait]
pub trait EgressGateway: Send + Sync {
    /// Check whether the outbound request is permitted under the current policy.
    fn check(&self, request: &EgressRequest) -> EgressDecision;

    /// Inject vault-backed credentials into the request headers if a credential spec
    /// exists for the target host. Mutates `request.headers` in place.
    async fn inject_credentials(&self, request: &mut EgressRequest) -> anyhow::Result<()>;
}
