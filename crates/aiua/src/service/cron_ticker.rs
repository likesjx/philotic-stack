//! Hotel cron scheduler — fires pre-packaged envelopes on a schedule.
//!
//! ## Slice 1 (local, non-guaranteed)
//! Wakes every second, queries due jobs, materialises a `TaskInvoke` envelope,
//! delivers to the role inbox, appends to the ledger, advances `next_fire_at`.
//!
//! ## Slice 2 (guaranteed, mesh-coordinated)
//! - `guaranteed = true` jobs use a staggered per-hotel offset
//!   (`offset_ms = PHILOTIC_CRON_OFFSET_SECS * 1000`).
//! - Before firing: checks `last_fired_epoch == Some(next_fire_at)` — if true,
//!   another hotel already handled this epoch; skip.
//! - After firing: appends a `CronFired` envelope (broadcast) so peer hotels can
//!   suppress their offset-delayed fire for the same epoch.
//! - Recovery scan at startup: guaranteed jobs where `next_fire_at < now` and
//!   `last_fired_epoch != Some(next_fire_at)` are fired immediately.
//!
//! ## Slice 3 (CronJobSync — mesh job propagation)
//! At startup, all local cron jobs are broadcast as `CronJobSync` upsert events
//! so that peer hotels replicate the definitions and can participate in
//! guaranteed firing without requiring a shared config file.
//!
//! ## Slice 4 (template interpolation)
//! The `payload` field supports `{timestamp}`, `{iso_timestamp}`, `{job_id}`,
//! `{node_id}`, and `{target_role}` placeholders, replaced at fire time via
//! [`ansible_mesh_core::cron::interpolate_payload`].

use crate::LedgerCommand;
use crate::service::guest_manager::GuestMaterializationRequester;
use crate::service::ipc::{InboxRegistry, ParkedInboundRegistry};
use ansible_mesh_core::cron::{
    CronInterpolationVars, CronJob, interpolate_payload, next_fire_after,
};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
use ansible_mesh_core::graph::RoleIncarnationRecord;
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
    /// Per-hotel stagger offset for guaranteed jobs (ms).
    /// Derived from `PHILOTIC_CRON_OFFSET_SECS * 1000`. Default 0.
    offset_ms: u64,
    parked_inbound: ParkedInboundRegistry,
    materialization_requester: Option<Arc<dyn GuestMaterializationRequester>>,
}

impl CronTicker {
    pub fn new(
        graph: Arc<GraphDomain>,
        dispatcher_tx: mpsc::Sender<LedgerCommand>,
        inboxes: InboxRegistry,
        local_node_id: impl Into<String>,
        offset_ms: u64,
        parked_inbound: ParkedInboundRegistry,
        materialization_requester: Option<Arc<dyn GuestMaterializationRequester>>,
    ) -> Self {
        Self {
            graph,
            dispatcher_tx,
            inboxes,
            local_node_id: local_node_id.into(),
            offset_ms,
            parked_inbound,
            materialization_requester,
        }
    }

    /// Resolve a cron job's `target_role` (expected shape: `role:{agent_id}:{role_name}`,
    /// matching `RoleIncarnationRecord::routing_role`) to the role incarnation it names,
    /// if one is configured.
    fn resolve_target_role_record(
        graph: &GraphDomain,
        target_role: &str,
    ) -> Option<RoleIncarnationRecord> {
        let (agent_id, role_name) = target_role.strip_prefix("role:")?.split_once(':')?;
        graph
            .get_role_incarnation(agent_id, role_name)
            .ok()
            .flatten()
    }

