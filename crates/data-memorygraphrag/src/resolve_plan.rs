//! Pure shadow contract. Not exported or connected to runtime capture.
//! No text, embeddings, model calls, graph I/O, or authenticated principals are
//! created here. Authority adapters must validate the event/evidence binding,
//! complete source manifest, authenticated session and current policy revision.

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RootId(String);

impl RootId {
    pub fn parse(value: &str) -> Result<Self, Denial> {
        let parts: Vec<_> = value.split(':').collect();
        if parts.len() != 3
            || parts[0] != "life"
            || value.len() > 512
            || parts[1..].iter().any(|s| s.is_empty())
            || !parts[1]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            || !parts[2]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            return Err(Denial::InvalidInput);
        }
        Ok(Self(value.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EventKey {
    /// Stable producer namespace plus producer-issued idempotency key.
    pub producer: String,
    pub event_id: String,
}

/// Bounded shadow support maps explicitly to existing ontology NODE_LABELS.
/// Other root types require a reviewed contract extension, not a new Claim label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootType {
    Goal,
    OpenLoop,
    Event,
}

impl RootType {
    pub fn ontology_label(self) -> &'static str {
        match self {
            Self::Goal => "Goal",
            Self::OpenLoop => "OpenLoop",
            Self::Event => "Event",
        }
    }
    fn id_prefix(self) -> &'static str {
        match self {
            Self::Goal => "goal",
            Self::OpenLoop => "open_loop",
            Self::Event => "event",
        }
    }
}

/// Unique binding is namespace/key, independently of event and proposed type.
/// Storage must reject conflicting types rather than mint another root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VerifiedRootKey {
    pub namespace: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootAnchor {
    ExactId(RootId),
    VerifiedKey { namespace: String, value: String },
    ApprovedAlias { namespace: String, value: String },
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// Only a verified key/approved alias can authorize create on absence.
    Absent,
    Unique(RootId),
    Ambiguous(BTreeSet<RootId>),
    Unverified,
}

/// Evidence-backed assessment is an input from a future governed resolver,
/// not a model confidence score or authorization token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Meaning {
    Same,
    AdditionalDetails,
    Distinct,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub event: EventKey,
    pub evidence_ids: BTreeSet<String>,
    /// Opaque digest of the complete canonical capture, including summary,
    /// details and source manifest. Authority must verify, never just trust it.
    pub payload_digest: String,
    pub anchor: RootAnchor,
    pub root_type: RootType,
    pub meaning: Meaning,
    /// ACL-filtered local semantic candidates. They never authorize merge.
    pub semantic_candidates: BTreeSet<RootId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteTarget {
    NewRoot {
        binding: VerifiedRootKey,
        root_type: RootType,
    },
    EvidenceOn(RootId),
    ExtensionOn(RootId),
    ReviewReceipt,
}

/// Server-side authority boundary, intentionally no permissive defaults.
/// This planner does not implement authenticated identity or policy persistence.
/// A revision is valid only after checking the complete capture and all inherited
/// source ACLs. The snapshot must remain consistent for the whole plan.
pub trait PlanningAuthority {
    fn authorize_capture(&self, capture: &Capture) -> Result<String, Denial>;
    fn resolve(&self, anchor: &RootAnchor) -> Result<Lookup, Denial>;
    fn can_read_root(&self, root: &RootId) -> bool;
    fn can_write(&self, target: &WriteTarget) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    MissingAuthority,
    InvalidInput,
    UnverifiedAnchor,
    InconsistentResolution,
    AccessDenied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewReason {
    SemanticCandidate,
    AmbiguousIdentity,
    AmbiguousMeaning,
    UnresolvedRoot,
    AnchorMeaningConflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    Same {
        root: RootId,
    },
    Extend {
        root: RootId,
    },
    NewRoot {
        proposed_id: RootId,
        binding: VerifiedRootKey,
        root_type: RootType,
    },
    Review {
        reason: ReviewReason,
        candidates: BTreeSet<RootId>,
    },
}

/// Deterministic receipt proposal, not a persisted receipt or write permission.
/// Storage must enforce uniqueness on event and reject the same event with a
/// different payload digest/evidence binding, in the same transaction as writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventReceipt {
    pub event: EventKey,
    pub payload_digest: String,
    pub evidence_ids: BTreeSet<String>,
    pub anchor: RootAnchor,
    pub root_type: RootType,
    pub policy_revision: String,
    pub disposition: Disposition,
}

