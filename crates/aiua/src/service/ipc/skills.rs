//! Skill registry: admin gate, audit, delegation, register/state handlers.
//!
//! Moved verbatim from `ipc/mod.rs` (IPC_DISPATCH_SPLIT); only item
//! visibility was widened so the parent module can reach it.

use super::*;

/// Is this guest identity's `role` a skill-administration role?
///
/// Accepts the bare names `orchestrator` / `management` AND their
/// role-incarnation routing form `role:{agent_id}:{name}` — which is what a
/// materialized role-incarnation philote actually registers as (see
/// `RoleIncarnationRecord::routing_role` and `philote/src/main.rs`).
/// DEF-105 (2026-09-04): the gate only compared against the bare names, so no
/// real incarnation had ever passed it — every prior registration in the
/// audit trail came from drill guests whose identity role was literally
/// `orchestrator`. Bjork's own orchestrator incarnation was refused
/// `register_skill` live while the distill whisper watched.
pub(in crate::service) fn skill_admin_role(role: &str) -> bool {
    let name = role
        .strip_prefix("role:")
        .and_then(|rest| rest.rsplit_once(':').map(|(_, name)| name))
        .unwrap_or(role);
    name == "orchestrator" || name == "management"
}

/// Shared authorization gate for every skill-administration IPC op
/// (`RegisterSkill`, `AssignSkill`, `RevokeSkill`, `SetSkillState`,
/// `ListSkillAudits`). Skills project tools onto agents, so administration is
/// restricted to authenticated guests holding the `orchestrator` or
/// `management` role. Centralized so new ops cannot fork the policy.
#[allow(clippy::result_large_err)]
// Err is the IpcResponse sent on the cold rejection path
pub(in crate::service) fn require_skill_admin<'a>(
    identity: Option<&'a GuestIdentity>,
    op: &str,
    code_prefix: &str,
    verb: &str,
) -> Result<&'a GuestIdentity, IpcResponse> {
    let Some(identity) = identity else {
        return Err(IpcResponse::error(
            op,
            format!("{code_prefix}_UNREGISTERED"),
            format!("guest must register before {verb}"),
        ));
    };
    if !skill_admin_role(&identity.role) {
        warn!(
            guest_id = %identity.guest_id,
            role = %identity.role,
            op = %op,
            "Rejected skill administration from unauthorized role"
        );
        return Err(IpcResponse::error(
            op,
            format!("{code_prefix}_FORBIDDEN"),
            format!("only orchestrator or management guests may {verb}"),
        ));
    }
    Ok(identity)
}

/// Exact-boundary agent ownership check for orchestrator-scoped skill ops.
///
/// Guest ids are formed as `{agent_id}` (base agent) or `{agent_id}:{role}`.
/// A bare `starts_with(agent_id)` lets agent `aria2` administer `aria`, so the
/// prefix must terminate at the `:` separator.
pub(in crate::service) fn guest_owns_agent(guest_id: &str, agent_id: &str) -> bool {
    guest_id == agent_id
        || guest_id
            .strip_prefix(agent_id)
            .is_some_and(|rest| rest.starts_with(':'))
}

/// Write one append-only skill administration audit entry, fail closed: the
/// caller must abort the mutation when this returns an error response.
#[allow(clippy::result_large_err)] // Err is the IpcResponse sent on the cold failure path
pub(in crate::service) fn record_skill_admin_audit(
    graph: &GraphDomain,
    identity: &GuestIdentity,
    op: &str,
    action: &str,
    skill_name: &str,
    validation_state: &str,
    detail: Option<String>,
) -> Result<(), IpcResponse> {
    let audit = SkillRegistrationAuditRecord {
        audit_id: Uuid::new_v4().to_string(),
        skill_name: skill_name.to_string(),
        registered_by: identity.guest_id.clone(),
        registered_by_role: identity.role.clone(),
        validation_state: validation_state.to_string(),
        registered_at: unix_ts(),
        action: action.to_string(),
        detail,
    };
    if let Err(e) = graph.record_skill_registration_audit(&audit) {
        warn!(
            skill_name = %skill_name,
            action = %action,
            "Failed to persist skill admin audit event: {e}"
        );
        return Err(IpcResponse::error(
            op,
            "SKILL_AUDIT_FAILED",
            format!("Failed to record skill admin audit: {e}"),
        ));
    }
    Ok(())
}

