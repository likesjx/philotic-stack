//! Install and verify the operator-facing LifeGraph creative flywheel cadence.
//!
//! The jobs use deterministic IDs, so rerunning this installer updates the
//! existing records instead of creating duplicates.
//!
//! Production install:
//!   PHILOTIC_HOTEL_SOCKET=~/.philotic/bjork/aiua-mac-jane.sock \
//!   LIFE_GRAPH_CADENCE_CHAT_ID=<operator-chat-id> \
//!     cargo run -p philotic-client --example life_graph_flywheel_cadence
//!
//! Immediate first-fire verification:
//!   LIFE_GRAPH_CADENCE_FIRST_FIRE=daily ... cargo run ...
//!   LIFE_GRAPH_CADENCE_FIRST_FIRE=weekly ... cargo run ...

use ansible_mesh_core::cron::{CronJob, CronJobSource, CronSessionTarget, next_fire_after};
use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

const DAILY_ID: &str = "lifegraph-flywheel-daily:mac-jane";
const WEEKLY_ID: &str = "lifegraph-flywheel-weekly:mac-jane";
const DAILY_SCHEDULE: &str = "0 0 12 * * * *";
const WEEKLY_SCHEDULE: &str = "0 0 22 * * SUN *";
const TARGET_ROLE: &str = "role:agent-bjork-01:orchestrator";
const PILOT_DOMAIN: &str = "LifeGraph creative systems";

#[tokio::main]
async fn main() -> Result<()> {
    let socket = std::env::var("PHILOTIC_HOTEL_SOCKET")
        .unwrap_or_else(|_| "/Users/jaredlikes/.philotic/bjork/aiua-mac-jane.sock".into());
    let first_fire = std::env::var("LIFE_GRAPH_CADENCE_FIRST_FIRE").unwrap_or_default();
    if !matches!(first_fire.as_str(), "" | "daily" | "weekly") {
        bail!("LIFE_GRAPH_CADENCE_FIRST_FIRE must be empty, daily, or weekly");
    }
    let chat_id = std::env::var("LIFE_GRAPH_CADENCE_CHAT_ID")
        .context("LIFE_GRAPH_CADENCE_CHAT_ID is required")?;

    let mut client = PhiloticClient::connect_at(
        &socket,
        GuestIdentity {
            guest_id: "lifegraph-flywheel-cadence-installer".into(),
            role: "lifegraph.flywheel.cadence.installer".into(),
            supported_tools: Vec::new(),
        },
    )
    .await
    .with_context(|| format!("failed to connect to hotel socket at {socket}"))?;

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_millis() as u64;

    let existing_jobs = list_jobs(&mut client).await?;
    let jobs = [
        cadence_job(
            DAILY_ID,
            DAILY_SCHEDULE,
            daily_payload(&chat_id),
            if first_fire == "daily" {
                Some(now_ms + 3_000)
            } else {
                None
            },
            now_ms,
            existing_jobs.iter().find(|job| job.id == DAILY_ID),
        )?,
        cadence_job(
            WEEKLY_ID,
            WEEKLY_SCHEDULE,
            weekly_payload(&chat_id),
            if first_fire == "weekly" {
                Some(now_ms + 3_000)
            } else {
                None
            },
            now_ms,
            existing_jobs.iter().find(|job| job.id == WEEKLY_ID),
        )?,
    ];

    for job in jobs {
        let response = client
            .send_request(IpcRequest::RegisterCronJob { job })
            .await
            .context("failed to register cadence job")?;
        match response {
            IpcResponse::Standard { ok: true, .. } => {}
            other => bail!("hotel rejected cadence registration: {other:?}"),
        }
    }

    let jobs = list_jobs(&mut client).await?;

    for (id, schedule) in [(DAILY_ID, DAILY_SCHEDULE), (WEEKLY_ID, WEEKLY_SCHEDULE)] {
        let job = jobs
            .iter()
            .find(|job| job.id == id)
            .with_context(|| format!("registered job missing from cron list: {id}"))?;
        if !job.enabled || job.schedule != schedule || job.target_role != TARGET_ROLE {
            bail!("registered cadence job has unexpected shape: {job:?}");
        }
        println!(
            "id={} schedule={} target_role={} next_fire_at={} last_fired_epoch={:?}",
            job.id, job.schedule, job.target_role, job.next_fire_at, job.last_fired_epoch
        );
    }

    println!("LifeGraph flywheel cadence installed (daily 12:00 UTC; weekly Sunday 22:00 UTC)");
    Ok(())
}

fn cadence_job(
    id: &str,
    schedule: &str,
    payload: String,
    first_fire_at: Option<u64>,
    now_ms: u64,
    existing: Option<&CronJob>,
) -> Result<CronJob> {
    let next_fire_at = first_fire_at
        .or_else(|| {
            existing
                .filter(|job| job.schedule == schedule && job.enabled)
                .map(|job| job.next_fire_at)
        })
        .unwrap_or(next_fire_after(schedule, now_ms)?);
    Ok(CronJob {
        id: id.into(),
        schedule: schedule.into(),
        target_role: TARGET_ROLE.into(),
        target_node_id: None,
        payload,
        guaranteed: false,
        enabled: true,
        last_fired_epoch: existing.and_then(|job| job.last_fired_epoch),
        next_fire_at,
        created_at: existing.map(|job| job.created_at).unwrap_or(now_ms),
        created_by: CronJobSource::Operator,
        silent_ok: false,
        session_target: CronSessionTarget::Isolated,
    })
}

