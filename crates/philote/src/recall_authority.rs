//! Consumer adapter for the canonical local task authority, not an issuer.
//!
//! This synchronous seam must run on a blocking worker: local resolution reads
//! SQLite. The hotel/SDK owner must supply the exact envelope, kernel-verified
//! consumer and server-classified endpoint for EACH assembly/attempt. Nothing
//! here registers a guest, issues a handle, or trusts payload identity fields.
use crate::{recall_selection::RecallAuthority, session::RecalledMemoryRecord};
use ansible_mesh_core::{
    privacy::{AuthenticatedAgent, ProcessingOperation, ProviderBoundary, authorize_processing},
    privacy_local::{
        LocalTaskAuthority, LocalTaskEnvelope, ResolvedLocalAuthority, VerifiedLocalSession,
    },
};

/// Implemented by the authenticated transport owner. Never cache a resolved
/// snapshot across attempts, retries, fallbacks, cancellation or re-entry.
pub trait RecallContextResolver {
    fn resolve_current(&self) -> Option<ResolvedLocalAuthority>;
    fn actual_boundary(&self) -> ProviderBoundary;
}

pub struct LocalRecallResolver<'a> {
    authority: &'a LocalTaskAuthority,
    envelope: &'a LocalTaskEnvelope,
    consumer: &'a VerifiedLocalSession,
    boundary: ProviderBoundary,
}
impl<'a> LocalRecallResolver<'a> {
    /// All inputs come from server-owned transport/endpoint configuration.
    /// A wire handle or GuestIdentity alone cannot construct the consumer proof.
    pub fn new(
        authority: &'a LocalTaskAuthority,
        envelope: &'a LocalTaskEnvelope,
        consumer: &'a VerifiedLocalSession,
        boundary: ProviderBoundary,
    ) -> Self {
        Self {
            authority,
            envelope,
            consumer,
            boundary,
        }
    }
}
impl RecallContextResolver for LocalRecallResolver<'_> {
    fn resolve_current(&self) -> Option<ResolvedLocalAuthority> {
        self.authority
            .resolve(
                self.envelope,
                self.consumer,
                ProcessingOperation::Inference,
                self.boundary,
            )
            .ok()
    }
    fn actual_boundary(&self) -> ProviderBoundary {
        self.boundary
    }
}

/// The server catalog must bind the COMPLETE record (content and provenance),
/// human-principal relation and session to a canonical resource for this actor.
/// A memory ID, vault tag or caller-supplied map is insufficient. No production
/// catalog adapter exists here; missing binding denies. This is a consumer lookup
/// contract, not another policy store or manifest issuer.
pub trait CanonicalRecallCatalog {
    fn resource_for_record(
        &self,
        actor: &AuthenticatedAgent,
        principal: &str,
        session: &str,
        record: &RecalledMemoryRecord,
    ) -> Option<String>;
}