/// Wire label for a skill validation state, plus any carried errors/reason.
pub(in crate::service) fn skill_state_label(state: &SkillValidationState) -> (String, Vec<String>) {
    match state {
        SkillValidationState::Validated => ("validated".to_string(), vec![]),
        SkillValidationState::Invalid { errors } => ("invalid".to_string(), errors.clone()),
        SkillValidationState::Draft => ("draft".to_string(), vec![]),
        SkillValidationState::Registered => ("registered".to_string(), vec![]),
        SkillValidationState::Active => ("active".to_string(), vec![]),
        SkillValidationState::Suspended { reason } => {
            ("suspended".to_string(), vec![reason.clone()])
        }
        SkillValidationState::Deprecated => ("deprecated".to_string(), vec![]),
    }
}

/// Resolve a spawn-by-skill-name delegation against the skill catalog.
///
/// When `delegation.skill_name` is set, the registered skill is the authority:
/// its stored `goal_template` (with `{{placeholder}}`s filled from
/// `skill_inputs`) becomes the goal, its `subagent_kind` overrides the
/// caller's, its `implied_tools` — plus the implied tools of its transitive
/// SkillDAG dependencies — bound the subagent's toolset, and its dependency
/// skills activate on the subagent. Unknown or administratively retired
/// (suspended/deprecated) skills are refused, fail closed. A non-empty caller
/// `goal` is appended to the rendered template as delegating-agent context.
///
/// Delegations with no `skill_name` pass through untouched.
#[allow(clippy::result_large_err)] // Err is the IpcResponse sent on the cold rejection path
pub(in crate::service) fn resolve_skill_delegation(
    graph: &GraphDomain,
    mut delegation: philotic_client::SubagentDelegation,
) -> Result<philotic_client::SubagentDelegation, IpcResponse> {
    let Some(skill_name) = delegation.skill_name.clone() else {
        return Ok(delegation);
    };

    let record = match graph.get_abstract_skill(&skill_name) {
        Ok(Some(record)) => record,
        Ok(None) => {
            return Err(IpcResponse::error(
                "spawn_subagent",
                "SKILL_NOT_FOUND",
                format!("skill [{skill_name}] not found in catalog"),
            ));
        }
        Err(e) => {
            return Err(IpcResponse::error(
                "spawn_subagent",
                "SKILL_LOOKUP_FAILED",
                format!("failed to look up skill [{skill_name}]: {e}"),
            ));
        }
    };
    if !record.validation_state.is_projectable() {
        let (state, _) = skill_state_label(&record.validation_state);
        return Err(IpcResponse::error(
            "spawn_subagent",
            "SKILL_RETIRED",
            format!("skill [{skill_name}] is {state} and cannot be spawned"),
        ));
    }

    // Goal: rendered template, caller goal appended as context.
    let mut goal = record.goal_template.clone().unwrap_or_default();
    for (key, value) in &delegation.skill_inputs {
        goal = goal.replace(&format!("{{{{{key}}}}}"), value);
    }
    let caller_goal = delegation.goal.trim().to_string();
    if goal.trim().is_empty() {
        goal = caller_goal.clone();
    } else if !caller_goal.is_empty() {
        goal = format!("{goal}\n\nAdditional context from the delegating agent: {caller_goal}");
    }
    if goal.trim().is_empty() {
        return Err(IpcResponse::error(
            "spawn_subagent",
            "SKILL_NO_GOAL",
            format!("skill [{skill_name}] has no goal template and no goal was provided"),
        ));
    }
    delegation.goal = goal;

    if let Some(kind) = record
        .subagent_kind
        .as_deref()
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
    {
        delegation.subagent_kind = kind.to_string();
    }

    // Tool bounds and dependency skills from the transitive DAG closure.
    let (resolved, diagnostics) =
        ansible_mesh_core::graph::resolve_transitive_skills(&[skill_name.clone()], |name| {
            graph.get_abstract_skill(name).ok().flatten()
        });
    if !diagnostics.is_empty() {
        warn!(
            skill_name = %skill_name,
            diagnostics = ?diagnostics,
            "SkillDAG resolution reported unresolvable edges during spawn-by-name"
        );
    }
    for dep_name in &resolved {
        if let Ok(Some(dep)) = graph.get_abstract_skill(dep_name) {
            if !dep.validation_state.is_projectable() {
                continue;
            }
            for tool in &dep.implied_tools {
                if !delegation.allowed_tools.contains(tool) {
                    delegation.allowed_tools.push(tool.clone());
                }
            }
        }
        if *dep_name != skill_name && !delegation.allowed_skills.contains(dep_name) {
            delegation.allowed_skills.push(dep_name.clone());
        }
    }

    info!(
        skill_name = %skill_name,
        subagent_kind = %delegation.subagent_kind,
        tool_count = delegation.allowed_tools.len(),
        "Resolved spawn-by-name delegation from skill catalog"
    );
    Ok(delegation)
}

