// Intentionally imports only the new shadow contract, without changing lib.rs.
#[allow(dead_code)]
#[path = "../src/capture_commit.rs"]
mod capture_commit;
#[allow(dead_code)]
#[path = "../src/capture_local_authority.rs"]
mod capture_local_authority;
#[allow(dead_code)]
#[path = "../src/capture_memgraph.rs"]
mod capture_memgraph;
#[path = "../src/resolve_plan.rs"]
mod resolve_plan;
use resolve_plan::*;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn pending_payload_requires_complete_source_manifest() {
    use ansible_mesh_core::privacy_storage::CapturedRecord;
    use capture_commit::CommitPayload;
    let mut record = CapturedRecord {
        producer: "synthetic".into(),
        event_id: "event".into(),
        recorded_by: "creator".into(),
        payload: r#"{"summary":"Synthetic","details":null,"source_ids":["source"],"evidence_ids":["source"]}"#.into(),
        sources: vec!["source".into()],
        captured_policy_revision: 1,
    };
    assert!(CommitPayload::from_pending(record.clone()).is_ok());
    record.sources.push("omitted-source".into());
    assert!(CommitPayload::from_pending(record.clone()).is_err());
    record.sources = vec!["source".into()];
    record.payload =
        r#"{"summary":"Synthetic","details":null,"source_ids":["source"],"agent_id":"owner"}"#
            .into();
    assert!(CommitPayload::from_pending(record).is_err());
}

struct Fixture {
    capture_allowed: bool,
    revision: String,
    lookup: Lookup,
    readable: BTreeSet<RootId>,
    writes_allowed: bool,
}
impl PlanningAuthority for Fixture {
    fn authorize_capture(&self, _: &Capture) -> Result<String, Denial> {
        if self.capture_allowed {
            Ok(self.revision.clone())
        } else {
            Err(Denial::AccessDenied)
        }
    }
    fn resolve(&self, _: &RootAnchor) -> Result<Lookup, Denial> {
        Ok(self.lookup.clone())
    }
    fn can_read_root(&self, id: &RootId) -> bool {
        self.readable.contains(id)
    }
    fn can_write(&self, _: &WriteTarget) -> bool {
        self.writes_allowed
    }
}
fn root(s: &str) -> RootId {
    RootId::parse(s).unwrap()
}
fn fixture(lookup: Lookup) -> Fixture {
    Fixture {
        capture_allowed: true,
        revision: "synthetic-revision-1".into(),
        lookup,
        readable: BTreeSet::from([root("life:goal:existing"), root("life:goal:other")]),
        writes_allowed: true,
    }
}
fn capture() -> Capture {
    Capture {
        event: EventKey {
            producer: "synthetic-sensor".into(),
            event_id: "event-1".into(),
        },
        evidence_ids: BTreeSet::from(["synthetic:evidence-1".into()]),
        payload_digest: "synthetic-complete-capture-digest".into(),
        anchor: RootAnchor::VerifiedKey {
            namespace: "synthetic-claim-key".into(),
            value: "existing-key".into(),
        },
        meaning: Meaning::Same,
        root_type: RootType::Goal,
        semantic_candidates: BTreeSet::new(),
    }
}

