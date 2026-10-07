//! Cron access control: ownership scoping, visibility and mutation gates.
//!
//! Moved verbatim from `ipc/mod.rs` (IPC_DISPATCH_SPLIT); only item
//! visibility was widened so the parent module can reach it.

use super::*;

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
