//! Cron access control: ownership scoping, visibility and mutation gates.
//!
//! Moved verbatim from `ipc/mod.rs` (IPC_DISPATCH_SPLIT); only item
//! visibility was widened so the parent module can reach it.

use super::*;
use ansible_mesh_core::cron::{CronJob, CronJobId, CronTurnPolicy};

// ── Cron ownership scoping ───────────────────────────────────────────────────
//
// Every role holds the cron tools (operator decision 2026-09-04: "open to
// everyone — but maybe only their crontabs"). The hotel scopes them: a guest
// sees and mutates only jobs its AGENT owns; orchestrator/management and the
// operator surfaces (philotic-web, CLI) see the whole hotel.

/// The agent that owns a guest: `agent-x` or `agent-x:role` → `agent-x`.
pub(in crate::service) fn cron_owner_agent_of_guest(guest_id: &str) -> &str {
    guest_id.split_once(':').map(|(a, _)| a).unwrap_or(guest_id)
}

/// Identities that see the whole hotel crontab: skill-admin roles
/// (orchestrator/management, bare or `role:{agent}:`-scoped) and the operator
/// surfaces. An unregistered connection is treated as the operator's own
/// process (the CLI/socket path that predates guest identities).
pub(in crate::service) fn cron_admin_identity(identity: &GuestIdentity) -> bool {
    skill_admin_role(&identity.role)
        || matches!(
            identity.role.as_str(),
            "operator" | "cli" | "philotic-web" | "desktop" | "membrane"
        )
}

/// Who may set a cron job's turn policy (tools, preapprovals, approval mode)?
/// Only the operator or a surface acting for the operator (operator decision
/// 2026-10-04). Deliberately STRICTER than [`cron_admin_identity`]: an
/// orchestrator/management role INCARNATION (`role:{agent}:orchestrator`) is
/// still an agent and must not grant its own fires tools or approvals. Bare
/// `management` is the identity philotic-web and the `phil` CLI connect as.
/// An unregistered connection is the operator's own socket process.
pub(in crate::service) fn cron_policy_authority(identity: Option<&GuestIdentity>) -> bool {
    match identity {
        None => true,
        Some(id) => matches!(
            id.role.as_str(),
            "operator" | "cli" | "philotic-web" | "desktop" | "management"
        ),
    }
}

/// Task keys only the CronTicker may set. A guest-emitted task carrying any of
/// them is forging a cron fire — `cron_policy` / `cron_preapproved_tools`
/// would otherwise let any guest grant a philote's turn standing tool
/// approval. The ticker delivers straight to inboxes, never via `EmitTask`,
/// so stripping these from every `EmitTask` frame loses nothing legitimate.
pub(super) const CRON_TICKER_ONLY_TASK_KEYS: [&str; 3] =
    ["cron_job_id", "cron_policy", "cron_preapproved_tools"];

/// Remove [`CRON_TICKER_ONLY_TASK_KEYS`] from a guest-emitted task. Returns the
/// task unchanged (same string) when nothing was forged or it is not a JSON
/// object.
pub(in crate::service) fn strip_forged_cron_keys(task_json: String) -> (String, Vec<&'static str>) {
    if !CRON_TICKER_ONLY_TASK_KEYS
        .iter()
        .any(|key| task_json.contains(key))
    {
        return (task_json, Vec::new());
    }
    let Ok(serde_json::Value::Object(mut obj)) =
        serde_json::from_str::<serde_json::Value>(&task_json)
    else {
        return (task_json, Vec::new());
    };
    let removed: Vec<&'static str> = CRON_TICKER_ONLY_TASK_KEYS
        .into_iter()
        .filter(|key| obj.remove(*key).is_some())
        .collect();
    if removed.is_empty() {
        return (task_json, removed);
    }
    (serde_json::Value::Object(obj).to_string(), removed)
}

/// Does `identity` own `job`? Ownership is by AGENT: a job created by any of
/// the agent's guests, or an operator job whose target role belongs to the
/// agent (`role:{agent}:{name}`, `{agent}:{name}`, or the bare agent id).
pub(in crate::service) fn cron_job_owned_by(
    job: &ansible_mesh_core::cron::CronJob,
    guest_id: &str,
) -> bool {
    let agent = cron_owner_agent_of_guest(guest_id);
    let created_by_agent = match &job.created_by {
        ansible_mesh_core::cron::CronJobSource::Guest(g) => cron_owner_agent_of_guest(g) == agent,
        ansible_mesh_core::cron::CronJobSource::Operator => false,
    };
    if created_by_agent {
        return true;
    }
    let target = job
        .target_role
        .strip_prefix("role:")
        .unwrap_or(&job.target_role);
    target == agent
        || target
            .strip_prefix(agent)
            .is_some_and(|rest| rest.starts_with(':'))
}