#[test]
fn same_meaning_resolves_existing_root_without_create() {
    let r = root("life:goal:existing");
    assert_eq!(
        plan(&capture(), Some(&fixture(Lookup::Unique(r.clone()))))
            .unwrap()
            .disposition,
        Disposition::Same { root: r }
    );
}
#[test]
fn new_details_extend_resolved_root() {
    let mut c = capture();
    c.meaning = Meaning::AdditionalDetails;
    let r = root("life:goal:existing");
    assert_eq!(
        plan(&c, Some(&fixture(Lookup::Unique(r.clone()))))
            .unwrap()
            .disposition,
        Disposition::Extend { root: r }
    );
}
#[test]
fn distinct_verified_absent_key_proposes_deterministic_new_root() {
    let mut c = capture();
    c.meaning = Meaning::Distinct;
    let f = fixture(Lookup::Absent);
    let a = plan(&c, Some(&f)).unwrap();
    assert_eq!(a, plan(&c, Some(&f)).unwrap());
    let Disposition::NewRoot {
        proposed_id,
        binding,
        root_type,
    } = a.disposition
    else {
        panic!("new root expected")
    };
    assert!(proposed_id.as_str().starts_with("life:goal:"));
    c.event.event_id = "event-2".into();
    assert_eq!(
        Disposition::NewRoot {
            proposed_id,
            binding,
            root_type
        },
        plan(&c, Some(&f)).unwrap().disposition
    );
}
#[test]
fn exact_id_must_be_canonical_present_and_match_authoritative_resolution() {
    for id in [
        "123",
        "display name",
        "life::x",
        "life:goal:",
        "life:goal:a:b",
        "life:goal:bad\0id",
        "life:goal:bad\u{1b}id",
        "life:goal:bad/id",
    ] {
        assert!(RootId::parse(id).is_err());
    }
    let mut c = capture();
    c.anchor = RootAnchor::ExactId(root("life:goal:existing"));
    assert!(matches!(
        plan(
            &c,
            Some(&fixture(Lookup::Unique(root("life:goal:existing"))))
        )
        .unwrap()
        .disposition,
        Disposition::Same { .. }
    ));
    assert_eq!(
        plan(&c, Some(&fixture(Lookup::Unique(root("life:goal:other"))))),
        Err(Denial::InconsistentResolution)
    );
    c.meaning = Meaning::Distinct;
    assert!(matches!(
        plan(&c, Some(&fixture(Lookup::Absent)))
            .unwrap()
            .disposition,
        Disposition::Review {
            reason: ReviewReason::UnresolvedRoot,
            ..
        }
    ));
}
#[test]
fn approved_alias_resolves_but_absent_alias_never_creates_root() {
    let mut c = capture();
    c.anchor = RootAnchor::ApprovedAlias {
        namespace: "synthetic-alias".into(),
        value: "approved".into(),
    };
    assert!(matches!(
        plan(
            &c,
            Some(&fixture(Lookup::Unique(root("life:goal:existing"))))
        )
        .unwrap()
        .disposition,
        Disposition::Same { .. }
    ));
    c.meaning = Meaning::Distinct;
    assert!(matches!(
        plan(&c, Some(&fixture(Lookup::Absent)))
            .unwrap()
            .disposition,
        Disposition::Review { .. }
    ));
}
#[test]
fn semantic_candidate_never_automerge_extend_or_create_even_with_verified_anchor() {
    for meaning in [Meaning::Same, Meaning::AdditionalDetails, Meaning::Distinct] {
        for lookup in [Lookup::Absent, Lookup::Unique(root("life:goal:existing"))] {
            let mut c = capture();
            c.meaning = meaning;
            c.semantic_candidates.insert(root("life:goal:other"));
            assert!(matches!(
                plan(&c, Some(&fixture(lookup))).unwrap().disposition,
                Disposition::Review {
                    reason: ReviewReason::SemanticCandidate,
                    ..
                }
            ));
        }
    }
}
#[test]
fn ambiguous_identity_or_meaning_goes_to_review() {
    let f = fixture(Lookup::Ambiguous(BTreeSet::from([
        root("life:goal:existing"),
        root("life:goal:other"),
    ])));
    assert!(matches!(
        plan(&capture(), Some(&f)).unwrap().disposition,
        Disposition::Review {
            reason: ReviewReason::AmbiguousIdentity,
            ..
        }
    ));
    let mut c = capture();
    c.meaning = Meaning::Ambiguous;
    assert!(matches!(
        plan(&c, Some(&fixture(Lookup::Absent)))
            .unwrap()
            .disposition,
        Disposition::Review {
            reason: ReviewReason::AmbiguousMeaning,
            ..
        }
    ));
}
#[test]
fn conflicting_meaning_on_same_key_requires_review() {
    let mut c = capture();
    c.meaning = Meaning::Distinct;
    assert!(matches!(
        plan(
            &c,
            Some(&fixture(Lookup::Unique(root("life:goal:existing"))))
        )
        .unwrap()
        .disposition,
        Disposition::Review {
            reason: ReviewReason::AnchorMeaningConflict,
            ..
        }
    ));
}
#[test]
fn missing_authority_revision_evidence_or_capture_access_fail_closed() {
    let c = capture();
    assert_eq!(plan(&c, None), Err(Denial::MissingAuthority));
    let mut f = fixture(Lookup::Absent);
    f.capture_allowed = false;
    assert_eq!(plan(&c, Some(&f)), Err(Denial::AccessDenied));
    f.capture_allowed = true;
    f.revision.clear();
    assert_eq!(plan(&c, Some(&f)), Err(Denial::MissingAuthority));
    let mut c = c;
    c.evidence_ids.clear();
    assert_eq!(plan(&c, Some(&f)), Err(Denial::InvalidInput));
}
#[test]
fn unreadable_root_or_semantic_candidate_never_appears_in_receipt() {
    let mut f = fixture(Lookup::Unique(root("life:goal:existing")));
    f.readable.clear();
    assert_eq!(plan(&capture(), Some(&f)), Err(Denial::AccessDenied));
    let mut c = capture();
    c.semantic_candidates.insert(root("life:goal:other"));
    f.lookup = Lookup::Absent;
    assert_eq!(plan(&c, Some(&f)), Err(Denial::AccessDenied));
}
#[test]
fn missing_root_anchor_reviews_and_unverified_key_or_alias_denies() {
    let mut c = capture();
    c.meaning = Meaning::Distinct;
    c.anchor = RootAnchor::Missing;
    assert!(matches!(
        plan(&c, Some(&fixture(Lookup::Absent)))
            .unwrap()
            .disposition,
        Disposition::Review {
            reason: ReviewReason::UnresolvedRoot,
            ..
        }
    ));
    c.anchor = RootAnchor::VerifiedKey {
        namespace: "not-verified".into(),
        value: "key".into(),
    };
    assert_eq!(
        plan(&c, Some(&fixture(Lookup::Unverified))),
        Err(Denial::UnverifiedAnchor)
    );
    c.semantic_candidates.insert(root("life:goal:existing"));
    assert_eq!(
        plan(&c, Some(&fixture(Lookup::Unverified))),
        Err(Denial::UnverifiedAnchor)
    );
}
#[test]
fn write_denial_applies_to_all_dispositions() {
    for (lookup, meaning) in [
        (Lookup::Unique(root("life:goal:existing")), Meaning::Same),
        (
            Lookup::Unique(root("life:goal:existing")),
            Meaning::AdditionalDetails,
        ),
        (Lookup::Absent, Meaning::Distinct),
        (Lookup::Absent, Meaning::Ambiguous),
    ] {
        let mut c = capture();
        c.meaning = meaning;
        let mut f = fixture(lookup);
        f.writes_allowed = false;
        assert_eq!(plan(&c, Some(&f)), Err(Denial::AccessDenied));
    }
}
#[test]
fn receipts_are_order_independent_and_bind_event_payload_evidence_and_revision() {
    let mut a = capture();
    a.evidence_ids.insert("synthetic:evidence-2".into());
    let mut b = capture();
    b.evidence_ids = ["synthetic:evidence-2", "synthetic:evidence-1"]
        .into_iter()
        .map(String::from)
        .collect();
    let mut f = fixture(Lookup::Unique(root("life:goal:existing")));
    let receipt = plan(&a, Some(&f)).unwrap();
    assert_eq!(receipt, plan(&b, Some(&f)).unwrap());
    b.payload_digest = "changed".into();
    assert_ne!(receipt, plan(&b, Some(&f)).unwrap());
    assert_eq!(receipt.event, plan(&b, Some(&f)).unwrap().event);
    f.revision = "synthetic-revision-2".into();
    assert_ne!(receipt, plan(&a, Some(&f)).unwrap());
}

