//! Procedure registry: register, patch propose/decide, trial evaluation.
//!
//! Moved verbatim from `ipc/mod.rs` (IPC_DISPATCH_SPLIT); only item
//! visibility was widened so the parent module can reach it.

use super::*;

/// [`handle_register_skill`] with the registration's `origin`.
///
/// Two Self-Improvement Loop gates live here, hotel-side so they hold for
/// every IPC caller and not only the philote tool path:
///
/// - **L5 prompt-guard.** `description` and `goal` are text that will be
///   rendered into future worker prompts. A `Dangerous` verdict rejects the
///   registration outright (audited as `rejected`); a `Caution` verdict is
///   recorded in `field_sources.prompt_guard` so the operator sees it when
///   promoting the skill.
/// - **L1 distill origin.** `origin = Some("distill[:<trigger>]")` marks a
///   registration produced by a distill whisper. Such records are forced to
///   `Draft` (unless Layer-1 validation already made them `Invalid`) and
///   tagged `agent_authored` / `distilled`, with the trigger preserved in
///   `field_sources`. A Draft grants nothing until an operator promotes it.
#[allow(clippy::too_many_arguments)]
pub(super) fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Procedural graphs P4: `procedure.patch`. Any registered guest may file a
/// patch — it lands `Pending` and nothing runs until an operator approves —
/// but the ops are dry-run against the current version and every text
/// field passes the L5 prompt-guard first, so a patch that cannot apply or
/// carries hazard text never enters the queue.
pub(in crate::service) fn handle_propose_procedure_patch(
    identity: Option<&GuestIdentity>,
    graph: &GraphDomain,
    procedure_id: String,
    ops: serde_json::Value,
    rationale: String,
    evidence_run_ids: Vec<String>,
    origin: Option<String>,
) -> IpcResponse {
    let Some(identity) = identity else {
        return IpcResponse::error(
            "propose_procedure_patch",
            "PROCEDURE_PATCH_UNREGISTERED",
            "guest must register before proposing procedure patches",
        );
    };
    let ops: Vec<ProcedurePatchOp> = match serde_json::from_value(ops) {
        Ok(ops) => ops,
        Err(e) => {
            return IpcResponse::error(
                "propose_procedure_patch",
                "PROCEDURE_PATCH_INVALID",
                format!("malformed ops: {e}"),
            );
        }
    };
    let current = match graph.get_procedure(&procedure_id) {
        Ok(Some(p)) => p,
        Ok(None) => {
            return IpcResponse::error(
                "propose_procedure_patch",
                "PROCEDURE_NOT_FOUND",
                format!("no procedure named {procedure_id}"),
            );
        }
        Err(e) => {
            return IpcResponse::error("propose_procedure_patch", "PROCEDURE_ERROR", e.to_string());
        }
    };
    if current.trial_of.is_some() {
        return IpcResponse::error(
            "propose_procedure_patch",
            "PROCEDURE_ON_TRIAL",
            format!(
                "{procedure_id} v{} is a candidate under trial; wait for the trial to decide",
                current.version
            ),
        );
    }
    let candidate = match current.apply_patch(&ops) {
        Ok(c) => c,
        Err(errors) => {
            return IpcResponse::error(
                "propose_procedure_patch",
                "PROCEDURE_PATCH_INVALID",
                errors.join("; "),
            );
        }
    };
    let patch = ProcedurePatchRecord {
        patch_id: uuid::Uuid::new_v4().to_string(),
        procedure_id: procedure_id.clone(),
        base_version: current.version,
        candidate_version: None,
        ops,
        rationale: rationale.trim().chars().take(600).collect(),
        evidence_run_ids,
        proposed_by: identity.guest_id.clone(),
        status: ProcedurePatchStatus::Pending,
        base_snapshot: None,
        trial: None,
        rejection_reason: None,
        created_at: unix_now_secs(),
        decided_at: None,
    };
    if let Some(hazard) =
        prompt_guard::detect_prompt_hazard_in(patch.text_fields()).filter(|h| h.is_dangerous())
    {
        warn!(
            procedure_id = %procedure_id,
            proposed_by = %identity.guest_id,
            hazard = hazard.description,
            "procedure.patch rejected by prompt-guard"
        );
        if let Err(response) = record_skill_admin_audit(
            graph,
            identity,
            "propose_procedure_patch",
            "rejected",
            &procedure_id,
            "rejected",
            Some(format!("prompt_guard:{}", hazard.description)),
        ) {
            return response;
        }
        return IpcResponse::error(
            "propose_procedure_patch",
            "PROCEDURE_PROMPT_HAZARD",
            hazard.denial_message(),
        );
    }
    if let Err(e) = graph.upsert_procedure_patch(&patch) {
        return IpcResponse::error(
            "propose_procedure_patch",
            "PROCEDURE_PATCH_ERROR",
            e.to_string(),
        );
    }
    if let Err(response) = record_skill_admin_audit(
        graph,
        identity,
        "propose_procedure_patch",
        "accepted",
        &procedure_id,
        "pending",
        Some(format!(
            "patch {} base v{} → candidate v{} ({} ops, origin {})",
            patch.patch_id,
            patch.base_version,
            candidate.version,
            patch.ops.len(),
            origin.as_deref().unwrap_or("agent")
        )),
    ) {
        return response;
    }
    IpcResponse::success(
        "propose_procedure_patch",
        Some(serde_json::json!({
            "patch_id": patch.patch_id,
            "procedure_id": procedure_id,
            "base_version": patch.base_version,
            "status": "pending",
            "ops": patch.ops.len(),
            "summary": patch.summary(),
        })),
    )
}