/// Handle an `IpcRequest::RegisterSkill` at the IPC boundary.
///
/// This is the actual authorization boundary: `skill.register` writes an abstract
/// skill into the graph that can later project tools onto agents, so it must not be
/// "open to any agent" via a raw IPC request. Registration is rejected unless the
/// caller passes [`require_skill_admin`]. Every accepted registration is recorded
/// as an append-only audit graph event capturing who registered what and when;
/// re-registering an existing name is audited as `update`.
///
/// The SkillDAG edges (`allowed_skills`), tool classes, subagent kind, and goal
/// template are persisted on the record — registration is the DAG authoring
/// surface, not just a name+tools write.
///
/// Extracted as a free function so the auth, persist, and audit behavior can be
/// unit-tested without driving the full `process_request` connection loop.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(in crate::service) fn handle_register_skill(
    identity: Option<&GuestIdentity>,
    graph: &GraphDomain,
    skill_name: String,
    description: String,
    subagent_kind: String,
    goal: String,
    allowed_tools: Vec<String>,
    allowed_classes: Vec<String>,
    allowed_skills: Vec<String>,
) -> IpcResponse {
    handle_register_skill_with_origin(
        identity,
        graph,
        skill_name,
        description,
        subagent_kind,
        goal,
        allowed_tools,
        allowed_classes,
        allowed_skills,
        None,
    )
}