#[test]
fn event_bounds_are_consistent_for_existing_and_new_roots() {
    let mut c = capture();
    for size in [128, 256] {
        c.event.producer = "p".repeat(size);
        c.event.event_id = "e".repeat(size);
        c.meaning = Meaning::Same;
        assert!(
            plan(
                &c,
                Some(&fixture(Lookup::Unique(root("life:goal:existing"))))
            )
            .is_ok()
        );
        c.meaning = Meaning::Distinct;
        assert!(plan(&c, Some(&fixture(Lookup::Absent))).is_ok());
    }
    c.event.event_id.push('e');
    for lookup in [Lookup::Absent, Lookup::Unique(root("life:goal:existing"))] {
        assert_eq!(plan(&c, Some(&fixture(lookup))), Err(Denial::InvalidInput));
    }
}

#[test]
fn verified_key_bound_guarantees_id_fits_all_supported_ontology_types() {
    let mut c = capture();
    c.meaning = Meaning::Distinct;
    c.anchor = RootAnchor::VerifiedKey {
        namespace: "n".repeat(124),
        value: "v".repeat(124),
    };
    for ty in [RootType::Goal, RootType::OpenLoop, RootType::Event] {
        c.root_type = ty;
        let ontology = include_str!("../src/ontology.rs");
        assert!(ontology.contains(&format!("\"{}\"", ty.ontology_label())));
        let Disposition::NewRoot {
            proposed_id,
            root_type,
            ..
        } = plan(&c, Some(&fixture(Lookup::Absent)))
            .unwrap()
            .disposition
        else {
            panic!("new root expected")
        };
        assert_eq!(root_type, ty);
        assert!(proposed_id.as_str().len() <= 512);
        assert!(!proposed_id.as_str().starts_with("life:claim:"));
    }
    c.anchor = RootAnchor::VerifiedKey {
        namespace: "n".repeat(125),
        value: "v".repeat(124),
    };
    for lookup in [Lookup::Absent, Lookup::Unique(root("life:goal:existing"))] {
        assert_eq!(plan(&c, Some(&fixture(lookup))), Err(Denial::InvalidInput));
    }
}