/// Procedural graphs P4: the operator's decision on a `Pending` patch.
/// `approve` re-applies the ops to the current version (re-validated), stores
/// the pre-approval record as the revert snapshot, projects the candidate as
/// `v+1` with `trial_of` set, and opens the trial window. `reject` keeps the
/// patch as negative evidence. Skill-admin gated like `skill.set_state`.
pub(in crate::service) fn handle_decide_procedure_patch(
    identity: Option<&GuestIdentity>,
    graph: &GraphDomain,
    patch_id: String,
    decision: String,
    reason: Option<String>,
) -> IpcResponse {
    let identity = match require_skill_admin(
        identity,
        "decide_procedure_patch",
        "DECIDE_PROCEDURE_PATCH",
        "deciding procedure patches",
    ) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let mut patch = match graph.get_procedure_patch(&patch_id) {
        Ok(Some(p)) => p,
        Ok(None) => {
            return IpcResponse::error(
                "decide_procedure_patch",
                "PROCEDURE_PATCH_NOT_FOUND",
                format!("no patch {patch_id}"),
            );
        }
        Err(e) => {
            return IpcResponse::error(
                "decide_procedure_patch",
                "PROCEDURE_PATCH_ERROR",
                e.to_string(),
            );
        }
    };
    if patch.status != ProcedurePatchStatus::Pending {
        return IpcResponse::error(
            "decide_procedure_patch",
            "PROCEDURE_PATCH_NOT_PENDING",
            format!("patch {patch_id} is {}", patch.status.as_str()),
        );
    }
    let now = unix_now_secs();
    match decision.trim() {
        "reject" => {
            patch.status = ProcedurePatchStatus::Rejected;
            patch.rejection_reason = Some(
                reason
                    .filter(|r| !r.trim().is_empty())
                    .unwrap_or_else(|| "rejected by operator".to_string()),
            );
            patch.decided_at = Some(now);
            if let Err(e) = graph.upsert_procedure_patch(&patch) {
                return IpcResponse::error(
                    "decide_procedure_patch",
                    "PROCEDURE_PATCH_ERROR",
                    e.to_string(),
                );
            }
            if let Err(response) = record_skill_admin_audit(
                graph,
                identity,
                "decide_procedure_patch",
                "rejected",
                &patch.procedure_id,
                "rejected",
                Some(format!(
                    "patch {patch_id}: {}",
                    patch.rejection_reason.clone().unwrap_or_default()
                )),
            ) {
                return response;
            }
            IpcResponse::success(
                "decide_procedure_patch",
                Some(serde_json::json!({
                    "patch_id": patch_id,
                    "procedure_id": patch.procedure_id,
                    "status": "rejected",
                })),
            )
        }
        "approve" => {
            let current = match graph.get_procedure(&patch.procedure_id) {
                Ok(Some(p)) => p,
                Ok(None) => {
                    return IpcResponse::error(
                        "decide_procedure_patch",
                        "PROCEDURE_NOT_FOUND",
                        format!("no procedure named {}", patch.procedure_id),
                    );
                }
                Err(e) => {
                    return IpcResponse::error(
                        "decide_procedure_patch",
                        "PROCEDURE_ERROR",
                        e.to_string(),
                    );
                }
            };
            if current.trial_of.is_some() {
                return IpcResponse::error(
                    "decide_procedure_patch",
                    "PROCEDURE_ON_TRIAL",
                    format!(
                        "{} v{} is already a candidate under trial",
                        patch.procedure_id, current.version
                    ),
                );
            }
            let mut candidate = match current.apply_patch(&patch.ops) {
                Ok(c) => c,
                Err(errors) => {
                    // The graph moved under the patch; keep it, but say why.
                    return IpcResponse::error(
                        "decide_procedure_patch",
                        "PROCEDURE_PATCH_INVALID",
                        format!(
                            "patch no longer applies to v{}: {}",
                            current.version,
                            errors.join("; ")
                        ),
                    );
                }
            };
            candidate.trial_of = Some(patch_id.clone());
            candidate.provenance = ProcedureProvenance::Refiner;
            candidate.updated_at = now;
            if let Err(e) = graph.upsert_procedure(&candidate) {
                return IpcResponse::error(
                    "decide_procedure_patch",
                    "PROCEDURE_ERROR",
                    e.to_string(),
                );
            }
            patch.status = ProcedurePatchStatus::Trial;
            patch.candidate_version = Some(candidate.version);
            patch.base_snapshot = Some(current);
            patch.trial = Some(TrialWindow {
                started_at: now,
                candidate_version: candidate.version,
                required_runs: trial_runs_required(),
                ..Default::default()
            });
            if let Err(e) = graph.upsert_procedure_patch(&patch) {
                return IpcResponse::error(
                    "decide_procedure_patch",
                    "PROCEDURE_PATCH_ERROR",
                    e.to_string(),
                );
            }
            if let Err(response) = record_skill_admin_audit(
                graph,
                identity,
                "decide_procedure_patch",
                "accepted",
                &patch.procedure_id,
                "trial",
                Some(format!(
                    "patch {patch_id} approved into v{} trial ({} runs)",
                    candidate.version,
                    trial_runs_required()
                )),
            ) {
                return response;
            }
            IpcResponse::success(
                "decide_procedure_patch",
                Some(serde_json::json!({
                    "patch_id": patch_id,
                    "procedure_id": patch.procedure_id,
                    "status": "trial",
                    "candidate_version": candidate.version,
                    "required_runs": trial_runs_required(),
                })),
            )
        }
        other => IpcResponse::error(
            "decide_procedure_patch",
            "PROCEDURE_PATCH_INVALID",
            format!("decision must be approve or reject, got {other:?}"),
        ),
    }
}

