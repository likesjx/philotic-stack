//! Layer 1 (pure/static) skill validation for delegation skill definitions.
//!
//! This module is responsible for the first validation gate in the skill lifecycle
//! state machine: `draft → validated`. Layer 1 performs only structural and internal
//! consistency checks that can be evaluated without any hotel, mesh, or catalog
//! access. It must be deterministic and have no side effects.
//!
//! ## Validation layers (summary)
//! - **Layer 1 (this module)**: Pure/static. No I/O. Always computable offline.
//! - **Layer 2**: Hotel/mesh capability gate (live capability lookup, async).
//! - **Layer 3**: Spawn-time point-in-time re-check (lightweight, async).
//!
//! ## Hook routing invariants
//! Hooks are opt-in, negotiated by the delegation skill definition. The skill
//! owns routing — infrastructure does not hardcode it. The following invariants
//! are enforced at Layer 1:
//!
//! - `ApprovalNeeded` hook **cannot** route to `Discard` (nobody can respond).
//! - `Discard` route without a `handler_skill` is a pointless subscription.
//! - `completion_route` **cannot** be `Discard` (system must know the job finished).
//! - `failure_route` **cannot** be `Discard` (system must know the job failed).
//! - Duplicate hook subscriptions on the same `HookKind` are not allowed.

use crate::graph::{AbstractSkillRecord, SkillValidationState};
use serde::{Deserialize, Serialize};

// ── Hook routing types ────────────────────────────────────────────────────────
//
// These types mirror `philotic-client`'s wire types exactly (serde-compatible)
// but live here so Layer 1 validation is self-contained with no external crate
// dependency.  When the two evolve, keep them in sync.

/// Which lifecycle event a subscription is targeting.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookKind {
    Progress,
    TurnStarted,
    ToolCall,
    TurnCompleted,
    ApprovalNeeded,
}

/// Where a hook event is routed when it fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum HookRoute {
    /// Deliver to the persona agent that spawned this subagent (default).
    PersonaAgent,
    /// Deliver to any currently active role with this name on the mesh.
    Role { role_name: String },
    /// Do not deliver outward; fire locally for side-effects only.
    /// Requires a `handler_skill` on the subscription — otherwise pointless.
    Discard,
}

impl Default for HookRoute {
    fn default() -> Self {
        Self::PersonaAgent
    }
}

/// A single hook subscription declared by the delegation skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookSubscription {
    pub hook_kind: HookKind,
    #[serde(default)]
    pub route: HookRoute,
    /// Required when `route` is `Discard`; optional otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler_skill: Option<String>,
}

// ── Lease types ───────────────────────────────────────────────────────────────

/// What the hotel does when a subagent's lease expires without renewal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IdleBehavior {
    /// Hotel terminates the subagent guest.
    #[default]
    Terminate,
    /// Hotel notifies the persona agent and waits.
    NotifyPersona,
    /// Hotel automatically renews the lease without persona involvement.
    AutoRenew,
}

/// Lease budget declared by the delegation skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillLeaseTerms {
    /// Nominal TTL in seconds.  Must be > 0.
    pub ttl_seconds: u64,
    /// If set, how often the persona must renew.  Must be < `ttl_seconds`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renewal_interval_seconds: Option<u64>,
    /// Hard lifetime cap.  If set, must be >= `ttl_seconds`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lifetime_seconds: Option<u64>,
    pub idle_behavior: IdleBehavior,
}

impl Default for SkillLeaseTerms {
    fn default() -> Self {
        Self {
            ttl_seconds: 300,
            renewal_interval_seconds: None,
            max_lifetime_seconds: None,
            idle_behavior: IdleBehavior::Terminate,
        }
    }
}

// ── Completion contract ───────────────────────────────────────────────────────

/// What the subagent must include in its terminal `subagent.complete` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SkillCompletionContract {
    /// Subagent must emit a human-readable task summary.
    #[serde(default)]
    pub summary_required: bool,
    /// Subagent must include artifact refs when it produced outputs.
    #[serde(default)]
    pub artifact_refs_expected: bool,
    /// Subagent must include a failure summary when `subagent.failed` fires.
    #[serde(default)]
    pub failure_summary_required: bool,
    /// Persona must acknowledge receipt before hotel marks the lease released.
    #[serde(default)]
    pub requires_parent_ack: bool,
}