// Simulates the REQUIRED atomic storage transaction; it is not production
// persistence. Reject stale key resolution before inserting any root/receipt.
#[derive(Default)]
struct SimulatedCommit {
    roots: BTreeMap<VerifiedRootKey, (RootId, RootType)>,
    receipts: BTreeMap<EventKey, EventReceipt>,
}
#[derive(Debug, PartialEq, Eq)]
enum CommitError {
    StaleResolution,
    StalePolicy,
    EventConflict,
    TypeConflict,
}
impl SimulatedCommit {
    fn commit(
        &mut self,
        receipt: EventReceipt,
        current_revision: &str,
    ) -> Result<RootId, CommitError> {
        if receipt.policy_revision != current_revision {
            return Err(CommitError::StalePolicy);
        }
        if let Some(existing) = self.receipts.get(&receipt.event) {
            if existing != &receipt {
                return Err(CommitError::EventConflict);
            }
            return match &existing.disposition {
                Disposition::NewRoot { proposed_id, .. } => Ok(proposed_id.clone()),
                Disposition::Same { root } => Ok(root.clone()),
                _ => panic!("fixture supports creation and reuse only"),
            };
        }
        let RootAnchor::VerifiedKey { namespace, value } = &receipt.anchor else {
            panic!("verified key expected")
        };
        let key = VerifiedRootKey {
            namespace: namespace.clone(),
            value: value.clone(),
        };
        let root = match &receipt.disposition {
            Disposition::NewRoot {
                proposed_id,
                binding,
                root_type,
            } => {
                assert_eq!(&key, binding);
                if let Some((_, bound_type)) = self.roots.get(binding) {
                    return Err(if bound_type != root_type {
                        CommitError::TypeConflict
                    } else {
                        CommitError::StaleResolution
                    });
                }
                self.roots
                    .insert(binding.clone(), (proposed_id.clone(), *root_type));
                proposed_id.clone()
            }
            Disposition::Same { root } => {
                if self.roots.get(&key) != Some(&(root.clone(), receipt.root_type)) {
                    return Err(CommitError::StaleResolution);
                }
                root.clone()
            }
            _ => panic!("fixture supports creation and reuse only"),
        };
        self.receipts.insert(receipt.event.clone(), receipt);
        Ok(root)
    }
}