fn valid_token(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn hex(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn plan(
    capture: &Capture,
    authority: Option<&dyn PlanningAuthority>,
) -> Result<EventReceipt, Denial> {
    let authority = authority.ok_or(Denial::MissingAuthority)?;
    if !valid_token(&capture.event.producer)
        || !valid_token(&capture.event.event_id)
        || !valid_token(&capture.payload_digest)
        || capture.evidence_ids.is_empty()
        || capture.evidence_ids.len() > 256
        || capture.semantic_candidates.len() > 256
        || capture.evidence_ids.iter().any(|s| !valid_token(s))
    {
        return Err(Denial::InvalidInput);
    }
    if let RootAnchor::VerifiedKey { namespace, value }
    | RootAnchor::ApprovedAlias { namespace, value } = &capture.anchor
        && (!valid_token(namespace) || !valid_token(value))
    {
        return Err(Denial::InvalidInput);
    }
    // Hex ID construction adds at most 16 prefix/separator bytes. The bound
    // applies to verified keys on every disposition, not just new-root plans.
    // Event components remain <=256 bytes and are never used to name a root.
    if let RootAnchor::VerifiedKey { namespace, value } = &capture.anchor
        && namespace.len() + value.len() > 248
    {
        return Err(Denial::InvalidInput);
    }
    let revision = authority.authorize_capture(capture)?;
    if !valid_token(&revision) {
        return Err(Denial::MissingAuthority);
    }
    let lookup = if capture.anchor == RootAnchor::Missing {
        Lookup::Unverified
    } else {
        authority.resolve(&capture.anchor)?
    };
    if lookup == Lookup::Unverified && capture.anchor != RootAnchor::Missing {
        return Err(Denial::UnverifiedAnchor);
    }
    let mut candidates = capture.semantic_candidates.clone();
    match &lookup {
        Lookup::Unique(root) => {
            if let RootAnchor::ExactId(expected) = &capture.anchor
                && root != expected
            {
                return Err(Denial::InconsistentResolution);
            }
            candidates.insert(root.clone());
        }
        Lookup::Ambiguous(roots) => {
            candidates.extend(roots.iter().cloned());
        }
        _ => {}
    }
    if candidates.len() > 256 {
        return Err(Denial::InvalidInput);
    }
    // Do not disclose even a candidate ID until read authority is proven.
    if candidates.iter().any(|id| !authority.can_read_root(id)) {
        return Err(Denial::AccessDenied);
    }
    let review = |reason| Disposition::Review {
        reason,
        candidates: candidates.clone(),
    };
    let disposition = if !capture.semantic_candidates.is_empty() {
        review(ReviewReason::SemanticCandidate)
    } else {
        match (&lookup, capture.meaning) {
            (Lookup::Ambiguous(_), _) => review(ReviewReason::AmbiguousIdentity),
            (_, Meaning::Ambiguous) => review(ReviewReason::AmbiguousMeaning),
            (Lookup::Unique(root), Meaning::Same) => Disposition::Same { root: root.clone() },
            (Lookup::Unique(root), Meaning::AdditionalDetails) => {
                Disposition::Extend { root: root.clone() }
            }
            (Lookup::Unique(_), Meaning::Distinct) => review(ReviewReason::AnchorMeaningConflict),
            (Lookup::Absent, Meaning::Distinct)
                if matches!(capture.anchor, RootAnchor::VerifiedKey { .. }) =>
            {
                let RootAnchor::VerifiedKey { namespace, value } = &capture.anchor else {
                    unreachable!()
                };
                // Injective encoding of the verified binding, not the event.
                // Two events observing absence propose the same canonical ID.
                let id = format!(
                    "life:{}:{}.{}",
                    capture.root_type.id_prefix(),
                    hex(namespace),
                    hex(value)
                );
                Disposition::NewRoot {
                    proposed_id: RootId::parse(&id)?,
                    binding: VerifiedRootKey {
                        namespace: namespace.clone(),
                        value: value.clone(),
                    },
                    root_type: capture.root_type,
                }
            }
            (Lookup::Unverified, _) if capture.anchor != RootAnchor::Missing => {
                return Err(Denial::UnverifiedAnchor);
            }
            _ => review(ReviewReason::UnresolvedRoot),
        }
    };
    let target = match &disposition {
        Disposition::Same { root } => WriteTarget::EvidenceOn(root.clone()),
        Disposition::Extend { root } => WriteTarget::ExtensionOn(root.clone()),
        Disposition::NewRoot {
            binding, root_type, ..
        } => WriteTarget::NewRoot {
            binding: binding.clone(),
            root_type: *root_type,
        },
        Disposition::Review { .. } => WriteTarget::ReviewReceipt,
    };
    if !authority.can_write(&target) {
        return Err(Denial::AccessDenied);
    }
    Ok(EventReceipt {
        event: capture.event.clone(),
        payload_digest: capture.payload_digest.clone(),
        evidence_ids: capture.evidence_ids.clone(),
        anchor: capture.anchor.clone(),
        root_type: capture.root_type,
        policy_revision: revision,
        disposition,
    })
}