// ── SkillDraft ────────────────────────────────────────────────────────────────

/// The full pre-registration input for a delegation skill.
///
/// This is what the meta-skill (`skill-creator`) elicits and assembles before
/// submitting to `skill.register`.  Every field has a declared source type in
/// the field-sourcing map.  Layer 1 validates only the structural and internal
/// consistency invariants that can be proven without any I/O.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDraft {
    // ── Identity ──────────────────────────────────────────────────────────────
    /// Stable identifier.  Non-empty, [a-z0-9_-] only, max 64 chars.
    pub skill_name: String,
    /// Human-readable purpose description.  Non-empty, max 2 048 chars.
    pub description: String,

    // ── Mission ───────────────────────────────────────────────────────────────
    /// The subagent role or kind being delegated to.  Non-empty.
    pub subagent_kind: String,
    /// Template for the goal string injected into the subagent context.  Non-empty.
    pub goal_template: String,

    // ── Grants ────────────────────────────────────────────────────────────────
    /// Explicit tool IDs the subagent may use.  Each entry non-empty.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Skill IDs the subagent may itself delegate to.  Each entry non-empty.
    #[serde(default)]
    pub allowed_skills: Vec<String>,
    /// Max model-turn budget (iterations).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration_budget: Option<u32>,

    // ── Lifecycle ─────────────────────────────────────────────────────────────
    pub lease_terms: SkillLeaseTerms,
    /// Declared hook subscriptions.  Hooks not listed here will not fire.
    #[serde(default)]
    pub hook_subscriptions: Vec<HookSubscription>,
    /// Where `subagent.complete` is routed.  Cannot be `Discard`.
    #[serde(default)]
    pub completion_route: HookRoute,
    /// Where `subagent.failed` is routed.  Cannot be `Discard`.
    #[serde(default)]
    pub failure_route: HookRoute,
    pub completion_contract: SkillCompletionContract,

    // ── Provenance ────────────────────────────────────────────────────────────
    /// JSON object mapping each field name to its source type descriptor.
    /// Must be a non-null JSON object (not array, not primitive).
    #[serde(default)]
    pub field_sources: serde_json::Value,
}

// ── ValidationError ───────────────────────────────────────────────────────────

/// An individual Layer 1 validation failure.
///
/// Layer 1 is exhaustive: **all** applicable errors are collected per call;
/// validation does not short-circuit on the first failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ValidationError {
    // ── Identity ──────────────────────────────────────────────────────────────
    /// `skill_name` is empty.
    EmptySkillName,
    /// `skill_name` contains characters outside `[a-z0-9_-]`.
    InvalidSkillNameChars { skill_name: String },
    /// `skill_name` exceeds 64 characters.
    SkillNameTooLong { len: usize },
    /// `description` is empty.
    EmptyDescription,
    /// `description` exceeds 2 048 characters.
    DescriptionTooLong { len: usize },

    // ── Mission ───────────────────────────────────────────────────────────────
    /// `subagent_kind` is empty.
    EmptySubagentKind,
    /// `goal_template` is empty.
    EmptyGoalTemplate,

    // ── Grants ────────────────────────────────────────────────────────────────
    /// An entry in `allowed_tools` is empty.
    EmptyToolEntry { index: usize },
    /// An entry in `allowed_skills` is empty.
    EmptySkillEntry { index: usize },
    /// `iteration_budget` is 0, which would make the subagent immediately stuck.
    ZeroIterationBudget,

    // ── Lease invariants ──────────────────────────────────────────────────────
    /// `lease_terms.ttl_seconds` is 0.
    ZeroLeaseTtl,
    /// `renewal_interval_seconds` is >= `ttl_seconds` (would always miss the window).
    RenewalIntervalNotLessThanTtl {
        renewal_interval_seconds: u64,
        ttl_seconds: u64,
    },
    /// `max_lifetime_seconds` is < `ttl_seconds` (shorter than a single lease period).
    MaxLifetimeLessThanTtl {
        max_lifetime_seconds: u64,
        ttl_seconds: u64,
    },

    // ── Hook routing invariants ───────────────────────────────────────────────
    /// The same `HookKind` appears more than once in `hook_subscriptions`.
    DuplicateHookSubscription { hook_kind: String },
    /// `ApprovalNeeded` hook is subscribed with `Discard` route — no agent can respond.
    ApprovalNeededHookDiscarded,
    /// A hook uses `Discard` route but has no `handler_skill` — pointless subscription.
    DiscardedHookWithNoHandler { hook_kind: String },
    /// `completion_route` is `Discard` — the system cannot know the job finished.
    CompletionRouteMustNotDiscard,
    /// `failure_route` is `Discard` — the system cannot know the job failed.
    FailureRouteMustNotDiscard,
    /// A `Role` route has an empty `role_name`.
    EmptyRoleNameInRoute { context: String },

    // ── Provenance ────────────────────────────────────────────────────────────
    /// `field_sources` is not a JSON object (null, array, or primitive).
    FieldSourcesNotAnObject,
}