#[test]
fn concurrent_absent_snapshots_share_binding_and_commit_one_root_in_either_order() {
    for reverse in [false, true] {
        let mut a = capture();
        a.meaning = Meaning::Distinct;
        let mut b = a.clone();
        b.event.event_id = "another-event".into();
        b.payload_digest = "another-capture-digest".into();
        let absent = fixture(Lookup::Absent);
        let pa = plan(&a, Some(&absent)).unwrap();
        let pb = plan(&b, Some(&absent)).unwrap();
        assert_eq!(pa.disposition, pb.disposition);
        assert_ne!(pa.event, pb.event);
        let (first, second, mut second_capture) = if reverse { (pb, pa, a) } else { (pa, pb, b) };
        let mut commit = SimulatedCommit::default();
        let canonical = commit
            .commit(first.clone(), "synthetic-revision-1")
            .unwrap();
        assert_eq!(
            commit.commit(second, "synthetic-revision-1"),
            Err(CommitError::StaleResolution)
        );
        assert_eq!(commit.roots.len(), 1);
        assert_eq!(commit.receipts.len(), 1);
        // Retry re-resolves the now-existing binding and uses an explicit
        // evidence-backed Same assessment rather than bypassing review.
        let mut current = fixture(Lookup::Unique(canonical.clone()));
        current.readable.insert(canonical.clone());
        second_capture.meaning = Meaning::Same;
        let retry = plan(&second_capture, Some(&current)).unwrap();
        assert_eq!(
            commit.commit(retry, "synthetic-revision-1"),
            Ok(canonical.clone())
        );
        assert_eq!(commit.roots.len(), 1);
        assert_eq!(commit.receipts.len(), 2);
        // Identical event replay is idempotent in this simulation.
        assert_eq!(
            commit.commit(first.clone(), "synthetic-revision-1"),
            Ok(canonical)
        );
        let mut changed = first;
        changed.payload_digest = "changed".into();
        assert_eq!(
            commit.commit(changed, "synthetic-revision-1"),
            Err(CommitError::EventConflict)
        );
        assert_eq!(commit.roots.len(), 1);
        assert_eq!(commit.receipts.len(), 2);
    }
}

#[test]
fn atomic_commit_simulation_revalidates_policy_and_rejects_conflicting_root_type() {
    let mut c = capture();
    c.meaning = Meaning::Distinct;
    let p = plan(&c, Some(&fixture(Lookup::Absent))).unwrap();
    let mut commit = SimulatedCommit::default();
    assert_eq!(
        commit.commit(p.clone(), "synthetic-revision-2"),
        Err(CommitError::StalePolicy)
    );
    assert!(commit.roots.is_empty());
    assert!(commit.receipts.is_empty());
    commit.commit(p, "synthetic-revision-1").unwrap();
    c.event.event_id = "second-event".into();
    c.root_type = RootType::Event;
    let conflicting = plan(&c, Some(&fixture(Lookup::Absent))).unwrap();
    assert_eq!(
        commit.commit(conflicting, "synthetic-revision-1"),
        Err(CommitError::TypeConflict)
    );
    assert_eq!(commit.roots.len(), 1);
    assert_eq!(commit.receipts.len(), 1);
}