    pub async fn run(self) {
        info!("CronTicker: started (offset_ms={})", self.offset_ms);

        // Slice 3: advertise local job definitions to mesh peers.
        self.startup_sync().await;

        // Slice 2: fire any missed guaranteed jobs before entering the tick loop.
        self.recovery_scan().await;

        let mut tick = interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            let now_ms = now_ms();

            // Non-guaranteed: fire-or-skip, no offset.
            let non_guaranteed = match self.graph.list_due_cron_jobs(now_ms, 0) {
                Ok(jobs) => jobs
                    .into_iter()
                    .filter(|j| !j.guaranteed)
                    .collect::<Vec<_>>(),
                Err(e) => {
                    error!("CronTicker: list_due_cron_jobs (non-guaranteed) failed: {e}");
                    continue;
                }
            };

            // Guaranteed: staggered by hotel offset.
            let guaranteed = match self.graph.list_due_cron_jobs(now_ms, self.offset_ms) {
                Ok(jobs) => jobs
                    .into_iter()
                    .filter(|j| j.guaranteed)
                    .collect::<Vec<_>>(),
                Err(e) => {
                    error!("CronTicker: list_due_cron_jobs (guaranteed) failed: {e}");
                    continue;
                }
            };

            for job in non_guaranteed.into_iter().chain(guaranteed) {
                self.fire(&job, now_ms).await;
            }
        }
    }

    /// Broadcast all locally-registered cron jobs as `CronJobSync` upsert events
    /// so that peer hotels replicate the definitions on connect.
    async fn startup_sync(&self) {
        let jobs = match self.graph.list_cron_jobs() {
            Ok(j) => j,
            Err(e) => {
                error!("CronTicker: startup_sync failed to list jobs: {e}");
                return;
            }
        };

        if jobs.is_empty() {
            return;
        }

        let now_ms = now_ms();
        for job in &jobs {
            self.broadcast_cron_job_sync_upsert(job, now_ms).await;
        }
        info!("CronTicker: startup_sync broadcast {} job(s)", jobs.len());
    }

    /// On startup, fire any guaranteed jobs whose epoch was missed while the
    /// hotel was down — unless another hotel already handled them.
    async fn recovery_scan(&self) {
        let now_ms = now_ms();
        let jobs = match self.graph.list_cron_jobs() {
            Ok(j) => j,
            Err(e) => {
                error!("CronTicker: recovery scan failed to list jobs: {e}");
                return;
            }
        };

        for job in jobs {
            if !job.enabled || !job.guaranteed {
                continue;
            }
            if job.next_fire_at >= now_ms {
                continue;
            }
            if job.last_fired_epoch == Some(job.next_fire_at) {
                // Already handled; advance schedule.
                if let Err(e) = self.advance_schedule(&job, job.next_fire_at) {
                    error!(
                        "CronTicker: recovery advance failed for job {}: {e}",
                        job.id
                    );
                }
                continue;
            }
            info!(
                "CronTicker: recovery fire for job {} (epoch={})",
                job.id, job.next_fire_at
            );
            self.fire(&job, now_ms).await;
        }
    }

    async fn fire(&self, job: &CronJob, now_ms: u64) {
        let fire_epoch = job.next_fire_at;

        // Guaranteed dedup: if last_fired_epoch already covers this epoch,
        // another hotel fired it; skip and advance our local schedule.
        if job.guaranteed && job.last_fired_epoch == Some(fire_epoch) {
            if let Err(e) = self.advance_schedule(job, fire_epoch) {
                error!("CronTicker: dedup advance failed for job {}: {e}", job.id);
            }
            return;
        }

        // Slice 4: interpolate payload template variables.
        let vars =
            CronInterpolationVars::new(fire_epoch, &job.id, &self.local_node_id, &job.target_role);
        let payload_data = interpolate_payload(&job.payload, &vars);

        let task_id = Uuid::new_v4();
        let task_json = build_cron_task_json(job, fire_epoch, &self.local_node_id, payload_data);

        // Durably append TaskInvoke to the event ledger.
        let task_env = EventEnvelope {
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

        if let Err(e) = self
            .dispatcher_tx
            .send(LedgerCommand::AppendLocal(task_env))
            .await
        {
            error!(
                "CronTicker: failed to append TaskInvoke for job {}: {e}",
                job.id
            );
            return;
        }

        // Deliver to the role inbox (local delivery). If the target role resolves to a
        // configured role incarnation that isn't currently subscribed (e.g. an on-demand
        // role guest that hasn't been spawned yet), park the task and trigger its
        // materialization instead of dropping it ledger-only.
        let target_role_record = Self::resolve_target_role_record(&self.graph, &job.target_role);
        let is_subscribed = {
            let guard = self.inboxes.lock().await;
            let role_subs = guard
                .get(job.target_role.as_str())
                .cloned()
                .unwrap_or_default();
            match &target_role_record {
                Some(record) => role_subs.iter().any(|s| s.guest_id == record.guest_id),
                None => !role_subs.is_empty(),
            }
        };

        match (&target_role_record, is_subscribed) {
            (Some(record), false) => {
                crate::service::ipc::IpcServer::park_and_materialize_local_role(
                    &self.graph,
                    &self.inboxes,
                    &self.parked_inbound,
                    self.materialization_requester.as_deref(),
                    &self.local_node_id,
                    &self.local_node_id,
                    task_id,
                    task_json,
                    record,
                )
                .await;
            }
            _ => {
                crate::service::ipc::IpcServer::deliver_inbound_task(
                    &self.inboxes,
                    &self.local_node_id,
                    &job.target_role,
                    target_role_record.as_ref().map(|r| r.guest_id.as_str()),
                    task_id,
                    task_json,
                )
                .await;
            }
        }

        info!(
            "CronTicker: fired job {} → role={} epoch={}",
            job.id, job.target_role, fire_epoch
        );

        // Guaranteed: broadcast CronFired so peer hotels suppress their offset fire.
        if job.guaranteed {
            self.broadcast_cron_fired(job, fire_epoch, now_ms).await;
        }

        if let Err(e) = self.advance_schedule(job, fire_epoch) {
            error!(
                "CronTicker: failed to advance schedule for job {}: {e}",
                job.id
            );
        }
    }

    async fn broadcast_cron_fired(&self, job: &CronJob, fire_epoch: u64, now_ms: u64) {
        let payload = serde_json::json!({
            "job_id": job.id,
            "fire_epoch": fire_epoch,
            "fired_by": self.local_node_id,
        })
        .to_string();

        let env = EventEnvelope {
            event_id: Uuid::new_v4(),
            seq: 0,
            source_node_id: self.local_node_id.clone(),
            target_node_id: None,
            source_agent_id: "cron-ticker".into(),
            target_agent_id: None,
            kind: EventKind::CronFired,
            corr_id: format!("cron-fired:{}:{}", job.id, fire_epoch),
            attempt: 0,
            created_at: now_ms,
            expires_at: None,
            payload: EventPayload::Inline { data: payload },
            trace: vec![format!("cron-ticker:{}", job.id)],
        };

        if let Err(e) = self
            .dispatcher_tx
            .send(LedgerCommand::AppendLocal(env))
            .await
        {
            warn!(
                "CronTicker: failed to broadcast CronFired for job {}: {e}",
                job.id
            );
        }
    }

    /// Broadcast a `CronJobSync` upsert so peer hotels replicate this job definition.
    pub(crate) async fn broadcast_cron_job_sync_upsert(&self, job: &CronJob, now_ms: u64) {
        let payload = serde_json::json!({
            "op": "upsert",
            "job": job,
        })
        .to_string();

        let env = EventEnvelope {
            event_id: Uuid::new_v4(),
            seq: 0,
            source_node_id: self.local_node_id.clone(),
            target_node_id: None,
            source_agent_id: "cron-ticker".into(),
            target_agent_id: None,
            kind: EventKind::CronJobSync,
            corr_id: format!("cron-sync:{}", job.id),
            attempt: 0,
            created_at: now_ms,
            expires_at: None,
            payload: EventPayload::Inline { data: payload },
            trace: vec![format!("cron-sync:{}", job.id)],
        };

        if let Err(e) = self
            .dispatcher_tx
            .send(LedgerCommand::AppendLocal(env))
            .await
        {
            warn!(
                "CronTicker: failed to broadcast CronJobSync upsert for {}: {e}",
                job.id
            );
        }
    }

    /// Broadcast a `CronJobSync` remove so peer hotels drop this job definition.
    pub(crate) async fn broadcast_cron_job_sync_remove(&self, job_id: &str, now_ms: u64) {
        let payload = serde_json::json!({
            "op": "remove",
            "job_id": job_id,
        })
        .to_string();

        let env = EventEnvelope {
            event_id: Uuid::new_v4(),
            seq: 0,
            source_node_id: self.local_node_id.clone(),
            target_node_id: None,
            source_agent_id: "cron-ticker".into(),
            target_agent_id: None,
            kind: EventKind::CronJobSync,
            corr_id: format!("cron-sync-remove:{}", job_id),
            attempt: 0,
            created_at: now_ms,
            expires_at: None,
            payload: EventPayload::Inline { data: payload },
            trace: vec![format!("cron-sync-remove:{}", job_id)],
        };

        if let Err(e) = self
            .dispatcher_tx
            .send(LedgerCommand::AppendLocal(env))
            .await
        {
            warn!(
                "CronTicker: failed to broadcast CronJobSync remove for {}: {e}",
                job_id
            );
        }
    }

    fn advance_schedule(&self, job: &CronJob, fire_epoch: u64) -> anyhow::Result<()> {
        let next = next_fire_after(&job.schedule, fire_epoch).unwrap_or_else(|e| {
            warn!(
                "CronTicker: no future occurrences for job {} ({}); disabling",
                job.id, e
            );
            u64::MAX
        });

        let mut updated = job.clone();
        updated.last_fired_epoch = Some(fire_epoch);

        if next == u64::MAX {
            updated.enabled = false;
        } else {
            updated.next_fire_at = next;
        }

        self.graph.upsert_cron_job(&updated)
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn build_cron_task_json(
    job: &CronJob,
    fire_epoch: u64,
    local_node_id: &str,
    payload_data: String,
) -> String {
    let Ok(payload_json) = serde_json::from_str::<serde_json::Value>(&payload_data) else {
        return serde_json::json!({
            "cron_job_id": job.id,
            "target_role": job.target_role,
            "fire_epoch": fire_epoch,
            "payload": payload_data,
        })
        .to_string();
    };

    let Some(signal_seed) = payload_json.get("paracrine_signal") else {
        return serde_json::json!({
            "cron_job_id": job.id,
            "target_role": job.target_role,
            "fire_epoch": fire_epoch,
            "payload": payload_data,
        })
        .to_string();
    };

    let mut signal = signal_seed
        .as_object()
        .cloned()
        .unwrap_or_else(serde_json::Map::new);
    signal
        .entry("signal_id")
        .or_insert_with(|| serde_json::Value::String(format!("cron:{}:{fire_epoch}", job.id)));
    signal
        .entry("signal_type")
        .or_insert_with(|| serde_json::Value::String("heartbeat".into()));
    signal
        .entry("scope")
        .or_insert_with(|| serde_json::Value::String("hotel".into()));
    signal
        .entry("source_node")
        .or_insert_with(|| serde_json::Value::String(local_node_id.to_string()));
    signal
        .entry("source_hotel")
        .or_insert_with(|| serde_json::Value::String(local_node_id.to_string()));
    signal
        .entry("target_role_type")
        .or_insert_with(|| serde_json::Value::String(job.target_role.clone()));
    signal
        .entry("cadence")
        .or_insert_with(|| serde_json::Value::String(job.schedule.clone()));
    signal
        .entry("priority")
        .or_insert_with(|| serde_json::Value::String("normal".into()));
    signal
        .entry("observed_at")
        .or_insert_with(|| serde_json::Value::Number(fire_epoch.into()));
    signal
        .entry("policy_tags")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    signal
        .entry("subject_refs")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));

    serde_json::json!({
        "action": "paracrine_signal",
        "source": "cron-ticker",
        "transport": "cron",
        "cron_job_id": job.id,
        "target_role": job.target_role,
        "fire_epoch": fire_epoch,
        "payload": payload_json,
        "paracrine_signal": serde_json::Value::Object(signal),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::cron::CronJobSource;

    fn test_job() -> CronJob {
        CronJob {
            id: "job-1".into(),
            schedule: "0 */15 * * * * *".into(),
            target_role: "attention-steward".into(),
            target_node_id: None,
            payload: "{}".into(),
            guaranteed: false,
            enabled: true,
            last_fired_epoch: None,
            next_fire_at: 1_000,
            created_at: 900,
            created_by: CronJobSource::Operator,
        }
    }

    #[test]
    fn cron_payload_without_paracrine_signal_keeps_legacy_task_shape() {
        let task = build_cron_task_json(
            &test_job(),
            1_234,
            "mac-jane-aiua-01",
            r#"{"hello":"world"}"#.into(),
        );
        let value: serde_json::Value = serde_json::from_str(&task).unwrap();

        assert_eq!(value["cron_job_id"], "job-1");
        assert_eq!(value["target_role"], "attention-steward");
        assert_eq!(value["fire_epoch"], 1_234);
        assert_eq!(value["payload"], r#"{"hello":"world"}"#);
        assert!(value.get("action").is_none());
    }

    #[test]
    fn cron_payload_with_paracrine_signal_builds_normalized_signal_task() {
        let task = build_cron_task_json(
            &test_job(),
            1_234,
            "mac-jane-aiua-01",
            r#"{
                "paracrine_signal": {
                    "signal_type": "life_graph.attention_scan",
                    "scope": "life_graph",
                    "policy_tags": ["observe_only"]
                },
                "payload_summary": "scan open loops"
            }"#
            .into(),
        );
        let value: serde_json::Value = serde_json::from_str(&task).unwrap();

        assert_eq!(value["action"], "paracrine_signal");
        assert_eq!(value["transport"], "cron");
        assert_eq!(value["cron_job_id"], "job-1");
        assert_eq!(value["paracrine_signal"]["signal_id"], "cron:job-1:1234");
        assert_eq!(
            value["paracrine_signal"]["signal_type"],
            "life_graph.attention_scan"
        );
        assert_eq!(value["paracrine_signal"]["source_node"], "mac-jane-aiua-01");
        assert_eq!(
            value["paracrine_signal"]["source_hotel"],
            "mac-jane-aiua-01"
        );
        assert_eq!(
            value["paracrine_signal"]["target_role_type"],
            "attention-steward"
        );
        assert_eq!(value["paracrine_signal"]["cadence"], "0 */15 * * * * *");
        assert_eq!(value["paracrine_signal"]["observed_at"], 1_234);
        assert_eq!(value["paracrine_signal"]["policy_tags"][0], "observe_only");
    }

    #[tokio::test]
    async fn fire_parks_and_materializes_dormant_role_incarnation_instead_of_dropping_task() {
        use ansible_mesh_core::NodeCapabilities;
        use ansible_mesh_core::graph::{RoleReadinessState, TurnLoopConfig};
        use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
        use ansible_mesh_core::storage::HotelRecord;
        use async_trait::async_trait;
        use std::collections::HashMap;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::sync::Mutex;

        struct MockRequester {
            calls: AtomicUsize,
            last_guest_id: Mutex<Option<String>>,
        }

        #[async_trait]
        impl GuestMaterializationRequester for MockRequester {
            async fn ensure_guest_active(&self, guest_id: &str) -> anyhow::Result<bool> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                *self.last_guest_id.lock().await = Some(guest_id.to_string());
                Ok(true)
            }
        }

        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: "/tmp/cron-ticker-test.sock".into(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-test".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-test:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: true,
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
            })
            .expect("seed role incarnation");

        let mut job = test_job();
        job.target_role = "role:agent-test:orchestrator".into();

        // Buffered and never read — `fire()` only ever pushes one ledger entry per call,
        // so the receiver just needs to stay alive to keep the send from failing.
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let requester = Arc::new(MockRequester {
            calls: AtomicUsize::new(0),
            last_guest_id: Mutex::new(None),
        });
        let parked_inbound: crate::service::ipc::ParkedInboundRegistry =
            Arc::new(Mutex::new(HashMap::new()));
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));

        let ticker = CronTicker::new(
            graph.clone(),
            dispatcher_tx,
            inboxes,
            "local-aiua-01",
            0,
            parked_inbound.clone(),
            Some(requester.clone() as Arc<dyn GuestMaterializationRequester>),
        );

        ticker.fire(&job, 1_000).await;

        assert_eq!(
            requester.calls.load(Ordering::SeqCst),
            1,
            "fire() should trigger on-demand materialization for the dormant role guest"
        );
        assert_eq!(
            requester.last_guest_id.lock().await.as_deref(),
            Some("agent-test:orchestrator"),
            "materialization must target the role incarnation's own guest_id, not a \
             cross-hotel philote-{{role}} placeholder"
        );

        let parked = parked_inbound.lock().await;
        assert_eq!(
            parked.get("agent-test:orchestrator").map(Vec::len),
            Some(1),
            "task should be parked under the role incarnation's own guest_id, not dropped"
        );

        let guest = graph
            .get_guest("local-hotel", "agent-test:orchestrator")
            .expect("get_guest should not error")
            .expect("on-demand materialization should have upserted the local role guest record");
        assert!(
            guest.is_active,
            "materialization must flip the dormant role guest active, not leave it dead"
        );
    }
}
