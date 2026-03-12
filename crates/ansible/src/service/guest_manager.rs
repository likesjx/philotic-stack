use ansible_mesh_core::materializer::Materializer;
use ansible_mesh_core::storage::GraphStorage;
use anyhow::{Context, Result};
use rusqlite::types::ValueRef;
use std::collections::HashMap;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;
use tokio::process::Command as TokioCommand;
use tokio::sync::Mutex;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

/// A Universal Materializer backed by the local OS Process space.
pub struct LocalProcessMaterializer {
    children: HashMap<String, tokio::process::Child>,
}

impl LocalProcessMaterializer {
    pub fn new() -> Self {
        Self {
            children: HashMap::new(),
        }
    }

    fn pid_exists(pid: u32) -> bool {
        // `kill -0` can report EPERM under some macOS execution contexts even for
        // processes we can still see and manage. `ps` gives us a stable liveness
        // check for the supervisor without false "guest is dead" results.
        ProcessCommand::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .arg("-o")
            .arg("pid=")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn terminate_pid(pid: u32) -> bool {
        ProcessCommand::new("/bin/kill")
            .arg("-9")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn parse_pid_value(value: ValueRef<'_>) -> Option<u32> {
        match value {
            ValueRef::Integer(pid) => u32::try_from(pid).ok(),
            ValueRef::Text(text) => std::str::from_utf8(text).ok()?.parse::<u32>().ok(),
            _ => None,
        }
    }
}

#[async_trait::async_trait]
impl Materializer for LocalProcessMaterializer {
    async fn spawn_guest(
        &mut self,
        guest_id: &str,
        config_json: &serde_json::Value,
    ) -> Result<String> {
        if let Some(cmd) = config_json.get("command").and_then(|c| c.as_str()) {
            // Resolve binary path: if PHILOTIC_BIN_DIR is set and the command is not
            // already absolute, prepend the bin dir. Falls back to PATH in dev mode.
            let resolved_cmd = if std::path::Path::new(cmd).is_absolute() {
                cmd.to_string()
            } else if let Ok(bin_dir) = std::env::var("PHILOTIC_BIN_DIR") {
                format!("{}/{}", bin_dir.trim_end_matches('/'), cmd)
            } else {
                cmd.to_string()
            };
            let mut command = TokioCommand::new(&resolved_cmd);
            if let Some(args) = config_json.get("args").and_then(|a| a.as_array()) {
                for arg in args {
                    if let Some(s) = arg.as_str() {
                        command.arg(s);
                    }
                }
            }
            if let Some(env_obj) = config_json.get("env").and_then(|e| e.as_object()) {
                for (key, value) in env_obj {
                    if let Some(value) = value.as_str() {
                        command.env(key, value);
                    }
                }
            }

            let child = command
                .spawn()
                .context("Failed to spawn OS child process")?;
            let child_pid = child.id().unwrap_or(0);
            self.children.insert(guest_id.to_string(), child);

            Ok(child_pid.to_string())
        } else {
            anyhow::bail!("Invalid config_json for Local OS Materializer. Missing 'command'.");
        }
    }

    async fn reclaim_guest(&mut self, guest_id: &str) -> Result<()> {
        if let Some(mut child) = self.children.remove(guest_id) {
            let pid = child.id().unwrap_or(0);
            match child.start_kill() {
                Ok(()) => info!(
                    "Forcefully retired tracked OS Process PID {} for Guest [{}].",
                    pid, guest_id
                ),
                Err(e) => warn!(
                    "Failed to start termination for tracked Guest [{}] PID {}: {}",
                    guest_id, pid, e
                ),
            }
            let _ = child.wait().await;
            return Ok(());
        }

        // NOTE: In the trait-abstracted world, the GuestManager passes the PID
        // to reclaim_guest. For now, LocalProcessMaterializer opens a throwaway
        // connection for backwards compatibility until the caller is fully refactored.
        if let Ok(local_graph) = crate::graph::ContextGraph::open("ansible_context.db") {
            let conn = local_graph.conn.lock().unwrap();
            let mut stmt =
                conn.prepare("SELECT active_pid FROM materialized_guests WHERE guest_id = ?")?;
            let mut rows = stmt.query(rusqlite::params![guest_id])?;
            if let Some(row) = rows.next()? {
                if let Some(pid) = Self::parse_pid_value(row.get_ref(0)?) {
                    if Self::pid_exists(pid) {
                        if Self::terminate_pid(pid) {
                            info!(
                                "Forcefully retired OS Process PID {} for Guest [{}].",
                                pid, guest_id
                            );
                        } else {
                            warn!(
                                "Failed to forcefully retire OS Process PID {} for Guest [{}].",
                                pid, guest_id
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn check_status(&mut self, guest_id: &str, active_id: &str) -> Result<bool> {
        if let Some(child) = self.children.get_mut(guest_id) {
            match child
                .try_wait()
                .context("Failed to query tracked child status")?
            {
                None => return Ok(true),
                Some(status) => {
                    warn!(
                        "Tracked Guest [{}] OS Process (PID: {}) has exited with status: {}",
                        guest_id, active_id, status
                    );
                }
            }
            self.children.remove(guest_id);
            return Ok(false);
        }

        if let Ok(pid) = active_id.parse::<u32>() {
            Ok(Self::pid_exists(pid))
        } else {
            warn!(
                "Invalid PID string '{}' for Guest [{}]",
                active_id, guest_id
            );
            Ok(false)
        }
    }
}

/// A centralized orchestrator for materializing and dematerializing Guests via a plugin Materializer interface.
pub struct GuestManager {
    hotel_name: String,
    graph: Arc<dyn GraphStorage>,
    materializer: Arc<Mutex<Box<dyn Materializer>>>,
}

impl GuestManager {
    pub fn new(
        hotel_name: impl Into<String>,
        graph: Arc<dyn GraphStorage>,
        materializer: Box<dyn Materializer>,
    ) -> Self {
        Self {
            hotel_name: hotel_name.into(),
            graph,
            materializer: Arc::new(Mutex::new(materializer)),
        }
    }

    fn clear_guest_pid(graph: &dyn GraphStorage, hotel_name: &str, guest_id: &str) {
        let _ = graph.set_guest_pid(hotel_name, guest_id, None);
    }

    /// Read all `is_active=1` Guests from the Graph, reclaim orphans, and invoke the underlying `Materializer`.
    pub async fn materialize_all(&self, mut shutdown_rx: broadcast::Receiver<()>) -> Result<()> {
        info!("--- BEGIN UNIVERSAL MATERIALIZATION ---");

        let guest_records = self.graph.list_guests(&self.hotel_name, true)?;

        for rec in guest_records {
            info!(
                "Materializing Guest [{}] (Role: {})",
                rec.guest_id, rec.role
            );

            // --- GHOST RECLAMATION ---
            if let Some(_pid) = &rec.active_pid {
                info!(
                    "Context Graph shows Ghost PID {} for Guest [{}]. Reclaiming identity...",
                    _pid, rec.guest_id
                );
                let mut mat = self.materializer.lock().await;
                if let Err(e) = mat.reclaim_guest(&rec.guest_id).await {
                    warn!("Reclamation error for {}: {}", rec.guest_id, e);
                }
                Self::clear_guest_pid(self.graph.as_ref(), &self.hotel_name, &rec.guest_id);
            }

            // --- SPAWNING ---
            let config: serde_json::Value =
                serde_json::from_str(&rec.config_json).unwrap_or_default();

            let mut mat = self.materializer.lock().await;
            match mat.spawn_guest(&rec.guest_id, &config).await {
                Ok(child_pid) => {
                    info!(
                        "✨ Successfully spawned identity for Guest [{}] (ID: {})",
                        rec.guest_id, child_pid
                    );
                    let _ =
                        self.graph
                            .set_guest_pid(&self.hotel_name, &rec.guest_id, Some(&child_pid));
                }
                Err(e) => {
                    Self::clear_guest_pid(self.graph.as_ref(), &self.hotel_name, &rec.guest_id);
                    error!("❌ Failed to materialize Guest [{}]: {}", rec.guest_id, e);
                }
            }
        }

        info!("--- END UNIVERSAL MATERIALIZATION ---");

        // Detach a task to wait for the universal shutdown signal broadcast
        let _materializer_clone = self.materializer.clone();
        tokio::spawn(async move {
            let _ = shutdown_rx.recv().await;
            info!("Universal Dematerialization Shutdown Triggered. Reaping active Guests...");
            // In a real implementation we would iterate and call reclaim_guest() on all active IDs.
        });

        Ok(())
    }

    /// An infinite loop that reconciles the SQLite desired state with the active `Materializer` state.
    pub async fn supervise_guests(self: Arc<Self>, mut shutdown_rx: broadcast::Receiver<()>) {
        info!("Started Guest Supervisor Reconciliation Loop");

        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = self.reconcile_all().await {
                        error!("Guest Supervisor Reconciliation Error: {}", e);
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("Guest Supervisor received universal shutdown signal. Terminating loop.");
                    break;
                }
            }
        }
    }

    async fn reconcile_all(&self) -> Result<()> {
        let all_guests = self.graph.list_guests(&self.hotel_name, false)?;

        for rec in all_guests {
            let config: serde_json::Value =
                serde_json::from_str(&rec.config_json).unwrap_or_default();

            let mut mat = self.materializer.lock().await;

            if rec.is_active {
                // Until guests publish a durable health signal or heartbeat, an assigned PID is
                // the strongest local source of truth we have for "already materialized".
                // Startup ghost reclamation clears stale rows before the supervisor begins.
                if rec.active_pid.is_none() {
                    info!(
                        "Supervisor: Guest [{}] is marked active but has no ID. Spawning...",
                        rec.guest_id
                    );
                    match mat.spawn_guest(&rec.guest_id, &config).await {
                        Ok(new_pid) => {
                            info!(
                                "Supervisor: ✨ Spawned missing Guest [{}] (ID: {})",
                                rec.guest_id, new_pid
                            );
                            let _ = self.graph.set_guest_pid(
                                &self.hotel_name,
                                &rec.guest_id,
                                Some(&new_pid),
                            );
                        }
                        Err(e) => error!(
                            "Supervisor: ❌ Failed to spawn missing Guest [{}]: {}",
                            rec.guest_id, e
                        ),
                    }
                }
            } else {
                if rec.active_pid.is_some() {
                    info!(
                        "Supervisor: Guest [{}] is marked INACTIVE but has an ID. Reclaiming...",
                        rec.guest_id
                    );
                    if let Err(e) = mat.reclaim_guest(&rec.guest_id).await {
                        error!(
                            "Supervisor: ❌ Failed to reclaim inactive Guest [{}]: {}",
                            rec.guest_id, e
                        );
                    }
                    Self::clear_guest_pid(self.graph.as_ref(), &self.hotel_name, &rec.guest_id);
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::NodeCapabilities;
    use ansible_mesh_core::storage::{
        GuestRecord, SessionEventRecord, SessionParticipantRecord, SessionRecord, SessionTurnRecord,
    };
    use anyhow::Result;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct TestGraphStorage {
        guests: StdMutex<Vec<GuestRecord>>,
    }

    impl TestGraphStorage {
        fn with_guests(guests: Vec<GuestRecord>) -> Self {
            Self {
                guests: StdMutex::new(guests),
            }
        }
    }

    impl GraphStorage for TestGraphStorage {
        fn load_node_capabilities(&self) -> Result<Option<NodeCapabilities>> {
            Ok(None)
        }

        fn save_node_capabilities(&self, _caps: &NodeCapabilities) -> Result<()> {
            Ok(())
        }

        fn get_config_value(&self, _key: &str) -> Result<Option<String>> {
            Ok(None)
        }

        fn set_config_value(&self, _key: &str, _value_json: &str) -> Result<()> {
            Ok(())
        }

        fn upsert_secret(&self, _secret: &ansible_mesh_core::storage::SecretRecord) -> Result<()> {
            Ok(())
        }

        fn get_secret(
            &self,
            _secret_ref: &str,
        ) -> Result<Option<ansible_mesh_core::storage::SecretRecord>> {
            Ok(None)
        }

        fn get_hotel(
            &self,
            _hotel_name: &str,
        ) -> Result<Option<ansible_mesh_core::storage::HotelRecord>> {
            Ok(None)
        }

        fn list_hotels(&self) -> Result<Vec<ansible_mesh_core::storage::HotelRecord>> {
            Ok(vec![])
        }

        fn upsert_hotel(&self, _hotel: &ansible_mesh_core::storage::HotelRecord) -> Result<()> {
            Ok(())
        }

        fn set_hotel_pid(&self, _hotel_name: &str, _pid: Option<&str>) -> Result<()> {
            Ok(())
        }

        fn list_guests(&self, hotel_name: &str, active_only: bool) -> Result<Vec<GuestRecord>> {
            let guests = self.guests.lock().unwrap();
            if active_only {
                Ok(guests
                    .iter()
                    .filter(|g| g.hotel_name == hotel_name && g.is_active)
                    .cloned()
                    .collect())
            } else {
                Ok(guests
                    .iter()
                    .filter(|g| g.hotel_name == hotel_name)
                    .cloned()
                    .collect())
            }
        }

        fn set_guest_pid(&self, hotel_name: &str, guest_id: &str, pid: Option<&str>) -> Result<()> {
            let mut guests = self.guests.lock().unwrap();
            let rec = guests
                .iter_mut()
                .find(|g| g.hotel_name == hotel_name && g.guest_id == guest_id)
                .expect("guest should exist");
            rec.active_pid = pid.map(|value| value.to_string());
            Ok(())
        }

        fn seed_guests(&self, _hotel_name: &str, guests: &[GuestRecord]) -> Result<()> {
            let mut stored = self.guests.lock().unwrap();
            *stored = guests.to_vec();
            Ok(())
        }

        fn upsert_agent_identity(
            &self,
            _identity: &ansible_mesh_core::storage::AgentIdentityRecord,
        ) -> Result<()> {
            Ok(())
        }

        fn get_agent_identity(
            &self,
            _agent_id: &str,
        ) -> Result<Option<ansible_mesh_core::storage::AgentIdentityRecord>> {
            Ok(None)
        }

        fn sync_apartment(
            &self,
            _agent_id: &str,
            _memory_type: &str,
            _content_json: &serde_json::Value,
        ) -> Result<()> {
            Ok(())
        }

        fn get_apartment(
            &self,
            _agent_id: &str,
            _memory_type: &str,
        ) -> Result<Option<serde_json::Value>> {
            Ok(None)
        }

        fn upsert_session(&self, _session: &SessionRecord) -> Result<()> {
            Ok(())
        }

        fn get_session(&self, _session_id: &str) -> Result<Option<SessionRecord>> {
            Ok(None)
        }

        fn upsert_session_participant(
            &self,
            _participant: &SessionParticipantRecord,
        ) -> Result<()> {
            Ok(())
        }

        fn list_session_participants(
            &self,
            _session_id: &str,
        ) -> Result<Vec<SessionParticipantRecord>> {
            Ok(vec![])
        }

        fn upsert_session_turn(&self, _turn: &SessionTurnRecord) -> Result<()> {
            Ok(())
        }

        fn get_session_turn(
            &self,
            _session_id: &str,
            _turn_id: &str,
        ) -> Result<Option<SessionTurnRecord>> {
            Ok(None)
        }

        fn list_session_turns(
            &self,
            _session_id: &str,
            _limit: usize,
        ) -> Result<Vec<SessionTurnRecord>> {
            Ok(vec![])
        }

        fn append_session_event(&self, _event: &SessionEventRecord) -> Result<()> {
            Ok(())
        }

        fn list_session_events(
            &self,
            _session_id: &str,
            _limit: usize,
        ) -> Result<Vec<SessionEventRecord>> {
            Ok(vec![])
        }

        fn upsert_abstract_tool(
            &self,
            _tool: &ansible_mesh_core::graph::AbstractToolRecord,
        ) -> Result<()> {
            Ok(())
        }

        fn get_abstract_tool(
            &self,
            _tool_name: &str,
        ) -> Result<Option<ansible_mesh_core::graph::AbstractToolRecord>> {
            Ok(None)
        }

        fn list_abstract_tools(&self) -> Result<Vec<ansible_mesh_core::graph::AbstractToolRecord>> {
            Ok(vec![])
        }
    }

    struct MockMaterializer {
        spawn_count: Arc<AtomicUsize>,
        reclaim_count: Arc<AtomicUsize>,
        status_by_pid: HashMap<String, bool>,
    }

    impl MockMaterializer {
        fn new(status_by_pid: HashMap<String, bool>) -> Self {
            Self {
                spawn_count: Arc::new(AtomicUsize::new(0)),
                reclaim_count: Arc::new(AtomicUsize::new(0)),
                status_by_pid,
            }
        }
    }

    #[async_trait::async_trait]
    impl Materializer for MockMaterializer {
        async fn spawn_guest(
            &mut self,
            _guest_id: &str,
            _config_json: &serde_json::Value,
        ) -> Result<String> {
            let next = self.spawn_count.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(format!("spawned-{next}"))
        }

        async fn reclaim_guest(&mut self, _guest_id: &str) -> Result<()> {
            self.reclaim_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn check_status(&mut self, _guest_id: &str, active_id: &str) -> Result<bool> {
            Ok(*self.status_by_pid.get(active_id).unwrap_or(&false))
        }
    }

    #[tokio::test]
    async fn local_process_materializer_tracks_spawned_child_status() {
        let mut materializer = LocalProcessMaterializer::new();
        let guest_id = "sleepy-guest";
        let pid = materializer
            .spawn_guest(
                guest_id,
                &json!({
                    "command": "/bin/sleep",
                    "args": ["30"]
                }),
            )
            .await
            .expect("spawn child");

        assert!(
            materializer
                .check_status(guest_id, &pid)
                .await
                .expect("status query should succeed")
        );

        materializer
            .reclaim_guest(guest_id)
            .await
            .expect("reclaim child");

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        assert!(
            !materializer
                .check_status(guest_id, &pid)
                .await
                .expect("status after reclaim should succeed")
        );
    }

    #[test]
    fn parse_pid_value_accepts_integer_and_text_sqlite_cells() {
        assert_eq!(
            LocalProcessMaterializer::parse_pid_value(ValueRef::Integer(1234)),
            Some(1234)
        );
        assert_eq!(
            LocalProcessMaterializer::parse_pid_value(ValueRef::Text(b"5678")),
            Some(5678)
        );
        assert_eq!(
            LocalProcessMaterializer::parse_pid_value(ValueRef::Null),
            None
        );
    }

    #[tokio::test]
    async fn reconcile_all_does_not_respawn_healthy_active_guest() {
        let pid = std::process::id().to_string();
        let graph: Arc<dyn GraphStorage> =
            Arc::new(TestGraphStorage::with_guests(vec![GuestRecord {
                hotel_name: "test-hotel".into(),
                guest_id: "guest-1".into(),
                role: "agent".into(),
                config_json: json!({ "command": "target/debug/agent-core" }).to_string(),
                is_active: true,
                active_pid: Some(pid.clone()),
            }]));

        let mock = MockMaterializer::new(HashMap::from([(pid.clone(), true)]));
        let spawn_count = mock.spawn_count.clone();
        let reclaim_count = mock.reclaim_count.clone();
        let manager = GuestManager::new("test-hotel", graph.clone(), Box::new(mock));

        manager
            .reconcile_all()
            .await
            .expect("reconcile should succeed");

        assert_eq!(spawn_count.load(Ordering::SeqCst), 0);
        assert_eq!(reclaim_count.load(Ordering::SeqCst), 0);

        let guests = graph.list_guests("test-hotel", false).expect("list guests");
        assert_eq!(guests[0].active_pid.as_deref(), Some(pid.as_str()));
    }

    #[tokio::test]
    async fn reconcile_all_does_not_respawn_active_guest_just_because_status_check_disagrees() {
        let pid = "424242".to_string();
        let graph: Arc<dyn GraphStorage> =
            Arc::new(TestGraphStorage::with_guests(vec![GuestRecord {
                hotel_name: "test-hotel".into(),
                guest_id: "guest-2".into(),
                role: "membrane".into(),
                config_json: json!({ "command": "target/debug/membrane" }).to_string(),
                is_active: true,
                active_pid: Some(pid.clone()),
            }]));

        let mock = MockMaterializer::new(HashMap::from([(pid.clone(), false)]));
        let spawn_count = mock.spawn_count.clone();
        let manager = GuestManager::new("test-hotel", graph.clone(), Box::new(mock));

        manager
            .reconcile_all()
            .await
            .expect("reconcile should succeed");

        assert_eq!(spawn_count.load(Ordering::SeqCst), 0);
        let guests = graph.list_guests("test-hotel", false).expect("list guests");
        assert_eq!(guests[0].active_pid.as_deref(), Some(pid.as_str()));
    }
}
