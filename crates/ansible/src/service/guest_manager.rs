use anyhow::{Context, Result};
use ansible_mesh_core::materializer::Materializer;
use ansible_mesh_core::storage::GraphStorage;
use std::process::{Command as ProcessCommand, Stdio};
use tokio::process::Command as TokioCommand;
use tokio::sync::broadcast;
use tracing::{error, info, warn};
use std::sync::Arc;
use tokio::sync::Mutex;

/// A Universal Materializer backed by the local OS Process space.
pub struct LocalProcessMaterializer {}

impl LocalProcessMaterializer {
    pub fn new() -> Self {
        Self {}
    }

    fn pid_exists(pid: u32) -> bool {
        ProcessCommand::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn terminate_pid(pid: u32) -> bool {
        ProcessCommand::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

#[async_trait::async_trait]
impl Materializer for LocalProcessMaterializer {
    async fn spawn_guest(&mut self, guest_id: &str, config_json: &serde_json::Value) -> Result<String> {
        if let Some(cmd) = config_json.get("command").and_then(|c| c.as_str()) {
            let mut command = TokioCommand::new(cmd);
            if let Some(args) = config_json.get("args").and_then(|a| a.as_array()) {
                for arg in args {
                    if let Some(s) = arg.as_str() {
                        command.arg(s);
                    }
                }
            }
            
            let mut child = command.spawn().context("Failed to spawn OS child process")?;
            let child_pid = child.id().unwrap_or(0);
            
            // We spawn a detached monitor task to gracefully reap it if it unexpectedly exits voluntarily
            let guest_id_clone = guest_id.to_string();
            tokio::spawn(async move {
                if let Ok(status) = child.wait().await {
                    warn!("Guest [{}] OS Process (PID: {}) has exited with status: {}", guest_id_clone, child_pid, status);
                }
            });

            Ok(child_pid.to_string())
        } else {
            anyhow::bail!("Invalid config_json for Local OS Materializer. Missing 'command'.");
        }
    }

    async fn reclaim_guest(&mut self, guest_id: &str) -> Result<()> {
        // NOTE: In the trait-abstracted world, the GuestManager passes the PID
        // to reclaim_guest. For now, LocalProcessMaterializer opens a throwaway
        // connection for backwards compatibility until the caller is fully refactored.
        if let Ok(local_graph) = crate::graph::ContextGraph::open("ansible_context.db") {
            let conn = local_graph.conn.lock().unwrap();
            let mut stmt = conn.prepare("SELECT active_pid FROM materialized_guests WHERE guest_id = ?")?;
            let mut rows = stmt.query(rusqlite::params![guest_id])?;
            if let Some(row) = rows.next()? {
                if let Some(pid_str) = row.get::<_, Option<String>>(0)? {
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        if Self::pid_exists(pid) {
                            if Self::terminate_pid(pid) {
                                info!("Forcefully retired OS Process PID {} for Guest [{}].", pid, guest_id);
                            } else {
                                warn!("Failed to forcefully retire OS Process PID {} for Guest [{}].", pid, guest_id);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
    
    async fn check_status(&self, guest_id: &str, active_id: &str) -> Result<bool> {
        if let Ok(pid) = active_id.parse::<u32>() {
            Ok(Self::pid_exists(pid))
        } else {
            warn!("Invalid PID string '{}' for Guest [{}]", active_id, guest_id);
            Ok(false)
        }
    }
}

/// A centralized orchestrator for materializing and dematerializing Guests via a plugin Materializer interface.
pub struct GuestManager {
    graph: Arc<dyn GraphStorage>,
    materializer: Arc<Mutex<Box<dyn Materializer>>>,
}

impl GuestManager {
    pub fn new(graph: Arc<dyn GraphStorage>, materializer: Box<dyn Materializer>) -> Self {
        Self {
            graph,
            materializer: Arc::new(Mutex::new(materializer)),
        }
    }

    fn clear_guest_pid(graph: &dyn GraphStorage, guest_id: &str) {
        let _ = graph.set_guest_pid(guest_id, None);
    }

    /// Read all `is_active=1` Guests from the Graph, reclaim orphans, and invoke the underlying `Materializer`.
    pub async fn materialize_all(&self, mut shutdown_rx: broadcast::Receiver<()>) -> Result<()> {
        info!("--- BEGIN UNIVERSAL MATERIALIZATION ---");
        
        let guest_records = self.graph.list_guests(true)?;

        for rec in guest_records {
            info!("Materializing Guest [{}] (Role: {})", rec.guest_id, rec.role);
            
            // --- GHOST RECLAMATION ---
            if let Some(_pid) = &rec.active_pid {
                info!("Context Graph shows Ghost PID {} for Guest [{}]. Reclaiming identity...", _pid, rec.guest_id);
                let mut mat = self.materializer.lock().await;
                if let Err(e) = mat.reclaim_guest(&rec.guest_id).await {
                    warn!("Reclamation error for {}: {}", rec.guest_id, e);
                }
                Self::clear_guest_pid(self.graph.as_ref(), &rec.guest_id);
            }

            // --- SPAWNING ---
            let config: serde_json::Value = serde_json::from_str(&rec.config_json).unwrap_or_default();
            
            let mut mat = self.materializer.lock().await;
            match mat.spawn_guest(&rec.guest_id, &config).await {
                Ok(child_pid) => {
                    info!("✨ Successfully spawned identity for Guest [{}] (ID: {})", rec.guest_id, child_pid);
                    let _ = self.graph.set_guest_pid(&rec.guest_id, Some(&child_pid));
                }
                Err(e) => {
                    Self::clear_guest_pid(self.graph.as_ref(), &rec.guest_id);
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
        let all_guests = self.graph.list_guests(false)?;

        for rec in all_guests {
            let config: serde_json::Value = serde_json::from_str(&rec.config_json).unwrap_or_default();
            
            let mut mat = self.materializer.lock().await;

            if rec.is_active {
                if let Some(ref pid) = rec.active_pid {
                    match mat.check_status(&rec.guest_id, pid).await {
                        Ok(true) => {}
                        Ok(false) | Err(_) => {
                            warn!("Supervisor: Guest [{}] (PID: {}) is dead or unreachable. Re-spawning...", rec.guest_id, pid);
                            Self::clear_guest_pid(self.graph.as_ref(), &rec.guest_id);
                            
                            match mat.spawn_guest(&rec.guest_id, &config).await {
                                Ok(new_pid) => {
                                    info!("Supervisor: ✨ Re-spawned Guest [{}] (New ID: {})", rec.guest_id, new_pid);
                                    let _ = self.graph.set_guest_pid(&rec.guest_id, Some(&new_pid));
                                }
                                Err(e) => error!("Supervisor: ❌ Failed to re-spawn Guest [{}]: {}", rec.guest_id, e),
                            }
                        }
                    }
                } else {
                    info!("Supervisor: Guest [{}] is marked active but has no ID. Spawning...", rec.guest_id);
                    match mat.spawn_guest(&rec.guest_id, &config).await {
                        Ok(new_pid) => {
                            info!("Supervisor: ✨ Spawned missing Guest [{}] (ID: {})", rec.guest_id, new_pid);
                            let _ = self.graph.set_guest_pid(&rec.guest_id, Some(&new_pid));
                        }
                        Err(e) => error!("Supervisor: ❌ Failed to spawn missing Guest [{}]: {}", rec.guest_id, e),
                    }
                }
            } else {
                if rec.active_pid.is_some() {
                    info!("Supervisor: Guest [{}] is marked INACTIVE but has an ID. Reclaiming...", rec.guest_id);
                    if let Err(e) = mat.reclaim_guest(&rec.guest_id).await {
                        error!("Supervisor: ❌ Failed to reclaim inactive Guest [{}]: {}", rec.guest_id, e);
                    }
                    Self::clear_guest_pid(self.graph.as_ref(), &rec.guest_id);
                }
            }
        }

        Ok(())
    }
}