/// Procedural graphs P4: close any trial window for `procedure_id` whose
/// candidate has enough runs. Accept keeps the candidate version and clears
/// its trial marker; reject reverts to the pre-approval snapshot and keeps
/// the patch as `Rejected` with both scores in the reason. Returns one
/// `{patch_id, accepted}` per decided patch; storage errors are logged, never
/// surfaced — a run record must not fail because a trial could not close.
pub(in crate::service) fn evaluate_procedure_trials(
    graph: &GraphDomain,
    procedure_id: &str,
) -> Vec<serde_json::Value> {
    let mut decided = Vec::new();
    let trials =
        match graph.list_procedure_patches(Some(procedure_id), Some(ProcedurePatchStatus::Trial)) {
            Ok(t) => t,
            Err(e) => {
                warn!(procedure_id, error = %e, "trial evaluation: cannot list patches");
                return decided;
            }
        };
    for mut patch in trials {
        let Some(candidate_version) = patch.candidate_version else {
            continue;
        };
        let required = patch
            .trial
            .as_ref()
            .map(|t| t.required_runs)
            .filter(|k| *k > 0)
            .unwrap_or_else(trial_runs_required);
        let candidate_runs =
            match graph.list_procedure_runs(procedure_id, Some(candidate_version), required) {
                Ok(r) => r,
                Err(e) => {
                    warn!(procedure_id, error = %e, "trial evaluation: cannot list candidate runs");
                    continue;
                }
            };
        let baseline_runs =
            match graph.list_procedure_runs(procedure_id, Some(patch.base_version), required) {
                Ok(r) => r,
                Err(e) => {
                    warn!(procedure_id, error = %e, "trial evaluation: cannot list baseline runs");
                    continue;
                }
            };
        let candidate_scores: Vec<f32> = candidate_runs.iter().map(|r| r.score).collect();
        let baseline_scores: Vec<f32> = baseline_runs.iter().map(|r| r.score).collect();
        let decision = decide_trial(&candidate_scores, &baseline_scores, required);
        let TrialDecision::Decided {
            accept,
            candidate_n,
            candidate_mean,
            baseline_n,
            baseline_mean,
        } = decision
        else {
            continue;
        };
        let now = unix_now_secs();
        if let Some(trial) = patch.trial.as_mut() {
            trial.candidate_n = candidate_n;
            trial.candidate_mean = candidate_mean;
            trial.baseline_n = baseline_n;
            trial.baseline_mean = baseline_mean;
        }
        patch.decided_at = Some(now);
        let outcome = if accept {
            match graph.get_procedure(procedure_id) {
                Ok(Some(mut current)) if current.version == candidate_version => {
                    current.trial_of = None;
                    current.updated_at = now;
                    if let Err(e) = graph.upsert_procedure(&current) {
                        warn!(procedure_id, error = %e, "trial accept: cannot clear trial marker");
                        continue;
                    }
                }
                Ok(_) => {
                    warn!(
                        procedure_id,
                        candidate_version,
                        "trial accept: candidate is no longer the current version"
                    );
                }
                Err(e) => {
                    warn!(procedure_id, error = %e, "trial accept: cannot read procedure");
                    continue;
                }
            }
            patch.status = ProcedurePatchStatus::Accepted;
            "accepted"
        } else {
            match patch.base_snapshot.clone() {
                Some(mut base) => {
                    base.updated_at = now;
                    if let Err(e) = graph.upsert_procedure(&base) {
                        warn!(procedure_id, error = %e, "trial reject: cannot revert to base snapshot");
                        continue;
                    }
                }
                None => {
                    warn!(procedure_id, patch_id = %patch.patch_id, "trial reject: no base snapshot to revert to");
                }
            }
            patch.status = ProcedurePatchStatus::Rejected;
            patch.rejection_reason = Some(format!(
                "trial: candidate v{candidate_version} mean {candidate_mean:.2} over {candidate_n} run(s) < baseline v{} mean {baseline_mean:.2} over {baseline_n} run(s)",
                patch.base_version
            ));
            "rejected"
        };
        if let Err(e) = graph.upsert_procedure_patch(&patch) {
            warn!(procedure_id, error = %e, "trial evaluation: cannot store the decision");
            continue;
        }
        info!(
            procedure_id,
            patch_id = %patch.patch_id,
            outcome,
            candidate_version,
            candidate_mean,
            baseline_mean,
            "procedure trial decided"
        );
        decided.push(serde_json::json!({
            "patch_id": patch.patch_id,
            "accepted": accept,
            "candidate_version": candidate_version,
            "candidate_mean": candidate_mean,
            "baseline_mean": baseline_mean,
        }));
    }
    decided
}

