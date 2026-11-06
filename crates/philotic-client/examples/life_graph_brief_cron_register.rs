//! Register the LifeGraph Daily Brief cron job (LIFE_GRAPH_ACTIVE proposal,
//! slice S3) with a hotel over IPC.
//!
//! The brief is an operator-invited standing digest: a cron fire delivers a
//! composition prompt to the chief-of-staff steward's role inbox; the model
//! runs the four named `life.recall` strategies and delivers one Telegram
//! message. Reactions feed `life.recall.feedback` (recall_utility EWMA) and
//! accumulate the SIL evidence the Attention Steward's active-gate waits for
//! (S4). Registration goes through `IpcRequest::RegisterCronJob`, so the
//! handler's normalization (target-role routing key, forced `Isolated`
//! session) applies — this is the same path the `cron.register` tool uses,
//! chosen over an architect-charter-style boot seeder because no host has
//! that env plumbing wired (see ATTENTION note in the proposal doc).
//!
//! Modes (first CLI arg): `register` (default) | `list` | `remove`
//!
//! Env:
//!   PHILOTIC_HOTEL_SOCKET                (default /tmp/philotic-aiua.sock)
//!   LIFE_GRAPH_BRIEF_HOTEL               (default vps-jane) — job id suffix
//!   LIFE_GRAPH_BRIEF_AGENT               (default agent-beacon)
//!   LIFE_GRAPH_BRIEF_ROLE                (default orchestrator)
//!   LIFE_GRAPH_BRIEF_SCHEDULE            (default "0 0 11 * * * *" = 11:00 UTC daily)
//!   LIFE_GRAPH_BRIEF_CHAT_ID             (default 7898847424)
//!   LIFE_GRAPH_BRIEF_CHAT_SOURCE         (default telegram)
//!   LIFE_GRAPH_BRIEF_FIRST_FIRE_IN_SECS  (optional) — override the first
//!       fire to now+N seconds for a live-green proof of the real job; the
//!       schedule advances to the normal daily slot after that first fire.

use anyhow::{Context, Result, bail};
use philotic_client::{CronJob, CronJobSource, GuestIdentity, IpcRequest, IpcResponse};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

const DRIVER_GUEST_ID: &str = "life-graph-brief-cron-register";
const DEFAULT_SCHEDULE: &str = "0 0 11 * * * *";
const DEFAULT_CHAT_ID: &str = "7898847424";

/// The standing brief contract, delivered as the cron fire's turn content.
/// Human-readable copy lives in skills/lifegraph-daily-brief/SKILL.md — this
/// string is the runtime truth (payload > SKILL.md, which no crate reads).
const BRIEF_PROMPT: &str = "Good morning — compose and deliver Jared's LifeGraph Daily Brief now. \
Gather, in order: (1) life.recall named_strategy=commitments_approaching with due_within_hours=72; \
(2) life.recall named_strategy=open_loops_by_context; \
(3) life.recall named_strategy=goals_and_next_actions; \
(4) life.recall named_strategy=re_entry_context. \
Then send ONE Telegram message with these sections, skipping any that are empty: \
'Due soon' — commitments with their dates, soonest first; \
'Open loops' — at most 5, stalest first; \
'Goals' — active goals, and when a NextAction ADVANCES one, show it indented under its goal; \
'Picking back up' — 1-2 lines of re-entry context. \
Describe items by their claim_summary in plain language; never show raw node ids. \
Keep the whole brief under 1200 characters, no preamble. \
Close with one line inviting reactions, e.g. 'Reply with: done <item>, stale <item>, noisy <item>, or useful <item>.' \
When Jared reacts to a brief line, file life.recall.feedback with the packet_id from the recall that surfaced it, \
rating useful|noisy|stale as he indicated, and the node in the matching refs array. \
If every section is empty, send exactly one line saying the LifeGraph has no active agenda items and invite him to add one.";

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_millis() as u64
}