pub(in crate::service) fn handle_register_skill_with_origin(
    identity: Option<&GuestIdentity>,
    graph: &GraphDomain,
    skill_name: String,
    description: String,
    subagent_kind: String,
    goal: String,
    allowed_tools: Vec<String>,
    allowed_classes: Vec<String>,
    allowed_skills: Vec<String>,
    origin: Option<String>,
) -> IpcResponse {
    let identity =
        match require_skill_admin(identity, "register_skill", "REGISTER", "registering skills") {
            Ok(identity) => identity,
            Err(response) => return response,
        };

    // L5: the prompt safety floor. Runs before validation and before any
    // audit/persist so a Dangerous goal never enters the catalog in any state.
    let prompt_hazard =
        prompt_guard::detect_prompt_hazard_in([description.as_str(), goal.as_str()]);
    if let Some(hazard) = prompt_hazard.filter(|h| h.is_dangerous()) {
        warn!(
            skill_name = %skill_name,
            registered_by = %identity.guest_id,
            hazard = hazard.description,
            "skill.register rejected by prompt-guard"
        );
        // Audit the rejection (fail closed on audit failure, like acceptance).
        if let Err(response) = record_skill_admin_audit(
            graph,
            identity,
            "register_skill",
            "rejected",
            &skill_name,
            "rejected",
            Some(format!("prompt_guard:{}", hazard.description)),
        ) {
            return response;
        }
        return IpcResponse::error(
            "register_skill",
            "SKILL_PROMPT_HAZARD",
            hazard.denial_message(),
        );
    }
    let distill_origin = origin
        .as_deref()
        .filter(|o| *o == "distill" || o.starts_with("distill:"))
        .map(|o| o.to_string());

    // Translate to a SkillDraft and run Layer 1 structural validation.
    let draft = SkillDraft {
        skill_name: skill_name.clone(),
        description: description.clone(),
        subagent_kind: subagent_kind.clone(),
        goal_template: goal.clone(),
        allowed_tools: allowed_tools.clone(),
        allowed_skills: allowed_skills.clone(),
        iteration_budget: None,
        lease_terms: ansible_mesh_core::validation::SkillLeaseTerms::default(),
        hook_subscriptions: vec![],
        completion_route: ansible_mesh_core::validation::HookRoute::default(),
        failure_route: ansible_mesh_core::validation::HookRoute::default(),
        completion_contract: ansible_mesh_core::validation::SkillCompletionContract::default(),
        // An empty object satisfies the "must be a JSON object" invariant.
        field_sources: serde_json::json!({}),
    };

    let validation_result = validate_skill_layer1(&draft);

    // Re-registering an existing name is an update, and is audited as such.
    let action = match graph.get_abstract_skill(&skill_name) {
        Ok(Some(_)) => "update",
        _ => "register",
    };

    let mut record = AbstractSkillRecord {
        skill_name: skill_name.clone(),
        description,
        implied_tools: allowed_tools,
        implied_classes: allowed_classes,
        allowed_skills,
        subagent_kind: (!subagent_kind.is_empty()).then_some(subagent_kind),
        goal_template: (!goal.is_empty()).then_some(goal),
        source_snapshot: Some(SkillSourceSnapshot {
            mesh_catalog_version: String::new(),
            hotel_policy_version: String::new(),
            registered_at: unix_ts(),
            registered_by: identity.guest_id.clone(),
        }),
        ..Default::default()
    };
    apply_validation_to_record(&mut record, validation_result);

    // L5 Caution: keep the registration but make the flag visible wherever
    // the record is read (skill.list, philotic-web, the promotion card).
    let mut field_sources = record
        .field_sources
        .as_object()
        .cloned()
        .unwrap_or_default();
    if let Some(hazard) = prompt_hazard {
        field_sources.insert(
            "prompt_guard".into(),
            serde_json::Value::String(format!(
                "{}:{}",
                hazard.verdict.as_str(),
                hazard.description
            )),
        );
    }
    // L1: a distilled skill is a proposal, never a grant. Force Draft unless
    // Layer-1 already rejected it, and carry the trigger for the curator.
    if let Some(origin) = distill_origin.as_deref() {
        if !matches!(
            record.validation_state,
            SkillValidationState::Invalid { .. }
        ) {
            record.validation_state = SkillValidationState::Draft;
        }
        for marker in ["agent_authored", "distilled"] {
            if !record.skill_markers.iter().any(|m| m == marker) {
                record.skill_markers.push(marker.to_string());
            }
        }
        field_sources.insert(
            "origin".into(),
            serde_json::Value::String(origin.to_string()),
        );
        // `distill:<trigger>[:<source turn id>]`: the turn id joins the Draft
        // skill back to the turn that fired the review (and its decisions
        // shadow trace).
        if let Some(rest) = origin.strip_prefix("distill:") {
            let (trigger, source_turn_id) = match rest.split_once(':') {
                Some((trigger, turn)) => (trigger, Some(turn)),
                None => (rest, None),
            };
            field_sources.insert(
                "trigger".into(),
                serde_json::Value::String(trigger.to_string()),
            );
            if let Some(turn) = source_turn_id.filter(|t| !t.is_empty()) {
                field_sources.insert(
                    "source_turn_id".into(),
                    serde_json::Value::String(turn.to_string()),
                );
            }
        }
    }
    if !field_sources.is_empty() {
        record.field_sources = serde_json::Value::Object(field_sources);
    }

    let (state_str, errors) = skill_state_label(&record.validation_state);

    // Audit trail — who / what / when — written as an append-only graph event
    // BEFORE the skill is persisted, so no skill can enter the catalog without a
    // corresponding audit record (fail closed).
    if let Err(response) = record_skill_admin_audit(
        graph,
        identity,
        "register_skill",
        action,
        &skill_name,
        &state_str,
        None,
    ) {
        return response;
    }

    if let Err(e) = graph.upsert_abstract_skill(&record) {
        warn!("Failed to persist skill [{}]: {}", skill_name, e);
        return IpcResponse::error(
            "register_skill",
            "SKILL_PERSIST_FAILED",
            format!("Failed to persist skill: {e}"),
        );
    }

    info!(
        skill_name = %skill_name,
        validation_state = %state_str,
        registered_by = %identity.guest_id,
        registered_by_role = %identity.role,
        action = %action,
        "Skill registered via IPC"
    );

    IpcResponse::SkillRegistered {
        skill_name,
        validation_state: state_str,
        validation_errors: errors,
    }
}