impl ValidationError {
    /// Short stable code string for logging/metrics.
    pub fn code(&self) -> &'static str {
        match self {
            Self::EmptySkillName => "empty_skill_name",
            Self::InvalidSkillNameChars { .. } => "invalid_skill_name_chars",
            Self::SkillNameTooLong { .. } => "skill_name_too_long",
            Self::EmptyDescription => "empty_description",
            Self::DescriptionTooLong { .. } => "description_too_long",
            Self::EmptySubagentKind => "empty_subagent_kind",
            Self::EmptyGoalTemplate => "empty_goal_template",
            Self::EmptyToolEntry { .. } => "empty_tool_entry",
            Self::EmptySkillEntry { .. } => "empty_skill_entry",
            Self::ZeroIterationBudget => "zero_iteration_budget",
            Self::ZeroLeaseTtl => "zero_lease_ttl",
            Self::RenewalIntervalNotLessThanTtl { .. } => "renewal_interval_not_less_than_ttl",
            Self::MaxLifetimeLessThanTtl { .. } => "max_lifetime_less_than_ttl",
            Self::DuplicateHookSubscription { .. } => "duplicate_hook_subscription",
            Self::ApprovalNeededHookDiscarded => "approval_needed_hook_discarded",
            Self::DiscardedHookWithNoHandler { .. } => "discarded_hook_with_no_handler",
            Self::CompletionRouteMustNotDiscard => "completion_route_must_not_discard",
            Self::FailureRouteMustNotDiscard => "failure_route_must_not_discard",
            Self::EmptyRoleNameInRoute { .. } => "empty_role_name_in_route",
            Self::FieldSourcesNotAnObject => "field_sources_not_an_object",
        }
    }
}

// ── validate_skill_layer1 ─────────────────────────────────────────────────────

