use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::materializer::Materializer;
use anyhow::{Context, Result};
use async_trait::async_trait;
use rusqlite::types::ValueRef;
use std::collections::HashMap;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::Mutex;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// A single stderr line emitted by a guest process.
pub struct GuestStderrLine {
    pub guest_id: String,
    pub line: String,
}

/// A Universal Materializer backed by the local OS Process space.
pub struct LocalProcessMaterializer {
    children: HashMap<String, tokio::process::Child>,
    db_path: String,
    hotel_socket: Option<String>,
    stderr_tx: Option<mpsc::Sender<GuestStderrLine>>,
}

impl LocalProcessMaterializer {
    pub fn new(db_path: impl Into<String>) -> Self {
        Self {
            children: HashMap::new(),
            db_path: db_path.into(),
            hotel_socket: None,
            stderr_tx: None,
        }
    }

    /// Supply the hotel's IPC socket path so all spawned guests receive
    /// PHILOTIC_HOTEL_SOCKET even when the hotel process itself wasn't started
    /// with that env var set.
    pub fn with_hotel_socket(mut self, socket: impl Into<String>) -> Self {
        self.hotel_socket = Some(socket.into());
        self
    }

    /// Attach a channel through which guest stderr lines are forwarded for
    /// storage in the heal_queue table. Must be called before guests spawn.
    pub fn with_stderr_sink(mut self, tx: mpsc::Sender<GuestStderrLine>) -> Self {
        self.stderr_tx = Some(tx);
        self
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
            // Always override with hotel's own socket path.  Prefer the value
            // supplied at construction time; fall back to the process env (for
            // cases where the hotel was started with the var already set).
            let socket_override = self
                .hotel_socket
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| {
                    std::env::var("PHILOTIC_HOTEL_SOCKET")
                        .ok()
                        .filter(|s| !s.trim().is_empty())
                });
            if let Some(socket) = socket_override {
                command.env("PHILOTIC_HOTEL_SOCKET", socket.trim());
            }

            command.stderr(std::process::Stdio::piped());

            let mut child = command.spawn().with_context(|| {
                format!(
                    "Failed to spawn OS child process for guest '{}' using command '{}'",
                    guest_id, resolved_cmd
                )
            })?;
            let child_pid = child.id().unwrap_or(0);

            // Take stderr BEFORE moving child into the map.
            if let Some(stderr) = child.stderr.take() {
                let gid = guest_id.to_string();
                let tx = self.stderr_tx.clone();
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        warn!(guest_id = %gid, "stderr: {}", line);
                        if let Some(ref tx) = tx {
                            let _ = tx.try_send(GuestStderrLine {
                                guest_id: gid.clone(),
                                line,
                            });
                        }
                    }
                });
            }

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

/// Maximum supervisor respawns per guest within [`RESPAWN_BUDGET_WINDOW_SECS`].
pub(crate) const RESPAWN_BUDGET_MAX: usize = 5;
/// Sliding flap-protection window (seconds). Also the cool-down after a breach:
/// the budget resets only after a clean window with no respawn attempts.
pub(crate) const RESPAWN_BUDGET_WINDOW_SECS: u64 = 600;

/// Verdict returned to a heal-triggered restart caller after consulting the
/// shared respawn budget. `Denied` covers both a just-breached budget and one
/// still in cool-down; only the *transition* to breach marks/alerts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealRestartVerdict {
    /// Budget permits this restart; the attempt has been recorded.
    Allowed,
    /// Budget exhausted — the caller must SKIP the restart (no kill, no respawn).
    Denied,
}

/// Outcome of asking the respawn budget whether a guest may be respawned now.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RespawnDecision {
    /// Respawn permitted; the attempt has been recorded against the budget.
    /// `resumed` is true when this is the first attempt after a breach cooled down.
    Allowed { resumed: bool },
    /// This call breached the budget — caller should mark the guest and alert once.
    JustExhausted,
    /// Budget already breached and the clean-window cool-down has not elapsed.
    StillExhausted,
}

#[derive(Default)]
struct GuestRespawnLedger {
    /// Unix-epoch seconds of respawn attempts inside the sliding window.
    attempts: Vec<u64>,
    /// Set when the budget was breached; cleared after a clean window elapses.
    exhausted_at: Option<u64>,
}