/// Handle an `IpcRequest::SetSkillState` at the IPC boundary.
///
/// The orchestrator's lifecycle lever: `active` reinstates a skill,
/// `suspended`/`deprecated` administratively retire it — retired skills stop
/// contributing implied tools and guidance to session projection (see
/// `SkillValidationState::is_projectable`). Gated and audited like every other
/// skill administration op.
pub(in crate::service) fn handle_set_skill_state(
    identity: Option<&GuestIdentity>,
    graph: &GraphDomain,
    skill_name: String,
    state: String,
    reason: Option<String>,
) -> IpcResponse {
    let identity = match require_skill_admin(
        identity,
        "set_skill_state",
        "SET_SKILL_STATE",
        "changing skill state",
    ) {
        Ok(identity) => identity,
        Err(response) => return response,
    };

    let mut record = match graph.get_abstract_skill(&skill_name) {
        Ok(Some(record)) => record,
        Ok(None) => {
            return IpcResponse::error(
                "set_skill_state",
                "SKILL_NOT_FOUND",
                format!("skill [{skill_name}] not found in catalog"),
            );
        }
        Err(e) => {
            return IpcResponse::error(
                "set_skill_state",
                "SKILL_LOOKUP_FAILED",
                format!("failed to look up skill: {e}"),
            );
        }
    };

    let new_state = match state.as_str() {
        "active" => SkillValidationState::Active,
        "suspended" => SkillValidationState::Suspended {
            reason: reason.clone().unwrap_or_default(),
        },
        "deprecated" => SkillValidationState::Deprecated,
        other => {
            return IpcResponse::error(
                "set_skill_state",
                "SET_SKILL_STATE_INVALID",
                format!(
                    "unsupported skill state [{other}]; expected active, suspended, or deprecated"
                ),
            );
        }
    };
    record.validation_state = new_state;
    let (state_str, _) = skill_state_label(&record.validation_state);

    if let Err(response) = record_skill_admin_audit(
        graph,
        identity,
        "set_skill_state",
        "set_state",
        &skill_name,
        &state_str,
        reason,
    ) {
        return response;
    }

    if let Err(e) = graph.upsert_abstract_skill(&record) {
        warn!("Failed to persist skill state for [{}]: {}", skill_name, e);
        return IpcResponse::error(
            "set_skill_state",
            "SKILL_PERSIST_FAILED",
            format!("Failed to persist skill: {e}"),
        );
    }

    info!(
        skill_name = %skill_name,
        skill_state = %state_str,
        changed_by = %identity.guest_id,
        "Skill lifecycle state changed via IPC"
    );

    IpcResponse::SkillStateSet {
        skill_name,
        skill_state: state_str,
    }
}

