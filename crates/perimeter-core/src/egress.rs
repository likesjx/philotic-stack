use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use ansible_mesh_core::ExposureTier;

/// Canonical outbound traffic classes used by policy and placement.
///
/// Classification is explicit input. Callers must not rely on the executor
/// guessing policy from a URL after the route has already been selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EgressTrafficClass {
    Communication,
    #[default]
    GeneralApi,
    Mcp,
    ModelProvider,
    MeshPeer,
    LocalResource,
    Artifact,
}

/// Behavior when a preferred exit hotel is not reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EgressFallback {
    #[default]
    Deny,
    LocalWithAudit,
}

/// Placement policy for the component that performs outbound network I/O.
///
/// `hotel_id` names a policy identity, not a hard-coded implementation host.
/// Deployment may bind that identity to `vps-jane`, but the architecture does
/// not make one physical machine a universal transit hop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum EgressPlacementPolicy {
    #[default]
    Local,
    PreferHotel {
        hotel_id: String,
        #[serde(default)]
        fallback: EgressFallback,
    },
    RequireHotel {
        hotel_id: String,
    },
    Deny,
}

/// Resolved execution target for one outbound request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum EgressPlacementDecision {
    ExecuteLocal { audit_fallback: bool },
    ExecuteAtHotel { hotel_id: String },
    Deny { reason: String },
}

/// Resolve an egress placement policy against current exit reachability.
///
/// This is intentionally a pure decision. Routing and materialization remain
/// hotel responsibilities and the eventual HTTP runner owns the network call.
pub fn decide_egress_placement(
    policy: &EgressPlacementPolicy,
    exit_hotel_reachable: bool,
) -> EgressPlacementDecision {
    match policy {
        EgressPlacementPolicy::Local => EgressPlacementDecision::ExecuteLocal {
            audit_fallback: false,
        },
        EgressPlacementPolicy::PreferHotel { hotel_id, fallback } => {
            if exit_hotel_reachable {
                EgressPlacementDecision::ExecuteAtHotel {
                    hotel_id: hotel_id.clone(),
                }
            } else {
                match fallback {
                    EgressFallback::Deny => EgressPlacementDecision::Deny {
                        reason: format!("preferred exit hotel '{hotel_id}' is unreachable"),
                    },
                    EgressFallback::LocalWithAudit => EgressPlacementDecision::ExecuteLocal {
                        audit_fallback: true,
                    },
                }
            }
        }
        EgressPlacementPolicy::RequireHotel { hotel_id } => {
            if exit_hotel_reachable {
                EgressPlacementDecision::ExecuteAtHotel {
                    hotel_id: hotel_id.clone(),
                }
            } else {
                EgressPlacementDecision::Deny {
                    reason: format!("required exit hotel '{hotel_id}' is unreachable"),
                }
            }
        }
        EgressPlacementPolicy::Deny => EgressPlacementDecision::Deny {
            reason: "external egress is disabled by placement policy".into(),
        },
    }
}

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
    pub traffic_class: EgressTrafficClass,
    /// The tier of the hotel listener the agent is operating on.
    pub tier: ExposureTier,
}

/// Trait implemented by the hotel's EgressGateway.
/// Agents submit outbound requests here for policy check and optional credential injection.
#[async_trait]
pub trait EgressGateway: Send + Sync {
    /// Check whether the outbound request is permitted under the current policy.
    fn check(&self, request: &EgressRequest) -> EgressDecision;

    /// Report whether policy names a credential binding for this target.
    ///
    /// This never resolves or returns the credential. Resolution belongs inside
    /// the bounded executor that performs the request.
    fn credential_binding_configured(&self, _request: &EgressRequest) -> bool {
        false
    }

    /// Inject vault-backed credentials into the request headers if a credential spec
    /// exists for the target host. Mutates `request.headers` in place.
    async fn inject_credentials(&self, request: &mut EgressRequest) -> anyhow::Result<()>;
}

/// Pure policy evaluation — no I/O. Used by `HotelEgressGateway::check()` and unit tests.
pub fn evaluate_egress_policy(
    policies: &HashMap<ExposureTier, EgressPolicy>,
    request: &EgressRequest,
) -> EgressDecision {
    let policy = match policies.get(&request.tier) {
        Some(p) => p,
        None => return EgressDecision::Allow,
    };

    let host = host_from_url(&request.target_url).unwrap_or("");

    if policy.denied_hosts.iter().any(|h| h == host || h == "*") {
        return EgressDecision::Deny {
            reason: format!(
                "host '{}' is in the denied list for tier {:?}",
                host, request.tier
            ),
        };
    }

    if !policy.allowed_hosts.is_empty()
        && !policy.allowed_hosts.iter().any(|h| h == host || h == "*")
    {
        return EgressDecision::Deny {
            reason: format!(
                "host '{}' is not in the allowed list for tier {:?}",
                host, request.tier
            ),
        };
    }

    match &policy.default_action {
        EgressDefaultAction::Allow => EgressDecision::Allow,
        EgressDefaultAction::AllowWithAudit => EgressDecision::AllowWithAudit,
        EgressDefaultAction::Deny => EgressDecision::Deny {
            reason: format!("tier {:?} default action is deny", request.tier),
        },
    }
}