/// Per-guest flap protection: at most [`RESPAWN_BUDGET_MAX`] respawns per
/// [`RESPAWN_BUDGET_WINDOW_SECS`] sliding window. On breach the guest is not
/// respawned again until a full clean window has elapsed since the breach.
/// Timestamps are injected so tests can drive a fake clock.
pub(crate) struct RespawnBudget {
    ledgers: std::sync::Mutex<HashMap<String, GuestRespawnLedger>>,
}

impl RespawnBudget {
    pub(crate) fn new() -> Self {
        Self {
            ledgers: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn check(&self, guest_id: &str, now: u64) -> RespawnDecision {
        let mut ledgers = self.ledgers.lock().unwrap();
        let ledger = ledgers.entry(guest_id.to_string()).or_default();

        let mut resumed = false;
        if let Some(exhausted_at) = ledger.exhausted_at {
            if now.saturating_sub(exhausted_at) < RESPAWN_BUDGET_WINDOW_SECS {
                return RespawnDecision::StillExhausted;
            }
            // A clean window has elapsed since the breach — reset the budget.
            ledger.attempts.clear();
            ledger.exhausted_at = None;
            resumed = true;
        }

        ledger
            .attempts
            .retain(|t| now.saturating_sub(*t) < RESPAWN_BUDGET_WINDOW_SECS);
        if ledger.attempts.len() >= RESPAWN_BUDGET_MAX {
            ledger.exhausted_at = Some(now);
            return RespawnDecision::JustExhausted;
        }
        ledger.attempts.push(now);
        RespawnDecision::Allowed { resumed }
    }
}

fn epoch_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A centralized orchestrator for materializing and dematerializing Guests via a plugin Materializer interface.
pub struct GuestManager {
    hotel_name: String,
    graph: Arc<GraphDomain>,
    materializer: Arc<Mutex<Box<dyn Materializer>>>,
    respawn_budget: RespawnBudget,
    heal_queue: Option<Arc<dyn ansible_mesh_core::heal_queue::HealQueueStorage>>,
}

#[async_trait]
pub trait GuestMaterializationRequester: Send + Sync {
    async fn ensure_guest_active(&self, guest_id: &str) -> Result<bool>;

    /// Consult the shared respawn budget before a heal-dispatcher-triggered
    /// restart. Records the attempt against the same budget the supervisor uses,
    /// and on breach marks the guest exhausted (heal entry + graph marker) exactly
    /// once. Returns [`HealRestartVerdict::Denied`] when the caller must skip the
    /// restart. The default implementation permits everything (no budget) so test
    /// doubles and non-supervising requesters stay unaffected.
    async fn check_heal_restart_budget(&self, _guest_id: &str) -> HealRestartVerdict {
        HealRestartVerdict::Allowed
    }
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
            respawn_budget: RespawnBudget::new(),
            heal_queue: None,
        }
    }

    /// Attach a heal-queue sink so respawn-budget breaches surface to
    /// heal-dispatcher / operators.
    pub fn with_heal_queue(
        mut self,
        hq: Arc<dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
    ) -> Self {
        self.heal_queue = Some(hq);
        self
    }

    fn clear_guest_pid(graph: &GraphDomain, hotel_name: &str, guest_id: &str) {
        let _ = graph.set_guest_pid(hotel_name, guest_id, None);
    }

    fn supervision_state_key(hotel_name: &str, guest_id: &str) -> String {
        format!("supervision_state:{hotel_name}:{guest_id}")
    }

    /// Mark the guest in the graph as having exhausted its respawn budget and
    /// push a heal-queue entry so heal-dispatcher / operators see the breach.
    fn mark_respawn_budget_exhausted(&self, guest_id: &str, now: u64) {
        let key = Self::supervision_state_key(&self.hotel_name, guest_id);
        let value = serde_json::json!({
            "state": "respawn_budget_exhausted",
            "since_epoch": now,
            "max_respawns": RESPAWN_BUDGET_MAX,
            "window_secs": RESPAWN_BUDGET_WINDOW_SECS,
        })
        .to_string();
        if let Err(e) = self.graph.set_config_value(&key, &value) {
            warn!(
                "Supervisor: failed to record supervision_state for [{}]: {}",
                guest_id, e
            );
        }
        if let Some(hq) = &self.heal_queue {
            let msg = format!(
                "supervisor: guest [{guest_id}] exhausted its respawn budget \
                 ({RESPAWN_BUDGET_MAX} respawns in {RESPAWN_BUDGET_WINDOW_SECS}s); \
                 respawns paused until a clean {RESPAWN_BUDGET_WINDOW_SECS}s window elapses"
            );
            if let Err(e) = hq.push_error(guest_id, &msg) {
                warn!(
                    "Supervisor: failed to push respawn-budget heal_queue entry for [{}]: {}",
                    guest_id, e
                );
            }
        }
    }