pub(in crate::service) fn cron_job_visible_to(
    job: &ansible_mesh_core::cron::CronJob,
    identity: Option<&GuestIdentity>,
) -> bool {
    match identity {
        None => true,
        Some(id) => cron_admin_identity(id) || cron_job_owned_by(job, &id.guest_id),
    }
}

pub(in crate::service) fn cron_forbidden(
    op: &str,
    job_id: &str,
    identity: Option<&GuestIdentity>,
) -> IpcResponse {
    warn!(
        op,
        job_id,
        guest_id = identity.map(|i| i.guest_id.as_str()).unwrap_or("-"),
        "cron mutation refused: job is not owned by the caller's agent"
    );
    IpcResponse::error(
        op,
        "CRON_FORBIDDEN",
        format!(
            "cron job {job_id} is not in your crontab — only jobs owned by your agent can be changed"
        ),
    )
}

/// Ownership gate for a mutation that must look the job up first (remove).
/// `Ok(())` when the job is missing (the existing handler reports that), or
/// when the caller owns it / is an admin surface.
#[allow(clippy::result_large_err)]
pub(in crate::service) fn cron_job_mutation_allowed(
    graph: &GraphDomain,
    job_id: &str,
    identity: Option<&GuestIdentity>,
    op: &str,
) -> Result<(), IpcResponse> {
    match graph.get_cron_job(job_id) {
        Ok(Some(job)) if !cron_job_visible_to(&job, identity) => {
            Err(cron_forbidden(op, job_id, identity))
        }
        _ => Ok(()),
    }
}