impl IpcServer {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_register_skill_request(
        skill_name: String,
        description: String,
        subagent_kind: String,
        goal: String,
        allowed_tools: Vec<String>,
        allowed_classes: Vec<String>,
        allowed_skills: Vec<String>,
        origin: Option<String>,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        handle_register_skill_with_origin(
            current_identity.as_ref(),
            graph,
            skill_name,
            description,
            subagent_kind,
            goal,
            allowed_tools,
            allowed_classes,
            allowed_skills,
            origin,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_set_skill_state_request(
        skill_name: String,
        state: String,
        reason: Option<String>,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        handle_set_skill_state(current_identity.as_ref(), graph, skill_name, state, reason)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_list_skill_audits(
        skill_name: Option<String>,
        limit: Option<u32>,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        if let Err(response) = require_skill_admin(
            current_identity.as_ref(),
            "list_skill_audits",
            "LIST_SKILL_AUDITS",
            "reading the skill audit trail",
        ) {
            return response;
        }
        let audits = match graph.list_skill_registration_audits() {
            Ok(audits) => audits,
            Err(e) => {
                return IpcResponse::error(
                    "list_skill_audits",
                    "LIST_SKILL_AUDITS_FAILED",
                    format!("failed to list skill audits: {e}"),
                );
            }
        };
        let limit = limit.unwrap_or(100) as usize;
        let skill_audits: Vec<serde_json::Value> = audits
            .iter()
            .filter(|audit| {
                skill_name
                    .as_deref()
                    .is_none_or(|name| audit.skill_name == name)
            })
            .rev()
            .take(limit)
            .map(|audit| {
                serde_json::json!({
                    "audit_id": audit.audit_id,
                    "skill_name": audit.skill_name,
                    "action": audit.action,
                    "by": audit.registered_by,
                    "by_role": audit.registered_by_role,
                    "validation_state": audit.validation_state,
                    "at": audit.registered_at,
                    "detail": audit.detail,
                })
            })
            .collect();
        IpcResponse::SkillAuditList { skill_audits }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_assign_skill(
        agent_id: String,
        role_name: String,
        skill_name: String,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        let identity = match require_skill_admin(
            current_identity.as_ref(),
            "assign_skill",
            "ASSIGN",
            "assigning skills",
        ) {
            Ok(identity) => identity,
            Err(response) => return response,
        };
        let is_management = identity.role == "management";
        if !is_management && !guest_owns_agent(&identity.guest_id, &agent_id) {
            return IpcResponse::error(
                "assign_skill",
                "ASSIGN_FORBIDDEN",
                "orchestrator guests may only assign skills for their own agent identity",
            );
        }
        // Verify the skill exists in the catalog.
        match graph.get_abstract_skill(&skill_name) {
            Ok(None) => {
                return IpcResponse::error(
                    "assign_skill",
                    "SKILL_NOT_FOUND",
                    format!("skill [{}] not found in catalog", skill_name),
                );
            }
            Err(e) => {
                return IpcResponse::error(
                    "assign_skill",
                    "SKILL_LOOKUP_FAILED",
                    format!("failed to look up skill: {e}"),
                );
            }
            Ok(Some(_)) => {}
        }
        // Load the role incarnation record.
        let role_record = match graph.get_role_incarnation(&agent_id, &role_name) {
            Ok(Some(r)) => r,
            Ok(None) => {
                return IpcResponse::error(
                    "assign_skill",
                    "ROLE_NOT_FOUND",
                    format!(
                        "role [{}] not configured for agent [{}]",
                        role_name, agent_id
                    ),
                );
            }
            Err(e) => {
                return IpcResponse::error(
                    "assign_skill",
                    "ROLE_LOOKUP_FAILED",
                    format!("failed to look up role: {e}"),
                );
            }
        };
        // Load the toolset profile.
        let mut profile = match graph.get_toolset_profile(&role_record.toolset_profile) {
            Ok(Some(p)) => p,
            Ok(None) => {
                return IpcResponse::error(
                    "assign_skill",
                    "PROFILE_NOT_FOUND",
                    format!(
                        "toolset profile [{}] not found",
                        role_record.toolset_profile
                    ),
                );
            }
            Err(e) => {
                return IpcResponse::error(
                    "assign_skill",
                    "PROFILE_LOOKUP_FAILED",
                    format!("failed to look up toolset profile: {e}"),
                );
            }
        };
        // Idempotent: if already assigned, return success.
        if !profile.allowed_skills.contains(&skill_name) {
            // Fail-closed audit before the mutation.
            if let Err(response) = record_skill_admin_audit(
                graph,
                identity,
                "assign_skill",
                "assign",
                &skill_name,
                "",
                Some(format!(
                    "agent={agent_id} role={role_name} profile={}",
                    profile.profile_name
                )),
            ) {
                return response;
            }
            profile.allowed_skills.push(skill_name.clone());
            if let Err(e) = graph.upsert_toolset_profile(&profile) {
                return IpcResponse::error(
                    "assign_skill",
                    "PROFILE_PERSIST_FAILED",
                    format!("failed to persist toolset profile: {e}"),
                );
            }
        }
        info!(role_name = %role_name, skill_name = %skill_name, "Skill assigned to role via IPC");
        IpcResponse::SkillAssigned {
            role_name,
            skill_name,
            operation: "assigned".into(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_revoke_skill(
        agent_id: String,
        role_name: String,
        skill_name: String,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        let identity = match require_skill_admin(
            current_identity.as_ref(),
            "revoke_skill",
            "REVOKE",
            "revoking skills",
        ) {
            Ok(identity) => identity,
            Err(response) => return response,
        };
        let is_management = identity.role == "management";
        if !is_management && !guest_owns_agent(&identity.guest_id, &agent_id) {
            return IpcResponse::error(
                "revoke_skill",
                "REVOKE_FORBIDDEN",
                "orchestrator guests may only revoke skills for their own agent identity",
            );
        }
        // Load the role incarnation record.
        let role_record = match graph.get_role_incarnation(&agent_id, &role_name) {
            Ok(Some(r)) => r,
            Ok(None) => {
                return IpcResponse::error(
                    "revoke_skill",
                    "ROLE_NOT_FOUND",
                    format!(
                        "role [{}] not configured for agent [{}]",
                        role_name, agent_id
                    ),
                );
            }
            Err(e) => {
                return IpcResponse::error(
                    "revoke_skill",
                    "ROLE_LOOKUP_FAILED",
                    format!("failed to look up role: {e}"),
                );
            }
        };
        // Load the toolset profile.
        let mut profile = match graph.get_toolset_profile(&role_record.toolset_profile) {
            Ok(Some(p)) => p,
            Ok(None) => {
                return IpcResponse::error(
                    "revoke_skill",
                    "PROFILE_NOT_FOUND",
                    format!(
                        "toolset profile [{}] not found",
                        role_record.toolset_profile
                    ),
                );
            }
            Err(e) => {
                return IpcResponse::error(
                    "revoke_skill",
                    "PROFILE_LOOKUP_FAILED",
                    format!("failed to look up toolset profile: {e}"),
                );
            }
        };
        // Idempotent: if not present, return success.
        if profile.allowed_skills.contains(&skill_name) {
            // Fail-closed audit before the mutation.
            if let Err(response) = record_skill_admin_audit(
                graph,
                identity,
                "revoke_skill",
                "revoke",
                &skill_name,
                "",
                Some(format!(
                    "agent={agent_id} role={role_name} profile={}",
                    profile.profile_name
                )),
            ) {
                return response;
            }
            profile.allowed_skills.retain(|s| s != &skill_name);
            if let Err(e) = graph.upsert_toolset_profile(&profile) {
                return IpcResponse::error(
                    "revoke_skill",
                    "PROFILE_PERSIST_FAILED",
                    format!("failed to persist toolset profile: {e}"),
                );
            }
        }
        info!(role_name = %role_name, skill_name = %skill_name, "Skill revoked from role via IPC");
        IpcResponse::SkillAssigned {
            role_name,
            skill_name,
            operation: "revoked".into(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_list_skills(
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        // The catalog names every tool a skill can project; require at
        // least a registered guest identity before enumerating it.
        if current_identity.is_none() {
            return IpcResponse::error(
                "list_skills",
                "LIST_SKILLS_UNREGISTERED",
                "guest must register before listing skills",
            );
        }
        let skills = match graph.list_abstract_skills() {
            Ok(s) => s,
            Err(e) => {
                return IpcResponse::error(
                    "list_skills",
                    "LIST_SKILLS_FAILED",
                    format!("failed to list skills: {e}"),
                );
            }
        };
        let json_skills: Vec<serde_json::Value> = skills
            .iter()
            .map(|s| {
                let (state_str, _) = skill_state_label(&s.validation_state);
                serde_json::json!({
                    "skill_name": s.skill_name,
                    "description": s.description,
                    "implied_tools": s.implied_tools,
                    "implied_classes": s.implied_classes,
                    "allowed_skills": s.allowed_skills,
                    "subagent_kind": s.subagent_kind,
                    "goal_template": s.goal_template,
                    "validation_state": state_str,
                })
            })
            .collect();
        IpcResponse::SkillList {
            skills: json_skills,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_get_toolset_profile(
        profile_name: String,
        graph: &GraphDomain,
    ) -> IpcResponse {
        match graph.get_toolset_profile(&profile_name) {
            Ok(Some(p)) => IpcResponse::success(
                "toolset_profile",
                Some(serde_json::to_value(&p).unwrap_or(serde_json::Value::Null)),
            ),
            Ok(None) => IpcResponse::success("toolset_profile", None),
            Err(e) => IpcResponse::error("toolset_profile", "PROFILE_ERROR", e.to_string()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_list_toolset_profiles(graph: &GraphDomain) -> IpcResponse {
        match graph.list_toolset_profiles() {
            Ok(profiles) => IpcResponse::success(
                "list_toolset_profiles",
                Some(serde_json::to_value(&profiles).unwrap_or(serde_json::Value::Array(vec![]))),
            ),
            Err(e) => IpcResponse::error("list_toolset_profiles", "PROFILES_ERROR", e.to_string()),
        }
    }
}
