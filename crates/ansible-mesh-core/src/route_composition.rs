//! Hotel-owned effective-route planning. This pure API grants no access and
//! performs no dispatch. The hotel supplies resolved candidates and a fresh
//! admission decision for each request, including direct overrides.
use crate::graph::ModelProfileRecord;
use crate::model_oracle::{model_restriction, ModelRestriction, RouteNeed};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Missing version preserves historically exclusive role ladders. V2 is an
/// explicit migration: agent preferences precede the hotel waterfall.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionVersion {
    #[default]
    LegacyExclusiveV1,
    PreferencesThenHotelV2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverrideMode {
    PreferWithFallback,
    StrictPin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteOverride {
    pub candidate: String,
    pub mode: OverrideMode,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutePolicy {
    #[serde(default)]
    pub version: CompositionVersion,
    #[serde(default)]
    pub agent_preferences: Vec<String>,
    #[serde(default)]
    pub direct_override: Option<RouteOverride>,
}

/// Exact dispatch identity, after alias resolution. Credential and endpoint
/// fields are opaque handles, never secrets/URLs containing authentication.
/// Policy scope distinguishes routes with different admission obligations.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CandidateIdentity {
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub credential_scope: String,
    pub hotel: String,
    pub incarnation: String,
    pub policy_scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Admission {
    Allowed,
    PrivacyDenied,
    AccessDenied,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ResolvedCandidate {
    pub identity: CandidateIdentity,
    pub profile: ModelProfileRecord,
    pub admission: Admission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteOrigin {
    DirectOverride,
    AgentPreference,
    HotelWaterfall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateDisposition {
    Included,
    Duplicate { first_candidate: String },
    UnknownCandidate,
    AdmissionDenied(Admission),
    OracleRestricted(ModelRestriction),
    IdentityMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteDiagnostic {
    pub candidate: String,
    pub origin: RouteOrigin,
    pub disposition: CandidateDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveRoute {
    pub candidates: Vec<CandidateIdentity>,
    pub diagnostics: Vec<RouteDiagnostic>,
    /// Strict pin forbids expanding this plan through an oracle safety net.
    pub strict_pin: bool,
}

/// Compose in caller-declared order, applying the oracle's existing hard
/// capability/context/trust/health gates to *every* resolved candidate.
/// Unknown admission fails closed. A denied strict pin yields an empty route;
/// it never authorizes a fallback. Ranking scores do not reorder preferences.
pub fn compose_effective_route(
    policy: &RoutePolicy,
    hotel_waterfall: &[String],
    registry: &BTreeMap<String, ResolvedCandidate>,
    need: &RouteNeed,
    now_secs: u64,
    cooloff_secs: u64,
) -> EffectiveRoute {
    let strict_pin = policy
        .direct_override
        .as_ref()
        .is_some_and(|o| o.mode == OverrideMode::StrictPin);
    let mut requested = Vec::new();
    if let Some(o) = &policy.direct_override {
        requested.push((&o.candidate, RouteOrigin::DirectOverride));
    }
    if !strict_pin {
        requested.extend(
            policy
                .agent_preferences
                .iter()
                .map(|id| (id, RouteOrigin::AgentPreference)),
        );
        if policy.version == CompositionVersion::PreferencesThenHotelV2
            || policy.agent_preferences.is_empty()
        {
            requested.extend(
                hotel_waterfall
                    .iter()
                    .map(|id| (id, RouteOrigin::HotelWaterfall)),
            );
        }
    }
    let mut route = EffectiveRoute {
        candidates: Vec::new(),
        diagnostics: Vec::new(),
        strict_pin,
    };
    let mut seen = BTreeMap::new();
    for (id, origin) in requested {
        let disposition = match registry.get(id) {
            None => CandidateDisposition::UnknownCandidate,
            Some(candidate) if candidate.admission != Admission::Allowed => {
                CandidateDisposition::AdmissionDenied(candidate.admission.clone())
            }
            Some(candidate)
                if candidate.identity.provider != candidate.profile.provider
                    || candidate.identity.model != candidate.profile.model_ref
                    || candidate.identity.hotel != candidate.profile.node_id =>
            {
                CandidateDisposition::IdentityMismatch
            }
            Some(candidate)
                if model_restriction(&candidate.profile, need, now_secs, cooloff_secs)
                    .is_some() =>
            {
                CandidateDisposition::OracleRestricted(
                    model_restriction(&candidate.profile, need, now_secs, cooloff_secs)
                        .expect("restriction checked above"),
                )
            }
            Some(candidate) => {
                if let Some(first_candidate) = seen.get(&candidate.identity) {
                    CandidateDisposition::Duplicate {
                        first_candidate: String::clone(first_candidate),
                    }
                } else {
                    seen.insert(candidate.identity.clone(), id.clone());
                    route.candidates.push(candidate.identity.clone());
                    CandidateDisposition::Included
                }
            }
        };
        route.diagnostics.push(RouteDiagnostic {
            candidate: id.clone(),
            origin,
            disposition,
        });
    }
    route
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_oracle::LatencyClass;

    fn fixture() -> (BTreeMap<String, ResolvedCandidate>, RouteNeed) {
        let registry = ["a", "b", "gemini"]
            .into_iter()
            .map(|id| {
                let identity = CandidateIdentity {
                    provider: id.into(),
                    model: id.into(),
                    endpoint: "endpoint-1".into(),
                    credential_scope: "credential-1".into(),
                    hotel: "hotel-1".into(),
                    incarnation: "guest-1".into(),
                    policy_scope: "policy-1".into(),
                };
                let profile = ModelProfileRecord {
                    provider: id.into(),
                    model_ref: id.into(),
                    node_id: "hotel-1".into(),
                    trust_tier: "local_trusted".into(),
                    task_kinds: vec!["text.generate".into()],
                    ..Default::default()
                };
                (
                    id.into(),
                    ResolvedCandidate {
                        identity,
                        profile,
                        admission: Admission::Allowed,
                    },
                )
            })
            .collect();
        let need = RouteNeed {
            request_class: "cognitive".into(),
            needs_tools: true,
            needs_structured: true,
            approx_context_tokens: 100,
            latency_class: LatencyClass::Interactive,
            trust_ceiling: "local_trusted".into(),
        };
        (registry, need)
    }
    fn ids(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }
    fn plan(
        policy: &RoutePolicy,
        registry: &BTreeMap<String, ResolvedCandidate>,
        need: &RouteNeed,
    ) -> EffectiveRoute {
        compose_effective_route(policy, &ids(&["b", "a"]), registry, need, 1000, 300)
    }
    fn policy() -> RoutePolicy {
        RoutePolicy {
            version: CompositionVersion::PreferencesThenHotelV2,
            agent_preferences: ids(&["a"]),
            ..Default::default()
        }
    }

    #[test]
    fn versioned_migration_preserves_legacy_exclusivity() {
        let (registry, need) = fixture();
        let legacy: RoutePolicy = serde_json::from_str(r#"{"agent_preferences":["a"]}"#).unwrap();
        assert_eq!(plan(&legacy, &registry, &need).candidates.len(), 1);
        assert_eq!(
            plan(&policy(), &registry, &need)
                .candidates
                .iter()
                .map(|c| c.provider.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(
            plan(&RoutePolicy::default(), &registry, &need).candidates[0].provider,
            "b"
        );
        assert!(serde_json::from_str::<RoutePolicy>(r#"{"version":"future_v3"}"#).is_err());
    }

    #[test]
    fn override_precedence_and_strict_pin() {
        let (registry, need) = fixture();
        let mut p = policy();
        p.direct_override = Some(RouteOverride {
            candidate: "gemini".into(),
            mode: OverrideMode::PreferWithFallback,
        });
        assert_eq!(
            plan(&p, &registry, &need)
                .candidates
                .iter()
                .map(|c| c.provider.as_str())
                .collect::<Vec<_>>(),
            vec!["gemini", "a", "b"]
        );
        p.direct_override.as_mut().unwrap().mode = OverrideMode::StrictPin;
        assert_eq!(plan(&p, &registry, &need).candidates.len(), 1);
        assert!(plan(&p, &registry, &need).strict_pin);
    }

    #[test]
    fn aliases_dedupe_after_resolution_and_explain_first_occurrence() {
        let (mut registry, need) = fixture();
        registry.insert("alias".into(), registry["a"].clone());
        let mut p = policy();
        p.agent_preferences = ids(&["alias", "a"]);
        let route = plan(&p, &registry, &need);
        assert_eq!(route.candidates.len(), 2);
        assert_eq!(
            route.diagnostics[1].disposition,
            CandidateDisposition::Duplicate {
                first_candidate: "alias".into()
            }
        );
        assert_eq!(route, plan(&p, &registry, &need));
    }

    #[test]
    fn meaningful_dispatch_distinctions_survive_deduplication() {
        let (mut registry, need) = fixture();
        let mut p = policy();
        for field in 0..7 {
            let mut c = registry["a"].clone();
            match field {
                0 => {
                    c.identity.provider = "other".into();
                    c.profile.provider = "other".into();
                }
                1 => {
                    c.identity.model = "other".into();
                    c.profile.model_ref = "other".into();
                }
                2 => c.identity.endpoint = "other".into(),
                3 => c.identity.credential_scope = "other".into(),
                4 => {
                    c.identity.hotel = "other".into();
                    c.profile.node_id = "other".into();
                }
                5 => c.identity.incarnation = "other".into(),
                _ => c.identity.policy_scope = "other".into(),
            }
            let id = format!("variant-{field}");
            registry.insert(id.clone(), c);
            p.agent_preferences.push(id);
        }
        assert_eq!(plan(&p, &registry, &need).candidates.len(), 9);
    }

    #[test]
    fn unknown_candidates_and_unknown_admission_fail_closed() {
        let (mut registry, need) = fixture();
        let mut p = policy();
        p.agent_preferences = ids(&["missing", "a"]);
        registry.get_mut("a").unwrap().admission = Admission::Unknown;
        let route = plan(&p, &registry, &need);
        assert_eq!(route.candidates.len(), 1);
        assert_eq!(
            route.diagnostics[0].disposition,
            CandidateDisposition::UnknownCandidate
        );
        assert_eq!(
            route.diagnostics[1].disposition,
            CandidateDisposition::AdmissionDenied(Admission::Unknown)
        );
    }

    #[test]
    fn privacy_and_access_denial_apply_to_overrides_and_waterfall() {
        for denial in [Admission::PrivacyDenied, Admission::AccessDenied] {
            let (mut registry, need) = fixture();
            registry.get_mut("a").unwrap().admission = denial.clone();
            let mut p = policy();
            p.direct_override = Some(RouteOverride {
                candidate: "a".into(),
                mode: OverrideMode::StrictPin,
            });
            let route = plan(&p, &registry, &need);
            assert!(route.candidates.is_empty());
            assert_eq!(
                route.diagnostics[0].disposition,
                CandidateDisposition::AdmissionDenied(denial)
            );
            p.direct_override.as_mut().unwrap().mode = OverrideMode::PreferWithFallback;
            assert_eq!(plan(&p, &registry, &need).candidates[0].provider, "b");
        }
    }

    #[test]
    fn oracle_restrictions_cannot_be_bypassed_by_any_origin() {
        for restriction in 0..7 {
            let (mut registry, need) = fixture();
            let a = registry.get_mut("a").unwrap();
            match restriction {
                0 => a.profile.trust_tier = "remote_cloud".into(),
                1 => a.profile.supports_tools = false,
                2 => a.profile.supports_structured = false,
                3 => a.profile.task_kinds = vec!["text.embed".into()],
                4 => a.profile.max_context_tokens = 1,
                5 => a.profile.status = "retired".into(),
                _ => {
                    a.profile.status = "degraded".into();
                    a.profile.updated_secs = 999;
                }
            }
            let mut p = policy();
            p.direct_override = Some(RouteOverride {
                candidate: "a".into(),
                mode: OverrideMode::PreferWithFallback,
            });
            let route = plan(&p, &registry, &need);
            assert_eq!(route.candidates.len(), 1);
            assert!(route
                .diagnostics
                .iter()
                .filter(|d| d.candidate == "a")
                .all(|d| matches!(d.disposition, CandidateDisposition::OracleRestricted(_))));
        }
    }

    #[test]
    fn recovery_and_identity_validation() {
        let (mut registry, need) = fixture();
        let a = registry.get_mut("a").unwrap();
        a.profile.status = "degraded".into();
        a.profile.updated_secs = 700;
        assert_eq!(plan(&policy(), &registry, &need).candidates.len(), 2);
        registry.get_mut("a").unwrap().identity.model = "mismatch".into();
        assert_eq!(
            plan(&policy(), &registry, &need).diagnostics[0].disposition,
            CandidateDisposition::IdentityMismatch
        );
    }
}
