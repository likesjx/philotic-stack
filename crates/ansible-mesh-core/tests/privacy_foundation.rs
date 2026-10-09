//! Synthetic records only; no service, live graph or external provider access.
use ansible_mesh_core::graph::ModelProfileRecord;
use ansible_mesh_core::model_oracle::{
    rank_models_with_privacy, LatencyClass, PrivacyRouteContext, RouteNeed,
};
use ansible_mesh_core::privacy::*;
use std::collections::{BTreeMap, BTreeSet};

struct Session(&'static str, &'static [&'static str]);
impl ServerAuthenticatedIdentity for Session {
    fn stable_agent_id(&self) -> &str {
        self.0
    }
    fn roles(&self) -> BTreeSet<String> {
        self.1.iter().map(|s| (*s).into()).collect()
    }
}
struct Store(BTreeMap<String, ResourcePolicy>);
impl PolicyAuthority for Store {
    fn policy(&self, resource: &str) -> Option<&ResourcePolicy> {
        self.0.get(resource)
    }
}
fn actor(id: &'static str, roles: &'static [&'static str]) -> AuthenticatedAgent {
    AuthenticatedAgent::from_server(&Session(id, roles)).unwrap()
}
fn store() -> Store {
    Store(BTreeMap::from([(
        "source".into(),
        ResourcePolicy::private("owner".into(), "creator".into()),
    )]))
}
fn process(
    store: &Store,
    actor: &AuthenticatedAgent,
    id: &str,
    op: ProcessingOperation,
    boundary: ProviderBoundary,
) -> Result<(), Denial> {
    authorize_processing(store, Some(actor), &[id.into()], op, boundary)
}
const OPS: [ProcessingOperation; 5] = [
    ProcessingOperation::Inference,
    ProcessingOperation::Embedding,
    ProcessingOperation::SemanticResolution,
    ProcessingOperation::SpeechToText,
    ProcessingOperation::TextToSpeech,
];

#[test]
fn excessive_lineage_depth_and_work_fail_closed() {
    let mut s = store();
    for i in 0..129 {
        let mut p = ResourcePolicy::private("owner".into(), "owner".into());
        p.sources.push(if i == 128 {
            "source".into()
        } else {
            format!("depth-{}", i + 1)
        });
        s.0.insert(format!("depth-{i}"), p);
    }
    let a = actor("owner", &[]);
    assert_eq!(
        authorize_read(&s, Some(&a), "depth-0"),
        Err(Denial::LineageLimit)
    );
    assert_eq!(
        authorize_processing(
            &s,
            Some(&a),
            &vec!["source".into(); 4097],
            ProcessingOperation::Inference,
            ProviderBoundary::LocalTrusted
        ),
        Err(Denial::LineageLimit)
    );
}

#[test]
fn identity_missing_or_empty_denies_and_is_not_a_request_serde_type() {
    assert_eq!(
        AuthenticatedAgent::from_server(&Session(" ", &[])).unwrap_err(),
        Denial::Unauthenticated
    );
    assert_eq!(
        authorize_read(&store(), None, "source"),
        Err(Denial::Unauthenticated)
    );
    assert_eq!(
        authorize_read(&store(), Some(&actor("display-name", &[])), "source"),
        Err(Denial::ReadForbidden)
    );
}

#[test]
fn owner_creator_and_rbac_are_separate_from_external_permission() {
    let mut s = store();
    s.0.get_mut("source")
        .unwrap()
        .read_roles
        .insert("researcher".into());
    for a in [
        actor("owner", &[]),
        actor("creator", &[]),
        actor("reader", &["researcher"]),
    ] {
        assert_eq!(authorize_read(&s, Some(&a), "source"), Ok(()));
        for op in OPS {
            assert_eq!(
                process(&s, &a, "source", op, ProviderBoundary::LocalTrusted),
                Ok(())
            );
            assert_eq!(
                process(&s, &a, "source", op, ProviderBoundary::External),
                Err(Denial::ExternalForbidden)
            );
            assert_eq!(
                process(&s, &a, "source", op, ProviderBoundary::Unknown),
                Err(Denial::UnknownProvider)
            );
        }
    }
    assert_eq!(
        authorize_read(&s, Some(&actor("stranger", &[])), "source"),
        Err(Denial::ReadForbidden)
    );
}

#[test]
fn only_owner_revokes_creator_and_revocation_reaches_existing_copy() {
    let mut s = store();
    let mut copy = ResourcePolicy::private("creator".into(), "creator".into());
    copy.sources.push("source".into());
    s.0.insert("copy".into(), copy);
    let creator = actor("creator", &[]);
    assert_eq!(authorize_read(&s, Some(&creator), "copy"), Ok(()));
    assert_eq!(
        s.0.get_mut("source").unwrap().revoke_creator(&creator),
        Err(Denial::OwnerRequired)
    );
    s.0.get_mut("source")
        .unwrap()
        .revoke_creator(&actor("owner", &[]))
        .unwrap();
    assert_eq!(
        authorize_read(&s, Some(&creator), "copy"),
        Err(Denial::ReadForbidden)
    );
}

#[test]
fn revoked_creator_can_still_hold_an_independent_rbac_grant() {
    let mut s = store();
    let policy = s.0.get_mut("source").unwrap();
    policy.read_roles.insert("researcher".into());
    policy.revoke_creator(&actor("owner", &[])).unwrap();
    assert_eq!(
        authorize_read(&s, Some(&actor("creator", &["researcher"])), "source"),
        Ok(())
    );
}

#[test]
fn derivative_cannot_weaken_private_source_even_with_explicit_external_grant() {
    let mut s = store();
    for (id, parent) in [("copy", "source"), ("summary", "copy")] {
        let mut p = ResourcePolicy::private("owner".into(), "owner".into());
        p.private = false;
        p.external_operations.extend(OPS);
        p.sources.push(parent.into());
        s.0.insert(id.into(), p);
    }
    for op in OPS {
        assert_eq!(
            process(
                &s,
                &actor("owner", &[]),
                "summary",
                op,
                ProviderBoundary::External
            ),
            Err(Denial::ExternalForbidden)
        );
    }
}

#[test]
fn nonprivate_requires_explicit_operation_grant_and_private_wins_over_grant() {
    let mut s = store();
    s.0.get_mut("source")
        .unwrap()
        .external_operations
        .insert(ProcessingOperation::Inference);
    let a = actor("owner", &[]);
    assert_eq!(
        process(
            &s,
            &a,
            "source",
            ProcessingOperation::Inference,
            ProviderBoundary::External
        ),
        Err(Denial::ExternalForbidden)
    );
    s.0.get_mut("source").unwrap().private = false;
    assert_eq!(
        process(
            &s,
            &a,
            "source",
            ProcessingOperation::Inference,
            ProviderBoundary::External
        ),
        Ok(())
    );
    assert_eq!(
        process(
            &s,
            &a,
            "source",
            ProcessingOperation::Embedding,
            ProviderBoundary::External
        ),
        Err(Denial::ExternalForbidden)
    );
}

#[test]
fn missing_policy_invalid_policy_cycle_and_empty_manifest_fail_closed() {
    let mut s = store();
    let a = actor("owner", &[]);
    assert_eq!(
        authorize_read(&s, Some(&a), "missing"),
        Err(Denial::MissingPolicy)
    );
    assert_eq!(
        authorize_processing(
            &s,
            Some(&a),
            &[],
            ProcessingOperation::Inference,
            ProviderBoundary::LocalTrusted
        ),
        Err(Denial::EmptyInput)
    );
    s.0.get_mut("source")
        .unwrap()
        .sources
        .push("missing".into());
    assert_eq!(
        authorize_read(&s, Some(&a), "source"),
        Err(Denial::MissingPolicy)
    );
    s.0.get_mut("source").unwrap().sources = vec!["source".into()];
    assert_eq!(
        authorize_read(&s, Some(&a), "source"),
        Err(Denial::SourceCycle)
    );
    s.0.get_mut("source").unwrap().owner.clear();
    assert_eq!(
        authorize_read(&s, Some(&a), "source"),
        Err(Denial::InvalidPolicy)
    );
}

#[test]
fn every_payload_source_must_allow_processing() {
    let mut s = store();
    let mut public = ResourcePolicy::private("owner".into(), "owner".into());
    public.private = false;
    public.external_operations.extend(OPS);
    s.0.insert("public".into(), public);
    let a = actor("owner", &[]);
    assert_eq!(
        authorize_processing(
            &s,
            Some(&a),
            &["public".into(), "source".into()],
            ProcessingOperation::Inference,
            ProviderBoundary::External
        ),
        Err(Denial::ExternalForbidden)
    );
}

#[test]
fn diamond_lineage_is_valid_and_intersects_all_sources() {
    let mut s = store();
    for id in ["left", "right", "joined"] {
        let mut p = ResourcePolicy::private("owner".into(), "owner".into());
        p.sources = if id == "joined" {
            vec!["left".into(), "right".into()]
        } else {
            vec!["source".into()]
        };
        s.0.insert(id.into(), p);
    }
    assert_eq!(
        authorize_read(&s, Some(&actor("owner", &[])), "joined"),
        Ok(())
    );
}

#[test]
fn existing_reflex_never_returns_cloud_fallback_when_private_local_is_unavailable() {
    let s = store();
    let a = actor("owner", &[]);
    let resources = vec!["source".into()];
    let privacy = PrivacyRouteContext {
        authority: &s,
        actor: Some(&a),
        resources: &resources,
        operation: ProcessingOperation::Inference,
    };
    let need = RouteNeed {
        request_class: "cognitive".into(),
        needs_tools: false,
        needs_structured: false,
        approx_context_tokens: 10,
        latency_class: LatencyClass::Interactive,
        trust_ceiling: "remote_cloud".into(),
    };
    let cloud = ModelProfileRecord {
        provider: "cloud".into(),
        model_ref: "cloud".into(),
        status: "healthy".into(),
        trust_tier: "remote_cloud".into(),
        ..Default::default()
    };
    let mut local = ModelProfileRecord {
        provider: "local".into(),
        model_ref: "local".into(),
        status: "healthy".into(),
        trust_tier: "local_trusted".into(),
        ..Default::default()
    };
    let classify = |p: &ModelProfileRecord| {
        if p.provider == "local" {
            ProviderBoundary::LocalTrusted
        } else {
            ProviderBoundary::External
        }
    };
    let ranked = rank_models_with_privacy(
        &[cloud.clone(), local.clone()],
        &need,
        1000,
        &privacy,
        classify,
    );
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].provider, "local");
    local.status = "unavailable".into();
    assert!(
        rank_models_with_privacy(&[cloud.clone(), local], &need, 1000, &privacy, classify)
            .is_empty()
    );
    assert!(
        rank_models_with_privacy(&[cloud], &need, 1000, &privacy, |_| {
            ProviderBoundary::Unknown
        })
        .is_empty()
    );
}

#[test]
fn dispatch_recheck_observes_revocation_after_ranking() {
    let mut s = store();
    let a = actor("creator", &[]);
    assert_eq!(
        process(
            &s,
            &a,
            "source",
            ProcessingOperation::Inference,
            ProviderBoundary::LocalTrusted
        ),
        Ok(())
    );
    s.0.get_mut("source")
        .unwrap()
        .revoke_creator(&actor("owner", &[]))
        .unwrap();
    assert_eq!(
        process(
            &s,
            &a,
            "source",
            ProcessingOperation::Inference,
            ProviderBoundary::LocalTrusted
        ),
        Err(Denial::ReadForbidden)
    );
}