#[tokio::main]
async fn main() -> Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "register".into());
    let hotel = env_or("LIFE_GRAPH_BRIEF_HOTEL", "vps-jane");
    let job_id = format!("lifegraph-daily-brief:{hotel}");

    let mut client = IpcClient::connect(GuestIdentity {
        guest_id: DRIVER_GUEST_ID.into(),
        role: "life-graph.brief.register".into(),
        supported_tools: Vec::new(),
    })
    .await
    .context("failed to connect to hotel IPC socket")?;

    match mode.as_str() {
        "register" => {
            let agent = env_or("LIFE_GRAPH_BRIEF_AGENT", "agent-beacon");
            let role = env_or("LIFE_GRAPH_BRIEF_ROLE", "orchestrator");
            let schedule = env_or("LIFE_GRAPH_BRIEF_SCHEDULE", DEFAULT_SCHEDULE);
            let chat_id = env_or("LIFE_GRAPH_BRIEF_CHAT_ID", DEFAULT_CHAT_ID);
            let chat_source = env_or("LIFE_GRAPH_BRIEF_CHAT_SOURCE", "telegram");

            let now = now_ms();
            let scheduled_next = ansible_mesh_core::cron::next_fire_after(&schedule, now)
                .context("invalid LIFE_GRAPH_BRIEF_SCHEDULE")?;
            let next_fire_at = match std::env::var("LIFE_GRAPH_BRIEF_FIRST_FIRE_IN_SECS") {
                Ok(secs) => {
                    let secs: u64 = secs.parse().context("FIRST_FIRE_IN_SECS not a number")?;
                    now + secs * 1000
                }
                Err(_) => scheduled_next,
            };

            let payload = json!({
                "message": BRIEF_PROMPT,
                "chat_id": chat_id,
                "source": chat_source,
                // Forwarded because created_by=Operator (cron_ticker guard):
                // an unattended brief must never park at WaitingApproval.
                "preapproved_tools": ["life.recall", "life.recall.feedback"],
            })
            .to_string();

            let job = CronJob {
                id: job_id.clone(),
                schedule,
                target_role: format!("role:{agent}:{role}"),
                target_node_id: None,
                payload,
                guaranteed: false,
                enabled: true,
                last_fired_epoch: None,
                next_fire_at,
                created_at: now,
                created_by: CronJobSource::Operator,
                silent_ok: false,
                // The RegisterCronJob handler forces Isolated for new jobs;
                // set it explicitly anyway so the intent is in the source.
                session_target: ansible_mesh_core::cron::CronSessionTarget::Isolated,
            };

            match client.send_request(IpcRequest::RegisterCronJob { job }).await? {
                IpcResponse::Standard { ok: true, .. } => {
                    println!(
                        "registered {job_id}: first fire at epoch_ms={next_fire_at} \
                         ({}s from now), then daily per schedule",
                        (next_fire_at.saturating_sub(now)) / 1000
                    );
                }
                other => bail!("RegisterCronJob rejected: {other:?}"),
            }
        }
        "remove" => {
            match client
                .send_request(IpcRequest::RemoveCronJob {
                    job_id: job_id.clone(),
                })
                .await?
            {
                IpcResponse::Standard { ok: true, .. } => println!("removed {job_id}"),
                other => bail!("RemoveCronJob rejected: {other:?}"),
            }
        }
        "list" => match client.send_request(IpcRequest::ListCronJobs).await? {
            IpcResponse::CronJobList { jobs } => {
                for job in jobs {
                    println!(
                        "{}  enabled={} next_fire_at={} target={}",
                        job.id, job.enabled, job.next_fire_at, job.target_role
                    );
                }
            }
            other => bail!("ListCronJobs rejected: {other:?}"),
        },
        other => bail!("unknown mode '{other}' (expected register|list|remove)"),
    }

    Ok(())
}

struct IpcClient {
    stream: UnixStream,
    read_buf: Vec<u8>,
}

impl IpcClient {
    async fn connect(identity: GuestIdentity) -> Result<Self> {
        let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
            .unwrap_or_else(|_| "/tmp/philotic-aiua.sock".to_string());
        let stream = UnixStream::connect(&socket_path)
            .await
            .with_context(|| format!("failed to connect hotel IPC socket at {socket_path}"))?;
        let mut client = Self {
            stream,
            read_buf: Vec::new(),
        };
        match client.send_request(IpcRequest::Register(identity)).await? {
            IpcResponse::Standard { ok: true, .. } => Ok(client),
            other => bail!("hotel rejected registration: {other:?}"),
        }
    }

    async fn send_request(&mut self, request: IpcRequest) -> Result<IpcResponse> {
        let payload = serde_json::to_vec(&request).context("failed to serialize IPC frame")?;
        let len = payload.len() as u32;
        self.stream.write_all(&len.to_be_bytes()).await?;
        self.stream.write_all(&payload).await?;
        loop {
            let response = self.read_response().await?;
            if matches!(
                response,
                IpcResponse::InboundTask { .. }
                    | IpcResponse::ApartmentUpdate { .. }
                    | IpcResponse::GracefulShutdown { .. }
                    | IpcResponse::MemoryConfig(_)
                    | IpcResponse::MuninnStatus { .. }
                    | IpcResponse::NetworkState { .. }
            ) {
                continue;
            }
            return Ok(response);
        }
    }

    async fn read_response(&mut self) -> Result<IpcResponse> {
        loop {
            if self.read_buf.len() >= 4 {
                let len = u32::from_be_bytes([
                    self.read_buf[0],
                    self.read_buf[1],
                    self.read_buf[2],
                    self.read_buf[3],
                ]) as usize;
                let frame_len = 4 + len;
                if self.read_buf.len() >= frame_len {
                    let payload = self.read_buf[4..frame_len].to_vec();
                    self.read_buf.drain(..frame_len);
                    return serde_json::from_slice(&payload)
                        .context("failed to decode IPC response frame");
                }
            }
            let mut chunk = [0_u8; 8192];
            let n = self.stream.read(&mut chunk).await?;
            if n == 0 {
                bail!("IPC stream closed while waiting for frame");
            }
            self.read_buf.extend_from_slice(&chunk[..n]);
        }
    }
}