/// Procedural graphs (doc:procedural-graphs P0): `procedure.register`.
///
/// Gated exactly like `skill.register`: a skill-admin identity, the L5
/// prompt-guard over every prompt-facing field (a `dangerous` verdict
/// rejects, a `caution` verdict lands the record in `Draft` regardless of the
/// requested state), mechanical validation, and a `skill_registration_audit`
/// row under op `register_procedure`. Agent and distill origins are forced
/// to `Draft` with `Agent` provenance so nothing an agent authored projects
/// before an operator promotes it. Re-registering an existing id bumps the
/// version and clears any trial marker.
pub(in crate::service) fn handle_register_procedure(
    identity: Option<&GuestIdentity>,
    graph: &GraphDomain,
    procedure: serde_json::Value,
    origin: Option<String>,
) -> IpcResponse {
    let identity = match require_skill_admin(
        identity,
        "register_procedure",
        "REGISTER_PROCEDURE",
        "registering procedures",
    ) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let mut record: ProcedureGraphRecord = match serde_json::from_value(procedure) {
        Ok(r) => r,
        Err(e) => {
            return IpcResponse::error(
                "register_procedure",
                "PROCEDURE_INVALID",
                format!("malformed procedure record: {e}"),
            );
        }
    };
    if let Err(errors) = record.validate() {
        return IpcResponse::error("register_procedure", "PROCEDURE_INVALID", errors.join("; "));
    }
    let hazard = prompt_guard::detect_prompt_hazard_in(record.text_fields());
    if let Some(hazard) = hazard.as_ref().filter(|h| h.is_dangerous()) {
        warn!(
            procedure_id = %record.procedure_id,
            registered_by = %identity.guest_id,
            hazard = hazard.description,
            "procedure.register rejected by prompt-guard"
        );
        if let Err(response) = record_skill_admin_audit(
            graph,
            identity,
            "register_procedure",
            "rejected",
            &record.procedure_id,
            "rejected",
            Some(format!("prompt_guard:{}", hazard.description)),
        ) {
            return response;
        }
        return IpcResponse::error(
            "register_procedure",
            "PROCEDURE_PROMPT_HAZARD",
            hazard.denial_message(),
        );
    }
    let agent_origin = origin
        .as_deref()
        .is_some_and(|o| o == "agent" || o.starts_with("distill"));
    if agent_origin {
        record.validation_state = SkillValidationState::Draft;
        record.provenance = ProcedureProvenance::Agent {
            agent_id: identity.guest_id.clone(),
        };
    } else {
        // Over the wire there are two authors: an agent (above) or the
        // operator. `Repo` is minted only by the boot seed and `Refiner` only
        // by the P4 gate, so neither can be claimed here — a wire record that
        // claimed `Repo` would be silently clobbered by the next seed.
        record.provenance = ProcedureProvenance::Operator;
    }
    if hazard.is_some() {
        record.validation_state = SkillValidationState::Draft;
    }
    match graph.get_procedure(&record.procedure_id) {
        Ok(Some(existing)) => {
            if record.version <= existing.version {
                record.version = existing.version + 1;
            }
        }
        Ok(None) => {}
        Err(e) => {
            return IpcResponse::error("register_procedure", "PROCEDURE_ERROR", e.to_string());
        }
    }
    record.trial_of = None;
    record.updated_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Err(e) = graph.upsert_procedure(&record) {
        return IpcResponse::error("register_procedure", "PROCEDURE_ERROR", e.to_string());
    }
    let (state_label, _) = skill_state_label(&record.validation_state);
    if let Err(response) = record_skill_admin_audit(
        graph,
        identity,
        "register_procedure",
        "accepted",
        &record.procedure_id,
        state_label.as_str(),
        Some(format!(
            "version {} origin {}",
            record.version,
            origin.as_deref().unwrap_or("operator")
        )),
    ) {
        return response;
    }
    IpcResponse::success(
        "register_procedure",
        Some(serde_json::json!({
            "procedure_id": record.procedure_id,
            "version": record.version,
            "validation_state": state_label,
            "nodes": record.nodes.len(),
            "edges": record.edges.len(),
        })),
    )
}