impl IpcServer {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_register_cron_job(
        mut job: CronJob,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        Self::normalize_cron_target_role(graph, &mut job);
        if job.target_role.starts_with("role:")
            && !crate::service::cron_ticker::cron_payload_reaches_an_agent(&job.payload)
        {
            return IpcResponse::error(
                "register_cron_job",
                "CRON_PAYLOAD_UNDELIVERABLE",
                format!(
                    "cron job NOT registered: a job for {} must carry its instruction in a \
                             `message` string, e.g. {{\"message\": \"Run the LifeGraph gardening review \
                             now: …\"}}. A payload with no `message`/`content` (got: {}) is dropped \
                             by the agent every time it fires.",
                    job.target_role,
                    job.payload.chars().take(160).collect::<String>()
                ),
            );
        }
        // Register is an upsert by id: overwriting a job the caller
        // does not own is a mutation of someone else's crontab.
        let existing = graph.get_cron_job(&job.id).ok().flatten();
        if let Some(existing) = existing.as_ref() {
            if !cron_job_visible_to(existing, current_identity.as_ref()) {
                return cron_forbidden("register_cron_job", &job.id, current_identity.as_ref());
            }
        }
        // Turn policy is operator-owned. A guest may not set one, and
        // a guest re-registering (editing) a job drops the operator's
        // policy — the operator approved THAT instruction, not
        // whatever the guest rewrote it to.
        if !cron_policy_authority(current_identity.as_ref()) {
            if job.policy.is_some() {
                return IpcResponse::error(
                    "register_cron_job",
                    "CRON_POLICY_OPERATOR_ONLY",
                    "cron job NOT registered: a cron job's tool/approval policy can only \
                             be set by the operator. Register the job without `policy` and ask \
                             the operator to set one."
                        .to_string(),
                );
            }
            if existing.as_ref().is_some_and(|e| e.policy.is_some()) {
                warn!(
                    job_id = %job.id,
                    "RegisterCronJob: guest edit cleared the job's operator-set policy"
                );
            }
        }
        job.policy = job
            .policy
            .take()
            .map(ansible_mesh_core::cron::CronTurnPolicy::normalized);
        // Ownership is stamped from the connection identity, never
        // trusted from the wire: a guest's jobs belong to its agent.
        if let Some(identity) = current_identity.as_ref() {
            if !cron_admin_identity(identity) {
                job.created_by = ansible_mesh_core::cron::CronJobSource::Guest(
                    cron_owner_agent_of_guest(&identity.guest_id).to_string(),
                );
            }
        }
        // New job registrations always get an isolated `cron:<job_id>`
        // session — `session_target` only defaults to `Main` via serde
        // when deserializing legacy rows straight from storage
        // (`default_session_target_legacy`). Registration is the only
        // path that mints brand-new jobs, so it is safe to force
        // `Isolated` here unconditionally; existing rows loaded from
        // the graph never pass through this handler again.
        job.session_target = ansible_mesh_core::cron::CronSessionTarget::Isolated;
        info!("RegisterCronJob: id={} role={}", job.id, job.target_role);
        match graph.upsert_cron_job(&job) {
            Ok(_) => IpcResponse::success(
                "register_cron_job",
                Some(serde_json::json!({ "job_id": job.id })),
            ),
            Err(e) => IpcResponse::Error(format!("RegisterCronJob failed: {e}")),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_remove_cron_job(
        job_id: CronJobId,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        info!("RemoveCronJob: id={}", job_id);
        if let Err(refusal) =
            cron_job_mutation_allowed(graph, &job_id, current_identity.as_ref(), "remove_cron_job")
        {
            return refusal;
        }
        match graph.remove_cron_job(&job_id) {
            Ok(_) => IpcResponse::success("remove_cron_job", None),
            Err(e) => IpcResponse::Error(format!("RemoveCronJob failed: {e}")),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_list_cron_jobs(
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        match graph.list_cron_jobs() {
            // Every role may list, but a guest sees only its own agent's
            // crontab; orchestrator/management/operator surfaces see all.
            Ok(jobs) => IpcResponse::CronJobList {
                jobs: jobs
                    .into_iter()
                    .filter(|job| cron_job_visible_to(job, current_identity.as_ref()))
                    .collect(),
            },
            Err(e) => IpcResponse::Error(format!("ListCronJobs failed: {e}")),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_enable_cron_job(
        job_id: CronJobId,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        match graph.get_cron_job(&job_id) {
            Ok(Some(mut job)) => {
                if !cron_job_visible_to(&job, current_identity.as_ref()) {
                    return cron_forbidden("enable_cron_job", &job_id, current_identity.as_ref());
                }
                job.enabled = true;
                match graph.upsert_cron_job(&job) {
                    Ok(_) => IpcResponse::success("enable_cron_job", None),
                    Err(e) => IpcResponse::Error(format!("EnableCronJob failed: {e}")),
                }
            }
            Ok(None) => IpcResponse::Error(format!("cron job not found: {job_id}")),
            Err(e) => IpcResponse::Error(format!("EnableCronJob failed: {e}")),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_disable_cron_job(
        job_id: CronJobId,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        match graph.get_cron_job(&job_id) {
            Ok(Some(mut job)) => {
                if !cron_job_visible_to(&job, current_identity.as_ref()) {
                    return cron_forbidden("disable_cron_job", &job_id, current_identity.as_ref());
                }
                job.enabled = false;
                match graph.upsert_cron_job(&job) {
                    Ok(_) => IpcResponse::success("disable_cron_job", None),
                    Err(e) => IpcResponse::Error(format!("DisableCronJob failed: {e}")),
                }
            }
            Ok(None) => IpcResponse::Error(format!("cron job not found: {job_id}")),
            Err(e) => IpcResponse::Error(format!("DisableCronJob failed: {e}")),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_set_cron_policy(
        job_id: CronJobId,
        policy: Option<CronTurnPolicy>,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        if !cron_policy_authority(current_identity.as_ref()) {
            warn!(
                job_id = %job_id,
                guest_id = current_identity.as_ref().map(|i| i.guest_id.as_str()).unwrap_or("-"),
                "SetCronPolicy refused: not an operator identity"
            );
            return IpcResponse::error(
                "set_cron_policy",
                "CRON_POLICY_OPERATOR_ONLY",
                format!(
                    "cron job {job_id}: only the operator can set a cron job's tool/approval policy"
                ),
            );
        }
        match graph.get_cron_job(&job_id) {
            Ok(Some(mut job)) => {
                job.policy = policy.map(ansible_mesh_core::cron::CronTurnPolicy::normalized);
                info!(
                    job_id = %job_id,
                    has_policy = job.policy.is_some(),
                    "SetCronPolicy"
                );
                match graph.upsert_cron_job(&job) {
                    Ok(_) => IpcResponse::success(
                        "set_cron_policy",
                        Some(serde_json::json!({
                            "job_id": job.id,
                            "policy": job.policy,
                        })),
                    ),
                    Err(e) => IpcResponse::Error(format!("SetCronPolicy failed: {e}")),
                }
            }
            Ok(None) => IpcResponse::Error(format!("cron job not found: {job_id}")),
            Err(e) => IpcResponse::Error(format!("SetCronPolicy failed: {e}")),
        }
    }
}