async fn list_jobs(client: &mut PhiloticClient) -> Result<Vec<CronJob>> {
    let response = client
        .send_request(IpcRequest::ListCronJobs)
        .await
        .context("failed to list cadence jobs")?;
    match response {
        IpcResponse::CronJobList { jobs } => Ok(jobs),
        other => bail!("unexpected cron list response: {other:?}"),
    }
}

fn daily_payload(chat_id: &str) -> String {
    json!({
        "message": format!(
            "Run Jared's read-only LifeGraph creative flywheel daily brief for the \
             `{PILOT_DOMAIN}` pilot. Call life.flywheel.brief with \
             pilot_domain=\"{PILOT_DOMAIN}\". Send one glanceable message with exactly \
             three short sections: Resume, Make, Unblock. Use at most the one returned \
             item per lane, cite its LifeGraph node id, and say \"Nothing surfaced\" for \
             a null lane. End with one concrete ten-minute creative action grounded in \
             the returned evidence. Do not create, confirm, resolve, or otherwise mutate \
             graph nodes during this brief."
        ),
        "source": "telegram",
        "chat_id": chat_id,
        "agent_id": "agent-bjork-01",
        "cron_preapproved_tools": ["life.flywheel.brief"]
    })
    .to_string()
}

fn weekly_payload(chat_id: &str) -> String {
    json!({
        "message": format!(
            "Run Jared's read-only weekly LifeGraph creative flywheel review for the \
             `{PILOT_DOMAIN}` pilot. Call life.flywheel.review with \
             pilot_domain=\"{PILOT_DOMAIN}\" and lookback_days=7, then call \
             life.flywheel.brief for the same pilot. Send one glanceable review covering \
             idea movement, experiments, artifacts, learnings, learning reuse, conversion, \
             unclassified inbox, and the best evidence-backed next experiment. Cite node \
             ids where the tools return them. Separate measured facts from recommendations. \
             Do not mutate graph nodes during this review."
        ),
        "source": "telegram",
        "chat_id": chat_id,
        "agent_id": "agent-bjork-01",
        "cron_preapproved_tools": [
            "life.flywheel.review",
            "life.flywheel.brief"
        ]
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn payloads_preapprove_only_read_only_flywheel_tools() {
        let daily: Value =
            serde_json::from_str(&daily_payload("test-chat")).expect("daily payload");
        assert_eq!(
            daily["cron_preapproved_tools"],
            json!(["life.flywheel.brief"])
        );

        let weekly: Value =
            serde_json::from_str(&weekly_payload("test-chat")).expect("weekly payload");
        assert_eq!(
            weekly["cron_preapproved_tools"],
            json!(["life.flywheel.review", "life.flywheel.brief"])
        );
        assert_eq!(daily["chat_id"], "test-chat");
        assert_eq!(weekly["chat_id"], "test-chat");
    }

    #[test]
    fn reinstall_preserves_fire_history_and_next_occurrence() {
        let existing = CronJob {
            id: DAILY_ID.into(),
            schedule: DAILY_SCHEDULE.into(),
            target_role: TARGET_ROLE.into(),
            target_node_id: None,
            payload: daily_payload("test-chat"),
            guaranteed: false,
            enabled: true,
            last_fired_epoch: Some(123),
            next_fire_at: 456,
            created_at: 100,
            created_by: CronJobSource::Operator,
            silent_ok: false,
            session_target: CronSessionTarget::Isolated,
        };

        let reinstalled = cadence_job(
            DAILY_ID,
            DAILY_SCHEDULE,
            daily_payload("test-chat"),
            None,
            999,
            Some(&existing),
        )
        .expect("cadence job");
        assert_eq!(reinstalled.last_fired_epoch, Some(123));
        assert_eq!(reinstalled.next_fire_at, 456);
        assert_eq!(reinstalled.created_at, 100);
    }

    #[test]
    fn first_fire_override_preserves_history() {
        let existing = CronJob {
            id: WEEKLY_ID.into(),
            schedule: WEEKLY_SCHEDULE.into(),
            target_role: TARGET_ROLE.into(),
            target_node_id: None,
            payload: weekly_payload("test-chat"),
            guaranteed: false,
            enabled: true,
            last_fired_epoch: Some(123),
            next_fire_at: 456,
            created_at: 100,
            created_by: CronJobSource::Operator,
            silent_ok: false,
            session_target: CronSessionTarget::Isolated,
        };

        let reinstalled = cadence_job(
            WEEKLY_ID,
            WEEKLY_SCHEDULE,
            weekly_payload("test-chat"),
            Some(1_000),
            999,
            Some(&existing),
        )
        .expect("cadence job");
        assert_eq!(reinstalled.last_fired_epoch, Some(123));
        assert_eq!(reinstalled.next_fire_at, 1_000);
        assert_eq!(reinstalled.created_at, 100);
    }
}