pub struct SharedRecallAuthority<R, C> {
    resolver: R,
    catalog: C,
}
impl<R, C> SharedRecallAuthority<R, C> {
    pub fn new(resolver: R, catalog: C) -> Self {
        Self { resolver, catalog }
    }
}
impl<R: RecallContextResolver, C: CanonicalRecallCatalog> RecallAuthority
    for SharedRecallAuthority<R, C>
{
    fn permits(
        &self,
        principal: &str,
        agent: &str,
        session: &str,
        record: &RecalledMemoryRecord,
    ) -> bool {
        if principal.trim().is_empty() || agent.trim().is_empty() || session.trim().is_empty() {
            return false;
        }
        let Some(context) = self.resolver.resolve_current() else {
            return false;
        };
        if context.actor.stable_agent_id() != agent {
            return false;
        }
        let Some(resource) =
            self.catalog
                .resource_for_record(&context.actor, principal, session, record)
        else {
            return false;
        };
        if resource.trim().is_empty() || !context.sources.contains(&resource) {
            return false;
        }
        authorize_processing(
            &context.policies,
            Some(&context.actor),
            &[resource],
            ProcessingOperation::Inference,
            self.resolver.actual_boundary(),
        )
        .is_ok()
    }
    // Agent-graph admission remains default-deny: the old graph seam carries no
    // graph content to bind to the exact payload. Do not authorize by name alone.
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::{
        privacy::{ResourcePolicy, ServerAuthenticatedIdentity},
        privacy_storage::PolicyStore,
    };
    use std::{
        collections::BTreeSet,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    struct Identity(&'static str);
    impl ServerAuthenticatedIdentity for Identity {
        fn stable_agent_id(&self) -> &str {
            self.0
        }
        fn roles(&self) -> BTreeSet<String> {
            BTreeSet::new()
        }
    }
    fn actor(id: &'static str) -> AuthenticatedAgent {
        AuthenticatedAgent::from_server(&Identity(id)).unwrap()
    }
    struct Resolver {
        store: Arc<PolicyStore>,
        _database: TestDatabase,
        valid: Arc<AtomicBool>,
        boundary: ProviderBoundary,
        manifest: Vec<String>,
    }
    struct TestDatabase(std::path::PathBuf);
    impl Drop for TestDatabase {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    impl RecallContextResolver for Resolver {
        fn resolve_current(&self) -> Option<ResolvedLocalAuthority> {
            self.valid
                .load(Ordering::SeqCst)
                .then(|| ResolvedLocalAuthority {
                    actor: actor("synthetic-agent"),
                    policies: self.store.snapshot().unwrap(),
                    sources: self.manifest.clone(),
                    payload_digest: "synthetic-only".into(),
                    consumer_incarnation: uuid::Uuid::nil(),
                })
        }
        fn actual_boundary(&self) -> ProviderBoundary {
            self.boundary
        }
    }
    struct Catalog(RecalledMemoryRecord);
    impl CanonicalRecallCatalog for Catalog {
        fn resource_for_record(
            &self,
            _: &AuthenticatedAgent,
            principal: &str,
            session: &str,
            record: &RecalledMemoryRecord,
        ) -> Option<String> {
            (principal == "synthetic-human" && session == "synthetic-session" && record == &self.0)
                .then(|| "resource-1".into())
        }
    }
    fn fixture(
        boundary: ProviderBoundary,
    ) -> (
        SharedRecallAuthority<Resolver, Catalog>,
        RecalledMemoryRecord,
    ) {
        // The canonical store requires a real path for its commit lease.
        let database = TestDatabase(std::env::temp_dir().join(format!(
            "context-recall-synthetic-{}.db",
            uuid::Uuid::new_v4()
        )));
        let store = Arc::new(PolicyStore::open(&database.0).unwrap());
        store
            .insert_policy(
                &actor("synthetic-owner"),
                "resource-1",
                &ResourcePolicy::private("synthetic-owner".into(), "synthetic-agent".into()),
            )
            .unwrap();
        let record = RecalledMemoryRecord {
            id: Some("memory-1".into()),
            concept: "synthetic".into(),
            content: "synthetic fact".into(),
            source: Some("synthetic provenance".into()),
            ..Default::default()
        };
        (
            SharedRecallAuthority::new(
                Resolver {
                    store,
                    _database: database,
                    valid: Arc::new(AtomicBool::new(true)),
                    boundary,
                    manifest: vec!["resource-1".into()],
                },
                Catalog(record.clone()),
            ),
            record,
        )
    }
    fn permits(a: &impl RecallAuthority, r: &RecalledMemoryRecord) -> bool {
        a.permits("synthetic-human", "synthetic-agent", "synthetic-session", r)
    }
    #[test]
    fn canonical_record_binding_and_manifest_are_both_required() {
        let (mut a, r) = fixture(ProviderBoundary::LocalTrusted);
        assert!(permits(&a, &r));
        let mut tampered = r.clone();
        tampered.content.push_str(" injected");
        assert!(!permits(&a, &tampered));
        tampered = r.clone();
        tampered.source = None;
        assert!(!permits(&a, &tampered));
        assert!(!a.permits("synthetic-human", "spoofed-agent", "synthetic-session", &r));
        assert!(!a.permits("other-human", "synthetic-agent", "synthetic-session", &r));
        assert!(!a.permits("synthetic-human", "synthetic-agent", "other-session", &r));
        a.resolver.manifest.clear();
        assert!(!permits(&a, &r));
    }
    #[test]
    fn retry_rechecks_policy_and_resolution_failure() {
        let (a, r) = fixture(ProviderBoundary::LocalTrusted);
        assert!(permits(&a, &r));
        a.resolver.valid.store(false, Ordering::SeqCst);
        assert!(!permits(&a, &r));
        a.resolver.valid.store(true, Ordering::SeqCst);
        a.resolver
            .store
            .revoke_creator(&actor("synthetic-owner"), "resource-1")
            .unwrap();
        assert!(!permits(&a, &r));
    }
    #[test]
    fn external_unknown_and_unbound_graph_fail_closed() {
        for boundary in [ProviderBoundary::External, ProviderBoundary::Unknown] {
            let (a, r) = fixture(boundary);
            assert!(!permits(&a, &r));
        }
        let (a, _) = fixture(ProviderBoundary::LocalTrusted);
        assert!(!a.permits_agent_graph("synthetic-human", "synthetic-agent", "synthetic-session"));
    }
    #[test]
    fn selection_uses_shared_gate_without_rewriting_source_records() {
        let (a, r) = fixture(ProviderBoundary::LocalTrusted);
        let records = vec![r.clone()];
        let (selected, _) = crate::recall_selection::select(
            Some("synthetic-human"),
            "synthetic-agent",
            "synthetic-session",
            Some(&a),
            &records,
            12,
            4096,
        );
        assert_eq!(selected, vec![&r]);
        assert_eq!(records, vec![r]);
        let (selected, _) = crate::recall_selection::select(
            Some("synthetic-human"),
            "synthetic-agent",
            "synthetic-session",
            None,
            &records,
            12,
            4096,
        );
        assert!(selected.is_empty());
    }

    #[test]
    fn session_assembly_rechecks_recall_and_persists_only_main_messages() {
        let (authority, record) = fixture(ProviderBoundary::LocalTrusted);
        let mut state = crate::session::SessionState::new(
            "synthetic-session".into(),
            "synthetic-agent".into(),
            "synthetic-channel".into(),
        );
        state.agent_profile.user_principal_id = Some("synthetic-human".into());
        let mut checkpoint = state.checkpoint_json();
        checkpoint["active_turn"] = serde_json::json!({
            "turn_id":"synthetic-turn", "phase":"waiting_tool", "user_content":"MAIN_USER",
            "recalled_memories":[record.clone()]
        });
        let mut state = crate::session::SessionState::from_checkpoint(&checkpoint).unwrap();
        // Profile authority is loaded separately, not restored from checkpoint.
        state.agent_profile.user_principal_id = Some("synthetic-human".into());
        assert!(state.active_turn.is_some());
        let (_, admitted, _) =
            state.model_request_payloads_with_recall_authority("MAIN_USER", &[], Some(&authority));
        assert!(admitted.to_string().contains("synthetic fact"));
        assert!(admitted.to_string().contains("synthetic provenance"));
        authority.resolver.valid.store(false, Ordering::SeqCst);
        // These are assembly fixtures; they do not run the runtime coordinator.
        for reentry in ["TRANSCRIPTION", "APPROVED", "DENIED", "FALLBACK"] {
            let (_, denied, _) =
                state.model_request_payloads_with_recall_authority(reentry, &[], Some(&authority));
            assert!(!denied.to_string().contains("synthetic fact"));
        }
        assert_eq!(
            state.active_turn.as_ref().unwrap().recalled_memories,
            vec![record]
        );
        state.complete_active_turn("MAIN_ASSISTANT".into()).unwrap();
        assert_eq!(state.recent_turns.len(), 1);
        assert_eq!(state.recent_turns[0].user_content, "MAIN_USER");
        assert_eq!(
            state.recent_turns[0].assistant_content.as_deref(),
            Some("MAIN_ASSISTANT")
        );
    }
}
