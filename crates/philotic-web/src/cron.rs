//! `phil cron` — the operator's surface over the hotel crontab
//! (`ListCronJobs` / `SetCronPolicy` IPC, same connect-and-call shape as
//! `config.rs`).
//!
//! A cron job's turn policy (tool allowlist, preapprovals, approval mode) is
//! operator-owned: the hotel accepts `SetCronPolicy` only from an operator
//! identity, and this CLI connects as `management` — the identity the hotel
//! treats as the operator's tooling. Agents can create jobs, never grant them
//! tools or approvals.

use anyhow::{anyhow, Context, Result};
use clap::{Subcommand, ValueEnum};
use philotic_client::{
    CronApprovalMode, CronJob, CronTurnPolicy, GuestIdentity, IpcRequest, IpcResponse,
    PhiloticClient,
};

use crate::start::socket_path;

#[derive(Subcommand, Debug)]
pub enum CronAction {
    /// List the hotel's cron jobs with their owner, schedule and policy.
    List {
        /// Hotel name to target (default: "default").
        #[arg(long, default_value = "default")]
        hotel: String,

        /// Emit the raw job records as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Set a job's turn policy — what its fires may use and run unprompted.
    ///
    /// Replaces the whole policy. Example: limit the daily brief to LifeGraph
    /// recall, run it without prompts, and never park for approval:
    ///
    ///   phil cron policy lifegraph-daily-brief:vps-jane --hotel vps-jane \
    ///     --allow life.recall,life.recall.feedback \
    ///     --preapprove life.recall,life.recall.feedback --approval deny
    Policy {
        /// Cron job id (see `phil cron list`).
        job_id: String,

        /// Hotel name to target (default: "default").
        #[arg(long, default_value = "default")]
        hotel: String,

        /// Only these tools may be used by the job's fires (comma-separated).
        /// Omit to keep the target role's full toolset.
        #[arg(long, value_delimiter = ',')]
        allow: Vec<String>,

        /// Tool classes allowed alongside `--allow` (comma-separated).
        #[arg(long = "allow-class", value_delimiter = ',')]
        allow_class: Vec<String>,

        /// Tools that run without an approval prompt (comma-separated).
        #[arg(long, value_delimiter = ',')]
        preapprove: Vec<String>,

        /// Tool classes that run without an approval prompt (comma-separated).
        #[arg(long = "preapprove-class", value_delimiter = ',')]
        preapprove_class: Vec<String>,

        /// What a fire does when a tool still needs approval.
        #[arg(long, value_enum, default_value_t = ApprovalArg::AskOrDeny)]
        approval: ApprovalArg,

        /// Remove the job's policy entirely (all other flags ignored).
        #[arg(long)]
        clear: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ApprovalArg {
    /// Ask in the job's chat; deny when it has no chat to ask.
    AskOrDeny,
    /// Never ask; deny anything not preapproved.
    Deny,
}

impl From<ApprovalArg> for CronApprovalMode {
    fn from(value: ApprovalArg) -> Self {
        match value {
            ApprovalArg::AskOrDeny => CronApprovalMode::AskOrDeny,
            ApprovalArg::Deny => CronApprovalMode::Deny,
        }
    }
}

pub async fn run(action: CronAction) -> Result<()> {
    match action {
        CronAction::List { hotel, json } => list(&hotel, json).await,
        CronAction::Policy {
            job_id,
            hotel,
            allow,
            allow_class,
            preapprove,
            preapprove_class,
            approval,
            clear,
        } => {
            let policy = (!clear).then(|| {
                CronTurnPolicy {
                    allowed_tools: (!allow.is_empty()).then_some(allow),
                    allowed_classes: allow_class,
                    preapproved_tools: preapprove,
                    preapproved_classes: preapprove_class,
                    approval_mode: approval.into(),
                }
                .normalized()
            });
            if let Some(p) = policy.as_ref() {
                if p.allowed_tools.is_none() && !p.allowed_classes.is_empty() {
                    anyhow::bail!("--allow-class only applies together with --allow");
                }
            }
            set_policy(&hotel, &job_id, policy).await
        }
    }
}

async fn ipc_client(hotel_name: &str) -> Result<PhiloticClient> {
    let identity = GuestIdentity {
        guest_id: "phil-cron".into(),
        role: "management".into(),
        supported_tools: vec![],
    };
    let socket = socket_path(hotel_name);
    PhiloticClient::connect_at(&socket, identity)
        .await
        .with_context(|| format!("failed to connect to hotel IPC at {socket}"))
}

async fn list(hotel: &str, json: bool) -> Result<()> {
    let mut client = ipc_client(hotel).await?;
    let jobs = match client
        .send_request(IpcRequest::ListCronJobs)
        .await
        .context("ListCronJobs IPC request failed")?
    {
        IpcResponse::CronJobList { jobs } => jobs,
        IpcResponse::Error(message) => return Err(anyhow!(message)),
        other => return Err(anyhow!("unexpected ListCronJobs response: {other:?}")),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&jobs)?);
        return Ok(());
    }
    for job in &jobs {
        println!("{}", describe(job));
    }
    Ok(())
}

fn describe(job: &CronJob) -> String {
    let owner = match &job.created_by {
        philotic_client::CronJobSource::Operator => "operator".to_string(),
        philotic_client::CronJobSource::Guest(g) => g.clone(),
    };
    let state = if job.enabled { "on " } else { "off" };
    let policy = match &job.policy {
        None => "policy: none".to_string(),
        Some(p) => format!(
            "policy: allow={} preapprove={} approval={:?}",
            p.allowed_tools
                .as_ref()
                .map(|t| t.join(","))
                .unwrap_or_else(|| "role".into()),
            if p.preapproved_tools.is_empty() {
                "-".to_string()
            } else {
                p.preapproved_tools.join(",")
            },
            p.approval_mode
        ),
    };
    format!(
        "{state} {id}  [{schedule}]  -> {target}  by {owner}  {policy}",
        id = job.id,
        schedule = job.schedule,
        target = job.target_role,
    )
}

async fn set_policy(hotel: &str, job_id: &str, policy: Option<CronTurnPolicy>) -> Result<()> {
    let mut client = ipc_client(hotel).await?;
    match client
        .send_request(IpcRequest::SetCronPolicy {
            job_id: job_id.into(),
            policy,
        })
        .await
        .context("SetCronPolicy IPC request failed")?
    {
        IpcResponse::Standard { ok: true, data, .. } => {
            let policy = data
                .as_ref()
                .and_then(|d| d.get("policy"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            println!("{job_id}: policy = {policy}");
            Ok(())
        }
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected SetCronPolicy response: {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_shows_owner_and_policy() {
        let mut job: CronJob = serde_json::from_value(serde_json::json!({
            "id": "brief",
            "schedule": "0 0 11 * * * *",
            "target_role": "role:agent-beacon:orchestrator",
            "target_node_id": null,
            "payload": "{}",
            "guaranteed": false,
            "enabled": true,
            "last_fired_epoch": null,
            "next_fire_at": 0,
            "created_at": 0,
            "created_by": "operator",
        }))
        .unwrap();
        assert!(describe(&job).contains("policy: none"));
        job.policy = Some(CronTurnPolicy {
            allowed_tools: Some(vec!["life.recall".into()]),
            preapproved_tools: vec!["life.recall".into()],
            approval_mode: CronApprovalMode::Deny,
            ..Default::default()
        });
        let line = describe(&job);
        assert!(line.contains("allow=life.recall"), "{line}");
        assert!(line.contains("approval=Deny"), "{line}");
    }
}