impl IpcServer {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_register_procedure_request(
        procedure: serde_json::Value,
        origin: Option<String>,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        handle_register_procedure(current_identity.as_ref(), graph, procedure, origin)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_get_procedure(procedure_id: String, graph: &GraphDomain) -> IpcResponse {
        match graph.get_procedure(&procedure_id) {
            Ok(Some(p)) => IpcResponse::success(
                "get_procedure",
                Some(serde_json::to_value(&p).unwrap_or(serde_json::Value::Null)),
            ),
            Ok(None) => IpcResponse::error(
                "get_procedure",
                "PROCEDURE_NOT_FOUND",
                format!("no procedure named {procedure_id}"),
            ),
            Err(e) => IpcResponse::error("get_procedure", "PROCEDURE_ERROR", e.to_string()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_list_procedures(graph: &GraphDomain) -> IpcResponse {
        match graph.list_procedures() {
            Ok(list) => IpcResponse::success(
                "list_procedures",
                Some(serde_json::json!({ "procedures": list })),
            ),
            Err(e) => IpcResponse::error("list_procedures", "PROCEDURE_ERROR", e.to_string()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_record_procedure_run(
        run: serde_json::Value,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        // The ledger is the refiner's evidence and the trial gate's
        // score source; an unregistered peer must not be able to
        // write either.
        let Some(identity) = current_identity.as_ref() else {
            return IpcResponse::error(
                "record_procedure_run",
                "PROCEDURE_RUN_UNREGISTERED",
                "guest must register before recording procedure runs",
            );
        };
        let mut run: ProcedureRunRecord = match serde_json::from_value(run) {
            Ok(r) => r,
            Err(e) => {
                return IpcResponse::error(
                    "record_procedure_run",
                    "PROCEDURE_RUN_INVALID",
                    format!("malformed run record: {e}"),
                );
            }
        };
        if run.run_id.trim().is_empty() || run.procedure_id.trim().is_empty() {
            return IpcResponse::error(
                "record_procedure_run",
                "PROCEDURE_RUN_INVALID",
                "run_id and procedure_id are required",
            );
        }
        if run.agent_id.trim().is_empty() {
            run.agent_id = identity.guest_id.clone();
        }
        if run.recorded_at == 0 {
            run.recorded_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
        }
        // The score is derived, never trusted from the wire.
        run.score = ProcedureRunRecord::score_for(&run.verdict, &run.basis);
        match graph.record_procedure_run(&run) {
            Ok(()) => {
                // P4: every run may close a trial window.
                let trials = evaluate_procedure_trials(graph, &run.procedure_id);
                IpcResponse::success(
                    "record_procedure_run",
                    Some(serde_json::json!({
                        "run_id": run.run_id,
                        "procedure_id": run.procedure_id,
                        "graph_version": run.graph_version,
                        "score": run.score,
                        "trials_decided": trials,
                    })),
                )
            }
            Err(e) => {
                IpcResponse::error("record_procedure_run", "PROCEDURE_RUN_ERROR", e.to_string())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_list_procedure_runs(
        procedure_id: String,
        graph_version: Option<u32>,
        limit: Option<usize>,
        graph: &GraphDomain,
    ) -> IpcResponse {
        match graph.list_procedure_runs(
            &procedure_id,
            graph_version,
            limit.unwrap_or(20).clamp(1, 200),
        ) {
            Ok(runs) => IpcResponse::success(
                "list_procedure_runs",
                Some(serde_json::json!({ "procedure_id": procedure_id, "runs": runs })),
            ),
            Err(e) => {
                IpcResponse::error("list_procedure_runs", "PROCEDURE_RUN_ERROR", e.to_string())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_propose_procedure_patch_request(
        procedure_id: String,
        ops: serde_json::Value,
        rationale: String,
        evidence_run_ids: Vec<String>,
        origin: Option<String>,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        handle_propose_procedure_patch(
            current_identity.as_ref(),
            graph,
            procedure_id,
            ops,
            rationale,
            evidence_run_ids,
            origin,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_list_procedure_patches(
        procedure_id: Option<String>,
        status: Option<String>,
        graph: &GraphDomain,
    ) -> IpcResponse {
        let status = match status.as_deref() {
            None => None,
            Some("pending") => Some(ProcedurePatchStatus::Pending),
            Some("trial") => Some(ProcedurePatchStatus::Trial),
            Some("accepted") => Some(ProcedurePatchStatus::Accepted),
            Some("rejected") => Some(ProcedurePatchStatus::Rejected),
            Some(other) => {
                return IpcResponse::error(
                    "list_procedure_patches",
                    "PROCEDURE_PATCH_INVALID",
                    format!("unknown status filter {other:?}"),
                );
            }
        };
        match graph.list_procedure_patches(procedure_id.as_deref(), status) {
            Ok(patches) => IpcResponse::success(
                "list_procedure_patches",
                Some(serde_json::json!({ "patches": patches })),
            ),
            Err(e) => IpcResponse::error(
                "list_procedure_patches",
                "PROCEDURE_PATCH_ERROR",
                e.to_string(),
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_decide_procedure_patch_request(
        patch_id: String,
        decision: String,
        reason: Option<String>,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        handle_decide_procedure_patch(current_identity.as_ref(), graph, patch_id, decision, reason)
    }
}