/// Validate a [`SkillDraft`] using only pure structural checks (Layer 1).
///
/// Returns `Ok(())` when all invariants pass or `Err(errors)` with the full
/// list of failures.  All errors are collected — validation does not
/// short-circuit.
///
/// This function is synchronous, infallible for valid Rust types, and has no
/// I/O.  It is safe to call from any context.
pub fn validate_skill_layer1(draft: &SkillDraft) -> Result<(), Vec<ValidationError>> {
    let mut errors: Vec<ValidationError> = Vec::new();

    // ── Identity ──────────────────────────────────────────────────────────────
    validate_skill_name(&draft.skill_name, &mut errors);
    validate_description(&draft.description, &mut errors);

    // ── Mission ───────────────────────────────────────────────────────────────
    if draft.subagent_kind.trim().is_empty() {
        errors.push(ValidationError::EmptySubagentKind);
    }
    if draft.goal_template.trim().is_empty() {
        errors.push(ValidationError::EmptyGoalTemplate);
    }

    // ── Grants ────────────────────────────────────────────────────────────────
    for (i, tool) in draft.allowed_tools.iter().enumerate() {
        if tool.trim().is_empty() {
            errors.push(ValidationError::EmptyToolEntry { index: i });
        }
    }
    for (i, skill) in draft.allowed_skills.iter().enumerate() {
        if skill.trim().is_empty() {
            errors.push(ValidationError::EmptySkillEntry { index: i });
        }
    }
    if let Some(budget) = draft.iteration_budget {
        if budget == 0 {
            errors.push(ValidationError::ZeroIterationBudget);
        }
    }

    // ── Lease invariants ──────────────────────────────────────────────────────
    validate_lease_terms(&draft.lease_terms, &mut errors);

    // ── Hook routing invariants ───────────────────────────────────────────────
    validate_hook_subscriptions(&draft.hook_subscriptions, &mut errors);
    validate_terminal_route(&draft.completion_route, "completion_route", &mut errors);
    validate_terminal_route(&draft.failure_route, "failure_route", &mut errors);

    if matches!(draft.completion_route, HookRoute::Discard) {
        errors.push(ValidationError::CompletionRouteMustNotDiscard);
    }
    if matches!(draft.failure_route, HookRoute::Discard) {
        errors.push(ValidationError::FailureRouteMustNotDiscard);
    }

    // ── Provenance ────────────────────────────────────────────────────────────
    if !draft.field_sources.is_object() {
        errors.push(ValidationError::FieldSourcesNotAnObject);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

// ── internal helpers ──────────────────────────────────────────────────────────

fn validate_skill_name(name: &str, errors: &mut Vec<ValidationError>) {
    if name.is_empty() {
        errors.push(ValidationError::EmptySkillName);
        return; // subsequent checks are moot
    }
    if name.len() > 64 {
        errors.push(ValidationError::SkillNameTooLong { len: name.len() });
    }
    let invalid_chars = !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if invalid_chars {
        errors.push(ValidationError::InvalidSkillNameChars {
            skill_name: name.to_string(),
        });
    }
}

fn validate_description(desc: &str, errors: &mut Vec<ValidationError>) {
    if desc.trim().is_empty() {
        errors.push(ValidationError::EmptyDescription);
    } else if desc.len() > 2048 {
        errors.push(ValidationError::DescriptionTooLong { len: desc.len() });
    }
}

fn validate_lease_terms(terms: &SkillLeaseTerms, errors: &mut Vec<ValidationError>) {
    if terms.ttl_seconds == 0 {
        errors.push(ValidationError::ZeroLeaseTtl);
        // Skip dependent checks when ttl is 0 to avoid misleading errors.
        return;
    }
    if let Some(ri) = terms.renewal_interval_seconds {
        if ri >= terms.ttl_seconds {
            errors.push(ValidationError::RenewalIntervalNotLessThanTtl {
                renewal_interval_seconds: ri,
                ttl_seconds: terms.ttl_seconds,
            });
        }
    }
    if let Some(ml) = terms.max_lifetime_seconds {
        if ml < terms.ttl_seconds {
            errors.push(ValidationError::MaxLifetimeLessThanTtl {
                max_lifetime_seconds: ml,
                ttl_seconds: terms.ttl_seconds,
            });
        }
    }
}

fn validate_hook_subscriptions(subs: &[HookSubscription], errors: &mut Vec<ValidationError>) {
    let mut seen = std::collections::HashSet::new();

    for sub in subs {
        let kind_key = format!("{:?}", sub.hook_kind);

        // Duplicate detection
        if !seen.insert(kind_key.clone()) {
            errors.push(ValidationError::DuplicateHookSubscription {
                hook_kind: kind_key.clone(),
            });
        }

        // ApprovalNeeded must not discard
        if sub.hook_kind == HookKind::ApprovalNeeded && matches!(sub.route, HookRoute::Discard) {
            errors.push(ValidationError::ApprovalNeededHookDiscarded);
        }

        // Discard without handler_skill is pointless
        if matches!(sub.route, HookRoute::Discard) && sub.handler_skill.is_none() {
            errors.push(ValidationError::DiscardedHookWithNoHandler {
                hook_kind: kind_key.clone(),
            });
        }

        // Role route must name a role
        if let HookRoute::Role { role_name } = &sub.route {
            if role_name.trim().is_empty() {
                errors.push(ValidationError::EmptyRoleNameInRoute {
                    context: format!("hook_subscriptions[{:?}]", sub.hook_kind),
                });
            }
        }
    }
}

/// Validate routes that appear in terminal positions (completion/failure routes).
/// These share the same "empty role_name" check but not the Discard check
/// (that is enforced by the caller with the specific error variant).
fn validate_terminal_route(route: &HookRoute, ctx: &str, errors: &mut Vec<ValidationError>) {
    if let HookRoute::Role { role_name } = route {
        if role_name.trim().is_empty() {
            errors.push(ValidationError::EmptyRoleNameInRoute {
                context: ctx.to_string(),
            });
        }
    }
}

// ── apply_validation_to_record ────────────────────────────────────────────────

/// Apply a Layer 1 validation result to an [`AbstractSkillRecord`] in place.
///
/// - On success, transitions state from `Draft` → `Validated`.
/// - On failure, transitions to `Invalid` with the serialized error codes.
///
/// This function does **not** persist the record; callers are responsible for
/// writing it back to storage.
pub fn apply_validation_to_record(
    record: &mut AbstractSkillRecord,
    result: Result<(), Vec<ValidationError>>,
) {
    match result {
        Ok(()) => {
            record.validation_state = SkillValidationState::Validated;
        }
        Err(errors) => {
            let codes: Vec<String> = errors.iter().map(|e| e.code().to_string()).collect();
            record.validation_state = SkillValidationState::Invalid { errors: codes };
        }
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Fixtures ──────────────────────────────────────────────────────────────

    fn valid_draft() -> SkillDraft {
        SkillDraft {
            skill_name: "data-analyst".to_string(),
            description: "Analyses provided datasets and returns a structured report.".to_string(),
            subagent_kind: "analyst-worker".to_string(),
            goal_template: "Analyse the dataset at {{dataset_ref}} and produce a report."
                .to_string(),
            allowed_tools: vec!["workspace.read".to_string(), "workspace.list".to_string()],
            allowed_skills: vec![],
            iteration_budget: Some(20),
            lease_terms: SkillLeaseTerms {
                ttl_seconds: 600,
                renewal_interval_seconds: Some(300),
                max_lifetime_seconds: Some(3600),
                idle_behavior: IdleBehavior::Terminate,
            },
            hook_subscriptions: vec![],
            completion_route: HookRoute::PersonaAgent,
            failure_route: HookRoute::PersonaAgent,
            completion_contract: SkillCompletionContract {
                summary_required: true,
                ..Default::default()
            },
            field_sources: json!({ "goal_template": "persona_input", "subagent_kind": "skill_registry" }),
        }
    }

    // ── Happy path ────────────────────────────────────────────────────────────

    #[test]
    fn valid_draft_passes() {
        assert!(validate_skill_layer1(&valid_draft()).is_ok());
    }

    #[test]
    fn valid_draft_with_hooks_passes() {
        let mut draft = valid_draft();
        draft.hook_subscriptions = vec![
            HookSubscription {
                hook_kind: HookKind::Progress,
                route: HookRoute::PersonaAgent,
                handler_skill: None,
            },
            HookSubscription {
                hook_kind: HookKind::TurnCompleted,
                route: HookRoute::Role {
                    role_name: "supervisor".to_string(),
                },
                handler_skill: None,
            },
        ];
        assert!(validate_skill_layer1(&draft).is_ok());
    }

    #[test]
    fn discard_route_with_handler_passes() {
        let mut draft = valid_draft();
        draft.hook_subscriptions = vec![HookSubscription {
            hook_kind: HookKind::ToolCall,
            route: HookRoute::Discard,
            handler_skill: Some("audit-logger".to_string()),
        }];
        assert!(validate_skill_layer1(&draft).is_ok());
    }

    // ── Identity errors ───────────────────────────────────────────────────────

    #[test]
    fn empty_skill_name() {
        let mut d = valid_draft();
        d.skill_name = "".to_string();
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::EmptySkillName)));
    }

    #[test]
    fn invalid_skill_name_chars() {
        let mut d = valid_draft();
        d.skill_name = "Data Analyst!".to_string();
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::InvalidSkillNameChars { .. })));
    }

    #[test]
    fn skill_name_too_long() {
        let mut d = valid_draft();
        d.skill_name = "a".repeat(65);
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::SkillNameTooLong { len } if *len == 65)));
    }

    #[test]
    fn empty_description() {
        let mut d = valid_draft();
        d.description = "   ".to_string();
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::EmptyDescription)));
    }

    #[test]
    fn description_too_long() {
        let mut d = valid_draft();
        d.description = "x".repeat(2049);
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::DescriptionTooLong { len } if *len == 2049)));
    }

    // ── Mission errors ────────────────────────────────────────────────────────

    #[test]
    fn empty_subagent_kind() {
        let mut d = valid_draft();
        d.subagent_kind = "".to_string();
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::EmptySubagentKind)));
    }

    #[test]
    fn empty_goal_template() {
        let mut d = valid_draft();
        d.goal_template = " ".to_string();
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::EmptyGoalTemplate)));
    }

    // ── Grant errors ──────────────────────────────────────────────────────────

    #[test]
    fn empty_tool_entry() {
        let mut d = valid_draft();
        d.allowed_tools = vec!["workspace.read".to_string(), "".to_string()];
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::EmptyToolEntry { index: 1 })));
    }

    #[test]
    fn empty_skill_entry() {
        let mut d = valid_draft();
        d.allowed_skills = vec!["".to_string()];
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::EmptySkillEntry { index: 0 })));
    }

    #[test]
    fn zero_iteration_budget() {
        let mut d = valid_draft();
        d.iteration_budget = Some(0);
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::ZeroIterationBudget)));
    }

    // ── Lease invariants ──────────────────────────────────────────────────────

    #[test]
    fn zero_lease_ttl() {
        let mut d = valid_draft();
        d.lease_terms.ttl_seconds = 0;
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::ZeroLeaseTtl)));
    }

    #[test]
    fn renewal_interval_equals_ttl() {
        let mut d = valid_draft();
        d.lease_terms.ttl_seconds = 300;
        d.lease_terms.renewal_interval_seconds = Some(300);
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs.iter().any(|e| matches!(
            e,
            ValidationError::RenewalIntervalNotLessThanTtl {
                renewal_interval_seconds: 300,
                ttl_seconds: 300
            }
        )));
    }

    #[test]
    fn renewal_interval_greater_than_ttl() {
        let mut d = valid_draft();
        d.lease_terms.ttl_seconds = 300;
        d.lease_terms.renewal_interval_seconds = Some(400);
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::RenewalIntervalNotLessThanTtl { .. })));
    }

    #[test]
    fn max_lifetime_less_than_ttl() {
        let mut d = valid_draft();
        d.lease_terms.ttl_seconds = 600;
        d.lease_terms.max_lifetime_seconds = Some(100);
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs.iter().any(|e| matches!(
            e,
            ValidationError::MaxLifetimeLessThanTtl {
                max_lifetime_seconds: 100,
                ttl_seconds: 600
            }
        )));
    }

    // ── Hook routing invariants ───────────────────────────────────────────────

    #[test]
    fn duplicate_hook_subscription() {
        let mut d = valid_draft();
        d.hook_subscriptions = vec![
            HookSubscription {
                hook_kind: HookKind::Progress,
                route: HookRoute::PersonaAgent,
                handler_skill: None,
            },
            HookSubscription {
                hook_kind: HookKind::Progress,
                route: HookRoute::PersonaAgent,
                handler_skill: None,
            },
        ];
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::DuplicateHookSubscription { .. })));
    }

    #[test]
    fn approval_needed_hook_discarded() {
        let mut d = valid_draft();
        d.hook_subscriptions = vec![HookSubscription {
            hook_kind: HookKind::ApprovalNeeded,
            route: HookRoute::Discard,
            handler_skill: Some("approval-logger".to_string()),
        }];
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::ApprovalNeededHookDiscarded)));
    }

    #[test]
    fn discarded_hook_with_no_handler() {
        let mut d = valid_draft();
        d.hook_subscriptions = vec![HookSubscription {
            hook_kind: HookKind::ToolCall,
            route: HookRoute::Discard,
            handler_skill: None, // no handler — invalid
        }];
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::DiscardedHookWithNoHandler { .. })));
    }

    #[test]
    fn completion_route_must_not_discard() {
        let mut d = valid_draft();
        d.completion_route = HookRoute::Discard;
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::CompletionRouteMustNotDiscard)));
    }

    #[test]
    fn failure_route_must_not_discard() {
        let mut d = valid_draft();
        d.failure_route = HookRoute::Discard;
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::FailureRouteMustNotDiscard)));
    }

    #[test]
    fn empty_role_name_in_hook_route() {
        let mut d = valid_draft();
        d.hook_subscriptions = vec![HookSubscription {
            hook_kind: HookKind::TurnCompleted,
            route: HookRoute::Role {
                role_name: "  ".to_string(),
            },
            handler_skill: None,
        }];
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::EmptyRoleNameInRoute { .. })));
    }

    #[test]
    fn empty_role_name_in_completion_route() {
        let mut d = valid_draft();
        d.completion_route = HookRoute::Role {
            role_name: "".to_string(),
        };
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs.iter().any(|e| matches!(
            e,
            ValidationError::EmptyRoleNameInRoute { context } if context == "completion_route"
        )));
    }

    #[test]
    fn empty_role_name_in_failure_route() {
        let mut d = valid_draft();
        d.failure_route = HookRoute::Role {
            role_name: "".to_string(),
        };
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs.iter().any(|e| matches!(
            e,
            ValidationError::EmptyRoleNameInRoute { context } if context == "failure_route"
        )));
    }

    // ── Provenance errors ─────────────────────────────────────────────────────

    #[test]
    fn field_sources_not_an_object_null() {
        let mut d = valid_draft();
        d.field_sources = serde_json::Value::Null;
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::FieldSourcesNotAnObject)));
    }

    #[test]
    fn field_sources_not_an_object_array() {
        let mut d = valid_draft();
        d.field_sources = json!(["a", "b"]);
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::FieldSourcesNotAnObject)));
    }

    #[test]
    fn field_sources_not_an_object_string() {
        let mut d = valid_draft();
        d.field_sources = json!("not-an-object");
        let errs = validate_skill_layer1(&d).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::FieldSourcesNotAnObject)));
    }

    // ── Multi-error collection ────────────────────────────────────────────────

    #[test]
    fn collects_multiple_errors() {
        let d = SkillDraft {
            skill_name: "".to_string(),
            description: "".to_string(),
            subagent_kind: "".to_string(),
            goal_template: "".to_string(),
            allowed_tools: vec!["".to_string()],
            allowed_skills: vec!["".to_string()],
            iteration_budget: Some(0),
            lease_terms: SkillLeaseTerms::default(), // valid, ttl=300
            hook_subscriptions: vec![],
            completion_route: HookRoute::Discard,
            failure_route: HookRoute::Discard,
            completion_contract: SkillCompletionContract::default(),
            field_sources: serde_json::Value::Null,
        };
        let errs = validate_skill_layer1(&d).unwrap_err();
        // Should collect: EmptySkillName, EmptyDescription, EmptySubagentKind,
        // EmptyGoalTemplate, EmptyToolEntry, EmptySkillEntry, ZeroIterationBudget,
        // CompletionRouteMustNotDiscard, FailureRouteMustNotDiscard, FieldSourcesNotAnObject
        assert!(
            errs.len() >= 10,
            "expected ≥10 errors, got {}: {:?}",
            errs.len(),
            errs
        );
    }

    // ── apply_validation_to_record ────────────────────────────────────────────

    #[test]
    fn apply_transitions_to_validated_on_success() {
        use crate::graph::AbstractSkillRecord;
        let mut record = AbstractSkillRecord {
            skill_name: "data-analyst".to_string(),
            ..Default::default()
        };
        apply_validation_to_record(&mut record, Ok(()));
        assert_eq!(record.validation_state, SkillValidationState::Validated);
    }

    #[test]
    fn apply_transitions_to_invalid_on_failure() {
        use crate::graph::AbstractSkillRecord;
        let mut record = AbstractSkillRecord {
            skill_name: "bad-skill".to_string(),
            ..Default::default()
        };
        let errors = vec![
            ValidationError::EmptyGoalTemplate,
            ValidationError::ZeroLeaseTtl,
        ];
        apply_validation_to_record(&mut record, Err(errors));
        match &record.validation_state {
            SkillValidationState::Invalid { errors: codes } => {
                assert!(codes.contains(&"empty_goal_template".to_string()));
                assert!(codes.contains(&"zero_lease_ttl".to_string()));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }
}