    /// Heal-restart flap protection, clock injected for tests.
    ///
    /// Consults the SAME [`RespawnBudget`] instance the supervisor reconcile loop
    /// uses (this is one shared `Arc<GuestManager>`), so heal-triggered restarts
    /// and supervisor respawns draw down a single budget. On the breaching call we
    /// mark the guest exhausted once (graph marker + heal entry) and reuse the
    /// existing cool-down/resume path; a still-exhausted budget is denied silently
    /// so we do not re-push a heal entry every dispatch cycle.
    pub(crate) fn check_heal_restart_budget_at(&self, guest_id: &str, now: u64) -> HealRestartVerdict {
        match self.respawn_budget.check(guest_id, now) {
            RespawnDecision::Allowed { resumed } => {
                if resumed {
                    info!(
                        "Heal-restart: Guest [{}] respawn budget cooled down after a clean window. Resuming restarts.",
                        guest_id
                    );
                    self.clear_respawn_budget_mark(guest_id);
                }
                HealRestartVerdict::Allowed
            }
            RespawnDecision::JustExhausted => {
                error!(
                    "Heal-restart: Guest [{}] exhausted its respawn budget ({} restarts in {}s). Skipping heal restart until a clean {}s window elapses.",
                    guest_id,
                    RESPAWN_BUDGET_MAX,
                    RESPAWN_BUDGET_WINDOW_SECS,
                    RESPAWN_BUDGET_WINDOW_SECS
                );
                self.mark_respawn_budget_exhausted(guest_id, now);
                HealRestartVerdict::Denied
            }
            RespawnDecision::StillExhausted => HealRestartVerdict::Denied,
        }
    }

