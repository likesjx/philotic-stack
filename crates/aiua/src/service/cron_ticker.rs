//! Hotel cron scheduler — fires pre-packaged envelopes on a schedule.
//!
//! `CronTicker` wakes every second, queries the Context Graph for due jobs
//! (Slice 1: local, non-guaranteed), materialises a `TaskInvoke` envelope,
//! delivers it to the registered role inbox, durably appends it to the ledger,
//! and advances `next_fire_at`.

use crate::LedgerCommand;
use crate::service::ipc::InboxRegistry;
use ansible_mesh_core::cron::{CronJob, next_fire_after};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};
use tracing::{error, info, warn};
use uuid::Uuid;

pub struct CronTicker {
    graph: Arc<GraphDomain>,
    dispatcher_tx: mpsc::Sender<LedgerCommand>,
    inboxes: InboxRegistry,
    local_node_id: String,
}

impl CronTicker {
    pub fn new(
        graph: Arc<GraphDomain>,
        dispatcher_tx: mpsc::Sender<LedgerCommand>,
        inboxes: InboxRegistry,
        local_node_id: impl Into<String>,
    ) -> Self {
        Self {
            graph,
            dispatcher_tx,
            inboxes,
            local_node_id: local_node_id.into(),
        }
    }

    pub async fn run(self) {
        info!("CronTicker: started");
        let mut tick = interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            let now_ms = now_ms();
            match self.graph.list_due_cron_jobs(now_ms, 0) {
                Ok(jobs) => {
                    for job in jobs {
                        self.fire(&job, now_ms).await;
                    }
                }
                Err(e) => {
                    error!("CronTicker: list_due_cron_jobs failed: {e}");
                }
            }
        }
    }

    async fn fire(&self, job: &CronJob, now_ms: u64) {
        let fire_epoch = job.next_fire_at;
        let payload_data = job.payload.replace("{timestamp}", &fire_epoch.to_string());

        let task_id = Uuid::new_v4();
        let task_json = serde_json::json!({
            "cron_job_id": job.id,
            "target_role": job.target_role,
            "fire_epoch": fire_epoch,
            "payload": payload_data,
        })
        .to_string();

        let env = EventEnvelope {
            event_id: task_id,
            seq: 0,
            source_node_id: self.local_node_id.clone(),
            target_node_id: Some(self.local_node_id.clone()),
            source_agent_id: "cron-ticker".into(),
            target_agent_id: Some(job.target_role.clone()),
            kind: EventKind::TaskInvoke,
            corr_id: format!("cron:{}", job.id),
            attempt: 0,
            created_at: now_ms,
            expires_at: None,
            payload: EventPayload::Inline {
                data: task_json.clone(),
            },
            trace: vec![format!("cron-ticker:{}", job.id)],
        };

        // Durably append to the event ledger.
        if let Err(e) = self
            .dispatcher_tx
            .send(LedgerCommand::AppendLocal(env))
            .await
        {
            error!(
                "CronTicker: failed to append ledger event for job {}: {e}",
                job.id
            );
            return;
        }

        // Deliver to the role inbox directly.
        crate::service::ipc::IpcServer::deliver_inbound_task(
            &self.inboxes,
            &self.local_node_id,
            &job.target_role,
            None,
            task_id,
            task_json,
        )
        .await;

        info!(
            "CronTicker: fired job {} → role={} epoch={}",
            job.id, job.target_role, fire_epoch
        );

        // Advance schedule.
        match next_fire_after(&job.schedule, now_ms) {
            Ok(next) => {
                let mut updated = job.clone();
                updated.last_fired_epoch = Some(fire_epoch);
                updated.next_fire_at = next;
                if let Err(e) = self.graph.upsert_cron_job(&updated) {
                    error!(
                        "CronTicker: failed to advance schedule for job {}: {e}",
                        job.id
                    );
                }
            }
            Err(e) => {
                warn!(
                    "CronTicker: no future occurrences for job {} ({}); disabling",
                    job.id, e
                );
                let mut updated = job.clone();
                updated.enabled = false;
                let _ = self.graph.upsert_cron_job(&updated);
            }
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
