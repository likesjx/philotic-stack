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

/// Strip ANSI SGR escape sequences (`ESC [ ... <letter>`) from a log line.
/// Guest tracing output carries color codes even when piped, so level-token
/// matching must happen on the stripped text.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for e in chars.by_ref() {
                    if e.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Decide whether a guest STDOUT line is a health signal worth persisting to
/// the heal queue.
///
/// Guests write their tracing logs to stdout, which historically was inherited
/// straight to the hotel's own stdout — so runtime `ERROR` events (external
/// API failures, auth 401s) never reached the self-heal circuit at all; only
/// true stderr (panics, crash output) did. Live gap 2026-07-20: membrane
/// Discord-405s on every reply and Beacon memory-401s were invisible to
/// `phil heal list`.
///
/// Volume control: only `ERROR`-level events are forwarded, plus `WARN`-level
/// events that carry an auth/API-failure marker (a 401 on the memory path is
/// logged at WARN but is a real standing fault). Everything else stays
/// log-only. Downstream `push_error` collapses near-identical lines within
/// its flood window, so a hot error loop cannot swamp the queue.
fn stdout_line_is_health_signal(line: &str) -> bool {
    let stripped = strip_ansi(line);
    if stripped.contains(" ERROR ") {
        return true;
    }
    if stripped.contains(" WARN ") {
        let lower = stripped.to_lowercase();
        return lower.contains("unauthorized")
            || lower.contains("permission denied")
            || lower.contains("forbidden")
            || lower.contains("api error")
            || lower.contains("api key");
    }
    false
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
        let tracked_child_is_live = if let Some(child) = self.children.get_mut(guest_id) {
            child.try_wait()?.is_none()
        } else {
            false
        };
        if tracked_child_is_live {
            anyhow::bail!(
                "Refusing to spawn duplicate OS child for guest '{}': a tracked child is still live",
                guest_id
            );
        }
        self.children.remove(guest_id);

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
            command.stdout(std::process::Stdio::piped());

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

            // Guest stdout: pass every line through to the hotel's stdout
            // verbatim (preserving the historical inherit behavior journald /
            // launchd log files rely on), and additionally forward ERROR-level
            // tracing events into the heal queue — guests log runtime faults
            // to STDOUT, so without this tap the self-heal circuit only ever
            // saw crash-time stderr (see stdout_line_is_health_signal).
            if let Some(stdout) = child.stdout.take() {
                let gid = guest_id.to_string();
                let tx = self.stderr_tx.clone();
                tokio::spawn(async move {
                    use std::io::Write;
                    let mut lines = BufReader::new(stdout).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        {
                            let mut out = std::io::stdout().lock();
                            let _ = writeln!(out, "{line}");
                        }
                        if let Some(ref tx) = tx {
                            if stdout_line_is_health_signal(&line) {
                                let _ = tx.try_send(GuestStderrLine {
                                    guest_id: gid.clone(),
                                    line,
                                });
                            }
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

// ── Heal-the-healer: dispatcher heartbeat watchdog (S2) ────────────────────
//
// The doctor check (`crates/philotic-web/src/doctor.rs::HealDispatcherStaleness`,
// #214) already *displays* a stale `heal_dispatcher.last_cycle_at` heartbeat
// offline. This closes the loop with live enforcement: a PID-alive-but-wedged
// heal-dispatcher (a hung classifier, blocked IPC — PID-liveness alone can't
// see it) gets restarted through the SAME respawn budget every other
// supervisor/heal restart draws down, never a parallel mechanism.

/// Role name the heal-dispatcher guest is materialized under (see
/// `crates/aiua/src/main.rs`, `GuestRecord { role: "heal-dispatcher", .. }`).
pub(crate) const HEAL_DISPATCHER_ROLE: &str = "heal-dispatcher";

/// Hotel config key the heal-dispatcher stamps at the end of each poll cycle.
/// MUST match `HEARTBEAT_KEY` in `crates/heal-dispatcher/src/main.rs` and
/// `HEAL_DISPATCHER_HEARTBEAT_KEY` in `crates/philotic-web/src/doctor.rs` —
/// this constant is the live-enforcement half of the same integration
/// contract doctor only reads offline.
pub(crate) const HEAL_DISPATCHER_HEARTBEAT_KEY: &str = "heal_dispatcher.last_cycle_at";

/// Mirrors heal-dispatcher's `POLL_INTERVAL_SECS`. Duplicated (not shared via
/// a common crate) for the same reason doctor.rs duplicates it: heal-dispatcher
/// ships bin-only.
pub(crate) const HEAL_DISPATCHER_POLL_INTERVAL_SECS: u64 = 30;

/// Heartbeat older than 3x the poll interval (or, for an absent heartbeat,
/// that long since we first observed the current PID) ⇒ treat the dispatcher
/// as wedged. Matches doctor's `HEAL_DISPATCHER_STALE_SECS` so live
/// enforcement and offline visibility always agree on the same threshold.
pub(crate) const HEAL_DISPATCHER_STALE_SECS: u64 = 3 * HEAL_DISPATCHER_POLL_INTERVAL_SECS;

/// Parse a stored heartbeat config value into epoch seconds. Mirrors
/// `philotic_web::doctor::parse_epoch_secs` (JSON number, JSON string, or a
/// bare integer); returns `None` on anything else so an unreadable value
/// degrades to "treat like absent" instead of panicking or reading false-fresh.
pub(crate) fn parse_heartbeat_epoch_secs(value_json: &str) -> Option<i64> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(value_json) {
        if let Some(n) = v.as_i64() {
            return Some(n);
        }
        if let Some(f) = v.as_f64() {
            return Some(f as i64);
        }
        if let Some(s) = v.as_str() {
            if let Ok(n) = s.trim().parse::<i64>() {
                return Some(n);
            }
        }
    }
    value_json.trim().parse::<i64>().ok()
}

/// Pure decision: given the dispatcher's last-read heartbeat (epoch seconds,
/// if any), when we first observed its current PID, and the current time —
/// is it wedged?
///
/// - A present, past-or-present heartbeat is stale once its age exceeds
///   [`HEAL_DISPATCHER_STALE_SECS`].
/// - A present but FUTURE heartbeat (clock skew, or a seconds/millis unit
///   mismatch with the writer) is never treated as stale here: an untrustworthy
///   timestamp must not drive a spurious restart. Doctor surfaces the mismatch
///   itself (as a Warning finding); this watchdog just declines to act on it.
/// - An absent heartbeat (an older dispatcher build, or one that has never
///   completed a first cycle) is judged against how long we've observed the
///   current PID, so a dispatcher wedged before its first cycle still gets
///   caught instead of running unchecked forever — bounded by the same
///   threshold, giving every fresh spawn one full grace window.
pub(crate) fn dispatcher_heartbeat_is_stale(
    heartbeat_epoch: Option<i64>,
    pid_first_seen_epoch: u64,
    now: u64,
) -> bool {
    match heartbeat_epoch {
        Some(ts) if ts >= 0 && (ts as u64) <= now => {
            now.saturating_sub(ts as u64) > HEAL_DISPATCHER_STALE_SECS
        }
        Some(_future_or_negative) => false,
        None => now.saturating_sub(pid_first_seen_epoch) > HEAL_DISPATCHER_STALE_SECS,
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
    /// In-memory `guest_id -> (last observed pid, first-seen epoch)` for the
    /// heal-dispatcher heartbeat watchdog (S2). Not persisted: a hotel
    /// restart re-observes the current PID fresh on the next tick, which
    /// simply grants one more grace window — acceptable, since the failure
    /// this guards against (a wedged-but-alive dispatcher) can't survive a
    /// hotel restart anyway.
    dispatcher_pid_tracking: std::sync::Mutex<HashMap<String, (String, u64)>>,
}

#[async_trait]
pub trait GuestMaterializationRequester: Send + Sync {
    async fn ensure_guest_active(&self, guest_id: &str) -> Result<bool>;
    async fn restart_guest(&self, guest_id: &str) -> Result<bool>;

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
            dispatcher_pid_tracking: std::sync::Mutex::new(HashMap::new()),
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

    /// Config key marking a guest as TTL-dormant (wakeable by on-demand
    /// materialization) rather than operator-deactivated (refused). Written
    /// by the supervisor's role-TTL sweep, cleared on wake.
    pub(crate) fn dormancy_marker_key(guest_id: &str) -> String {
        format!("guest_dormancy:{guest_id}")
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
    pub(crate) fn check_heal_restart_budget_at(
        &self,
        guest_id: &str,
        now: u64,
    ) -> HealRestartVerdict {
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
            // Dormant ≠ disabled: a TTL-dormant guest (marker written by the
            // supervisor's role-TTL sweep) is wakeable on demand — that is
            // the whole point of lazy specialist materialization. An
            // operator deactivation carries no marker and stays refused.
            let marker_key = Self::dormancy_marker_key(&current_rec.guest_id);
            let is_dormant = self
                .graph
                .get_config_value(&marker_key)
                .ok()
                .flatten()
                .is_some();
            if !is_dormant {
                return Ok(false);
            }
            info!(
                "On-demand materialization: waking TTL-dormant guest [{}].",
                current_rec.guest_id
            );
            self.graph
                .set_guest_active(&self.hotel_name, &current_rec.guest_id, true)?;
            let _ = self.graph.remove_config_value(&marker_key);
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

    /// Reclaim and respawn one guest while holding the materializer lock for the
    /// whole transition. This prevents the supervisor or another restart request
    /// from observing the cleared PID and spawning a second incarnation.
    pub async fn restart_guest(&self, guest_id: &str) -> Result<bool> {
        let mut mat = self.materializer.lock().await;
        let Some(current_rec) =
            Self::refresh_guest_record(self.graph.as_ref(), &self.hotel_name, guest_id)?
        else {
            return Ok(false);
        };
        if !current_rec.is_active {
            return Ok(false);
        }

        if let Err(err) = mat.reclaim_guest(&current_rec.guest_id).await {
            warn!(
                "Restart materialization: reclaim failed for [{}]: {}",
                current_rec.guest_id, err
            );
        }
        if let Some(active_pid) = current_rec.active_pid.as_deref() {
            if let Ok(pid) = active_pid.parse::<u32>() {
                if LocalProcessMaterializer::pid_exists(pid) {
                    warn!(
                        "Restart materialization: Guest [{}] PID {} survived reclaim; killing directly.",
                        current_rec.guest_id, pid
                    );
                    LocalProcessMaterializer::terminate_pid(pid);
                }
            }
        }
        Self::clear_guest_pid(self.graph.as_ref(), &self.hotel_name, &current_rec.guest_id);

        let config: serde_json::Value =
            serde_json::from_str(&current_rec.config_json).unwrap_or_default();
        let new_pid = mat.spawn_guest(&current_rec.guest_id, &config).await?;
        self.graph
            .set_guest_pid(&self.hotel_name, &current_rec.guest_id, Some(&new_pid))?;
        info!(
            "Restart materialization: spawned Guest [{}] (PID {}).",
            current_rec.guest_id, new_pid
        );
        Ok(true)
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
                    // S2 heal-the-healer: a dead heal-dispatcher PID is already
                    // caught by reconcile_all() above; this catches the case
                    // reconcile_all can't — a PID that's alive but wedged
                    // (stale heartbeat, not cycling).
                    if let Err(e) = self.check_heal_dispatcher_heartbeat().await {
                        warn!("Heal-dispatcher heartbeat watchdog error: {}", e);
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
                        "Supervisor: Guest [{}] has exceeded its role TTL. Going dormant (wakeable).",
                        rec.guest_id
                    );
                    let _ = self
                        .graph
                        .set_guest_active(&self.hotel_name, &rec.guest_id, false);
                    // Dormant ≠ disabled. TTL expiry writes the same
                    // `is_active=0` bit an operator deactivation writes, and
                    // `ensure_guest_active` refuses inactive guests — which
                    // made every TTL-dormant specialist permanently
                    // unreachable by delegation (DEF-086 family). Mark the
                    // deactivation as dormancy so on-demand materialization
                    // may wake it; operator deactivations never set this
                    // marker and stay refused.
                    let _ = self.graph.set_config_value(
                        &Self::dormancy_marker_key(&rec.guest_id),
                        "\"ttl_dormant\"",
                    );
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
                    if let Some(refreshed_pid) = current_rec.active_pid.as_deref() {
                        if mat
                            .check_status(&current_rec.guest_id, refreshed_pid)
                            .await?
                        {
                            info!(
                                "Supervisor: Guest [{}] gained live PID [{}] while waiting to reconcile. Skipping duplicate spawn.",
                                current_rec.guest_id, refreshed_pid
                            );
                            continue;
                        }
                        if let Err(e) = mat.reclaim_guest(&current_rec.guest_id).await {
                            warn!(
                                "Supervisor: reclaim of refreshed stale Guest [{}] failed: {}",
                                current_rec.guest_id, e
                            );
                        }
                        Self::clear_guest_pid(
                            self.graph.as_ref(),
                            &self.hotel_name,
                            &current_rec.guest_id,
                        );
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

    /// S2 heal-the-healer: detect a heal-dispatcher whose PID is alive but
    /// whose `heal_dispatcher.last_cycle_at` heartbeat has gone stale (wedged
    /// — a hung classifier, blocked IPC, ...) and restart it through the same
    /// respawn budget every other supervisor/heal-triggered restart shares.
    /// Budget exhaustion is not handled here at all: `check_heal_restart_budget_at`
    /// already marks the graph and pushes the throttled heal-queue escalation
    /// on the breaching call, exactly as it does for IPC-triggered heal
    /// restarts — no parallel escalation path is introduced.
    async fn check_heal_dispatcher_heartbeat(&self) -> Result<()> {
        self.check_heal_dispatcher_heartbeat_at(epoch_now()).await
    }

    /// Clock injected for deterministic tests; see [`check_heal_dispatcher_heartbeat`].
    async fn check_heal_dispatcher_heartbeat_at(&self, now: u64) -> Result<()> {
        let guests = self.graph.list_guests(&self.hotel_name, false)?;
        let Some(rec) = guests.into_iter().find(|g| g.role == HEAL_DISPATCHER_ROLE) else {
            return Ok(()); // not deployed on this hotel — nothing to watch
        };
        if !rec.is_active {
            return Ok(());
        }
        let Some(active_pid) = rec.active_pid.clone() else {
            // No PID: the ordinary reconcile_all() PID-liveness path handles
            // (re)spawning a genuinely dead guest.
            return Ok(());
        };

        // Grant a fresh grace window whenever the observed PID changes (a
        // respawn happened, ours or otherwise), so a just-spawned dispatcher
        // is never judged against a heartbeat left over from its predecessor.
        let first_seen = {
            let mut tracking = self.dispatcher_pid_tracking.lock().unwrap();
            let entry = tracking
                .entry(rec.guest_id.clone())
                .or_insert_with(|| (active_pid.clone(), now));
            if entry.0 != active_pid {
                *entry = (active_pid.clone(), now);
            }
            entry.1
        };

        let is_live = {
            let mut mat = self.materializer.lock().await;
            mat.check_status(&rec.guest_id, &active_pid).await?
        };
        if !is_live {
            // Dead PID: reconcile_all() already owns this path.
            return Ok(());
        }

        let heartbeat_epoch = self
            .graph
            .get_config_value(HEAL_DISPATCHER_HEARTBEAT_KEY)?
            .and_then(|raw| parse_heartbeat_epoch_secs(&raw));

        if !dispatcher_heartbeat_is_stale(heartbeat_epoch, first_seen, now) {
            return Ok(());
        }

        warn!(
            guest_id = %rec.guest_id,
            heartbeat_epoch = ?heartbeat_epoch,
            first_seen_pid_at = first_seen,
            now,
            "Heal-dispatcher watchdog: heartbeat stale (process alive, not cycling) — requesting heal-restart"
        );

        if self.check_heal_restart_budget_at(&rec.guest_id, now) == HealRestartVerdict::Denied {
            // Either just breached (already marked + escalated inside
            // check_heal_restart_budget_at) or still cooling down (already
            // escalated once, intentionally quiet on every subsequent tick).
            return Ok(());
        }

        {
            let mut mat = self.materializer.lock().await;
            if let Err(e) = mat.reclaim_guest(&rec.guest_id).await {
                warn!(
                    "Heal-dispatcher watchdog: reclaim failed for [{}]: {}",
                    rec.guest_id, e
                );
            }
        }
        if let Ok(pid) = active_pid.parse::<u32>() {
            if LocalProcessMaterializer::pid_exists(pid) {
                warn!(
                    "Heal-dispatcher watchdog: PID {} survived reclaim; killing directly.",
                    pid
                );
                LocalProcessMaterializer::terminate_pid(pid);
            }
        }
        Self::clear_guest_pid(self.graph.as_ref(), &self.hotel_name, &rec.guest_id);

        // Never silently absent: every auto-restart is visible in the heal
        // queue too, not just the logs — independent of whether this attempt
        // happens to stay under budget (budget exhaustion has its own
        // escalation above, via mark_respawn_budget_exhausted).
        if let Some(hq) = &self.heal_queue {
            let msg = format!(
                "heal-dispatcher watchdog: guest [{}] heartbeat stale (>{}s); auto-restarted",
                rec.guest_id, HEAL_DISPATCHER_STALE_SECS
            );
            if let Err(e) = hq.push_error(&rec.guest_id, &msg) {
                warn!(
                    "Heal-dispatcher watchdog: failed to push heal_queue entry for [{}]: {}",
                    rec.guest_id, e
                );
            }
        }

        match self.ensure_guest_active(&rec.guest_id).await {
            Ok(true) => info!(
                guest_id = %rec.guest_id,
                "Heal-dispatcher watchdog: restarted stale-heartbeat dispatcher."
            ),
            Ok(false) => warn!(
                guest_id = %rec.guest_id,
                "Heal-dispatcher watchdog: restart requested but guest was not re-materialized."
            ),
            Err(e) => error!(
                guest_id = %rec.guest_id,
                "Heal-dispatcher watchdog: respawn failed: {}", e
            ),
        }

        Ok(())
    }
}

#[async_trait]
impl GuestMaterializationRequester for GuestManager {
    async fn ensure_guest_active(&self, guest_id: &str) -> Result<bool> {
        Self::ensure_guest_active(self, guest_id).await
    }

    async fn restart_guest(&self, guest_id: &str) -> Result<bool> {
        Self::restart_guest(self, guest_id).await
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
        replacement_guest_on_second_list: Option<GraphNode>,
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
                replacement_guest_on_second_list: None,
            }
        }

        fn with_guests_cleared_on_second_list(guests: Vec<GuestRecord>) -> Self {
            let mut adapter = Self::with_guests(guests);
            adapter.clear_guests_on_second_list = true;
            adapter
        }

        fn with_guest_replaced_on_second_list(
            initial: GuestRecord,
            replacement: GuestRecord,
        ) -> Self {
            let mut adapter = Self::with_guests(vec![initial]);
            let key = format!("guest:{}:{}", replacement.hotel_name, replacement.guest_id);
            adapter.replacement_guest_on_second_list = Some(GraphNode {
                node_key: key,
                kind: "guest".to_string(),
                label: Some(replacement.guest_id.clone()),
                data: serde_json::to_value(replacement).unwrap(),
            });
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
                if call_index >= 1 {
                    if let Some(replacement) = &self.replacement_guest_on_second_list {
                        self.nodes
                            .lock()
                            .unwrap()
                            .insert(replacement.node_key.clone(), replacement.clone());
                    }
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

    #[tokio::test]
    async fn local_process_materializer_refuses_duplicate_live_child() {
        let mut materializer = LocalProcessMaterializer::new("aiua_context.db");
        let guest_id = "single-incarnation-guest";
        let config = json!({
            "command": "/bin/sleep",
            "args": ["30"]
        });
        let original_pid = materializer
            .spawn_guest(guest_id, &config)
            .await
            .expect("spawn original child");

        let duplicate = materializer.spawn_guest(guest_id, &config).await;
        assert!(
            duplicate.is_err(),
            "a live tracked child must fence a duplicate spawn"
        );
        assert!(
            materializer
                .check_status(guest_id, &original_pid)
                .await
                .expect("original child status")
        );

        materializer
            .reclaim_guest(guest_id)
            .await
            .expect("reclaim original child");
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

    /// Dormant ≠ disabled: a guest deactivated by the role-TTL sweep (marker
    /// present) must be wakeable on demand — before this, TTL dormancy wrote
    /// the same is_active=0 bit as an operator deactivation and every
    /// delegation to a dormant specialist was refused forever (DEF-086
    /// family). An operator deactivation (no marker) must STAY refused.
    #[tokio::test]
    async fn ensure_guest_active_wakes_ttl_dormant_but_refuses_operator_deactivation() {
        let storage = ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open(":memory:")
            .expect("open sqlite");
        let graph = Arc::new(GraphDomain::new(Arc::new(storage.adapter())));
        let guests = vec![
            GuestRecord {
                hotel_name: "test-hotel".into(),
                guest_id: "test-hotel:philote-Chronos".into(),
                role: "Chronos".into(),
                config_json: json!({ "command": "philote" }).to_string(),
                is_active: false,
                active_pid: None,
                last_active_at: None,
            },
            GuestRecord {
                hotel_name: "test-hotel".into(),
                guest_id: "test-hotel:philote-Muse".into(),
                role: "Muse".into(),
                config_json: json!({ "command": "philote" }).to_string(),
                is_active: false,
                active_pid: None,
                last_active_at: None,
            },
        ];
        graph
            .seed_guests("test-hotel", &guests)
            .expect("seed guests");
        // Chronos went dormant via the TTL sweep; Muse was operator-disabled.
        graph
            .set_config_value(
                &GuestManager::dormancy_marker_key("test-hotel:philote-Chronos"),
                "\"ttl_dormant\"",
            )
            .expect("set marker");

        let mock = MockMaterializer::new(HashMap::new());
        let spawn_count = mock.spawn_count.clone();
        let manager = GuestManager::new("test-hotel", graph.clone(), Box::new(mock));

        let woke = manager
            .ensure_guest_active("test-hotel:philote-Chronos")
            .await
            .expect("ensure dormant");
        assert!(woke, "TTL-dormant guest must wake on demand");
        assert_eq!(spawn_count.load(Ordering::SeqCst), 1, "wake must spawn");
        let rec = graph
            .get_guest("test-hotel", "test-hotel:philote-Chronos")
            .expect("get")
            .expect("exists");
        assert!(rec.is_active, "woken guest must be re-activated");
        assert!(
            graph
                .get_config_value(&GuestManager::dormancy_marker_key(
                    "test-hotel:philote-Chronos"
                ))
                .expect("get marker")
                .is_none(),
            "dormancy marker must clear on wake"
        );

        let refused = manager
            .ensure_guest_active("test-hotel:philote-Muse")
            .await
            .expect("ensure disabled");
        assert!(!refused, "operator-deactivated guest must stay refused");
        assert_eq!(
            spawn_count.load(Ordering::SeqCst),
            1,
            "no spawn for operator-deactivated guest"
        );
    }

    #[tokio::test]
    async fn ensure_guest_active_requires_guest_seeded_under_hotel_name_not_node_id() {
        // Regression for a subagent-materialization dead-lease found live
        // 2026-08-28: `handle_spawn_subagent` seeded the fresh subagent guest
        // record under `local_node_id` (e.g. "mac-jane-aiua-01"), but
        // `GuestManager` — and every other seed_guests call site — is keyed by
        // `hotel_name` (e.g. "mac-jane", a distinct string). `list_guests`
        // builds its lookup prefix from `hotel_name`, so the guest was
        // invisible to `ensure_guest_active`: it silently returned `Ok(false)`,
        // no spawn was attempted, and `subagent.spawn` returned success with a
        // lease that no worker process ever backed.
        let hotel_name = "mac-jane";
        let local_node_id = "mac-jane-aiua-01"; // distinct from hotel_name, as in production
        let storage = ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open(":memory:")
            .expect("open sqlite");
        let graph = Arc::new(GraphDomain::new(Arc::new(storage.adapter())));

        let subagent_guest = GuestRecord {
            hotel_name: hotel_name.into(),
            guest_id: "subagent-fixture-01".into(),
            role: "philote-worker".into(),
            config_json: json!({ "command": "philote-worker" }).to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        };

        let mock = MockMaterializer::new(HashMap::new());
        let spawn_count = mock.spawn_count.clone();
        let manager = GuestManager::new(hotel_name, graph.clone(), Box::new(mock));

        // The fix: seeded under hotel_name — the materializer finds and spawns it.
        graph
            .seed_guests(hotel_name, &[subagent_guest.clone()])
            .expect("seed under hotel_name");
        let activated = manager
            .ensure_guest_active("subagent-fixture-01")
            .await
            .expect("ensure_guest_active");
        assert!(activated, "guest seeded under hotel_name must materialize");
        assert_eq!(spawn_count.load(Ordering::SeqCst), 1);

        // Characterizes the bug: seeded under node_id instead (the original
        // `handle_spawn_subagent` set BOTH the seed_guests argument and the
        // GuestRecord.hotel_name field to local_node_id) — invisible to the
        // hotel-name-keyed lookup, so no spawn is attempted for it.
        let orphaned = GuestRecord {
            hotel_name: local_node_id.into(),
            guest_id: "subagent-fixture-02".into(),
            ..subagent_guest
        };
        graph
            .seed_guests(local_node_id, std::slice::from_ref(&orphaned))
            .expect("seed under node_id (the old bug)");
        let activated_orphan = manager
            .ensure_guest_active("subagent-fixture-02")
            .await
            .expect("ensure_guest_active");
        assert!(
            !activated_orphan,
            "guest seeded under node_id must NOT be found by a hotel-name-keyed manager"
        );
        assert_eq!(
            spawn_count.load(Ordering::SeqCst),
            1,
            "no spawn attempted for the node-id-keyed orphan"
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
        let manager = GuestManager::new(
            "test-hotel",
            graph.clone(),
            Box::new(MockMaterializer::new(HashMap::new())),
        )
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
        let _ =
            manager.check_heal_restart_budget_at("flappy", base + RESPAWN_BUDGET_MAX as u64 + 1);
        let pushes = heal.pushes.lock().unwrap();
        assert_eq!(
            pushes.len(),
            1,
            "only the breaching transition files a heal entry"
        );
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
        let manager = GuestManager::new(
            "test-hotel",
            graph.clone(),
            Box::new(MockMaterializer::new(HashMap::new())),
        );

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

    #[tokio::test]
    async fn reconcile_all_skips_spawn_when_guest_gains_live_pid_after_snapshot() {
        let initial = GuestRecord {
            hotel_name: "test-hotel".into(),
            guest_id: "membrane-gateway".into(),
            role: "membrane".into(),
            config_json: json!({ "command": "target/debug/membrane-telegram" }).to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        };
        let mut replacement = initial.clone();
        replacement.active_pid = Some("replacement-pid".into());
        let graph = make_domain(TestGraphAdapter::with_guest_replaced_on_second_list(
            initial,
            replacement,
        ));

        let mock = MockMaterializer::new(HashMap::from([("replacement-pid".to_string(), true)]));
        let spawn_count = mock.spawn_count.clone();
        let reclaim_count = mock.reclaim_count.clone();
        let manager = GuestManager::new("test-hotel", graph, Box::new(mock));

        manager
            .reconcile_all()
            .await
            .expect("reconcile should succeed");

        assert_eq!(
            spawn_count.load(Ordering::SeqCst),
            0,
            "a replacement that became live while the supervisor waited must not be duplicated"
        );
        assert_eq!(reclaim_count.load(Ordering::SeqCst), 0);
    }

    // ── Heal-the-healer: dispatcher heartbeat watchdog (S2) ────────────────

    #[test]
    fn parse_heartbeat_epoch_secs_is_defensive() {
        assert_eq!(
            parse_heartbeat_epoch_secs("1750000000"),
            Some(1_750_000_000)
        );
        assert_eq!(
            parse_heartbeat_epoch_secs("\"1750000000\""),
            Some(1_750_000_000)
        );
        assert_eq!(
            parse_heartbeat_epoch_secs("  1750000000  "),
            Some(1_750_000_000)
        );
        assert_eq!(parse_heartbeat_epoch_secs("not-a-number"), None);
        assert_eq!(parse_heartbeat_epoch_secs("\"nope\""), None);
    }

    #[test]
    fn dispatcher_heartbeat_staleness_present_heartbeat() {
        let now: u64 = 1_000_000;
        let now_i = now as i64;
        // Exactly at the boundary is still fresh.
        assert!(!dispatcher_heartbeat_is_stale(
            Some(now_i - HEAL_DISPATCHER_STALE_SECS as i64),
            0,
            now
        ));
        // One second past the boundary is stale.
        assert!(dispatcher_heartbeat_is_stale(
            Some(now_i - HEAL_DISPATCHER_STALE_SECS as i64 - 1),
            0,
            now
        ));
    }

    #[test]
    fn dispatcher_heartbeat_staleness_future_heartbeat_is_never_stale() {
        // Clock skew / a seconds-millis unit mismatch must not drive a
        // spurious restart — doctor surfaces the mismatch itself.
        let now: u64 = 1_000_000;
        assert!(!dispatcher_heartbeat_is_stale(
            Some(now as i64 + 50_000),
            0,
            now
        ));
    }

    #[test]
    fn dispatcher_heartbeat_staleness_absent_heartbeat_uses_pid_first_seen() {
        let now = 1_000_000;
        // PID observed recently: within the grace window, not stale yet.
        assert!(!dispatcher_heartbeat_is_stale(
            None,
            now - HEAL_DISPATCHER_STALE_SECS,
            now
        ));
        // PID observed long enough ago with STILL no heartbeat: stale.
        assert!(dispatcher_heartbeat_is_stale(
            None,
            now - HEAL_DISPATCHER_STALE_SECS - 1,
            now
        ));
    }

    fn heal_dispatcher_guest(guest_id: &str, active_pid: Option<&str>) -> GuestRecord {
        GuestRecord {
            hotel_name: "test-hotel".into(),
            guest_id: guest_id.into(),
            role: HEAL_DISPATCHER_ROLE.into(),
            config_json: json!({ "command": "heal-dispatcher" }).to_string(),
            is_active: true,
            active_pid: active_pid.map(|s| s.to_string()),
            last_active_at: None,
        }
    }

    #[tokio::test]
    async fn heal_dispatcher_watchdog_ignores_fresh_heartbeat() {
        let pid = "12345".to_string();
        let graph = make_domain(TestGraphAdapter::with_guests(vec![heal_dispatcher_guest(
            "test-hotel:heal-dispatcher",
            Some(&pid),
        )]));
        graph
            .set_config_value(HEAL_DISPATCHER_HEARTBEAT_KEY, "1000")
            .expect("set heartbeat");

        let mock = MockMaterializer::new(HashMap::from([(pid.clone(), true)]));
        let spawn_count = mock.spawn_count.clone();
        let reclaim_count = mock.reclaim_count.clone();
        let manager = GuestManager::new("test-hotel", graph.clone(), Box::new(mock));

        // now - heartbeat = 30s, well under the 90s staleness threshold.
        manager
            .check_heal_dispatcher_heartbeat_at(1_030)
            .await
            .expect("watchdog tick");

        assert_eq!(spawn_count.load(Ordering::SeqCst), 0);
        assert_eq!(reclaim_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn heal_dispatcher_watchdog_ignores_dead_pid() {
        // A dead PID is reconcile_all()'s job, not this watchdog's — even
        // with a stale heartbeat, an already-dead dispatcher must not be
        // double-restarted here.
        let pid = "12345".to_string();
        let graph = make_domain(TestGraphAdapter::with_guests(vec![heal_dispatcher_guest(
            "test-hotel:heal-dispatcher",
            Some(&pid),
        )]));
        graph
            .set_config_value(HEAL_DISPATCHER_HEARTBEAT_KEY, "0")
            .expect("set heartbeat");

        let mock = MockMaterializer::new(HashMap::from([(pid.clone(), false)]));
        let spawn_count = mock.spawn_count.clone();
        let reclaim_count = mock.reclaim_count.clone();
        let manager = GuestManager::new("test-hotel", graph.clone(), Box::new(mock));

        manager
            .check_heal_dispatcher_heartbeat_at(1_000_000)
            .await
            .expect("watchdog tick");

        assert_eq!(spawn_count.load(Ordering::SeqCst), 0);
        assert_eq!(reclaim_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn heal_dispatcher_watchdog_ignores_other_roles() {
        let pid = "12345".to_string();
        let graph = make_domain(TestGraphAdapter::with_guests(vec![GuestRecord {
            hotel_name: "test-hotel".into(),
            guest_id: "test-hotel:tool-runner".into(),
            role: "tool".into(),
            config_json: json!({ "command": "tool-runner" }).to_string(),
            is_active: true,
            active_pid: Some(pid.clone()),
            last_active_at: None,
        }]));
        // No heartbeat at all recorded — if this watchdog mistakenly matched
        // a non-heal-dispatcher guest, an absent heartbeat this long after
        // "first seen" would trigger a restart.
        let mock = MockMaterializer::new(HashMap::from([(pid.clone(), true)]));
        let spawn_count = mock.spawn_count.clone();
        let reclaim_count = mock.reclaim_count.clone();
        let manager = GuestManager::new("test-hotel", graph.clone(), Box::new(mock));

        manager
            .check_heal_dispatcher_heartbeat_at(1_000_000)
            .await
            .expect("watchdog tick");

        assert_eq!(spawn_count.load(Ordering::SeqCst), 0);
        assert_eq!(reclaim_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn heal_dispatcher_watchdog_restarts_wedged_dispatcher_and_files_heal_entry() {
        let pid = "12345".to_string();
        let graph = make_domain(TestGraphAdapter::with_guests(vec![heal_dispatcher_guest(
            "test-hotel:heal-dispatcher",
            Some(&pid),
        )]));
        graph
            .set_config_value(HEAL_DISPATCHER_HEARTBEAT_KEY, "1000")
            .expect("set heartbeat");

        let mock = MockMaterializer::new(HashMap::from([
            (pid.clone(), true),
            ("spawned-1".to_string(), true),
        ]));
        let spawn_count = mock.spawn_count.clone();
        let reclaim_count = mock.reclaim_count.clone();
        let heal = Arc::new(MockHealQueue::new());
        let manager = GuestManager::new("test-hotel", graph.clone(), Box::new(mock))
            .with_heal_queue(heal.clone());

        // now - heartbeat = 91s, one second past the 90s staleness threshold.
        manager
            .check_heal_dispatcher_heartbeat_at(1_091)
            .await
            .expect("watchdog tick");

        assert_eq!(
            reclaim_count.load(Ordering::SeqCst),
            1,
            "wedged dispatcher must be killed"
        );
        assert_eq!(
            spawn_count.load(Ordering::SeqCst),
            1,
            "and respawned exactly once"
        );
        let pushes = heal.pushes.lock().unwrap();
        assert_eq!(pushes.len(), 1, "auto-restart must be filed, never silent");
        assert_eq!(pushes[0].0, "test-hotel:heal-dispatcher");
        assert!(pushes[0].1.contains("auto-restarted"));

        let guests = graph.list_guests("test-hotel", false).expect("list guests");
        assert_eq!(guests[0].active_pid.as_deref(), Some("spawned-1"));
    }

    #[tokio::test]
    async fn heal_dispatcher_watchdog_absent_heartbeat_past_grace_window_restarts() {
        // A dispatcher wedged before completing its FIRST cycle never writes
        // a heartbeat at all — this must still be caught, judged against how
        // long the PID has been observed, not left to run unchecked forever.
        let pid = "12345".to_string();
        let graph = make_domain(TestGraphAdapter::with_guests(vec![heal_dispatcher_guest(
            "test-hotel:heal-dispatcher",
            Some(&pid),
        )]));
        // No heartbeat key written at all.

        let mock = MockMaterializer::new(HashMap::from([
            (pid.clone(), true),
            ("spawned-1".to_string(), true),
        ]));
        let spawn_count = mock.spawn_count.clone();
        let manager = GuestManager::new("test-hotel", graph.clone(), Box::new(mock));

        // First tick observes the PID (grace window starts at t=1000); still
        // within the grace window, so no restart yet.
        manager
            .check_heal_dispatcher_heartbeat_at(1_000)
            .await
            .expect("first tick");
        assert_eq!(spawn_count.load(Ordering::SeqCst), 0);

        // Past the grace window with the SAME pid still no heartbeat: restart.
        manager
            .check_heal_dispatcher_heartbeat_at(1_000 + HEAL_DISPATCHER_STALE_SECS + 1)
            .await
            .expect("second tick");
        assert_eq!(spawn_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn heal_dispatcher_watchdog_stops_at_shared_respawn_budget_and_escalates() {
        // A dispatcher that never recovers (heartbeat permanently frozen) must
        // stop being auto-restarted once it hits the SAME respawn budget every
        // other heal/supervisor restart shares, and the budget exhaustion must
        // escalate exactly once — never a parallel/unbounded restart loop.
        let pid = "12345".to_string();
        let mut status = HashMap::from([(pid.clone(), true)]);
        for i in 1..=(RESPAWN_BUDGET_MAX + 2) {
            status.insert(format!("spawned-{i}"), true);
        }
        let graph = make_domain(TestGraphAdapter::with_guests(vec![heal_dispatcher_guest(
            "test-hotel:heal-dispatcher",
            Some(&pid),
        )]));
        graph
            .set_config_value(HEAL_DISPATCHER_HEARTBEAT_KEY, "0")
            .expect("set heartbeat");

        let mock = MockMaterializer::new(status);
        let spawn_count = mock.spawn_count.clone();
        let heal = Arc::new(MockHealQueue::new());
        let manager = GuestManager::new("test-hotel", graph.clone(), Box::new(mock))
            .with_heal_queue(heal.clone());

        // Ticks 60s apart: heartbeat frozen at 0 is always stale, and 60s
        // spacing accumulates RESPAWN_BUDGET_MAX attempts inside one 600s
        // sliding window (mirrors `heal_restart_budget_skips_and_marks_after_sixth_in_window`,
        // just with a wider, still sub-window, spacing to also exercise the
        // per-tick PID-first-seen bookkeeping across respawns).
        for i in 0..(RESPAWN_BUDGET_MAX + 2) {
            let now = 1_000 + (i as u64) * 60;
            manager
                .check_heal_dispatcher_heartbeat_at(now)
                .await
                .expect("watchdog tick");
        }

        assert_eq!(
            spawn_count.load(Ordering::SeqCst),
            RESPAWN_BUDGET_MAX,
            "heal-restarts must stop at the shared respawn budget"
        );

        let pushes = heal.pushes.lock().unwrap();
        // One heal_queue entry per successful auto-restart, plus exactly one
        // budget-exhaustion escalation — never silently absent, never doubled.
        assert_eq!(pushes.len(), RESPAWN_BUDGET_MAX + 1);
        assert!(
            pushes.last().unwrap().1.contains("exhausted"),
            "final push must be the budget-exhaustion escalation: {:?}",
            pushes.last()
        );

        let state = graph
            .get_config_value("supervision_state:test-hotel:test-hotel:heal-dispatcher")
            .expect("config read")
            .expect("supervision_state should be set");
        assert!(state.contains("respawn_budget_exhausted"));
    }

    #[test]
    fn stdout_health_signal_matches_error_level_with_ansi() {
        // Real journal sample shape: tracing fmt with ANSI color codes.
        let line = "\u{1b}[2m2026-07-20T13:20:12.050455Z\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m \u{1b}[2mmembrane_discord\u{1b}[0m\u{1b}[2m:\u{1b}[0m Failed to handle agent reply: Discord API error 405 Method Not Allowed";
        assert!(stdout_line_is_health_signal(line));
    }

    #[test]
    fn stdout_health_signal_matches_warn_with_auth_marker() {
        let line = "\u{1b}[33m WARN\u{1b}[0m memory_core::rest_client: Cross-scope activation failed for vault; continuing with others vault=self_agent-beacon error=HTTP status client error (401 Unauthorized) for url (http://127.0.0.1:8475/api/activate)";
        assert!(stdout_line_is_health_signal(line));
    }

    #[test]
    fn stdout_health_signal_skips_info_and_plain_warn() {
        assert!(!stdout_line_is_health_signal(
            "2026-07-20T13:20:12Z  INFO agent_core::runtime::turn_loop: Agent dispatch: action peek"
        ));
        // A WARN without an auth/API marker stays log-only (e.g. the mesh
        // EMSGSIZE broadcast warns every 30s — pure noise for the heal queue).
        assert!(!stdout_line_is_health_signal(
            "2026-07-20T13:18:55Z  WARN aiua::service::mesh_runtime: hotel-state broadcast to 100.79.239.64:13112: Message too long (os error 40)"
        ));
        // Message BODIES that merely contain the word "error" without an
        // ERROR level token do not match.
        assert!(!stdout_line_is_health_signal(
            "2026-07-20T13:20:12Z  INFO philote: tool result contained field error=none"
        ));
    }

    #[test]
    fn strip_ansi_removes_sgr_sequences() {
        assert_eq!(
            strip_ansi("\u{1b}[31mERROR\u{1b}[0m plain \u{1b}[2mdim\u{1b}[0m"),
            "ERROR plain dim"
        );
        assert_eq!(strip_ansi("no escapes"), "no escapes");
    }
}
