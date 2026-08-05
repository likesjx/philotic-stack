use serde::{Deserialize, Serialize};

/// Valid proposal statuses and their allowed transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalStatus {
    Draft,
    Proposed,
    Accepted,
    AcceptedCurrentSlice,
    InProgress,
    Implemented,
    Deferred,
    Superseded,
    Historical,
}

impl ProposalStatus {
    /// Parse from the various string formats used in frontmatter.
    ///
    /// Deliberately NOT `std::str::FromStr`: this is a lenient parser over
    /// hand-written frontmatter where an unrecognised value is simply "not a
    /// status", not an error. `FromStr` would force a `Result` and an error
    /// type that every caller would immediately discard.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s
            .trim()
            .to_lowercase()
            .replace([' ', '\u{2014}'], "-")
            .as_str()
        {
            "draft" => Some(Self::Draft),
            "proposed" => Some(Self::Proposed),
            "accepted" => Some(Self::Accepted),
            "accepted-current-slice" | "accepted-for-current-slice" => {
                Some(Self::AcceptedCurrentSlice)
            }
            "in-progress" => Some(Self::InProgress),
            "implemented" => Some(Self::Implemented),
            "deferred" => Some(Self::Deferred),
            "superseded" => Some(Self::Superseded),
            "historical" => Some(Self::Historical),
            _ => None,
        }
    }

    /// Canonical string representation for frontmatter writeback
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Proposed => "proposed",
            Self::Accepted => "accepted",
            Self::AcceptedCurrentSlice => "accepted-current-slice",
            Self::InProgress => "in-progress",
            Self::Implemented => "implemented",
            Self::Deferred => "deferred",
            Self::Superseded => "superseded",
            Self::Historical => "historical",
        }
    }

    /// Valid transitions from this status
    pub fn valid_transitions(&self) -> Vec<ProposalStatus> {
        match self {
            Self::Draft => vec![Self::Proposed, Self::Deferred],
            Self::Proposed => vec![
                Self::Accepted,
                Self::AcceptedCurrentSlice,
                Self::Deferred,
                Self::Superseded,
            ],
            Self::Accepted => vec![
                Self::AcceptedCurrentSlice,
                Self::InProgress,
                Self::Deferred,
                Self::Superseded,
            ],
            Self::AcceptedCurrentSlice => vec![
                Self::InProgress,
                Self::Implemented,
                Self::Deferred,
                Self::Superseded,
            ],
            Self::InProgress => vec![
                Self::Implemented,
                Self::AcceptedCurrentSlice,
                Self::Deferred,
            ],
            Self::Implemented => vec![Self::Historical, Self::Superseded],
            Self::Deferred => vec![Self::Proposed, Self::Accepted, Self::Historical],
            Self::Superseded => vec![Self::Historical],
            Self::Historical => vec![],
        }
    }

    pub fn can_transition_to(&self, target: &ProposalStatus) -> bool {
        self.valid_transitions().contains(target)
    }
}

/// Valid seam statuses
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SeamStatus {
    Registered,
    NotStarted,
    InProgress,
    Complete,
    Blocked,
    Deferred,
}

/// Frontmatter fields that the graph is allowed to write back to docs
pub fn writable_frontmatter_fields() -> Vec<&'static str> {
    vec!["status", "last_updated", "active_seams", "implemented_by"]
}

/// Validate that a proposed status transition is allowed
pub fn validate_transition(current: &str, proposed: &str) -> Result<(), String> {
    let from = ProposalStatus::from_str(current)
        .ok_or_else(|| format!("Unknown current status: {current}"))?;
    let to = ProposalStatus::from_str(proposed)
        .ok_or_else(|| format!("Unknown proposed status: {proposed}"))?;

    if from.can_transition_to(&to) {
        Ok(())
    } else {
        Err(format!(
            "Invalid transition: {} → {}. Valid targets: {:?}",
            from.as_str(),
            to.as_str(),
            from.valid_transitions()
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
        ))
    }
}

/// The SVER domain registry — maps domain names to descriptions
pub fn domain_registry() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "deployment-distribution",
            "Deployment, packaging, distribution",
        ),
        ("governance", "Architectural rules, roadmap, process"),
        ("graph-domain-layer", "Graph storage and domain abstraction"),
        ("membrane-transport", "External protocol gateways"),
        ("memory-context", "Memory, context, cognitive engine"),
        ("mesh-placement", "Multi-hotel, mesh topology"),
        ("migration-parity", "OpenClaw parity and migration"),
        ("operator-control-plane", "Admin surfaces, control plane"),
        ("operator-experience", "Onboarding, UX, CLI"),
        ("product-management-plane", "Project intelligence, graph"),
        ("runtime-sessions", "Agent loop, sessions, roles"),
        ("tooling-execution", "Tool running, sandbox, security"),
        ("workflow-docs", "Documentation, tagging, process"),
        ("philotic-sandbox", "Secure execution environment"),
    ]
}
