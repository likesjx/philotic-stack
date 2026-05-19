use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::materializer::Materializer;
use anyhow::{Context, Result};
use async_trait::async_trait;
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
    db_path: String,
}

impl LocalProcessMaterializer {
    pub fn new(db_path: impl Into<String>) -> Self {
        Self {
            children: HashMap::new(),
            db_path: db_path.into(),
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
            .arg("stat=")
            .stderr(Stdio::null())
            .output()
            .map(|output| {
                if !output.status.success() {
                    return false;
                }
                let stat = String::from_utf8_lossy(&output.stdout).trim().to_string();
                !stat.is_empty() && !stat.starts_with('Z')
            })
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
            // Always override with hotel's current socket path so stale DB values
            // don't cause guests to connect to the wrong socket.
            if let Ok(socket) = std::env::var("PHILOTIC_HOTEL_SOCKET") {
                if !socket.trim().is_empty() {
                    command.env("PHILOTIC_HOTEL_SOCKET", socket.trim());
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
        if let Ok(local_graph) = crate::graph::ContextGraph::open(&self.db_path) {
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
    graph: Arc<GraphDomain>,
    materializer: Arc<Mutex<Box<dyn Materializer>>>,
}

#[async_trait]
pub trait GuestMaterializationRequester: Send + Sync {
    async fn ensure_guest_active(&self, guest_id: &str) -> Result<bool>;
}

impl GuestManager {
    pub fn new(
        hotel_name: impl Into<String>,
        graph: Arc<GraphDomain>,
        materializer: Box<dyn Materializer>,
    ) -> Self {
        Self {
            hotel_name: hotel_name.into(),
            graph,
            materializer: Arc::new(Mutex::new(materializer)),
        }
    }

    fn clear_guest_pid(graph: &GraphDomain, hotel_name: &str, guest_id: &str) {
        let _ = graph.set_guest_pid(hotel_name, guest_id, None);
    }

    fn refresh_guest_record(
        graph: &GraphDomain,
        hotel_name: &str,
        guest_id: &str,
    ) -> Result<Option<ansible_mesh_core::storage::GuestRecord>> {
        Ok(graph
            .list_guests(hotel_name, false)?
            .into_iter()
            .find(|guest| guest.guest_id == guest_id))
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

    pub async fn ensure_guest_active(&self, guest_id: &str) -> Result<bool> {
        // Role incarnations (e.g. "agent-bjork-01:orchestrator") are not separate entries
        // in materialized_guests — they live inside the base philote process. When a role
        // sub-guest ID is requested and not found directly, fall back to the base agent ID.
        let effective_id = match Self::refresh_guest_record(
            self.graph.as_ref(),
            &self.hotel_name,
            guest_id,
        )? {
            Some(_) => guest_id.to_string(),
            None => {
                if let Some(base_id) = guest_id.split_once(':').map(|(base, _)| base) {
                    if Self::refresh_guest_record(self.graph.as_ref(), &self.hotel_name, base_id)?
                        .is_some()
                    {
                        info!(
                            "On-demand materialization: role guest [{}] not in materialized_guests; \
                             falling back to base agent [{}].",
                            guest_id, base_id
                        );
                        base_id.to_string()
                    } else {
                        return Ok(false);
                    }
                } else {
                    return Ok(false);
                }
            }
        };
        // Acquire the spawn lock BEFORE reading active_pid so that concurrent callers
        // see a fresh DB snapshot rather than racing on a stale active_pid=None
        // (TOCTOU: multiple concurrent ensure_guest_active calls each saw None, each spawned).
        let mut mat = self.materializer.lock().await;
        let Some(current_rec) =
            Self::refresh_guest_record(self.graph.as_ref(), &self.hotel_name, &effective_id)?
        else {
            return Ok(false);
        };
        if !current_rec.is_active {
            return Ok(false);
        }
        if let Some(active_pid) = current_rec.active_pid.as_deref() {
            let is_live = mat.check_status(&current_rec.guest_id, active_pid).await?;
            if is_live {
                return Ok(true);
            }
            warn!(
                "On-demand materialization: Guest [{}] had stale PID [{}]. Reclaiming before respawn.",
                current_rec.guest_id, active_pid
            );
            if let Err(err) = mat.reclaim_guest(&current_rec.guest_id).await {
                warn!(
                    "On-demand materialization: reclaim failed for [{}]: {}",
                    current_rec.guest_id, err
                );
            }
            Self::clear_guest_pid(self.graph.as_ref(), &self.hotel_name, &current_rec.guest_id);
        }

        let config: serde_json::Value =
            serde_json::from_str(&current_rec.config_json).unwrap_or_default();
        match mat.spawn_guest(&current_rec.guest_id, &config).await {
            Ok(new_pid) => {
                info!(
                    "On-demand materialization: spawned Guest [{}] (PID {}).",
                    current_rec.guest_id, new_pid
                );
                self.graph.set_guest_pid(
                    &self.hotel_name,
                    &current_rec.guest_id,
                    Some(&new_pid),
                )?;
                Ok(true)
            }
            Err(err) => {
                error!(
                    "On-demand materialization: failed to spawn Guest [{}]: {}",
                    current_rec.guest_id, err
                );
                Ok(false)
            }
        }
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
            let mut mat = self.materializer.lock().await;

            if rec.is_active {
                // TTL check: if this guest is a role incarnation with inactive_ttl_seconds set,
                // and it has been idle longer than that TTL, deactivate it rather than respawning.
                // Non-membrane-owner role guests only — membrane-owner guests are never reclaimed by TTL.
                let ttl_expired = {
                    let role_incarnations = self
                        .graph
                        .list_role_incarnations_by_guest_id(&rec.guest_id)
                        .unwrap_or_default();
                    if let Some(role_rec) = role_incarnations.first() {
                        if let Some(ttl_secs) = role_rec.inactive_ttl_seconds {
                            if let Some(last_active) = rec.last_active_at {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs();
                                now.saturating_sub(last_active) > ttl_secs
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };
                if ttl_expired {
                    info!(
                        "Supervisor: Guest [{}] has exceeded its role TTL. Deactivating.",
                        rec.guest_id
                    );
                    let _ = self
                        .graph
                        .set_guest_active(&self.hotel_name, &rec.guest_id, false);
                    if let Some(_pid) = rec.active_pid.as_deref() {
                        let mut mat = self.materializer.lock().await;
                        let _ = mat.reclaim_guest(&rec.guest_id).await;
                        drop(mat);
                    }
                    Self::clear_guest_pid(self.graph.as_ref(), &self.hotel_name, &rec.guest_id);
                    continue;
                }

                let mut should_spawn = rec.active_pid.is_none();
                if let Some(active_pid) = rec.active_pid.as_deref() {
                    let is_live = mat.check_status(&rec.guest_id, active_pid).await?;
                    if !is_live {
                        warn!(
                            "Supervisor: Guest [{}] is marked active but PID [{}] is dead/stale. Reclaiming and respawning.",
                            rec.guest_id, active_pid
                        );
                        if let Err(e) = mat.reclaim_guest(&rec.guest_id).await {
                            warn!(
                                "Supervisor: reclaim during stale active guest cleanup failed for [{}]: {}",
                                rec.guest_id, e
                            );
                        }
                        Self::clear_guest_pid(self.graph.as_ref(), &self.hotel_name, &rec.guest_id);
                        should_spawn = true;
                    }
                }

                if should_spawn {
                    let Some(current_rec) = Self::refresh_guest_record(
                        self.graph.as_ref(),
                        &self.hotel_name,
                        &rec.guest_id,
                    )?
                    else {
                        info!(
                            "Supervisor: Guest [{}] disappeared from desired state before respawn. Skipping stale snapshot respawn.",
                            rec.guest_id
                        );
                        continue;
                    };
                    if !current_rec.is_active {
                        info!(
                            "Supervisor: Guest [{}] is no longer active in desired state before respawn. Skipping.",
                            rec.guest_id
                        );
                        continue;
                    }
                    let config: serde_json::Value =
                        serde_json::from_str(&current_rec.config_json).unwrap_or_default();
                    info!(
                        "Supervisor: Guest [{}] is marked active but has no ID. Spawning...",
                        current_rec.guest_id
                    );
                    match mat.spawn_guest(&current_rec.guest_id, &config).await {
                        Ok(new_pid) => {
                            info!(
                                "Supervisor: ✨ Spawned missing Guest [{}] (ID: {})",
                                current_rec.guest_id, new_pid
                            );
                            let _ = self.graph.set_guest_pid(
                                &self.hotel_name,
                                &current_rec.guest_id,
                                Some(&new_pid),
                            );
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            let _ = self.graph.set_guest_last_active(
                                &self.hotel_name,
                                &current_rec.guest_id,
                                now,
                            );
                        }
                        Err(e) => error!(
                            "Supervisor: ❌ Failed to spawn missing Guest [{}]: {}",
                            current_rec.guest_id, e
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

#[async_trait]
impl GuestMaterializationRequester for GuestManager {
    async fn ensure_guest_active(&self, guest_id: &str) -> Result<bool> {
        Self::ensure_guest_active(self, guest_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::domain::GraphDomain;
    use ansible_mesh_core::graph::{GraphEdge, GraphNode};
    use ansible_mesh_core::storage::{GraphAdapter, GuestRecord};
    use anyhow::Result;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Minimal `GraphAdapter` that stores nodes in memory.
    /// Tracks calls to `list_nodes_by_kind("guest")` so we can simulate
    /// the race condition in `reconcile_all_skips_respawn_when_guest_was_removed_after_snapshot`.
    struct TestGraphAdapter {
        nodes: StdMutex<HashMap<String, GraphNode>>,
        list_guest_calls: AtomicUsize,
        clear_guests_on_second_list: bool,
    }

    impl TestGraphAdapter {
        fn with_guests(guests: Vec<GuestRecord>) -> Self {
            let mut nodes = HashMap::new();
            for rec in &guests {
                let key = format!("guest:{}:{}", rec.hotel_name, rec.guest_id);
                nodes.insert(
                    key.clone(),
                    GraphNode {
                        node_key: key,
                        kind: "guest".to_string(),
                        label: Some(rec.guest_id.clone()),
                        data: serde_json::to_value(rec).unwrap(),
                    },
                );
            }
            Self {
                nodes: StdMutex::new(nodes),
                list_guest_calls: AtomicUsize::new(0),
                clear_guests_on_second_list: false,
            }
        }

        fn with_guests_cleared_on_second_list(guests: Vec<GuestRecord>) -> Self {
            let mut adapter = Self::with_guests(guests);
            adapter.clear_guests_on_second_list = true;
            adapter
        }
    }

    impl GraphAdapter for TestGraphAdapter {
        fn upsert_node(&self, node: &GraphNode) -> Result<()> {
            self.nodes
                .lock()
                .unwrap()
                .insert(node.node_key.clone(), node.clone());
            Ok(())
        }

        fn get_node(&self, node_key: &str) -> Result<Option<GraphNode>> {
            Ok(self.nodes.lock().unwrap().get(node_key).cloned())
        }

        fn delete_node(&self, node_key: &str) -> Result<()> {
            self.nodes.lock().unwrap().remove(node_key);
            Ok(())
        }

        fn list_nodes_by_kind(&self, kind: &str) -> Result<Vec<GraphNode>> {
            if kind == "guest" {
                let call_index = self.list_guest_calls.fetch_add(1, Ordering::SeqCst);
                if self.clear_guests_on_second_list && call_index >= 1 {
                    self.nodes.lock().unwrap().retain(|_, n| n.kind != "guest");
                }
            }
            Ok(self
                .nodes
                .lock()
                .unwrap()
                .values()
                .filter(|n| n.kind == kind)
                .cloned()
                .collect())
        }

        fn upsert_edge(&self, _edge: &GraphEdge) -> Result<()> {
            Ok(())
        }

        fn delete_edge(&self, _edge_key: &str) -> Result<()> {
            Ok(())
        }

        fn list_edges_from(
            &self,
            _src_node_key: &str,
            _edge_kind: Option<&str>,
        ) -> Result<Vec<GraphEdge>> {
            Ok(vec![])
        }
    }

    fn make_domain(adapter: TestGraphAdapter) -> Arc<GraphDomain> {
        Arc::new(GraphDomain::new(Arc::new(adapter)))
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
        let mut materializer = LocalProcessMaterializer::new("aiua_context.db");
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
        let graph = make_domain(TestGraphAdapter::with_guests(vec![GuestRecord {
            hotel_name: "test-hotel".into(),
            guest_id: "guest-1".into(),
            role: "agent".into(),
            config_json: json!({ "command": "target/debug/philote" }).to_string(),
            is_active: true,
            active_pid: Some(pid.clone()),
            last_active_at: None,
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
    async fn reconcile_all_respawns_active_guest_when_status_check_disagrees() {
        let pid = "424242".to_string();
        let graph = make_domain(TestGraphAdapter::with_guests(vec![GuestRecord {
            hotel_name: "test-hotel".into(),
            guest_id: "guest-2".into(),
            role: "membrane".into(),
            config_json: json!({ "command": "target/debug/membrane" }).to_string(),
            is_active: true,
            active_pid: Some(pid.clone()),
            last_active_at: None,
        }]));

        let mock = MockMaterializer::new(HashMap::from([(pid.clone(), false)]));
        let spawn_count = mock.spawn_count.clone();
        let reclaim_count = mock.reclaim_count.clone();
        let manager = GuestManager::new("test-hotel", graph.clone(), Box::new(mock));

        manager
            .reconcile_all()
            .await
            .expect("reconcile should succeed");

        assert_eq!(spawn_count.load(Ordering::SeqCst), 1);
        assert_eq!(reclaim_count.load(Ordering::SeqCst), 1);
        let guests = graph.list_guests("test-hotel", false).expect("list guests");
        assert_eq!(guests[0].active_pid.as_deref(), Some("spawned-1"));
    }

    #[tokio::test]
    async fn reconcile_all_skips_respawn_when_guest_was_removed_after_snapshot() {
        let pid = "424243".to_string();
        let graph = make_domain(TestGraphAdapter::with_guests_cleared_on_second_list(vec![
            GuestRecord {
                hotel_name: "test-hotel".into(),
                guest_id: "guest-3".into(),
                role: "membrane".into(),
                config_json: json!({ "command": "target/debug/membrane" }).to_string(),
                is_active: true,
                active_pid: Some(pid.clone()),
                last_active_at: None,
            },
        ]));

        let mock = MockMaterializer::new(HashMap::from([(pid.clone(), false)]));
        let spawn_count = mock.spawn_count.clone();
        let reclaim_count = mock.reclaim_count.clone();
        let manager = GuestManager::new("test-hotel", graph.clone(), Box::new(mock));

        manager
            .reconcile_all()
            .await
            .expect("reconcile should succeed");

        assert_eq!(spawn_count.load(Ordering::SeqCst), 0);
        assert_eq!(reclaim_count.load(Ordering::SeqCst), 1);
        let guests = graph.list_guests("test-hotel", false).expect("list guests");
        assert!(guests.is_empty());
    }
}
