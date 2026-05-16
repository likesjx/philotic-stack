use philotic_graph::fleet;
use philotic_graph::sver::{self, ProposalStatus};

#[test]
fn test_valid_transitions() {
    // Draft → Proposed is valid
    assert!(ProposalStatus::Draft.can_transition_to(&ProposalStatus::Proposed));

    // Proposed → Accepted is valid
    assert!(ProposalStatus::Proposed.can_transition_to(&ProposalStatus::Accepted));

    // Accepted → InProgress is valid
    assert!(ProposalStatus::Accepted.can_transition_to(&ProposalStatus::InProgress));

    // InProgress → Implemented is valid
    assert!(ProposalStatus::InProgress.can_transition_to(&ProposalStatus::Implemented));

    // Implemented → Historical is valid
    assert!(ProposalStatus::Implemented.can_transition_to(&ProposalStatus::Historical));
}

#[test]
fn test_invalid_transitions() {
    // Draft → Implemented is NOT valid (must go through Proposed/Accepted first)
    assert!(!ProposalStatus::Draft.can_transition_to(&ProposalStatus::Implemented));

    // Historical → anything is NOT valid (terminal state)
    assert!(!ProposalStatus::Historical.can_transition_to(&ProposalStatus::Draft));
    assert!(!ProposalStatus::Historical.can_transition_to(&ProposalStatus::Proposed));

    // Implemented → Draft is NOT valid
    assert!(!ProposalStatus::Implemented.can_transition_to(&ProposalStatus::Draft));
}

#[test]
fn test_validate_transition_function() {
    assert!(sver::validate_transition("draft", "proposed").is_ok());
    assert!(sver::validate_transition("proposed", "accepted").is_ok());
    assert!(sver::validate_transition("draft", "implemented").is_err());
    assert!(sver::validate_transition("unknown-status", "proposed").is_err());
    assert!(sver::validate_transition("draft", "nonexistent").is_err());
}

#[test]
fn test_proposal_status_from_str() {
    assert_eq!(
        ProposalStatus::from_str("draft"),
        Some(ProposalStatus::Draft)
    );
    assert_eq!(
        ProposalStatus::from_str("in-progress"),
        Some(ProposalStatus::InProgress)
    );
    assert_eq!(
        ProposalStatus::from_str("accepted-current-slice"),
        Some(ProposalStatus::AcceptedCurrentSlice)
    );
    assert_eq!(
        ProposalStatus::from_str("accepted-for-current-slice"),
        Some(ProposalStatus::AcceptedCurrentSlice)
    );
    assert_eq!(
        ProposalStatus::from_str("  Draft  "),
        Some(ProposalStatus::Draft)
    );
    assert_eq!(ProposalStatus::from_str("invalid"), None);
}

#[test]
fn test_proposal_status_as_str_roundtrip() {
    let statuses = [
        ProposalStatus::Draft,
        ProposalStatus::Proposed,
        ProposalStatus::Accepted,
        ProposalStatus::AcceptedCurrentSlice,
        ProposalStatus::InProgress,
        ProposalStatus::Implemented,
        ProposalStatus::Deferred,
        ProposalStatus::Superseded,
        ProposalStatus::Historical,
    ];
    for status in &statuses {
        let s = status.as_str();
        let parsed = ProposalStatus::from_str(s);
        assert_eq!(parsed, Some(*status), "roundtrip failed for {s}");
    }
}

#[test]
fn test_domain_registry_has_entries() {
    let domains = sver::domain_registry();
    assert!(!domains.is_empty());
    // Verify at least a few expected domains
    let names: Vec<&str> = domains.iter().map(|(n, _)| *n).collect();
    assert!(names.contains(&"governance"));
    assert!(names.contains(&"runtime-sessions"));
    assert!(names.contains(&"memory-context"));
    assert!(names.contains(&"philotic-sandbox"));
}

#[test]
fn test_writable_frontmatter_fields() {
    let fields = sver::writable_frontmatter_fields();
    assert!(fields.contains(&"status"));
    assert!(fields.contains(&"last_updated"));
    assert!(fields.contains(&"active_seams"));
    assert!(fields.contains(&"implemented_by"));
}

#[test]
fn test_fleet_known_agents() {
    let agents = fleet::known_agents();
    assert!(!agents.is_empty());
    let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
    assert!(names.contains(&"jane"));
    assert!(names.contains(&"aria"));
    assert!(names.contains(&"beacon"));
    assert!(names.contains(&"hermes"));
    assert!(names.contains(&"human"));
    assert!(names.contains(&"claude-code"));
}

#[test]
fn test_fleet_lookup_agent() {
    let aria = fleet::lookup_agent("aria");
    assert!(aria.is_some());
    let aria = aria.unwrap();
    assert_eq!(aria.display_name, "Aria");
    assert_eq!(aria.role, "architect");

    assert!(fleet::lookup_agent("nonexistent").is_none());
}