pub fn host_from_url(url: &str) -> Option<&str> {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host_port = without_scheme.split('/').next().unwrap_or(without_scheme);
    Some(host_port.split(':').next().unwrap_or(host_port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn internet_policy(default_action: EgressDefaultAction) -> EgressPolicy {
        EgressPolicy {
            tier: ExposureTier::Internet,
            default_action,
            require_tls: true,
            allowed_hosts: vec![],
            denied_hosts: vec![],
            credentials: HashMap::new(),
        }
    }

    fn req(url: &str) -> EgressRequest {
        EgressRequest {
            agent_id: "agent-test".into(),
            target_url: url.into(),
            method: "POST".into(),
            headers: HashMap::new(),
            traffic_class: EgressTrafficClass::GeneralApi,
            tier: ExposureTier::Internet,
        }
    }

    #[test]
    fn preferred_exit_uses_named_hotel_when_reachable() {
        let decision = decide_egress_placement(
            &EgressPlacementPolicy::PreferHotel {
                hotel_id: "vps-jane".into(),
                fallback: EgressFallback::LocalWithAudit,
            },
            true,
        );
        assert_eq!(
            decision,
            EgressPlacementDecision::ExecuteAtHotel {
                hotel_id: "vps-jane".into()
            }
        );
    }

    #[test]
    fn preferred_exit_can_fall_back_locally_with_audit() {
        let decision = decide_egress_placement(
            &EgressPlacementPolicy::PreferHotel {
                hotel_id: "vps-jane".into(),
                fallback: EgressFallback::LocalWithAudit,
            },
            false,
        );
        assert_eq!(
            decision,
            EgressPlacementDecision::ExecuteLocal {
                audit_fallback: true
            }
        );
    }

    #[test]
    fn required_exit_fails_closed_when_unreachable() {
        let decision = decide_egress_placement(
            &EgressPlacementPolicy::RequireHotel {
                hotel_id: "vps-jane".into(),
            },
            false,
        );
        assert_eq!(
            decision,
            EgressPlacementDecision::Deny {
                reason: "required exit hotel 'vps-jane' is unreachable".into()
            }
        );
    }

    #[test]
    fn deny_mode_never_selects_an_executor() {
        assert_eq!(
            decide_egress_placement(&EgressPlacementPolicy::Deny, true),
            EgressPlacementDecision::Deny {
                reason: "external egress is disabled by placement policy".into()
            }
        );
    }

    #[test]
    fn placement_policy_has_stable_data_shape() {
        let policy = EgressPlacementPolicy::PreferHotel {
            hotel_id: "vps-jane".into(),
            fallback: EgressFallback::LocalWithAudit,
        };
        let json = serde_json::to_value(&policy).expect("serialize placement policy");
        assert_eq!(json["mode"], "prefer_hotel");
        assert_eq!(json["hotel_id"], "vps-jane");
        assert_eq!(json["fallback"], "local_with_audit");

        let decoded: EgressPlacementPolicy =
            serde_json::from_value(json).expect("deserialize placement policy");
        assert_eq!(decoded, policy);
    }

    #[test]
    fn no_policy_allows() {
        let policies: HashMap<ExposureTier, EgressPolicy> = HashMap::new();
        assert!(evaluate_egress_policy(&policies, &req("https://api.example.com/v1")).is_allowed());
    }

    #[test]
    fn default_allow_permits() {
        let policies = [(
            ExposureTier::Internet,
            internet_policy(EgressDefaultAction::Allow),
        )]
        .into_iter()
        .collect();
        assert_eq!(
            evaluate_egress_policy(&policies, &req("https://api.example.com/v1")),
            EgressDecision::Allow
        );
    }

    #[test]
    fn default_deny_blocks() {
        let policies = [(
            ExposureTier::Internet,
            internet_policy(EgressDefaultAction::Deny),
        )]
        .into_iter()
        .collect();
        assert!(
            !evaluate_egress_policy(&policies, &req("https://api.example.com/v1")).is_allowed()
        );
    }

    #[test]
    fn denied_host_blocks() {
        let mut policy = internet_policy(EgressDefaultAction::Allow);
        policy.denied_hosts = vec!["api.example.com".into()];
        let policies = [(ExposureTier::Internet, policy)].into_iter().collect();
        assert!(
            !evaluate_egress_policy(&policies, &req("https://api.example.com/v1")).is_allowed()
        );
    }

    #[test]
    fn allow_list_permits_listed_host() {
        let mut policy = internet_policy(EgressDefaultAction::Allow);
        policy.allowed_hosts = vec!["api.example.com".into()];
        let policies = [(ExposureTier::Internet, policy)].into_iter().collect();
        assert!(evaluate_egress_policy(&policies, &req("https://api.example.com/v1")).is_allowed());
    }

    #[test]
    fn allow_list_blocks_unlisted_host() {
        let mut policy = internet_policy(EgressDefaultAction::Allow);
        policy.allowed_hosts = vec!["api.example.com".into()];
        let policies = [(ExposureTier::Internet, policy)].into_iter().collect();
        assert!(
            !evaluate_egress_policy(&policies, &req("https://other.example.com/v1")).is_allowed()
        );
    }

    #[test]
    fn audit_variant_is_allowed() {
        let policies = [(
            ExposureTier::Internet,
            internet_policy(EgressDefaultAction::AllowWithAudit),
        )]
        .into_iter()
        .collect();
        let decision = evaluate_egress_policy(&policies, &req("https://api.example.com/v1"));
        assert!(decision.is_allowed());
        assert_eq!(decision, EgressDecision::AllowWithAudit);
    }

    #[test]
    fn host_parsing() {
        assert_eq!(
            host_from_url("https://api.perplexity.ai/chat"),
            Some("api.perplexity.ai")
        );
        assert_eq!(host_from_url("http://localhost:8080/v1"), Some("localhost"));
        assert_eq!(host_from_url("https://example.com"), Some("example.com"));
    }
}