    /// Remove the exhausted mark once the budget cools down and respawns resume.
    fn clear_respawn_budget_mark(&self, guest_id: &str) {
        let key = Self::supervision_state_key(&self.hotel_name, guest_id);
        if let Err(e) = self.graph.remove_config_value(&key) {
            warn!(
                "Supervisor: failed to clear supervision_state for [{}]: {}",
                guest_id, e
            );
        }
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
            if let Some(ref pid_str) = rec.active_pid {
                info!(
                    "Context Graph shows Ghost PID {} for Guest [{}]. Reclaiming identity...",
                    pid_str, rec.guest_id
                );
                {
                    let mut mat = self.materializer.lock().await;
                    if let Err(e) = mat.reclaim_guest(&rec.guest_id).await {
                        warn!("Reclamation error for {}: {}", rec.guest_id, e);
                    }
                }
                // Belt-and-suspenders: after a restart the materializer's children map is
                // empty and the legacy materialized_guests table is unused, so reclaim_guest
                // may silently skip the kill. Finish the job with a direct signal if the
                // process is still alive.
                if let Ok(pid) = pid_str.parse::<u32>() {
                    if LocalProcessMaterializer::pid_exists(pid) {
                        warn!(
                            "Guest [{}] PID {} survived reclaim_guest; killing directly.",
                            rec.guest_id, pid
                        );
                        LocalProcessMaterializer::terminate_pid(pid);
                    }
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

        // Detach a task to reap all active guests on shutdown
        let materializer_clone = self.materializer.clone();
        let graph_clone = self.graph.clone();
        let hotel_clone = self.hotel_name.clone();
        tokio::spawn(async move {
            let _ = shutdown_rx.recv().await;
            info!("Universal Dematerialization Shutdown Triggered. Reaping active Guests...");

            if let Ok(all_guests) = graph_clone.list_guests(&hotel_clone, false) {
                for rec in all_guests {
                    if rec.is_active {
                        info!("Shutdown: Reclaiming guest [{}]...", rec.guest_id);
                        let mut mat = materializer_clone.lock().await;
                        if let Err(e) = mat.reclaim_guest(&rec.guest_id).await {
                            warn!(
                                "Shutdown: Failed to reclaim guest [{}]: {}",
                                rec.guest_id, e
                            );
                        }
                        drop(mat);

                        // Belt-and-suspenders: kill directly if PID still exists
                        if let Some(active_pid) = rec.active_pid.as_deref() {
                            if let Ok(pid) = active_pid.parse::<u32>() {
                                if LocalProcessMaterializer::pid_exists(pid) {
                                    info!(
                                        "Shutdown: Killing guest [{}] PID {} directly.",
                                        rec.guest_id, pid
                                    );
                                    LocalProcessMaterializer::terminate_pid(pid);
                                }
                            }
                        }
                    }
                }
            }
            info!("Universal Dematerialization complete.");
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
                        // Belt-and-suspenders: if PID still exists after reclaim_guest, kill directly.
                        if let Ok(pid) = active_pid.parse::<u32>() {
                            if LocalProcessMaterializer::pid_exists(pid) {
                                warn!(
                                    "Supervisor: Guest [{}] PID {} survived reclaim_guest; killing directly.",
                                    rec.guest_id, pid
                                );
                                LocalProcessMaterializer::terminate_pid(pid);
                            }
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
                    // Flap protection: a crash-looping guest gets at most
                    // RESPAWN_BUDGET_MAX respawns per RESPAWN_BUDGET_WINDOW_SECS
                    // sliding window; on breach, respawns pause until a clean
                    // window elapses since the breach.
                    let now = epoch_now();
                    match self.respawn_budget.check(&current_rec.guest_id, now) {
                        RespawnDecision::Allowed { resumed } => {
                            if resumed {
                                info!(
                                    "Supervisor: Guest [{}] respawn budget cooled down after a clean window. Resuming respawns.",
                                    current_rec.guest_id
                                );
                                self.clear_respawn_budget_mark(&current_rec.guest_id);
                            }
                        }
                        RespawnDecision::JustExhausted => {
                            error!(
                                "Supervisor: Guest [{}] exhausted its respawn budget ({} respawns in {}s). Pausing respawns until a clean {}s window elapses.",
                                current_rec.guest_id,
                                RESPAWN_BUDGET_MAX,
                                RESPAWN_BUDGET_WINDOW_SECS,
                                RESPAWN_BUDGET_WINDOW_SECS
                            );
                            self.mark_respawn_budget_exhausted(&current_rec.guest_id, now);
                            continue;
                        }
                        RespawnDecision::StillExhausted => {
                            continue;
                        }
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

    async fn check_heal_restart_budget(&self, guest_id: &str) -> HealRestartVerdict {
        self.check_heal_restart_budget_at(guest_id, epoch_now())
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
            config_json: json!({ "command": "target/debug/membrane-telegram" }).to_string(),
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

    // ── Respawn budget (flap protection) ──────────────────────────────────

    #[test]
    fn respawn_budget_allows_up_to_max_within_window() {
        let budget = RespawnBudget::new();
        for i in 0..RESPAWN_BUDGET_MAX {
            assert_eq!(
                budget.check("guest-a", 1_000 + i as u64),
                RespawnDecision::Allowed { resumed: false },
                "attempt {i} should be allowed"
            );
        }
        assert_eq!(
            budget.check("guest-a", 1_000 + RESPAWN_BUDGET_MAX as u64),
            RespawnDecision::JustExhausted
        );
    }

    #[test]
    fn respawn_budget_stays_exhausted_until_clean_window() {
        let budget = RespawnBudget::new();
        for i in 0..RESPAWN_BUDGET_MAX {
            let _ = budget.check("guest-a", 1_000 + i as u64);
        }
        let breach_at = 1_100;
        assert_eq!(
            budget.check("guest-a", breach_at),
            RespawnDecision::JustExhausted
        );
        // Any attempt inside the clean-window cool-down stays exhausted.
        assert_eq!(
            budget.check("guest-a", breach_at + 1),
            RespawnDecision::StillExhausted
        );
        assert_eq!(
            budget.check("guest-a", breach_at + RESPAWN_BUDGET_WINDOW_SECS - 1),
            RespawnDecision::StillExhausted
        );
    }

    #[test]
    fn respawn_budget_resets_after_clean_window() {
        let budget = RespawnBudget::new();
        for i in 0..RESPAWN_BUDGET_MAX {
            let _ = budget.check("guest-a", 1_000 + i as u64);
        }
        let breach_at = 1_100;
        assert_eq!(
            budget.check("guest-a", breach_at),
            RespawnDecision::JustExhausted
        );
        // A full clean window after the breach resets the budget.
        let resume_at = breach_at + RESPAWN_BUDGET_WINDOW_SECS;
        assert_eq!(
            budget.check("guest-a", resume_at),
            RespawnDecision::Allowed { resumed: true }
        );
        // And the fresh budget allows the remaining attempts again.
        for i in 1..RESPAWN_BUDGET_MAX {
            assert_eq!(
                budget.check("guest-a", resume_at + i as u64),
                RespawnDecision::Allowed { resumed: false }
            );
        }
        assert_eq!(
            budget.check("guest-a", resume_at + RESPAWN_BUDGET_MAX as u64),
            RespawnDecision::JustExhausted
        );
    }

    #[test]
    fn respawn_budget_old_attempts_age_out() {
        let budget = RespawnBudget::new();
        // Attempts spaced wider than the window never accumulate to a breach.
        let mut t = 1_000;
        for _ in 0..(RESPAWN_BUDGET_MAX * 3) {
            assert_eq!(
                budget.check("guest-a", t),
                RespawnDecision::Allowed { resumed: false }
            );
            t += RESPAWN_BUDGET_WINDOW_SECS;
        }
    }

    #[test]
    fn respawn_budget_tracks_guests_independently() {
        let budget = RespawnBudget::new();
        for i in 0..RESPAWN_BUDGET_MAX {
            let _ = budget.check("guest-a", 1_000 + i as u64);
        }
        assert_eq!(
            budget.check("guest-a", 1_100),
            RespawnDecision::JustExhausted
        );
        // guest-b is unaffected by guest-a's breach.
        assert_eq!(
            budget.check("guest-b", 1_100),
            RespawnDecision::Allowed { resumed: false }
        );
    }

    /// Heal-queue mock capturing push_error calls.
    struct MockHealQueue {
        pushes: StdMutex<Vec<(String, String)>>,
    }

    impl MockHealQueue {
        fn new() -> Self {
            Self {
                pushes: StdMutex::new(Vec::new()),
            }
        }
    }

    impl ansible_mesh_core::heal_queue::HealQueueStorage for MockHealQueue {
        fn push_error(&self, guest_id: &str, raw_text: &str) -> Result<String> {
            self.pushes
                .lock()
                .unwrap()
                .push((guest_id.to_string(), raw_text.to_string()));
            Ok("mock-id".to_string())
        }
        fn pending_errors(
            &self,
            _limit: usize,
        ) -> Result<Vec<ansible_mesh_core::heal_queue::HealQueueRow>> {
            Ok(vec![])
        }
        fn update_triage(
            &self,
            _id: &str,
            _severity: &str,
            _pattern_tag: &str,
            _heal_action: &str,
        ) -> Result<()> {
            Ok(())
        }
        fn resolve(&self, _id: &str, _outcome: &str) -> Result<()> {
            Ok(())
        }
        fn vacuum_old(&self, _older_than_secs: u64) -> Result<usize> {
            Ok(0)
        }
    }

    #[tokio::test]
    async fn reconcile_all_stops_respawning_after_budget_exhausted() {
        // A guest whose every spawned PID immediately reads as dead simulates a
        // crash loop: each reconcile pass reclaims and respawns until the
        // budget breaches, after which respawns stop, the graph is marked, and
        // exactly one heal-queue entry is pushed.
        let graph = make_domain(TestGraphAdapter::with_guests(vec![GuestRecord {
            hotel_name: "test-hotel".into(),
            guest_id: "flappy".into(),
            role: "membrane".into(),
            config_json: json!({ "command": "target/debug/membrane-telegram" }).to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        }]));

        // MockMaterializer::check_status defaults to false for unknown PIDs,
        // so every spawned PID is immediately considered dead.
        let mock = MockMaterializer::new(HashMap::new());
        let spawn_count = mock.spawn_count.clone();
        let heal = Arc::new(MockHealQueue::new());
        let manager = GuestManager::new("test-hotel", graph.clone(), Box::new(mock))
            .with_heal_queue(heal.clone());

        // Drive well past the budget within a single real-time window.
        for _ in 0..(RESPAWN_BUDGET_MAX + 3) {
            manager
                .reconcile_all()
                .await
                .expect("reconcile should succeed");
        }

        assert_eq!(
            spawn_count.load(Ordering::SeqCst),
            RESPAWN_BUDGET_MAX,
            "respawns must stop at the budget"
        );

        // Breach is marked in the graph...
        let state = graph
            .get_config_value("supervision_state:test-hotel:flappy")
            .expect("config read")
            .expect("supervision_state should be set");
        assert!(state.contains("respawn_budget_exhausted"));

        // ...and surfaced to the heal queue exactly once.
        let pushes = heal.pushes.lock().unwrap();
        assert_eq!(pushes.len(), 1);
        assert_eq!(pushes[0].0, "flappy");
        assert!(pushes[0].1.contains("respawn budget"));
    }

    #[tokio::test]
    async fn heal_restart_budget_skips_and_marks_after_sixth_in_window() {
        // The heal-restart path shares the supervisor's RespawnBudget. Six
        // heal-triggered restarts of the same guest inside one window: the first
        // five are Allowed, the sixth is Denied (skipped), the graph is marked
        // respawn_budget_exhausted, and exactly one heal-queue entry is pushed.
        let graph = make_domain(TestGraphAdapter::with_guests(vec![GuestRecord {
            hotel_name: "test-hotel".into(),
            guest_id: "flappy".into(),
            role: "membrane".into(),
            config_json: json!({ "command": "target/debug/membrane-telegram" }).to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        }]));
        let heal = Arc::new(MockHealQueue::new());
        let manager = GuestManager::new("test-hotel", graph.clone(), Box::new(MockMaterializer::new(HashMap::new())))
            .with_heal_queue(heal.clone());

        // Fake clock: all six attempts fall inside a single sliding window.
        let base = 10_000u64;
        for i in 0..RESPAWN_BUDGET_MAX {
            assert_eq!(
                manager.check_heal_restart_budget_at("flappy", base + i as u64),
                HealRestartVerdict::Allowed,
                "heal restart {i} should be allowed within budget"
            );
        }
        // The sixth (RESPAWN_BUDGET_MAX + 1) breaches → skipped.
        assert_eq!(
            manager.check_heal_restart_budget_at("flappy", base + RESPAWN_BUDGET_MAX as u64),
            HealRestartVerdict::Denied,
            "the 6th heal restart in the window must be skipped"
        );

        // Breach is marked in the graph...
        let state = graph
            .get_config_value("supervision_state:test-hotel:flappy")
            .expect("config read")
            .expect("supervision_state should be set");
        assert!(state.contains("respawn_budget_exhausted"));

        // ...and surfaced to the heal queue exactly once (StillExhausted must not re-push).
        let _ = manager.check_heal_restart_budget_at("flappy", base + RESPAWN_BUDGET_MAX as u64 + 1);
        let pushes = heal.pushes.lock().unwrap();
        assert_eq!(pushes.len(), 1, "only the breaching transition files a heal entry");
        assert_eq!(pushes[0].0, "flappy");
    }

    #[tokio::test]
    async fn heal_restart_budget_resumes_after_clean_window() {
        // After a full clean window since the breach, the heal-restart budget
        // cools down: the guest may be restarted again and the graph marker is cleared.
        let graph = make_domain(TestGraphAdapter::with_guests(vec![GuestRecord {
            hotel_name: "test-hotel".into(),
            guest_id: "flappy".into(),
            role: "membrane".into(),
            config_json: json!({ "command": "target/debug/membrane-telegram" }).to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        }]));
        let manager = GuestManager::new("test-hotel", graph.clone(), Box::new(MockMaterializer::new(HashMap::new())));

        let base = 10_000u64;
        for i in 0..RESPAWN_BUDGET_MAX {
            let _ = manager.check_heal_restart_budget_at("flappy", base + i as u64);
        }
        let breach_at = base + RESPAWN_BUDGET_MAX as u64;
        assert_eq!(
            manager.check_heal_restart_budget_at("flappy", breach_at),
            HealRestartVerdict::Denied
        );
        // A full clean window later, restarts resume and the marker is cleared.
        assert_eq!(
            manager.check_heal_restart_budget_at("flappy", breach_at + RESPAWN_BUDGET_WINDOW_SECS),
            HealRestartVerdict::Allowed
        );
        assert!(
            graph
                .get_config_value("supervision_state:test-hotel:flappy")
                .expect("config read")
                .is_none(),
            "resumed restart must clear the exhausted marker"
        );
    }

    #[tokio::test]
    async fn reconcile_all_skips_respawn_when_guest_was_removed_after_snapshot() {
        let pid = "424243".to_string();
        let graph = make_domain(TestGraphAdapter::with_guests_cleared_on_second_list(vec![
            GuestRecord {
                hotel_name: "test-hotel".into(),
                guest_id: "guest-3".into(),
                role: "membrane".into(),
                config_json: json!({ "command": "target/debug/membrane-telegram" }).to_string(),
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
