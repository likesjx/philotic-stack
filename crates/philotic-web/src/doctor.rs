//! `phil doctor` — self-diagnosis, with an optional repair engine (slice 1).
//!
//! Named, versioned checks against the hotel's context DB (and, for a
//! handful of checks, `launchctl`/`pgrep`/filesystem probes) that reproduce
//! real philotic incidents in seconds instead of ad-hoc SQL + `launchctl
//! print`. No check's `detect()` may write to the context DB (opened
//! `SQLITE_OPEN_READ_ONLY`) or mutate any system state — detection stays
//! read-only in every slice.
//!
//! Slice 1 adds an opt-in repair engine on top of the slice-0 checks:
//! `Check::repair()` evaluates (dry-run) or executes (`--fix`) a fix for one
//! finding. Only `logs.rotation-missing` and `ipc.stale-socket` are ever
//! auto-applied; `ports.hotel-record-drift` and `proc.orphan-instances`
//! always require explicit operator confirmation
//! (`RepairOutcome::NeedsConfirm`); `vault.key-source-divergence` is never
//! repaired by doctor at all (`RepairOutcome::NotRepairable`) — key material
//! is only ever touched through the RotateSecret IPC path. Anything a repair
//! "removes" is quarantined (renamed aside), never deleted. See
//! `PHIL_DOCTOR_EXPLAIN_PROPOSAL.md` for the full design; this module
//! implements the five slice-0 checks plus their slice-1 repair coverage.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::str::FromStr;

use crate::init::{active_profile, philotic_dir, profile_dir};

// ── Severity ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
            Severity::Critical => "critical",
        };
        write!(f, "{s}")
    }
}

impl FromStr for Severity {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "info" => Ok(Severity::Info),
            "warning" | "warn" => Ok(Severity::Warning),
            "error" => Ok(Severity::Error),
            "critical" | "crit" => Ok(Severity::Critical),
            other => {
                anyhow::bail!("invalid severity '{other}' (expected info|warning|error|critical)")
            }
        }
    }
}

// ── Finding / Check ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub check_id: String,
    pub severity: Severity,
    pub message: String,
    pub evidence: serde_json::Value,
    pub fix_hint: String,
    /// Metadata only in slice 0 — no `repair()` exists yet; this documents
    /// which checks the (future) repair engine is expected to cover.
    pub auto_repairable: bool,
}

/// Outcome of evaluating (dry-run) or executing (`apply`) a check's repair
/// for one [`Finding`].
///
/// `Planned`/`NeedsConfirm`/`NotRepairable` are the only outcomes a
/// `apply = false` (dry-run) call may return — dry-run never mutates
/// anything. `Applied` is only returned once a repair has actually
/// executed under `apply = true`; `NeedsConfirm` is returned unchanged in
/// both modes for checks that always require an explicit operator decision.
/// `Skipped` covers an `apply = true` attempt that could not complete (e.g.
/// missing sudo) without treating that as a hard error.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RepairOutcome {
    /// Dry-run: what `--fix` would do, described but not executed.
    Planned { description: String },
    /// `--fix` executed the repair.
    Applied { description: String },
    /// This check's repair always requires an explicit operator decision —
    /// never auto-applied, in either dry-run or `--fix` mode.
    NeedsConfirm { description: String, risk: String },
    /// `--fix` attempted the repair but could not complete it (e.g. no
    /// non-interactive sudo); `reason` includes the manual fallback.
    Skipped { reason: String },
    /// No repair engine coverage for this check at all (guided-fix only).
    NotRepairable,
}

/// A named, versioned diagnostic.
pub trait Check {
    /// Namespaced, stable id (e.g. "ports.hotel-record-drift"). Stable
    /// across releases — CI `--only`/`--skip` and suppressions key on it.
    fn id(&self) -> &'static str;

    /// Nominal severity for catalog listings; individual findings may carry
    /// a different (usually higher) severity than this default.
    fn severity(&self) -> Severity;

    /// Detect the condition. Must not write to `ctx` or any system state —
    /// `ctx.conn` is opened read-only, so an accidental write errors rather
    /// than silently mutating the DB.
    fn detect(&self, ctx: &DoctorCtx) -> Result<Vec<Finding>>;

    /// Evaluate (`apply = false`) or execute (`apply = true`) this check's
    /// repair for one finding it produced. Default: no repair coverage.
    ///
    /// Routing is per-check, not driven by `Finding::auto_repairable` — that
    /// slice-0 flag documents which checks the (then-future) repair engine
    /// was *expected* to cover, and its true/false values do not line up
    /// with slice-1 apply-vs-confirm semantics (e.g. logs is `false` but
    /// auto-repairs; ports/orphans are `true` but require confirmation).
    fn repair(&self, _ctx: &DoctorCtx, _finding: &Finding, _apply: bool) -> Result<RepairOutcome> {
        Ok(RepairOutcome::NotRepairable)
    }
}

// ── DoctorCtx ────────────────────────────────────────────────────────────

pub struct DoctorCtx {
    pub hotel: String,
    pub profile_dir: PathBuf,
    pub db_path: PathBuf,
    pub conn: Connection,
}

/// Filename of the hotel's primary context DB — must stay in sync with
/// `startup_test_db_path()` in `crates/aiua/src/main.rs`, which resolves the
/// real hotel's primary store as `profile_dir().join("context.db")`. This is
/// deliberately **not** `aiua_context.db`: for at least the `bjork` profile
/// that file holds only operator-auth tables (no `vault_secrets`,
/// `graph_nodes`, `hotels`, ...), so opening it made doctor's vault/ports
/// checks either false-positive or hard-error with "no such table".
const CONTEXT_DB_FILENAME: &str = "context.db";

impl DoctorCtx {
    /// Resolve and open the hotel's context DB.
    ///
    /// Precedence: `db_override` (`--db`) > `profile_override` (`--profile`)
    /// > `PHILOTIC_PROFILE` env. There is deliberately **no** current-working-
    /// directory fallback — grading a stray `context.db`/`aiua_context.db`
    /// that happens to sit in the CWD produced a real false-positive
    /// "critical" vault finding in the field. When none of the three sources
    /// resolve, `open()` refuses with a clear, actionable error instead of
    /// silently picking a file.
    pub fn open(
        hotel: &str,
        db_override: Option<PathBuf>,
        profile_override: Option<String>,
    ) -> Result<Self> {
        let (db_path, resolved_profile_dir) =
            resolve_db_target(db_override, profile_override.as_deref())?;
        Self::open_at_with_profile_dir(hotel, db_path, resolved_profile_dir)
    }

    /// Open against an explicit DB path, defaulting `profile_dir` to the
    /// env-derived `init::profile_dir()` (used directly by tests against a
    /// fixture DB, where the profile dir rarely matters). Production code
    /// goes through [`DoctorCtx::open`] instead, so this is effectively a
    /// test-only entry point despite being `pub`.
    #[allow(dead_code)]
    pub fn open_at(hotel: &str, db_path: PathBuf) -> Result<Self> {
        Self::open_at_with_profile_dir(hotel, db_path, profile_dir())
    }

    fn open_at_with_profile_dir(
        hotel: &str,
        db_path: PathBuf,
        resolved_profile_dir: PathBuf,
    ) -> Result<Self> {
        if !db_path.exists() {
            anyhow::bail!(
                "context DB not found at {} — has this hotel ever booted?",
                db_path.display()
            );
        }
        let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("open {} read-only", db_path.display()))?;
        conn.busy_timeout(std::time::Duration::from_millis(2000))?;
        Ok(Self {
            hotel: hotel.to_string(),
            profile_dir: resolved_profile_dir,
            db_path,
            conn,
        })
    }
}

/// Resolve which context DB `open()` should target, and the profile
/// directory used for profile-relative artifacts (`aiua.log`, the repair
/// journal). See [`DoctorCtx::open`] for precedence and the no-CWD-fallback
/// rationale.
fn resolve_db_target(
    db_override: Option<PathBuf>,
    profile_override: Option<&str>,
) -> Result<(PathBuf, PathBuf)> {
    if let Some(path) = db_override {
        let dir = path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        return Ok((path, dir));
    }
    if let Some(name) = profile_override {
        let dir = philotic_dir().join(name);
        return Ok((dir.join(CONTEXT_DB_FILENAME), dir));
    }
    match active_profile() {
        Some(name) => {
            let dir = philotic_dir().join(name);
            Ok((dir.join(CONTEXT_DB_FILENAME), dir))
        }
        None => anyhow::bail!(
            "no hotel targeted — set PHILOTIC_PROFILE=<profile>, or pass --profile <name> or --db <path>"
        ),
    }
}

/// True when `table` exists in `conn`'s schema. Used by checks whose
/// `detect()` reads a table (`hotels`, `vault_secrets`, ...) that a
/// wrong-DB or pre-migration store may not have, so a missing table can be
/// reported as a clear, check-scoped finding instead of a raw SQL error that
/// aborts the whole run.
fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        params![table],
        |row| row.get(0),
    )?;
    Ok(exists)
}

/// If `table` is missing from `ctx`'s DB, returns `Some` with a single
/// warning-severity finding explaining the check was skipped and inviting
/// the operator to double-check `--db`/`--profile`; returns `None` when the
/// table exists so the caller's normal detection logic proceeds. Never
/// itself a hard error unless the `sqlite_master` lookup fails.
fn missing_table_finding(
    ctx: &DoctorCtx,
    check_id: &'static str,
    table: &str,
) -> Result<Option<Vec<Finding>>> {
    if table_exists(&ctx.conn, table)? {
        return Ok(None);
    }
    Ok(Some(vec![Finding {
        check_id: check_id.to_string(),
        severity: Severity::Warning,
        message: format!(
            "expected table '{table}' not present in {} — is this the right hotel DB?",
            ctx.db_path.display()
        ),
        evidence: json!({"table": table, "db_path": ctx.db_path.to_string_lossy()}),
        fix_hint: format!(
            "this store has no '{table}' table — verify --db/--profile (or PHILOTIC_PROFILE) \
             targets the hotel's primary {CONTEXT_DB_FILENAME}, not a stale or auth-only DB; \
             check skipped, not run"
        ),
        auto_repairable: false,
    }]))
}

// ── Repair journal ───────────────────────────────────────────────────────
//
// Append-only, one JSON object per line, one line per successfully applied
// repair. Anything a repair "removes" must be quarantined (renamed aside)
// rather than deleted — the journal records `before`/`after` so a quarantine
// (or any other applied repair) can always be traced back.

fn repair_journal_path(profile_dir: &std::path::Path) -> PathBuf {
    profile_dir.join("doctor-repair-journal.jsonl")
}

fn repair_journal_next_id(path: &std::path::Path) -> Result<u64> {
    if !path.exists() {
        return Ok(1);
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read repair journal {}", path.display()))?;
    Ok(content.lines().filter(|l| !l.trim().is_empty()).count() as u64 + 1)
}

/// Append one `Applied` repair to `<profile_dir>/doctor-repair-journal.jsonl`.
/// Only called from an `apply = true` repair path — dry-run never calls this.
fn append_repair_journal(
    profile_dir: &std::path::Path,
    check_id: &str,
    action: &str,
    before: serde_json::Value,
    after: serde_json::Value,
) -> Result<()> {
    use std::io::Write;

    std::fs::create_dir_all(profile_dir)
        .with_context(|| format!("create profile dir {}", profile_dir.display()))?;
    let path = repair_journal_path(profile_dir);
    let id = repair_journal_next_id(&path)?;
    let entry = json!({
        "id": id,
        "check_id": check_id,
        "action": action,
        "before": before,
        "after": after,
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open repair journal {}", path.display()))?;
    writeln!(file, "{}", serde_json::to_string(&entry)?)
        .with_context(|| format!("append repair journal {}", path.display()))?;
    Ok(())
}

/// Search ancestors of the current working directory for `scripts/<name>`,
/// the same pattern `mcp::find_uat_script` uses — robust to being invoked
/// from any subdirectory of the repo checkout, `None` (not an error) when
/// the binary is running detached from a repo checkout at all.
fn find_repo_script(name: &str) -> Option<PathBuf> {
    let current = std::env::current_dir().ok()?;
    for dir in current.ancestors() {
        let candidate = dir.join("scripts").join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// ── Catalog ──────────────────────────────────────────────────────────────

pub fn catalog() -> Vec<Box<dyn Check>> {
    vec![
        Box::new(PortsHotelRecordDrift),
        Box::new(ProcOrphanInstances),
        Box::new(IpcStaleSocket),
        Box::new(VaultKeySourceDivergence),
        Box::new(LogsRotationMissing),
        Box::new(SecretsStorePermissions),
        Box::new(HealQueueDepth),
        Box::new(HealOldestPendingAge),
        Box::new(HealDispatcherStaleness),
        Box::new(SystemDiskSpace),
    ]
}

// ── ports.hotel-record-drift ────────────────────────────────────────────

/// Mirrors `sanitize_hotel_name` in `crates/aiua/src/main.rs:490` exactly —
/// duplicated here because aiua ships bin-only (no lib target) and doctor
/// must work standalone, including when the hotel binary can't even boot.
fn sanitize_hotel_name(hotel_name: &str) -> String {
    hotel_name
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '-',
        })
        .collect()
}

/// Mirrors `hotel_base_port` in `crates/aiua/src/main.rs:500` exactly.
fn deterministic_base_port(sanitized_hotel_name: &str) -> u16 {
    let mut hash: u16 = 0;
    for byte in sanitized_hotel_name.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(byte as u16);
    }
    10_000 + (hash % 20_000)
}

fn evaluate_ports_drift(
    check_id: &'static str,
    hotel_name: &str,
    actual: Option<(u16, u16, u16)>,
) -> Vec<Finding> {
    let Some((mesh, blob, execution)) = actual else {
        return vec![Finding {
            check_id: check_id.to_string(),
            severity: Severity::Warning,
            message: format!(
                "no hotels row found for '{hotel_name}' — hotel has never booted, or --hotel is wrong"
            ),
            evidence: json!({"hotel": hotel_name}),
            fix_hint: format!(
                "run `phil load` then `phil start --hotel {hotel_name}`, or check the --hotel spelling"
            ),
            auto_repairable: false,
        }];
    };

    let safe = sanitize_hotel_name(hotel_name);
    let base = deterministic_base_port(&safe);
    let expected = (base, base + 1, base + 2);

    if (mesh, blob, execution) == expected {
        return Vec::new();
    }

    vec![Finding {
        check_id: check_id.to_string(),
        severity: Severity::Error,
        message: format!(
            "hotel '{hotel_name}' persisted ports {mesh}/{blob}/{execution} diverge from the \
             deterministic default {}/{}/{} — a fallback port cluster \
             (nearest_available_base_port, main.rs:594) may have been written back \
             permanently by resolve_runtime_ports (main.rs:618, persisted at main.rs:7286-7289)",
            expected.0, expected.1, expected.2
        ),
        evidence: json!({
            "hotel": hotel_name,
            "actual": {"mesh_port": mesh, "blob_port": blob, "execution_port": execution},
            "expected_default": {
                "mesh_port": expected.0,
                "blob_port": expected.1,
                "execution_port": expected.2
            },
        }),
        fix_hint: format!(
            "verify the canonical port cluster ({}, {}, {}) is free, then restore \
             hotels.mesh_port/blob_port/execution_port and restart the hotel; if these ports \
             were deliberately customized, this finding is expected and can be ignored",
            expected.0, expected.1, expected.2
        ),
        auto_repairable: true,
    }]
}

struct PortsHotelRecordDrift;

impl Check for PortsHotelRecordDrift {
    fn id(&self) -> &'static str {
        "ports.hotel-record-drift"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn detect(&self, ctx: &DoctorCtx) -> Result<Vec<Finding>> {
        if let Some(findings) = missing_table_finding(ctx, self.id(), "hotels")? {
            return Ok(findings);
        }
        let row: Option<(u16, u16, u16)> = ctx
            .conn
            .query_row(
                "SELECT mesh_port, blob_port, execution_port FROM hotels WHERE hotel_name = ?1",
                params![ctx.hotel],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        Ok(evaluate_ports_drift(self.id(), &ctx.hotel, row))
    }

    /// Restoring the canonical ports requires writing `hotels.mesh_port` /
    /// `blob_port` / `execution_port` and restarting the hotel — always
    /// `NeedsConfirm`, in both dry-run and `--fix` mode; never auto-applied.
    fn repair(&self, _ctx: &DoctorCtx, finding: &Finding, _apply: bool) -> Result<RepairOutcome> {
        if let Some(expected) = finding.evidence.get("expected_default") {
            Ok(RepairOutcome::NeedsConfirm {
                description: format!(
                    "restore hotels.mesh_port/blob_port/execution_port to the canonical \
                     default {}/{}/{} and restart the hotel",
                    expected["mesh_port"], expected["blob_port"], expected["execution_port"]
                ),
                risk: "writes the hotels row and requires a hotel restart; the canonical \
                       ports may already be in use by something else — operator must confirm \
                       before applying"
                    .to_string(),
            })
        } else {
            Ok(RepairOutcome::NeedsConfirm {
                description:
                    "run `phil load` then `phil start --hotel <name>` to create the hotels \
                     row, or verify the --hotel spelling"
                        .to_string(),
                risk: "no destructive action, but creates new hotel state — operator must \
                       confirm the hotel identity first"
                    .to_string(),
            })
        }
    }
}

// ── proc.orphan-instances ───────────────────────────────────────────────

fn launchd_pid(hotel: &str) -> Option<u32> {
    let target = crate::service::service_target(hotel);
    let output = std::process::Command::new("launchctl")
        .args(["print", &target])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("pid = ") {
            return rest.trim().parse().ok();
        }
    }
    None
}

/// True when `cmdline` (a process's full argv joined with whitespace)
/// contains a literal `--hotel <hotel>` token pair — i.e. `hotel` is an
/// exact argument, not merely a substring of some other hotel's name.
///
/// `pgrep -f` does unanchored substring matching, so a naive
/// `"aiua --hotel {hotel}"` pattern for hotel `jane` would also match a
/// real `aiua --hotel jane2 --foo` process. Parsing argv tokens instead of
/// pattern-matching raw text avoids that prefix collision entirely.
fn command_line_has_exact_hotel_arg(cmdline: &str, hotel: &str) -> bool {
    let tokens: Vec<&str> = cmdline.split_whitespace().collect();
    tokens
        .windows(2)
        .any(|pair| pair[0] == "--hotel" && pair[1] == hotel)
}

fn pid_command_line(pid: u32) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn running_aiua_pids(hotel: &str) -> Vec<u32> {
    // Broad substring match to find candidate `aiua --hotel ...`
    // processes, then verify each candidate's actual argv contains an
    // exact `--hotel <hotel>` pair. This avoids `pgrep -f`'s unanchored
    // substring matching flagging e.g. hotel `jane` against a real
    // `aiua --hotel jane2` process.
    let output = std::process::Command::new("pgrep")
        .args(["-f", "aiua --hotel"])
        .output();
    let candidates: Vec<u32> = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| l.trim().parse::<u32>().ok())
            .collect(),
        _ => return Vec::new(),
    };

    candidates
        .into_iter()
        .filter(|&pid| {
            pid_command_line(pid)
                .map(|cmdline| command_line_has_exact_hotel_arg(&cmdline, hotel))
                .unwrap_or(false)
        })
        .collect()
}

fn hotel_active_pid(conn: &Connection, hotel: &str) -> Result<Option<u32>> {
    let raw: Option<Option<String>> = conn
        .query_row(
            "SELECT active_pid FROM hotels WHERE hotel_name = ?1",
            params![hotel],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    Ok(raw.flatten().and_then(|s| s.trim().parse::<u32>().ok()))
}

fn evaluate_orphans(
    check_id: &'static str,
    launchd_pid: Option<u32>,
    active_pid: Option<u32>,
    running: &[u32],
) -> Vec<Finding> {
    let orphans: Vec<u32> = running
        .iter()
        .copied()
        .filter(|pid| Some(*pid) != launchd_pid && Some(*pid) != active_pid)
        .collect();

    if orphans.is_empty() {
        return Vec::new();
    }

    vec![Finding {
        check_id: check_id.to_string(),
        severity: Severity::Critical,
        message: format!(
            "{} orphan aiua process(es) neither owned by launchd (pid {:?}) nor \
             hotels.active_pid ({:?}): {:?}",
            orphans.len(),
            launchd_pid,
            active_pid,
            orphans
        ),
        evidence: json!({
            "orphan_pids": orphans,
            "launchd_pid": launchd_pid,
            "active_pid": active_pid,
            "running_pids": running,
        }),
        fix_hint: format!(
            "SIGTERM the orphan pid(s) only, never the launchd-owned pid: {}",
            orphans
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        auto_repairable: true,
    }]
}

struct ProcOrphanInstances;

impl Check for ProcOrphanInstances {
    fn id(&self) -> &'static str {
        "proc.orphan-instances"
    }

    fn severity(&self) -> Severity {
        Severity::Critical
    }

    fn detect(&self, ctx: &DoctorCtx) -> Result<Vec<Finding>> {
        if let Some(findings) = missing_table_finding(ctx, self.id(), "hotels")? {
            return Ok(findings);
        }
        let launchd = launchd_pid(&ctx.hotel);
        let active = hotel_active_pid(&ctx.conn, &ctx.hotel)?;
        let running = running_aiua_pids(&ctx.hotel);
        Ok(evaluate_orphans(self.id(), launchd, active, &running))
    }

    /// SIGTERMing the orphan pid(s) is destructive if the pid list is ever
    /// wrong — always `NeedsConfirm`, in both dry-run and `--fix` mode;
    /// never auto-killed.
    fn repair(&self, _ctx: &DoctorCtx, finding: &Finding, _apply: bool) -> Result<RepairOutcome> {
        let orphans = finding
            .evidence
            .get("orphan_pids")
            .cloned()
            .unwrap_or_else(|| json!([]));
        Ok(RepairOutcome::NeedsConfirm {
            description: format!(
                "SIGTERM the orphan pid(s) only, never the launchd-owned pid: {orphans}"
            ),
            risk: "signaling the wrong pid could take down the supervised hotel process — \
                   operator must confirm the exact pid list before signaling"
                .to_string(),
        })
    }
}

// ── ipc.stale-socket ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SocketProbe {
    Missing,
    Alive,
    Stale,
}

fn probe_socket(path: &str) -> SocketProbe {
    let p = std::path::Path::new(path);
    if !p.exists() {
        return SocketProbe::Missing;
    }
    match std::os::unix::net::UnixStream::connect(p) {
        Ok(_) => SocketProbe::Alive,
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => SocketProbe::Stale,
        // Any other error (e.g. not-a-socket, permission denied) is ambiguous —
        // don't claim "stale" without the specific evidence that proves it.
        Err(_) => SocketProbe::Alive,
    }
}

fn evaluate_stale_socket(check_id: &'static str, path: &str, probe: SocketProbe) -> Vec<Finding> {
    if probe != SocketProbe::Stale {
        return Vec::new();
    }
    vec![Finding {
        check_id: check_id.to_string(),
        severity: Severity::Error,
        message: format!("socket {path} exists but refuses connections (no live owner)"),
        evidence: json!({"path": path}),
        fix_hint: format!(
            "verify no aiua process owns {path}, then remove or rename it \
             (a future --fix will rename to {path}.stale-<ts>)"
        ),
        auto_repairable: true,
    }]
}

struct IpcStaleSocket;

impl Check for IpcStaleSocket {
    fn id(&self) -> &'static str {
        "ipc.stale-socket"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn detect(&self, ctx: &DoctorCtx) -> Result<Vec<Finding>> {
        let path = crate::start::socket_path(&ctx.hotel);
        let probe = probe_socket(&path);
        Ok(evaluate_stale_socket(self.id(), &path, probe))
    }

    /// Quarantines the confirmed-dead socket by renaming it to `<path>.stale`
    /// — never `rm`s it, so a wrong "stale" call is always recoverable.
    fn repair(&self, ctx: &DoctorCtx, finding: &Finding, apply: bool) -> Result<RepairOutcome> {
        let path = finding
            .evidence
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let stale_path = format!("{path}.stale");
        let description =
            format!("quarantine stale socket {path} by renaming it to {stale_path} (never delete)");

        if !apply {
            return Ok(RepairOutcome::Planned { description });
        }

        match std::fs::rename(&path, &stale_path) {
            Ok(()) => {
                append_repair_journal(
                    &ctx.profile_dir,
                    self.id(),
                    "quarantine-socket",
                    json!({"path": path}),
                    json!({"path": stale_path}),
                )?;
                Ok(RepairOutcome::Applied { description })
            }
            Err(e) => Ok(RepairOutcome::Skipped {
                reason: format!(
                    "failed to rename {path} to {stale_path}: {e}; manual: mv {path} {stale_path}"
                ),
            }),
        }
    }
}

// ── vault.key-source-divergence ─────────────────────────────────────────

const VAULT_ENV_KEY: &str = "PHILOTIC_VAULT_MASTER_KEY";
const VAULT_KEY_ID_ENV_KEY: &str = "PHILOTIC_VAULT_KEY_ID";
const VAULT_KEYCHAIN_SERVICE: &str = "ai.philotic.hotel-vault";
const VAULT_KEYCHAIN_DEFAULT_ACCOUNT: &str = "default-root-key";

fn fingerprint(key: &[u8]) -> String {
    let digest = Sha256::digest(key);
    hex::encode(&digest[..4])
}

fn try_decrypt(key: &[u8], ciphertext_b64: &str, nonce_b64: &str) -> bool {
    if key.len() != 32 {
        return false;
    }
    let Ok(ciphertext) = BASE64_STANDARD.decode(ciphertext_b64) else {
        return false;
    };
    let Ok(nonce_bytes) = BASE64_STANDARD.decode(nonce_b64) else {
        return false;
    };
    if nonce_bytes.len() != 12 {
        return false;
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(&nonce_bytes);
    cipher.decrypt(nonce, ciphertext.as_ref()).is_ok()
}

fn decode_key(raw: &str) -> Option<Vec<u8>> {
    let bytes = BASE64_STANDARD.decode(raw.trim()).ok()?;
    if bytes.len() == 32 {
        Some(bytes)
    } else {
        None
    }
}

fn env_key_source() -> Option<Vec<u8>> {
    std::env::var(VAULT_ENV_KEY)
        .ok()
        .and_then(|v| decode_key(&v))
}

fn file_key_source() -> Option<Vec<u8>> {
    let home = std::env::var_os("HOME")?;
    let path = PathBuf::from(home)
        .join(".philotic")
        .join("vault-master-key.env");
    let content = std::fs::read_to_string(path).ok()?;
    content.lines().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return None;
        }
        let (key, value) = trimmed.split_once('=')?;
        if key.trim() == VAULT_ENV_KEY {
            decode_key(value.trim())
        } else {
            None
        }
    })
}

fn vault_key_account() -> String {
    std::env::var(VAULT_KEY_ID_ENV_KEY)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| VAULT_KEYCHAIN_DEFAULT_ACCOUNT.to_string())
}

/// Read-only Keychain lookup. Unlike aiua's `load_or_create_root_key`, this
/// never generates or stores a key on a miss — doctor must not write.
fn keychain_key_source() -> Option<Vec<u8>> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            VAULT_KEYCHAIN_SERVICE,
            "-a",
            &vault_key_account(),
            "-w",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    decode_key(raw.trim())
}

/// Precedence order matches `load_or_create_root_key`
/// (`crates/aiua/src/vault.rs:164`, fixed by b87b581): env -> file -> keychain.
fn resolve_vault_sources() -> Vec<(&'static str, Option<Vec<u8>>)> {
    vec![
        ("env", env_key_source()),
        ("file", file_key_source()),
        ("keychain", keychain_key_source()),
    ]
}

/// One secret enumerated from either the legacy `vault_secrets` table or a
/// `graph_nodes` row whose `node_key` starts with `secret:` — the two real
/// secret stores in a hotel's context DB (see module docs at the top of the
/// vault section). `referenced` is precomputed by [`collect_vault_secrets`]
/// against `node_config` and every *other* `graph_nodes` row.
#[derive(Debug, Clone)]
struct VaultSecretProbe {
    secret_ref: String,
    secret_kind: String,
    source: &'static str,
    ciphertext_b64: String,
    nonce_b64: String,
    referenced: bool,
}

/// Evidence-only view of a [`VaultSecretProbe`] — deliberately excludes
/// `ciphertext_b64`/`nonce_b64`. Findings must never carry plaintext or
/// ciphertext material, only refs/kinds/booleans.
#[derive(Debug, Clone, Serialize)]
struct VaultSecretEvidence<'a> {
    secret_ref: &'a str,
    secret_kind: &'a str,
    source: &'a str,
    referenced: bool,
    decrypts: bool,
}

fn evaluate_vault_divergence(
    check_id: &'static str,
    sources: &[(&'static str, Option<Vec<u8>>)],
    secrets: &[VaultSecretProbe],
) -> Vec<Finding> {
    let fingerprints: serde_json::Value = serde_json::to_value(
        sources
            .iter()
            .map(|(name, key)| (name.to_string(), key.as_ref().map(|k| fingerprint(k))))
            .collect::<std::collections::HashMap<_, _>>(),
    )
    .unwrap_or(serde_json::Value::Null);

    let effective = sources
        .iter()
        .find_map(|(name, key)| key.as_ref().map(|k| (*name, k)));

    let Some((source_name, key)) = effective else {
        if secrets.is_empty() {
            // No key configured yet and nothing to decrypt — not a problem
            // before the first secret is stored.
            return Vec::new();
        }
        return vec![Finding {
            check_id: check_id.to_string(),
            severity: Severity::Warning,
            message: format!(
                "no vault key source resolvable offline (no {VAULT_ENV_KEY} env, key file, or \
                 Keychain entry found) while {} secret row(s) exist across vault_secrets + \
                 graph_nodes",
                secrets.len()
            ),
            evidence: json!({"sources": fingerprints, "secret_count": secrets.len()}),
            fix_hint: format!(
                "set {VAULT_ENV_KEY} or create ~/.philotic/vault-master-key.env, or run the \
                 hotel once so it bootstraps a Keychain key"
            ),
            auto_repairable: false,
        }];
    };

    if secrets.is_empty() {
        return Vec::new();
    }

    // Decrypt-test every secret from both stores under the effective key,
    // and classify referenced-vs-orphan failures separately — a referenced
    // secret that fails to decrypt will break live wiring on restart; an
    // orphan that fails to decrypt is legacy dead weight nothing depends on.
    let mut evidence_secrets: Vec<VaultSecretEvidence> = Vec::with_capacity(secrets.len());
    let mut referenced_failed: Vec<&VaultSecretProbe> = Vec::new();
    let mut orphan_failed: Vec<&VaultSecretProbe> = Vec::new();
    let mut decrypted_count = 0usize;

    for secret in secrets {
        let decrypts = try_decrypt(key, &secret.ciphertext_b64, &secret.nonce_b64);
        if decrypts {
            decrypted_count += 1;
        } else if secret.referenced {
            referenced_failed.push(secret);
        } else {
            orphan_failed.push(secret);
        }
        evidence_secrets.push(VaultSecretEvidence {
            secret_ref: &secret.secret_ref,
            secret_kind: &secret.secret_kind,
            source: secret.source,
            referenced: secret.referenced,
            decrypts,
        });
    }

    let total = secrets.len();
    let referenced_failures = referenced_failed.len();
    let orphan_failures = orphan_failed.len();

    if referenced_failures == 0 && orphan_failures == 0 {
        // Secrets that decrypt fine produce no finding.
        return Vec::new();
    }

    let summary_suffix = format!(
        "{decrypted_count}/{total} decrypt; {orphan_failures} orphan undecryptable; \
         {referenced_failures} referenced failures"
    );

    let (severity, message, fix_hint) = if referenced_failures > 0 {
        let message = if referenced_failures == 1 {
            let s = referenced_failed[0];
            format!(
                "referenced secret {} ({}) does not decrypt under effective key '{source_name}' \
                 — live wiring will break on restart ({summary_suffix})",
                s.secret_ref, s.secret_kind
            )
        } else {
            let refs: Vec<&str> = referenced_failed
                .iter()
                .map(|s| s.secret_ref.as_str())
                .collect();
            format!(
                "{referenced_failures} referenced secrets do not decrypt under effective key \
                 '{source_name}' — live wiring will break on restart: {} ({summary_suffix})",
                refs.join(", ")
            )
        };
        let fix_hint = "residual divergence after the env->file->keychain precedence fix \
                         (b87b581) — re-encrypt the affected REFERENCED secret(s) via the \
                         RotateSecret IPC path under the now-effective key before the next \
                         restart; doctor never touches key material"
            .to_string();
        (Severity::Critical, message, fix_hint)
    } else {
        let message = if orphan_failures == 1 {
            let s = orphan_failed[0];
            format!(
                "orphaned secret {} ({}) does not decrypt and is referenced by nothing — \
                 legacy dead weight, safe to prune ({summary_suffix})",
                s.secret_ref, s.secret_kind
            )
        } else {
            let refs: Vec<&str> = orphan_failed
                .iter()
                .map(|s| s.secret_ref.as_str())
                .collect();
            format!(
                "{orphan_failures} orphaned secrets do not decrypt and are referenced by \
                 nothing — legacy dead weight, safe to prune: {} ({summary_suffix})",
                refs.join(", ")
            )
        };
        let fix_hint = "referenced by no config key or graph node — not a live-wiring risk; \
                         safe to prune via the vault admin path once an operator confirms it's \
                         truly dead; doctor never deletes vault rows itself"
            .to_string();
        (Severity::Warning, message, fix_hint)
    };

    vec![Finding {
        check_id: check_id.to_string(),
        severity,
        message,
        evidence: json!({
            "effective_source": source_name,
            "effective_fingerprint": fingerprint(key),
            "sources": fingerprints,
            "total": total,
            "decrypted": decrypted_count,
            "orphan_undecryptable": orphan_failures,
            "referenced_failures": referenced_failures,
            "secrets": evidence_secrets,
        }),
        fix_hint,
        auto_repairable: false,
    }]
}

struct VaultKeySourceDivergence;

impl Check for VaultKeySourceDivergence {
    fn id(&self) -> &'static str {
        "vault.key-source-divergence"
    }

    fn severity(&self) -> Severity {
        Severity::Critical
    }

    fn detect(&self, ctx: &DoctorCtx) -> Result<Vec<Finding>> {
        // The legacy `vault_secrets` table missing doesn't necessarily mean
        // there's nothing to check: the hotel's live, canonical secret store
        // is `graph_nodes` `secret:*` rows (see `graph_node_secrets_from_db`).
        // Only report "table not found, skipped" when *both* stores are
        // empty — never let the presence check on one legacy table swallow
        // secrets that live exclusively in the other, current store.
        if !table_exists(&ctx.conn, "vault_secrets")?
            && graph_node_secrets_from_db(&ctx.conn)?.is_empty()
        {
            if let Some(findings) = missing_table_finding(ctx, self.id(), "vault_secrets")? {
                return Ok(findings);
            }
        }
        let secrets = collect_vault_secrets(&ctx.conn)?;
        let sources = resolve_vault_sources();
        Ok(evaluate_vault_divergence(self.id(), &sources, &secrets))
    }

    /// Guided-only, forever: doctor never touches key material. Explicit
    /// override of the trait default so this is a documented decision, not
    /// an oversight — the fix path is the RotateSecret IPC call, run by an
    /// operator, not by this repair engine.
    fn repair(&self, _ctx: &DoctorCtx, _finding: &Finding, _apply: bool) -> Result<RepairOutcome> {
        Ok(RepairOutcome::NotRepairable)
    }
}

/// One row read straight off a store, before dedup/referenced classification.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawSecret {
    secret_ref: String,
    secret_kind: String,
    ciphertext_b64: String,
    nonce_b64: String,
}

/// Enumerate secrets from the legacy `vault_secrets` table — the store
/// `phil doctor` checked exclusively before slice 3. On the live bjork
/// hotel this table holds exactly one row: an orphan referenced by no
/// config/graph node, encrypted under a pre-re-key master key.
///
/// Returns an empty vec (rather than erroring) when the table itself is
/// absent, mirroring `graph_node_secrets_from_db`'s handling of a missing
/// `graph_nodes` table — `collect_vault_secrets` calls both unconditionally
/// and relies on each degrading gracefully so a DB with only one of the two
/// stores still gets a full, correct scan of whichever store it does have.
fn vault_secrets_from_db(conn: &Connection) -> Result<Vec<RawSecret>> {
    if !table_exists(conn, "vault_secrets")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare("SELECT secret_ref, secret_kind, ciphertext_b64, nonce_b64 FROM vault_secrets")?;
    let secrets = stmt
        .query_map([], |row| {
            Ok(RawSecret {
                secret_ref: row.get(0)?,
                secret_kind: row.get(1)?,
                ciphertext_b64: row.get(2)?,
                nonce_b64: row.get(3)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(secrets)
}

/// Enumerate secrets from `graph_nodes` rows whose `node_key` starts with
/// `secret:` — the hotel's real, live secret store (see `NODE_KIND_SECRET`
/// / `GraphDomain::upsert_secret` in `ansible-mesh-core`). Each row's
/// `data_json` is a serialized `SecretRecord`; a row that fails to parse as
/// one is skipped rather than hard-erroring the whole check (defensive
/// against a future schema change doctor doesn't know about yet).
///
/// Returns `(node_key, RawSecret)` pairs — the node_key is kept so the
/// referenced-check can exclude a secret's *own* graph node when scanning
/// `graph_nodes` for references to it (its own `data_json` trivially
/// contains its own `secret_ref`, which would otherwise self-count as
/// "referenced").
fn graph_node_secrets_from_db(conn: &Connection) -> Result<Vec<(String, RawSecret)>> {
    if !table_exists(conn, "graph_nodes")? {
        return Ok(Vec::new());
    }
    let mut stmt =
        conn.prepare("SELECT node_key, data_json FROM graph_nodes WHERE node_key LIKE 'secret:%'")?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    let mut out = Vec::with_capacity(rows.len());
    for (node_key, data_json) in rows {
        if let Ok(rec) =
            serde_json::from_str::<ansible_mesh_core::storage::SecretRecord>(&data_json)
        {
            out.push((
                node_key,
                RawSecret {
                    secret_ref: rec.secret_ref,
                    secret_kind: rec.secret_kind,
                    ciphertext_b64: rec.ciphertext_b64,
                    nonce_b64: rec.nonce_b64,
                },
            ));
        }
    }
    Ok(out)
}

/// All `node_config.value_json` values — scanned in memory (rather than a
/// SQL `LIKE`) to sidestep `%`/`_` LIKE-wildcard escaping on secret refs.
fn node_config_values(conn: &Connection) -> Result<Vec<String>> {
    if !table_exists(conn, "node_config")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare("SELECT value_json FROM node_config")?;
    let values = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(values)
}

/// All `(node_key, data_json)` pairs in `graph_nodes` (every kind, not just
/// `secret:*`) — a secret can be referenced from a non-secret node too
/// (e.g. a role or config node embedding the ref).
fn all_graph_node_data_jsons(conn: &Connection) -> Result<Vec<(String, String)>> {
    if !table_exists(conn, "graph_nodes")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare("SELECT node_key, data_json FROM graph_nodes")?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// True when `secret_ref` appears in any `node_config.value_json` row, or in
/// any `graph_nodes.data_json` row other than `exclude_node_key` (the
/// secret's own node, when it has one — table-only secrets pass `None`).
///
/// Deliberately a raw substring scan, *not* a quoted/exact-field match: live
/// wiring sites serialize the ref at varying JSON-nesting depths — a plain
/// `"secret_ref"` JSON string, a `{"secret_ref": "..."}` field, or (as seen
/// on the live bjork hotel, e.g. `config:oidc_github_client_secret_ref`) a
/// *double*-JSON-encoded `{"value":"\"secret_ref\""}` string where the ref
/// is wrapped in escaped `\"` rather than a bare `"`. Requiring a literal
/// `"secret_ref"` quoted match was tried and reverted: it silently
/// misclassified real, live-referenced secrets (mesh-hotel keys, vault
/// tokens) as unreferenced orphans against the live bjork DB, downgrading a
/// Critical divergence finding to Warning — exactly the referenced/orphan
/// misclassification this check exists to catch. A bare substring match is
/// the only encoding-agnostic option; the false-positive risk it carries
/// (one secret_ref being a textual substring of another) is effectively
/// nil in practice since refs are UUID-suffixed
/// (`secret://hotel/default/<kind>/<32-hex>`).
fn secret_ref_is_referenced(
    node_config_values: &[String],
    graph_data_jsons: &[(String, String)],
    secret_ref: &str,
    exclude_node_key: Option<&str>,
) -> bool {
    if node_config_values.iter().any(|v| v.contains(secret_ref)) {
        return true;
    }
    graph_data_jsons.iter().any(|(node_key, data_json)| {
        Some(node_key.as_str()) != exclude_node_key && data_json.contains(secret_ref)
    })
}

/// Enumerate secrets from *both* stores (`vault_secrets` legacy table +
/// `graph_nodes` `secret:*` rows), de-duping by `secret_ref` — the
/// graph-node copy wins on conflict since that store is the hotel's live,
/// canonical vault; `vault_secrets` is a pre-migration leftover. Each
/// returned secret carries a precomputed `referenced` flag.
fn collect_vault_secrets(conn: &Connection) -> Result<Vec<VaultSecretProbe>> {
    let table_secrets = vault_secrets_from_db(conn)?;
    let graph_secrets = graph_node_secrets_from_db(conn)?;
    let node_config_values = node_config_values(conn)?;
    let graph_data_jsons = all_graph_node_data_jsons(conn)?;

    let mut by_ref: std::collections::BTreeMap<String, VaultSecretProbe> =
        std::collections::BTreeMap::new();

    for raw in table_secrets {
        let referenced = secret_ref_is_referenced(
            &node_config_values,
            &graph_data_jsons,
            &raw.secret_ref,
            None,
        );
        by_ref.insert(
            raw.secret_ref.clone(),
            VaultSecretProbe {
                secret_ref: raw.secret_ref,
                secret_kind: raw.secret_kind,
                source: "vault_secrets",
                ciphertext_b64: raw.ciphertext_b64,
                nonce_b64: raw.nonce_b64,
                referenced,
            },
        );
    }

    // Inserted after the table pass so a ref present in both stores ends up
    // classified as the graph-node copy (de-dup — see doc comment above).
    for (node_key, raw) in graph_secrets {
        let referenced = secret_ref_is_referenced(
            &node_config_values,
            &graph_data_jsons,
            &raw.secret_ref,
            Some(&node_key),
        );
        by_ref.insert(
            raw.secret_ref.clone(),
            VaultSecretProbe {
                secret_ref: raw.secret_ref,
                secret_kind: raw.secret_kind,
                source: "graph_node",
                ciphertext_b64: raw.ciphertext_b64,
                nonce_b64: raw.nonce_b64,
                referenced,
            },
        );
    }

    Ok(by_ref.into_values().collect())
}

// ── logs.rotation-missing ───────────────────────────────────────────────

fn newsyslog_dropin_path() -> PathBuf {
    PathBuf::from("/etc/newsyslog.d/philotic.conf")
}

const LOG_ROTATION_WARN_BYTES: u64 = 256 * 1024 * 1024;

fn evaluate_log_rotation(
    check_id: &'static str,
    log_path: &str,
    log_size_bytes: Option<u64>,
    drop_in_exists: bool,
) -> Vec<Finding> {
    if drop_in_exists {
        return Vec::new();
    }

    let severity = match log_size_bytes {
        Some(bytes) if bytes > LOG_ROTATION_WARN_BYTES => Severity::Error,
        _ => Severity::Warning,
    };

    let message = match log_size_bytes {
        Some(bytes) => format!(
            "no newsyslog rotation for {log_path} ({:.1} MiB) — launchd StandardOutPath never rotates",
            bytes as f64 / (1024.0 * 1024.0)
        ),
        None => format!(
            "no newsyslog rotation configured for {log_path} — launchd StandardOutPath never rotates"
        ),
    };

    vec![Finding {
        check_id: check_id.to_string(),
        severity,
        message,
        evidence: json!({
            "log_path": log_path,
            "size_bytes": log_size_bytes,
            "dropin_path": newsyslog_dropin_path().to_string_lossy(),
        }),
        fix_hint:
            "run scripts/install-log-rotation.sh, or sudo tee /etc/newsyslog.d/philotic.conf \
                   with a line rotating ~/.philotic/*/aiua*.log at 50MB, keep 5, bzip2-compressed"
                .to_string(),
        auto_repairable: false,
    }]
}

struct LogsRotationMissing;

impl Check for LogsRotationMissing {
    fn id(&self) -> &'static str {
        "logs.rotation-missing"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn detect(&self, ctx: &DoctorCtx) -> Result<Vec<Finding>> {
        let log_path = ctx.profile_dir.join("aiua.log");
        let size = std::fs::metadata(&log_path).ok().map(|m| m.len());
        let drop_in_exists = newsyslog_dropin_path().exists();
        Ok(evaluate_log_rotation(
            self.id(),
            &log_path.to_string_lossy(),
            size,
            drop_in_exists,
        ))
    }

    /// Auto-repairable: runs `scripts/install-log-rotation.sh` (idempotent —
    /// see the script's own header). Falls back to writing the drop-in
    /// directly if the script can't be found (e.g. running detached from a
    /// repo checkout). The drop-in lives under `/etc` and typically needs
    /// sudo; a permission failure is reported as `Skipped` with the manual
    /// command, never a hard error or a panic.
    fn repair(&self, ctx: &DoctorCtx, _finding: &Finding, apply: bool) -> Result<RepairOutcome> {
        let description = "install newsyslog drop-in via scripts/install-log-rotation.sh (or \
                            write /etc/newsyslog.d/philotic.conf directly)"
            .to_string();
        if !apply {
            return Ok(RepairOutcome::Planned { description });
        }

        let dropin = newsyslog_dropin_path();
        let before = json!({"dropin_exists": dropin.exists()});
        let manual_hint = format!(
            "manual: sudo tee {} <<'CONF'\n{}\nCONF",
            dropin.display(),
            newsyslog_dropin_content()
        );

        let outcome: std::result::Result<(), String> =
            match find_repo_script("install-log-rotation.sh") {
                Some(script) => match std::process::Command::new("bash").arg(&script).output() {
                    Ok(out) if out.status.success() => {
                        if dropin.exists() {
                            Ok(())
                        } else {
                            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                            Err(format!(
                            "install-log-rotation.sh ran but {} still doesn't exist (likely no \
                             non-interactive sudo available). {manual_hint}\nscript said: {stdout}",
                            dropin.display()
                        ))
                        }
                    }
                    Ok(out) => {
                        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                        Err(format!(
                            "install-log-rotation.sh exited {}: {stderr}. {manual_hint}",
                            out.status
                        ))
                    }
                    Err(e) => Err(format!(
                        "failed to execute install-log-rotation.sh: {e}. {manual_hint}"
                    )),
                },
                None => write_newsyslog_dropin_directly(&dropin).map_err(|e| {
                    format!(
                    "scripts/install-log-rotation.sh not found and direct write failed: {e:#}. \
                     {manual_hint}"
                )
                }),
            };

        match outcome {
            Ok(()) => {
                let after = json!({"dropin_exists": dropin.exists()});
                append_repair_journal(
                    &ctx.profile_dir,
                    self.id(),
                    "install-log-rotation",
                    before,
                    after,
                )?;
                Ok(RepairOutcome::Applied { description })
            }
            Err(reason) => Ok(RepairOutcome::Skipped { reason }),
        }
    }
}

/// The exact drop-in body `scripts/install-log-rotation.sh` writes — kept in
/// sync manually since the fallback path (no script found) must produce the
/// same content the script would.
fn newsyslog_dropin_content() -> String {
    let owner = std::env::var("USER").unwrap_or_else(|_| "root".to_string());
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    format!(
        "# Philotic hotel log rotation (managed by scripts/install-log-rotation.sh)\n\
         # logfilename                              [owner:group]        mode count size when  flags\n\
         {home}/.philotic/*/aiua*.log    {owner}:staff    644  5     51200 *     GJ"
    )
}

/// Direct fallback when `scripts/install-log-rotation.sh` cannot be found
/// (e.g. the binary is running detached from a repo checkout). Writing under
/// `/etc` will fail with a permissions error on any non-root invocation —
/// that's expected and surfaced by the caller as `Skipped`, not a panic.
fn write_newsyslog_dropin_directly(dropin: &std::path::Path) -> Result<()> {
    if let Some(parent) = dropin.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(dropin, newsyslog_dropin_content())
        .with_context(|| format!("write {}", dropin.display()))?;
    Ok(())
}

// ── secrets.store-permissions ───────────────────────────────────────────
//
// context.db carries both the legacy vault_secrets table and the live
// graph_nodes `secret:*` rows — i.e. every ciphertext + nonce in the
// hotel's vault. Encrypted-at-rest means a loose file mode isn't an
// immediate plaintext leak (the master key is Keychain- or file-protected,
// never stored alongside the DB), but a secret-bearing store should still
// not be group/other-readable. Wants db=0600, dir=0700.

#[cfg(unix)]
mod unix_perms {
    use std::fs;
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    /// Lower 9 permission bits (`rwxrwxrwx`) of `path`, or `None` if the
    /// path can't be stat'd (e.g. already gone, or permission denied).
    pub fn mode(path: &Path) -> Option<u32> {
        fs::metadata(path)
            .ok()
            .map(|m| m.permissions().mode() & 0o777)
    }

    pub fn chmod(path: &Path, new_mode: u32) -> io::Result<()> {
        fs::set_permissions(path, fs::Permissions::from_mode(new_mode))
    }
}

const WANT_DB_MODE: u32 = 0o600;
const WANT_DIR_MODE: u32 = 0o700;
/// Any bit here set means group or other has some access at all.
const GROUP_OTHER_ANY_BIT: u32 = 0o077;
/// Group- or other-writable bits specifically — the escalated-severity case.
const GROUP_OTHER_WRITE_BITS: u32 = 0o022;

fn evaluate_store_permissions(
    check_id: &'static str,
    db_path: &str,
    db_mode: Option<u32>,
    dir_path: &str,
    dir_mode: Option<u32>,
) -> Vec<Finding> {
    let db_loose = db_mode.is_some_and(|m| m & GROUP_OTHER_ANY_BIT != 0);
    let dir_loose = dir_mode.is_some_and(|m| m & GROUP_OTHER_ANY_BIT != 0);

    if !db_loose && !dir_loose {
        return Vec::new();
    }

    let world_writable = db_mode.is_some_and(|m| m & GROUP_OTHER_WRITE_BITS != 0)
        || dir_mode.is_some_and(|m| m & GROUP_OTHER_WRITE_BITS != 0);
    let severity = if world_writable {
        Severity::Error
    } else {
        Severity::Warning
    };

    let mut parts = Vec::new();
    if db_loose {
        parts.push(format!(
            "{db_path} is {:04o} (want {WANT_DB_MODE:04o})",
            db_mode.unwrap_or(0)
        ));
    }
    if dir_loose {
        parts.push(format!(
            "{dir_path} is {:04o} (want {WANT_DIR_MODE:04o})",
            dir_mode.unwrap_or(0)
        ));
    }

    vec![Finding {
        check_id: check_id.to_string(),
        severity,
        message: format!(
            "secret-bearing context DB store is group/other-accessible: {} — encrypted at \
             rest (master key is Keychain/file-protected, not stored alongside the DB) so this \
             isn't an immediate plaintext leak, but a vault-adjacent store should not be \
             world-readable",
            parts.join("; ")
        ),
        evidence: json!({
            "db_path": db_path,
            "db_mode": db_mode.map(|m| format!("{m:04o}")),
            "dir_path": dir_path,
            "dir_mode": dir_mode.map(|m| format!("{m:04o}")),
            "want_db_mode": format!("{WANT_DB_MODE:04o}"),
            "want_dir_mode": format!("{WANT_DIR_MODE:04o}"),
        }),
        fix_hint: format!("chmod {WANT_DB_MODE:o} {db_path} && chmod {WANT_DIR_MODE:o} {dir_path}"),
        auto_repairable: true,
    }]
}

struct SecretsStorePermissions;

impl Check for SecretsStorePermissions {
    fn id(&self) -> &'static str {
        "secrets.store-permissions"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn detect(&self, ctx: &DoctorCtx) -> Result<Vec<Finding>> {
        #[cfg(unix)]
        {
            let db_mode = unix_perms::mode(&ctx.db_path);
            let dir_mode = unix_perms::mode(&ctx.profile_dir);
            return Ok(evaluate_store_permissions(
                self.id(),
                &ctx.db_path.to_string_lossy(),
                db_mode,
                &ctx.profile_dir.to_string_lossy(),
                dir_mode,
            ));
        }
        #[cfg(not(unix))]
        {
            Ok(Vec::new())
        }
    }

    /// Auto-repairable: tightening the operator's OWN files to owner-only
    /// is reversible and only ever narrows access, never widens it — unlike
    /// every other repair here this doesn't need `NeedsConfirm`. Applied on
    /// `--fix`, Planned on dry-run; before/after modes are journaled.
    fn repair(&self, ctx: &DoctorCtx, finding: &Finding, apply: bool) -> Result<RepairOutcome> {
        #[cfg(unix)]
        {
            let db_path = finding
                .evidence
                .get("db_path")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let dir_path = finding
                .evidence
                .get("dir_path")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let description =
                format!("chmod {WANT_DB_MODE:o} {db_path} && chmod {WANT_DIR_MODE:o} {dir_path}");

            if !apply {
                return Ok(RepairOutcome::Planned { description });
            }

            let db_before = unix_perms::mode(std::path::Path::new(&db_path));
            let dir_before = unix_perms::mode(std::path::Path::new(&dir_path));

            let mut errors: Vec<String> = Vec::new();
            if let Err(e) = unix_perms::chmod(std::path::Path::new(&db_path), WANT_DB_MODE) {
                errors.push(format!("chmod {WANT_DB_MODE:o} {db_path}: {e}"));
            }
            if let Err(e) = unix_perms::chmod(std::path::Path::new(&dir_path), WANT_DIR_MODE) {
                errors.push(format!("chmod {WANT_DIR_MODE:o} {dir_path}: {e}"));
            }

            if !errors.is_empty() {
                return Ok(RepairOutcome::Skipped {
                    reason: format!("{}; manual: {description}", errors.join("; ")),
                });
            }

            let db_after = unix_perms::mode(std::path::Path::new(&db_path));
            let dir_after = unix_perms::mode(std::path::Path::new(&dir_path));
            append_repair_journal(
                &ctx.profile_dir,
                self.id(),
                "chmod-store-permissions",
                json!({
                    "db_mode": db_before.map(|m| format!("{m:04o}")),
                    "dir_mode": dir_before.map(|m| format!("{m:04o}")),
                }),
                json!({
                    "db_mode": db_after.map(|m| format!("{m:04o}")),
                    "dir_mode": dir_after.map(|m| format!("{m:04o}")),
                }),
            )?;
            Ok(RepairOutcome::Applied { description })
        }
        #[cfg(not(unix))]
        {
            let _ = (ctx, finding, apply);
            Ok(RepairOutcome::NotRepairable)
        }
    }
}

// ── self-heal circuit observability ─────────────────────────────────────
//
// Three read-only checks that make the self-heal circuit's health visible at
// a glance (finding F3-observability). All three read `heal_queue` /
// `node_config`, which live in the same `context.db` doctor already opens
// read-only — so, like every other check, they work offline even when the
// hotel can't boot. None of them repair anything (visibility only): the
// trait-default `repair()` (`NotRepairable`) is inherited unchanged.
//
// SAFETY: this is the self-heal circuit itself. A visibility check that
// false-cleans is worse than no check — so every branch that can't prove
// "healthy" fails toward a finding (or a soft note), never toward a silent
// green. In particular the dispatcher heartbeat is a contract with the
// heal-dispatcher sibling lane (`heal_dispatcher.last_cycle_at`, written each
// 30s cycle); an unexpected value (absent, unparseable, or a future
// timestamp from a seconds/millis format mismatch) is surfaced, never read as
// fresh.

/// Depth (pending + assigned) at which the backlog is a warning / critical.
const HEAL_DEPTH_WARN_ABOVE: i64 = 100;
const HEAL_DEPTH_CRITICAL_ABOVE: i64 = 500;

/// Age of the oldest still-`pending` entry at which the dispatcher is visibly
/// behind (warn) or effectively stalled (critical).
const HEAL_OLDEST_PENDING_WARN_SECS: i64 = 600; // 10 min
const HEAL_OLDEST_PENDING_CRITICAL_SECS: i64 = 1800; // 30 min

/// Hotel config key the heal-dispatcher stamps at the end of each poll cycle.
/// Fixed name — the integration seam with the heal-dispatcher sibling lane.
const HEAL_DISPATCHER_HEARTBEAT_KEY: &str = "heal_dispatcher.last_cycle_at";
/// The dispatcher's poll interval (`POLL_INTERVAL_SECS` in
/// `crates/heal-dispatcher/src/main.rs`). Duplicated because heal-dispatcher
/// ships bin-only and doctor must stand alone.
const HEAL_DISPATCHER_POLL_INTERVAL_SECS: i64 = 30;
/// Heartbeat older than 3× the poll interval ⇒ the dispatcher is very likely
/// wedged or down. PID-liveness alone can't catch a *wedged* (still-running,
/// not-cycling) dispatcher — only a stale heartbeat can.
const HEAL_DISPATCHER_STALE_SECS: i64 = 3 * HEAL_DISPATCHER_POLL_INTERVAL_SECS; // 90s

fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Render a non-negative second count as a compact `Nm Ns` / `Ns` string.
fn human_age(secs: i64) -> String {
    if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Parse a stored `node_config.value_json` into epoch **seconds**.
///
/// Contract with the heal-dispatcher sibling lane: the heartbeat is written as
/// a JSON number of epoch **seconds** (matching `heal_queue.timestamp`, which
/// is `SystemTime … as_secs()`). We parse defensively — JSON number, JSON
/// string, or a bare integer — and return `None` on anything we can't read as
/// an integer, so an unexpected encoding degrades to a soft "unreadable" note
/// rather than a false-clean or a hard error. A value that parses but is in
/// the *wrong unit* (e.g. milliseconds) is deliberately NOT normalized here —
/// it surfaces downstream as an implausible future timestamp, which the age
/// evaluators flag rather than silently trusting.
fn parse_epoch_secs(value_json: &str) -> Option<i64> {
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

/// Read `node_config.<key>` as epoch seconds, or `None` when the key (or the
/// `node_config` table itself) is absent or unparseable. Never a hard error
/// for an absent key — absence is a legitimate, softly-handled state.
fn read_config_epoch_secs(conn: &Connection, key: &str) -> Result<Option<i64>> {
    // Canonical config store: GraphDomain writes config values as `graph_nodes`
    // rows keyed `config:<key>` with `data_json = {"value": "<v>"}` — this is
    // where the heal-dispatcher (and provider tokens, etc.) actually land. The
    // legacy `node_config` table holds only a couple of keys; check it as a
    // fallback so older writers still resolve.
    if table_exists(conn, "graph_nodes")? {
        let node_key = format!("config:{key}");
        let data_json: Option<String> = conn
            .query_row(
                "SELECT data_json FROM graph_nodes WHERE node_key = ?1",
                params![node_key],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(dj) = data_json {
            // Extract the inner {"value": ...} then reuse parse_epoch_secs,
            // which already handles a number or a numeric string.
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&dj) {
                if let Some(inner) = v.get("value") {
                    if let Some(ts) = parse_epoch_secs(&inner.to_string()) {
                        return Ok(Some(ts));
                    }
                }
            }
        }
    }

    if table_exists(conn, "node_config")? {
        let raw: Option<String> = conn
            .query_row(
                "SELECT value_json FROM node_config WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        return Ok(raw.as_deref().and_then(parse_epoch_secs));
    }

    Ok(None)
}

// ── heal.queue-depth ─────────────────────────────────────────────────────

fn evaluate_heal_queue_depth(check_id: &'static str, pending: i64, assigned: i64) -> Vec<Finding> {
    let backlog = pending.saturating_add(assigned);
    let severity = if backlog > HEAL_DEPTH_CRITICAL_ABOVE {
        Severity::Critical
    } else if backlog > HEAL_DEPTH_WARN_ABOVE {
        Severity::Warning
    } else {
        return Vec::new();
    };

    vec![Finding {
        check_id: check_id.to_string(),
        severity,
        message: format!(
            "self-heal queue backlog is {backlog} unresolved entr{} ({pending} pending, \
             {assigned} assigned) — warn>{HEAL_DEPTH_WARN_ABOVE}, \
             critical>{HEAL_DEPTH_CRITICAL_ABOVE}; the heal-dispatcher is not draining the \
             queue fast enough",
            if backlog == 1 { "y" } else { "ies" }
        ),
        evidence: json!({
            "pending": pending,
            "assigned": assigned,
            "backlog": backlog,
            "warn_above": HEAL_DEPTH_WARN_ABOVE,
            "critical_above": HEAL_DEPTH_CRITICAL_ABOVE,
        }),
        fix_hint:
            "check heal.dispatcher-staleness first: a growing backlog with a fresh dispatcher \
             heartbeat means triage/classification is failing (not that the dispatcher is down); \
             a stale heartbeat means the dispatcher is wedged — restart it"
                .to_string(),
        auto_repairable: false,
    }]
}

struct HealQueueDepth;

impl Check for HealQueueDepth {
    fn id(&self) -> &'static str {
        "heal.queue-depth"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn detect(&self, ctx: &DoctorCtx) -> Result<Vec<Finding>> {
        if let Some(findings) = missing_table_finding(ctx, self.id(), "heal_queue")? {
            return Ok(findings);
        }
        let pending: i64 = ctx.conn.query_row(
            "SELECT COUNT(*) FROM heal_queue WHERE status = 'pending'",
            [],
            |row| row.get(0),
        )?;
        let assigned: i64 = ctx.conn.query_row(
            "SELECT COUNT(*) FROM heal_queue WHERE status = 'assigned'",
            [],
            |row| row.get(0),
        )?;
        Ok(evaluate_heal_queue_depth(self.id(), pending, assigned))
    }
}

// ── heal.oldest-pending-age ──────────────────────────────────────────────

fn evaluate_heal_oldest_pending(
    check_id: &'static str,
    oldest_ts: Option<i64>,
    now: i64,
) -> Vec<Finding> {
    let Some(ts) = oldest_ts else {
        // No pending rows — nothing is waiting on the dispatcher.
        return Vec::new();
    };
    let age = now - ts;

    // Future timestamp (clock skew or a bad write): NOT a drain problem, and
    // must never read as "fresh". Surface it softly instead of returning clean.
    if age < 0 {
        return vec![Finding {
            check_id: check_id.to_string(),
            severity: Severity::Info,
            message: format!(
                "oldest pending heal entry is timestamped {} in the future (ts={ts}, now={now}) \
                 — clock skew or a bad write; not treated as a backlog age",
                human_age(age.abs())
            ),
            evidence: json!({"oldest_ts": ts, "now": now, "age_secs": age}),
            fix_hint: "verify the hotel clock and how heal_queue rows are timestamped; a future \
                       timestamp masks real backlog age from this check"
                .to_string(),
            auto_repairable: false,
        }];
    }

    let severity = if age > HEAL_OLDEST_PENDING_CRITICAL_SECS {
        Severity::Critical
    } else if age > HEAL_OLDEST_PENDING_WARN_SECS {
        Severity::Warning
    } else {
        return Vec::new();
    };

    vec![Finding {
        check_id: check_id.to_string(),
        severity,
        message: format!(
            "oldest pending heal entry has waited {} (warn>{}s, critical>{}s) — the \
             heal-dispatcher is behind or wedged and this entry is not being triaged",
            human_age(age),
            HEAL_OLDEST_PENDING_WARN_SECS,
            HEAL_OLDEST_PENDING_CRITICAL_SECS
        ),
        evidence: json!({
            "oldest_ts": ts,
            "now": now,
            "age_secs": age,
            "warn_above_secs": HEAL_OLDEST_PENDING_WARN_SECS,
            "critical_above_secs": HEAL_OLDEST_PENDING_CRITICAL_SECS,
        }),
        fix_hint: "check heal.dispatcher-staleness — a stale dispatcher heartbeat alongside this \
                   means the dispatcher is wedged; restart it and confirm the queue drains"
            .to_string(),
        auto_repairable: false,
    }]
}

struct HealOldestPendingAge;

impl Check for HealOldestPendingAge {
    fn id(&self) -> &'static str {
        "heal.oldest-pending-age"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn detect(&self, ctx: &DoctorCtx) -> Result<Vec<Finding>> {
        if let Some(findings) = missing_table_finding(ctx, self.id(), "heal_queue")? {
            return Ok(findings);
        }
        // MIN over an empty match set returns a single NULL row → None.
        let oldest: Option<i64> = ctx.conn.query_row(
            "SELECT MIN(timestamp) FROM heal_queue WHERE status = 'pending'",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        Ok(evaluate_heal_oldest_pending(
            self.id(),
            oldest,
            now_epoch_secs(),
        ))
    }
}

// ── heal.dispatcher-staleness ────────────────────────────────────────────

fn evaluate_heal_dispatcher_staleness(
    check_id: &'static str,
    last_cycle_at: Option<i64>,
    now: i64,
) -> Vec<Finding> {
    let Some(ts) = last_cycle_at else {
        // Absent heartbeat: either an older dispatcher build that doesn't emit
        // one yet, or a dispatcher that has never completed a cycle. Doctor
        // can't distinguish, so this degrades to a soft Info note — NOT a hard
        // failure — rather than false-alarming every not-yet-upgraded hotel.
        return vec![Finding {
            check_id: check_id.to_string(),
            severity: Severity::Info,
            message: format!(
                "no '{HEAL_DISPATCHER_HEARTBEAT_KEY}' heartbeat in node_config — heal-dispatcher \
                 liveness can't be confirmed offline (dispatcher build predates the heartbeat, or \
                 it has never completed a cycle)"
            ),
            evidence: json!({"heartbeat_key": HEAL_DISPATCHER_HEARTBEAT_KEY, "present": false}),
            fix_hint: format!(
                "if the heal-dispatcher is deployed and running, confirm it stamps \
                 '{HEAL_DISPATCHER_HEARTBEAT_KEY}' each cycle; a build predating that heartbeat \
                 can't be liveness-checked by doctor"
            ),
            auto_repairable: false,
        }];
    };

    let age = now - ts;

    // Future heartbeat: clock skew, or a seconds/millis unit mismatch with the
    // writer (a millis value read as seconds lands ~decades in the future).
    // This must NEVER be read as "fresh" — that would silently mask a wedged
    // dispatcher. Surface the mismatch as a warning instead.
    if age < 0 {
        return vec![Finding {
            check_id: check_id.to_string(),
            severity: Severity::Warning,
            message: format!(
                "heal-dispatcher heartbeat '{HEAL_DISPATCHER_HEARTBEAT_KEY}' is {} in the future \
                 (last_cycle_at={ts}, now={now}) — clock skew or a seconds/millis format \
                 mismatch; dispatcher liveness can't be trusted",
                human_age(age.abs())
            ),
            evidence: json!({
                "heartbeat_key": HEAL_DISPATCHER_HEARTBEAT_KEY,
                "present": true,
                "last_cycle_at": ts,
                "now": now,
                "age_secs": age,
            }),
            fix_hint: "confirm the hotel clock and that the heal-dispatcher writes \
                       heal_dispatcher.last_cycle_at in epoch SECONDS (not millis)"
                .to_string(),
            auto_repairable: false,
        }];
    }

    if age > HEAL_DISPATCHER_STALE_SECS {
        return vec![Finding {
            check_id: check_id.to_string(),
            severity: Severity::Critical,
            message: format!(
                "heal-dispatcher last cycled {} ago (>{}s = 3× the {}s poll interval) — likely \
                 wedged or down; the heal queue will not drain while it's stalled, and \
                 PID-liveness alone can't catch a wedged (running-but-not-cycling) dispatcher",
                human_age(age),
                HEAL_DISPATCHER_STALE_SECS,
                HEAL_DISPATCHER_POLL_INTERVAL_SECS
            ),
            evidence: json!({
                "heartbeat_key": HEAL_DISPATCHER_HEARTBEAT_KEY,
                "present": true,
                "last_cycle_at": ts,
                "now": now,
                "age_secs": age,
                "stale_above_secs": HEAL_DISPATCHER_STALE_SECS,
                "poll_interval_secs": HEAL_DISPATCHER_POLL_INTERVAL_SECS,
            }),
            fix_hint: "restart the heal-dispatcher guest and inspect its logs for a stuck cycle \
                       (blocked IPC, hung classifier); confirm the heartbeat resumes advancing"
                .to_string(),
            auto_repairable: false,
        }];
    }

    Vec::new()
}

struct HealDispatcherStaleness;

impl Check for HealDispatcherStaleness {
    fn id(&self) -> &'static str {
        "heal.dispatcher-staleness"
    }

    fn severity(&self) -> Severity {
        Severity::Critical
    }

    fn detect(&self, ctx: &DoctorCtx) -> Result<Vec<Finding>> {
        let last_cycle_at = read_config_epoch_secs(&ctx.conn, HEAL_DISPATCHER_HEARTBEAT_KEY)?;
        Ok(evaluate_heal_dispatcher_staleness(
            self.id(),
            last_cycle_at,
            now_epoch_secs(),
        ))
    }
}

// ── system.disk-space ────────────────────────────────────────────────────

// A filling disk wedges *everything* (ENOSPC) with no warning of its own —
// the hotel can't write its context DB, cargo can't build, logs can't rotate.
// This guard makes a filling disk visible while there's still headroom to act.
// Thresholds are on the volume backing `ctx.profile_dir` (~/.philotic/<profile>),
// the disk that actually matters for the hotel.
const DISK_AVAIL_WARN_KB: u64 = 15 * 1024 * 1024; // 15 GiB
const DISK_AVAIL_CRITICAL_KB: u64 = 5 * 1024 * 1024; // 5 GiB
const DISK_USED_WARN_PCT: u8 = 90;
const DISK_USED_CRITICAL_PCT: u8 = 96;

/// Parse a macOS `df -k` report into `(avail_kb, used_pct)`.
///
/// macOS `df -k` columns are:
/// `Filesystem 1024-blocks Used Available Capacity iused ifree %iused Mounted`.
/// Field index 3 (0-based) is Available (KB); field 4 is Capacity, e.g. `"83%"`.
/// The header line is skipped, then all remaining tokens are flattened before
/// indexing so a device name `df` wrapped onto its own line doesn't shift the
/// field positions. Returns `None` on any shape/parse surprise so `detect`
/// degrades to a soft note instead of panicking.
fn parse_df_kb(output: &str) -> Option<(u64, u8)> {
    let mut lines = output.lines();
    let _header = lines.next()?;
    let tokens: Vec<&str> = lines.flat_map(|l| l.split_whitespace()).collect();
    if tokens.len() < 5 {
        return None;
    }
    let avail_kb: u64 = tokens[3].parse().ok()?;
    let used_pct: u8 = tokens[4].trim_end_matches('%').parse().ok()?;
    Some((avail_kb, used_pct))
}

/// Pure threshold logic for the disk-space guard, split out so it is
/// unit-testable without shelling out to `df`.
///
/// CRITICAL when available < 5 GiB OR capacity ≥ 96%; WARNING when available
/// < 15 GiB OR capacity ≥ 90%; otherwise clean.
fn evaluate_disk_space(check_id: &'static str, avail_kb: u64, used_pct: u8) -> Vec<Finding> {
    let severity = if avail_kb < DISK_AVAIL_CRITICAL_KB || used_pct >= DISK_USED_CRITICAL_PCT {
        Severity::Critical
    } else if avail_kb < DISK_AVAIL_WARN_KB || used_pct >= DISK_USED_WARN_PCT {
        Severity::Warning
    } else {
        return Vec::new();
    };

    let avail_gib = avail_kb as f64 / (1024.0 * 1024.0);
    vec![Finding {
        check_id: check_id.to_string(),
        severity,
        message: format!(
            "profile-dir volume has {avail_gib:.1} GiB free at {used_pct}% used \
             (warn <15 GiB or ≥{DISK_USED_WARN_PCT}%, critical <5 GiB or ≥{DISK_USED_CRITICAL_PCT}%) \
             — a full disk hard-blocks the hotel, cargo builds, and log rotation with ENOSPC"
        ),
        evidence: json!({
            "avail_kb": avail_kb,
            "avail_gib": (avail_gib * 100.0).round() / 100.0,
            "used_pct": used_pct,
            "warn_avail_kb": DISK_AVAIL_WARN_KB,
            "critical_avail_kb": DISK_AVAIL_CRITICAL_KB,
            "warn_used_pct": DISK_USED_WARN_PCT,
            "critical_used_pct": DISK_USED_CRITICAL_PCT,
        }),
        fix_hint:
            "the usual bulk consumer is the regenerable cargo build dir (<repo>/target). \
             Reclaim with `cargo clean` in the code repo (or `rm -rf <repo>/target <repo>/dist-ci`); \
             also always-safe: `rm -f ~/.philotic/*/context.db.bak* ~/.philotic/*/*.quarantine`"
                .to_string(),
        auto_repairable: false,
    }]
}

/// Read-only disk-space guard on the volume backing `ctx.profile_dir`.
struct SystemDiskSpace;

impl Check for SystemDiskSpace {
    fn id(&self) -> &'static str {
        "system.disk-space"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn detect(&self, ctx: &DoctorCtx) -> Result<Vec<Finding>> {
        // No libc/nix dep available: shell out to `df -k <profile_dir>`. Any
        // failure (df missing, non-zero exit, unparseable output) degrades to a
        // soft Info note — this is the doctor that runs when things are already
        // going wrong, so it must never panic or hard-error on its own probe.
        let output = std::process::Command::new("df")
            .arg("-k")
            .arg(&ctx.profile_dir)
            .output();
        let parsed = match output {
            Ok(o) if o.status.success() => {
                parse_df_kb(&String::from_utf8_lossy(&o.stdout))
            }
            _ => None,
        };
        match parsed {
            Some((avail_kb, used_pct)) => {
                Ok(evaluate_disk_space(self.id(), avail_kb, used_pct))
            }
            None => Ok(vec![Finding {
                check_id: self.id().to_string(),
                severity: Severity::Info,
                message: format!(
                    "could not read free space for {} via `df -k` — disk-space guard \
                     is not reporting (this is a soft note, not a failure)",
                    ctx.profile_dir.display()
                ),
                evidence: json!({"profile_dir": ctx.profile_dir.to_string_lossy(), "df_ok": false}),
                fix_hint: "run `df -k <profile_dir>` by hand to confirm free space; \
                           the guard degrades quietly when df is unavailable or its \
                           output format is unexpected"
                    .to_string(),
                auto_repairable: false,
            }]),
        }
    }

    fn repair(&self, _ctx: &DoctorCtx, _finding: &Finding, _apply: bool) -> Result<RepairOutcome> {
        // Disk cleanup is an operator decision in BOTH dry-run and apply: doctor
        // can't know the code-repo path and must NEVER `rm` a build dir unprompted.
        Ok(RepairOutcome::NeedsConfirm {
            description:
                "reclaim space by removing regenerable build artifacts: run `cargo clean` in the \
                 code repo (or `rm -rf <repo>/target <repo>/dist-ci`). Always-safe extras: \
                 `rm -f ~/.philotic/*/context.db.bak* ~/.philotic/*/*.quarantine`"
                    .to_string(),
            risk: "removes regenerable build artifacts; confirm no build/deploy is mid-flight"
                .to_string(),
        })
    }
}

// ── CLI entry ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct DoctorReport {
    ok: bool,
    hotel: String,
    /// Resolved context DB path actually opened — surfaced so a wrong-DB
    /// false positive (or a graceful "expected table not present" finding)
    /// is obvious at a glance rather than requiring a re-run with `-v`.
    db_path: String,
    checks_run: usize,
    findings: Vec<Finding>,
    /// One entry per finding, in the same order, correlated by `check_id`.
    /// Populated whether or not `--fix` was passed: without `--fix` these
    /// are dry-run plans (`Planned`/`NeedsConfirm`/`NotRepairable`); with
    /// `--fix`, auto-repairable checks report `Applied`/`Skipped` while
    /// `NeedsConfirm`/`NotRepairable` checks are unchanged either way.
    repairs: Vec<RepairEntry>,
}

#[derive(Debug, Serialize)]
struct RepairEntry {
    check_id: String,
    outcome: RepairOutcome,
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    hotel: String,
    json: bool,
    severity_min: String,
    only: Vec<String>,
    skip: Vec<String>,
    list_checks: bool,
    fix: bool,
    profile: Option<String>,
    db: Option<PathBuf>,
) -> Result<()> {
    let checks = catalog();

    if list_checks {
        println!("{:<28} {}", "CHECK ID", "SEVERITY");
        for check in &checks {
            println!("{:<28} {}", check.id(), check.severity());
        }
        return Ok(());
    }

    let threshold = match Severity::from_str(&severity_min) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("phil doctor: {e}");
            std::process::exit(2);
        }
    };

    let ctx = match DoctorCtx::open(&hotel, db, profile) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("phil doctor: {e:#}");
            std::process::exit(2);
        }
    };

    let selected: Vec<&Box<dyn Check>> = checks
        .iter()
        .filter(|c| only.is_empty() || only.iter().any(|id| id == c.id()))
        .filter(|c| !skip.iter().any(|id| id == c.id()))
        .collect();
    let selected_ids: Vec<&'static str> = selected.iter().map(|c| c.id()).collect();

    // `apply` mirrors `--fix` into every repair() call below: dry-run
    // (apply=false) computes plans only and never writes; `--fix`
    // (apply=true) executes auto-repairable checks. Either way this loop is
    // the only place `repair()` is called, so it stays paired with the
    // exact finding that produced it — no re-detecting or guessing which
    // check a finding came from.
    let mut finding_repairs: Vec<(Finding, RepairOutcome)> = Vec::new();
    for check in &selected {
        let findings = match check.detect(&ctx) {
            Ok(findings) => findings,
            Err(e) => {
                eprintln!("phil doctor: check '{}' failed: {e:#}", check.id());
                std::process::exit(2);
            }
        };
        for mut finding in findings {
            // Stamp the resolved DB path into every finding's evidence so a
            // wrong-DB false positive (or a "table not present" skip) is
            // visible right on the finding, not just in the header/report.
            if let serde_json::Value::Object(ref mut map) = finding.evidence {
                map.entry("db_path".to_string())
                    .or_insert_with(|| json!(ctx.db_path.to_string_lossy()));
            }
            let outcome = match check.repair(&ctx, &finding, fix) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("phil doctor: repair for '{}' failed: {e:#}", check.id());
                    std::process::exit(2);
                }
            };
            finding_repairs.push((finding, outcome));
        }
    }

    let above_threshold = finding_repairs
        .iter()
        .filter(|(f, _)| f.severity >= threshold)
        .count();
    let ok = above_threshold == 0;

    if json {
        let findings: Vec<Finding> = finding_repairs.iter().map(|(f, _)| f.clone()).collect();
        let repairs: Vec<RepairEntry> = finding_repairs
            .iter()
            .map(|(f, o)| RepairEntry {
                check_id: f.check_id.clone(),
                outcome: o.clone(),
            })
            .collect();
        let report = DoctorReport {
            ok,
            hotel: hotel.clone(),
            db_path: ctx.db_path.to_string_lossy().into_owned(),
            checks_run: selected.len(),
            findings,
            repairs,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(
            &hotel,
            &ctx.db_path,
            selected.len(),
            &selected_ids,
            &finding_repairs,
            threshold,
            fix,
        );
    }

    std::process::exit(if ok { 0 } else { 1 });
}

fn format_repair_outcome(outcome: &RepairOutcome, fix: bool) -> String {
    match outcome {
        RepairOutcome::Planned { description } => {
            format!("would {description} (dry-run; pass --fix to apply)")
        }
        RepairOutcome::Applied { description } => format!("applied — {description}"),
        RepairOutcome::NeedsConfirm { description, risk } => format!(
            "needs operator confirmation — {description} (risk: {risk}); never auto-applied{}",
            if fix { " by --fix" } else { "" }
        ),
        RepairOutcome::Skipped { reason } => format!("skipped — {reason}"),
        RepairOutcome::NotRepairable => {
            "not repairable by doctor — guided fix only, see 'fix' above".to_string()
        }
    }
}

fn print_human(
    hotel: &str,
    db_path: &std::path::Path,
    checks_run: usize,
    check_ids: &[&'static str],
    finding_repairs: &[(Finding, RepairOutcome)],
    threshold: Severity,
    fix: bool,
) {
    println!(
        "phil doctor — hotel {hotel} (db: {}), offline, {checks_run} checks\n",
        db_path.display()
    );
    for id in check_ids {
        let for_check: Vec<&(Finding, RepairOutcome)> = finding_repairs
            .iter()
            .filter(|(f, _)| f.check_id == *id)
            .collect();
        if for_check.is_empty() {
            println!("  \u{2713} {id}");
            continue;
        }
        for (f, outcome) in for_check {
            let marker = if f.severity >= threshold {
                "\u{2717}"
            } else {
                "!"
            };
            println!("  {marker} {:<28} [{}] {}", id, f.severity, f.message);
            if !f.fix_hint.is_empty() {
                println!("      fix: {}", f.fix_hint);
            }
            println!("      repair: {}", format_repair_outcome(outcome, fix));
        }
    }
    let above = finding_repairs
        .iter()
        .filter(|(f, _)| f.severity >= threshold)
        .count();
    println!(
        "\n{above} finding(s) at/above {threshold} threshold \u{00b7} exit {}",
        if above == 0 { 0 } else { 1 }
    );
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;

    /// Create a fixture context DB (full schema via the real storage layer,
    /// same as production) and hand back its path for a read-only DoctorCtx.
    fn fixture_db() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("aiua_context.db");
        {
            let storage = SqliteGraphStorage::open(&path).expect("open fixture db");
            drop(storage);
        }
        (dir, path)
    }

    fn insert_hotel(
        conn: &Connection,
        hotel: &str,
        mesh: u16,
        blob: u16,
        execution: u16,
        active_pid: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO hotels (hotel_name, capabilities_json, mesh_port, blob_port, execution_port, ipc_socket_path, active_pid)
             VALUES (?1, '{}', ?2, ?3, ?4, '/tmp/x.sock', ?5)",
            params![hotel, mesh, blob, execution, active_pid],
        )
        .expect("insert hotel");
    }

    fn insert_secret(conn: &Connection, secret_ref: &str, ciphertext_b64: &str, nonce_b64: &str) {
        conn.execute(
            "INSERT INTO vault_secrets (secret_ref, secret_kind, scope, allowed_roles_json, allowed_guests_json, ciphertext_b64, nonce_b64, created_at, updated_at)
             VALUES (?1, 'k', 's', '[]', '[]', ?2, ?3, 0, 0)",
            params![secret_ref, ciphertext_b64, nonce_b64],
        )
        .expect("insert secret");
    }

    /// Insert a `graph_nodes` row shaped exactly like
    /// `GraphDomain::upsert_secret` produces in production: `node_key =
    /// "secret:<ref>"`, `data_json` = the serialized `SecretRecord`.
    fn insert_graph_secret(
        conn: &Connection,
        secret_ref: &str,
        secret_kind: &str,
        ciphertext_b64: &str,
        nonce_b64: &str,
    ) {
        let record = ansible_mesh_core::storage::SecretRecord {
            secret_ref: secret_ref.to_string(),
            secret_kind: secret_kind.to_string(),
            scope: "hotel".to_string(),
            allowed_roles: vec![],
            allowed_guests: vec![],
            ciphertext_b64: ciphertext_b64.to_string(),
            nonce_b64: nonce_b64.to_string(),
            created_at: 0,
            updated_at: 0,
        };
        let node_key = format!("secret:{secret_ref}");
        let data_json = serde_json::to_string(&record).expect("serialize SecretRecord");
        conn.execute(
            "INSERT INTO graph_nodes (node_key, kind, label, data_json, updated_at) VALUES (?1, 'secret', ?2, ?3, 0)",
            params![node_key, secret_ref, data_json],
        )
        .expect("insert graph secret node");
    }

    /// Insert a `node_config` row referencing `value` (e.g. a `secret_ref`)
    /// in its `value_json`, mirroring `config:<key>_ref -> secret_ref`
    /// wiring in production.
    fn insert_node_config_ref(conn: &Connection, key: &str, value: &str) {
        conn.execute(
            "INSERT INTO node_config (key, value_json) VALUES (?1, ?2)",
            params![key, json!(value).to_string()],
        )
        .expect("insert node_config");
    }

    fn probe(
        secret_ref: &str,
        secret_kind: &str,
        source: &'static str,
        ciphertext_b64: String,
        nonce_b64: String,
        referenced: bool,
    ) -> VaultSecretProbe {
        VaultSecretProbe {
            secret_ref: secret_ref.to_string(),
            secret_kind: secret_kind.to_string(),
            source,
            ciphertext_b64,
            nonce_b64,
            referenced,
        }
    }

    fn encrypt_for_test(key: &[u8; 32], plaintext: &str) -> (String, String) {
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
        let nonce_bytes = [7u8; 12];
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_bytes())
            .expect("encrypt");
        (
            BASE64_STANDARD.encode(ciphertext),
            BASE64_STANDARD.encode(nonce_bytes),
        )
    }

    // ── ports.hotel-record-drift ────────────────────────────────────────

    #[test]
    fn ports_drift_matches_deterministic_default_is_clean() {
        let base = deterministic_base_port(&sanitize_hotel_name("jane"));
        let findings = evaluate_ports_drift(
            "ports.hotel-record-drift",
            "jane",
            Some((base, base + 1, base + 2)),
        );
        assert!(findings.is_empty());
    }

    #[test]
    fn ports_drift_flags_fallback_persisted() {
        let base = deterministic_base_port(&sanitize_hotel_name("jane"));
        let findings = evaluate_ports_drift(
            "ports.hotel-record-drift",
            "jane",
            Some((base + 500, base + 501, base + 502)),
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].auto_repairable);
    }

    #[test]
    fn ports_drift_missing_hotel_row_is_warning_not_silent() {
        let findings = evaluate_ports_drift("ports.hotel-record-drift", "ghost", None);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
    }

    #[test]
    fn ports_drift_check_against_fixture_db() {
        let (_dir, path) = fixture_db();
        let base = deterministic_base_port(&sanitize_hotel_name("jane"));
        {
            let storage = SqliteGraphStorage::open(&path).expect("open");
            let conn = storage.raw_conn().lock().unwrap();
            insert_hotel(&conn, "jane", base + 900, base + 901, base + 902, None);
        }
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let findings = PortsHotelRecordDrift.detect(&ctx).expect("detect");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].check_id, "ports.hotel-record-drift");
    }

    // ── proc.orphan-instances ────────────────────────────────────────────

    #[test]
    fn orphans_none_when_all_running_pids_owned() {
        let findings = evaluate_orphans("proc.orphan-instances", Some(100), Some(100), &[100]);
        assert!(findings.is_empty());
    }

    #[test]
    fn orphans_flags_process_owned_by_neither_launchd_nor_active_pid() {
        let findings = evaluate_orphans("proc.orphan-instances", Some(100), Some(100), &[100, 999]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Critical);
        assert_eq!(findings[0].evidence["orphan_pids"], json!([999]));
    }

    #[test]
    fn orphans_no_running_processes_is_clean() {
        let findings = evaluate_orphans("proc.orphan-instances", None, None, &[]);
        assert!(findings.is_empty());
    }

    // ── proc.orphan-instances (hotel-arg prefix-collision matching) ───────
    //
    // Regression coverage for the `pgrep -f` unanchored-substring bug: a
    // naive "aiua --hotel jane" pattern would also match a real
    // `aiua --hotel jane2` process, which could tell an operator to SIGTERM
    // a healthy jane2 hotel. `command_line_has_exact_hotel_arg` is the
    // pattern-matching logic `running_aiua_pids` uses to filter pgrep's
    // broad candidate list down to exact `--hotel <hotel>` argv pairs.

    #[test]
    fn hotel_arg_match_rejects_prefix_collision_jane_vs_jane2() {
        // The exact scenario from the finding: hotel "jane" must not match
        // a real "jane2" hotel process.
        assert!(!command_line_has_exact_hotel_arg(
            "/usr/local/bin/aiua --hotel jane2 --foo",
            "jane",
        ));
        // And the reverse must also hold: "jane2" must not match a "jane"
        // process either.
        assert!(!command_line_has_exact_hotel_arg(
            "/usr/local/bin/aiua --hotel jane --foo",
            "jane2",
        ));
    }

    #[test]
    fn hotel_arg_match_accepts_exact_argument() {
        assert!(command_line_has_exact_hotel_arg(
            "/usr/local/bin/aiua --hotel jane --foo bar",
            "jane",
        ));
        // Exact match still holds when --hotel is the last token pair.
        assert!(command_line_has_exact_hotel_arg(
            "/usr/local/bin/aiua --hotel jane",
            "jane",
        ));
    }

    #[test]
    fn hotel_arg_match_rejects_unrelated_hotel() {
        assert!(!command_line_has_exact_hotel_arg(
            "/usr/local/bin/aiua --hotel bjork --foo",
            "jane",
        ));
    }

    #[test]
    fn running_aiua_pids_filters_prefix_collision_candidates() {
        // End-to-end (minus the real `pgrep`/`ps` shellouts) exercise of the
        // filtering logic `running_aiua_pids` applies: given pgrep's broad
        // "aiua --hotel" candidates, only pids whose full argv contains an
        // exact `--hotel jane` pair should survive — a `jane2` process must
        // be filtered out even though it shares the same pgrep hit.
        let candidate_cmdlines = [
            (100u32, "/usr/local/bin/aiua --hotel jane"),
            (200u32, "/usr/local/bin/aiua --hotel jane2 --foo"),
            (300u32, "/usr/local/bin/aiua --hotel jane --verbose"),
        ];
        let surviving: Vec<u32> = candidate_cmdlines
            .iter()
            .filter(|(_, cmdline)| command_line_has_exact_hotel_arg(cmdline, "jane"))
            .map(|(pid, _)| *pid)
            .collect();
        assert_eq!(surviving, vec![100, 300]);
    }

    // ── ipc.stale-socket ─────────────────────────────────────────────────

    #[test]
    fn socket_probe_missing_path_is_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nope.sock");
        assert_eq!(probe_socket(path.to_str().unwrap()), SocketProbe::Missing);
    }

    #[test]
    fn socket_probe_bound_and_listening_is_alive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("alive.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        assert_eq!(probe_socket(path.to_str().unwrap()), SocketProbe::Alive);
    }

    #[test]
    fn socket_probe_leftover_file_with_no_listener_is_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("stale.sock");
        {
            let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
            drop(listener); // leaves the socket special file behind, unlinked by nobody
        }
        // Closing the listener does not make connect() refuse *instantly* — the
        // kernel can briefly satisfy a connect from the listen backlog before it
        // tears the socket down, so a single probe occasionally still sees Alive.
        // Poll until the leftover file settles to Stale (bounded ~500ms).
        let ps = path.to_str().unwrap();
        let mut probe = probe_socket(ps);
        for _ in 0..50 {
            if probe == SocketProbe::Stale {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            probe = probe_socket(ps);
        }
        assert_eq!(probe, SocketProbe::Stale);
    }

    #[test]
    fn evaluate_stale_socket_only_fires_on_stale() {
        assert!(evaluate_stale_socket("ipc.stale-socket", "/x", SocketProbe::Missing).is_empty());
        assert!(evaluate_stale_socket("ipc.stale-socket", "/x", SocketProbe::Alive).is_empty());
        let findings = evaluate_stale_socket("ipc.stale-socket", "/x", SocketProbe::Stale);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].auto_repairable);
    }

    // ── vault.key-source-divergence ─────────────────────────────────────

    #[test]
    fn vault_fingerprint_differs_per_key_and_is_deterministic() {
        let a = fingerprint(&[1u8; 32]);
        let b = fingerprint(&[2u8; 32]);
        assert_ne!(a, b);
        assert_eq!(a, fingerprint(&[1u8; 32]));
        assert_eq!(a.len(), 8);
    }

    #[test]
    fn vault_try_decrypt_roundtrip() {
        let key = [9u8; 32];
        let (ct, nonce) = encrypt_for_test(&key, "s3cr3t");
        assert!(try_decrypt(&key, &ct, &nonce));
        assert!(!try_decrypt(&[8u8; 32], &ct, &nonce));
    }

    #[test]
    fn vault_evaluate_clean_when_all_secrets_decrypt_under_effective_key() {
        let key = [3u8; 32];
        let (ct, nonce) = encrypt_for_test(&key, "hello");
        let sources = vec![
            ("env", Some(key.to_vec())),
            ("file", None),
            ("keychain", None),
        ];
        let secrets = vec![probe(
            "secret://x",
            "api_key",
            "vault_secrets",
            ct,
            nonce,
            true,
        )];
        let findings = evaluate_vault_divergence("vault.key-source-divergence", &sources, &secrets);
        assert!(
            findings.is_empty(),
            "expected no findings, got {findings:?}"
        );
    }

    #[test]
    fn vault_evaluate_referenced_failure_is_critical() {
        let real_key = [3u8; 32];
        let wrong_key = [4u8; 32];
        let (ct, nonce) = encrypt_for_test(&real_key, "hello");
        let sources = vec![
            ("env", Some(wrong_key.to_vec())),
            ("file", None),
            ("keychain", None),
        ];
        let secrets = vec![probe(
            "secret://referenced",
            "openai_api_key",
            "graph_node",
            ct.clone(),
            nonce,
            true, // referenced
        )];
        let findings = evaluate_vault_divergence("vault.key-source-divergence", &sources, &secrets);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Critical);
        assert!(!findings[0].auto_repairable);
        assert!(findings[0].message.contains("secret://referenced"));
        assert!(findings[0].message.contains("live wiring will break"));
        assert_eq!(findings[0].evidence["referenced_failures"], json!(1));
        assert_eq!(findings[0].evidence["orphan_undecryptable"], json!(0));
        // Never leak ciphertext/nonce material into evidence.
        let dump = findings[0].evidence.to_string();
        assert!(!dump.contains(&ct), "must not leak ciphertext");
    }

    #[test]
    fn vault_evaluate_orphan_failure_is_warning_not_critical() {
        let real_key = [3u8; 32];
        let wrong_key = [4u8; 32];
        let (ct, nonce) = encrypt_for_test(&real_key, "hello");
        let sources = vec![
            ("env", Some(wrong_key.to_vec())),
            ("file", None),
            ("keychain", None),
        ];
        let secrets = vec![probe(
            "secret://hotel/default/api_key/835c1f45",
            "api_key",
            "vault_secrets",
            ct,
            nonce,
            false, // unreferenced — the exact orphan scenario from the field
        )];
        let findings = evaluate_vault_divergence("vault.key-source-divergence", &sources, &secrets);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].severity,
            Severity::Warning,
            "orphan must NOT be critical: {findings:?}"
        );
        assert!(findings[0].message.contains("orphaned secret"));
        assert!(findings[0].message.contains("safe to prune"));
        assert_eq!(findings[0].evidence["referenced_failures"], json!(0));
        assert_eq!(findings[0].evidence["orphan_undecryptable"], json!(1));
    }

    #[test]
    fn vault_evaluate_referenced_failure_wins_severity_over_coexisting_orphan_failure() {
        let real_key = [3u8; 32];
        let wrong_key = [4u8; 32];
        let (ct1, nonce1) = encrypt_for_test(&real_key, "hello");
        let (ct2, nonce2) = encrypt_for_test(&real_key, "world");
        let sources = vec![
            ("env", Some(wrong_key.to_vec())),
            ("file", None),
            ("keychain", None),
        ];
        let secrets = vec![
            probe(
                "secret://referenced",
                "openai_api_key",
                "graph_node",
                ct1,
                nonce1,
                true,
            ),
            probe(
                "secret://orphan",
                "api_key",
                "vault_secrets",
                ct2,
                nonce2,
                false,
            ),
        ];
        let findings = evaluate_vault_divergence("vault.key-source-divergence", &sources, &secrets);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Critical);
        assert_eq!(findings[0].evidence["referenced_failures"], json!(1));
        assert_eq!(findings[0].evidence["orphan_undecryptable"], json!(1));
    }

    #[test]
    fn vault_evaluate_no_source_but_secrets_exist_is_warning() {
        let sources = vec![("env", None), ("file", None), ("keychain", None)];
        let secrets = vec![probe(
            "secret://x",
            "k",
            "vault_secrets",
            "ct".to_string(),
            "n".to_string(),
            false,
        )];
        let findings = evaluate_vault_divergence("vault.key-source-divergence", &sources, &secrets);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
    }

    #[test]
    fn vault_evaluate_no_source_and_no_secrets_is_clean() {
        let sources = vec![("env", None), ("file", None), ("keychain", None)];
        let findings = evaluate_vault_divergence("vault.key-source-divergence", &sources, &[]);
        assert!(findings.is_empty());
    }

    #[test]
    fn vault_secrets_from_db_reads_fixture_rows() {
        let (_dir, path) = fixture_db();
        let key = [5u8; 32];
        let (ct, nonce) = encrypt_for_test(&key, "provider-secret");
        {
            let storage = SqliteGraphStorage::open(&path).expect("open");
            let conn = storage.raw_conn().lock().unwrap();
            insert_secret(&conn, "secret://x", &ct, &nonce);
        }
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let secrets = vault_secrets_from_db(&ctx.conn).expect("read secrets");
        assert_eq!(
            secrets,
            vec![RawSecret {
                secret_ref: "secret://x".to_string(),
                secret_kind: "k".to_string(),
                ciphertext_b64: ct,
                nonce_b64: nonce,
            }]
        );
    }

    #[test]
    fn graph_node_secrets_from_db_reads_secret_prefixed_nodes_only() {
        let (_dir, path) = fixture_db();
        let key = [6u8; 32];
        let (ct, nonce) = encrypt_for_test(&key, "graph-secret");
        {
            let storage = SqliteGraphStorage::open(&path).expect("open");
            let conn = storage.raw_conn().lock().unwrap();
            insert_graph_secret(&conn, "secret://graph/one", "openai_api_key", &ct, &nonce);
            // A non-secret graph node must never be picked up by the
            // `secret:%` LIKE filter.
            conn.execute(
                "INSERT INTO graph_nodes (node_key, kind, label, data_json, updated_at) VALUES ('role:brain', 'role', 'brain', '{}', 0)",
                [],
            )
            .expect("insert unrelated node");
        }
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let secrets = graph_node_secrets_from_db(&ctx.conn).expect("read graph secrets");
        assert_eq!(secrets.len(), 1);
        assert_eq!(secrets[0].0, "secret:secret://graph/one");
        assert_eq!(secrets[0].1.secret_ref, "secret://graph/one");
        assert_eq!(secrets[0].1.secret_kind, "openai_api_key");
    }

    // ── collect_vault_secrets: full detect()-path referenced/orphan/dedup ──

    #[test]
    fn collect_vault_secrets_classifies_referenced_via_node_config() {
        let (_dir, path) = fixture_db();
        let key = [7u8; 32];
        let (ct, nonce) = encrypt_for_test(&key, "s");
        {
            let storage = SqliteGraphStorage::open(&path).expect("open");
            let conn = storage.raw_conn().lock().unwrap();
            insert_graph_secret(&conn, "secret://ref-via-config", "vault-token", &ct, &nonce);
            insert_node_config_ref(
                &conn,
                "config:gemini_api_key_ref",
                "secret://ref-via-config",
            );
        }
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let secrets = collect_vault_secrets(&ctx.conn).expect("collect");
        assert_eq!(secrets.len(), 1);
        assert!(
            secrets[0].referenced,
            "expected node_config wiring to mark referenced"
        );
    }

    #[test]
    fn collect_vault_secrets_classifies_referenced_via_other_graph_node() {
        let (_dir, path) = fixture_db();
        let key = [7u8; 32];
        let (ct, nonce) = encrypt_for_test(&key, "s");
        {
            let storage = SqliteGraphStorage::open(&path).expect("open");
            let conn = storage.raw_conn().lock().unwrap();
            insert_graph_secret(&conn, "secret://ref-via-node", "vault-token", &ct, &nonce);
            conn.execute(
                "INSERT INTO graph_nodes (node_key, kind, label, data_json, updated_at) VALUES ('role:brain', 'role', 'brain', ?1, 0)",
                params![json!({"secret_ref": "secret://ref-via-node"}).to_string()],
            )
            .expect("insert referencing node");
        }
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let secrets = collect_vault_secrets(&ctx.conn).expect("collect");
        assert_eq!(secrets.len(), 1);
        assert!(secrets[0].referenced);
    }

    #[test]
    fn collect_vault_secrets_does_not_self_reference_own_graph_node() {
        // A secret's own data_json trivially contains its own secret_ref —
        // that must never count as "referenced by something else".
        let (_dir, path) = fixture_db();
        let key = [7u8; 32];
        let (ct, nonce) = encrypt_for_test(&key, "s");
        {
            let storage = SqliteGraphStorage::open(&path).expect("open");
            let conn = storage.raw_conn().lock().unwrap();
            insert_graph_secret(&conn, "secret://lonely", "vault-token", &ct, &nonce);
        }
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let secrets = collect_vault_secrets(&ctx.conn).expect("collect");
        assert_eq!(secrets.len(), 1);
        assert!(
            !secrets[0].referenced,
            "must not self-reference via its own node_key/data_json"
        );
    }

    /// Regression guard for the live-bjork wiring shape: a `config:*_ref`
    /// graph node whose `data_json` double-JSON-encodes the ref as
    /// `{"value":"\"secret://...\""}` (escaped `\"`, not a bare `"`) must
    /// still be recognized as a reference. A stricter "must be
    /// bare-quoted" matcher was tried during review and silently broke
    /// this exact shape, downgrading a live Critical divergence finding to
    /// Warning — see the doc comment on `secret_ref_is_referenced`.
    #[test]
    fn secret_ref_is_referenced_matches_double_json_encoded_value_field() {
        let node_config_values: Vec<String> = vec![];
        let graph_data_jsons = vec![(
            "config:oidc_github_client_secret_ref".to_string(),
            // Mirrors the exact bytes observed on the live bjork context.db.
            "{\"value\":\"\\\"secret://hotel/default/vault-token/92bda314c29e42f991b808c195c21c45\\\"\"}".to_string(),
        )];
        assert!(
            secret_ref_is_referenced(
                &node_config_values,
                &graph_data_jsons,
                "secret://hotel/default/vault-token/92bda314c29e42f991b808c195c21c45",
                None,
            ),
            "double-JSON-encoded (escaped-quote) wiring must still count as referenced"
        );
    }

    #[test]
    fn collect_vault_secrets_dedups_by_secret_ref_preferring_graph_node() {
        let (_dir, path) = fixture_db();
        let table_key = [1u8; 32];
        let graph_key = [2u8; 32];
        let (table_ct, table_nonce) = encrypt_for_test(&table_key, "table-copy");
        let (graph_ct, graph_nonce) = encrypt_for_test(&graph_key, "graph-copy");
        {
            let storage = SqliteGraphStorage::open(&path).expect("open");
            let conn = storage.raw_conn().lock().unwrap();
            insert_secret(&conn, "secret://dup", &table_ct, &table_nonce);
            insert_graph_secret(&conn, "secret://dup", "api_key", &graph_ct, &graph_nonce);
        }
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let secrets = collect_vault_secrets(&ctx.conn).expect("collect");
        assert_eq!(
            secrets.len(),
            1,
            "duplicate secret_ref across stores must be de-duped"
        );
        assert_eq!(secrets[0].source, "graph_node");
        assert_eq!(secrets[0].ciphertext_b64, graph_ct);
    }

    /// End-to-end regression coverage for the exact field scenario: 15
    /// live, referenced graph-node secrets that all decrypt fine, plus one
    /// legacy `vault_secrets` orphan that does not decrypt under the
    /// current key. Must classify as one Warning finding, never Critical.
    #[test]
    fn collect_vault_secrets_field_scenario_orphan_warning_not_critical() {
        let (_dir, path) = fixture_db();
        let live_key = [42u8; 32];
        let stale_key = [99u8; 32]; // pre-re-key master key, no longer effective
        {
            let storage = SqliteGraphStorage::open(&path).expect("open");
            let conn = storage.raw_conn().lock().unwrap();
            for i in 0..15 {
                let secret_ref = format!("secret://hotel/default/live/{i}");
                let (ct, nonce) = encrypt_for_test(&live_key, "live-secret");
                insert_graph_secret(&conn, &secret_ref, "openai_api_key", &ct, &nonce);
                insert_node_config_ref(&conn, &format!("config:key_{i}_ref"), &secret_ref);
            }
            let (orphan_ct, orphan_nonce) = encrypt_for_test(&stale_key, "dead-weight");
            insert_secret(
                &conn,
                "secret://hotel/default/api_key/835c1f45",
                &orphan_ct,
                &orphan_nonce,
            );
        }
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let secrets = collect_vault_secrets(&ctx.conn).expect("collect");
        assert_eq!(secrets.len(), 16);

        let sources = vec![
            ("env", Some(live_key.to_vec())),
            ("file", None),
            ("keychain", None),
        ];
        let findings = evaluate_vault_divergence("vault.key-source-divergence", &sources, &secrets);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].severity,
            Severity::Warning,
            "field scenario must be Warning/orphan, not Critical: {findings:?}"
        );
        assert_eq!(findings[0].evidence["referenced_failures"], json!(0));
        assert_eq!(findings[0].evidence["orphan_undecryptable"], json!(1));
        assert_eq!(findings[0].evidence["decrypted"], json!(15));
        assert_eq!(findings[0].evidence["total"], json!(16));
    }

    // ── logs.rotation-missing ───────────────────────────────────────────

    #[test]
    fn log_rotation_clean_when_dropin_exists() {
        let findings =
            evaluate_log_rotation("logs.rotation-missing", "/x/aiua.log", Some(1_000), true);
        assert!(findings.is_empty());
    }

    #[test]
    fn log_rotation_warns_when_dropin_missing() {
        let findings =
            evaluate_log_rotation("logs.rotation-missing", "/x/aiua.log", Some(1_000), false);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
    }

    #[test]
    fn log_rotation_escalates_to_error_over_threshold() {
        let big = LOG_ROTATION_WARN_BYTES + 1;
        let findings =
            evaluate_log_rotation("logs.rotation-missing", "/x/aiua.log", Some(big), false);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
    }

    #[test]
    fn log_rotation_missing_file_still_flags_missing_dropin() {
        let findings = evaluate_log_rotation("logs.rotation-missing", "/x/aiua.log", None, false);
        assert_eq!(findings.len(), 1);
    }

    // ── secrets.store-permissions ────────────────────────────────────────

    #[test]
    fn store_permissions_clean_when_tight() {
        let findings = evaluate_store_permissions(
            "secrets.store-permissions",
            "/x/context.db",
            Some(0o600),
            "/x",
            Some(0o700),
        );
        assert!(findings.is_empty(), "expected clean, got {findings:?}");
    }

    #[test]
    fn store_permissions_flags_world_readable_db() {
        let findings = evaluate_store_permissions(
            "secrets.store-permissions",
            "/x/context.db",
            Some(0o644),
            "/x",
            Some(0o700),
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].auto_repairable);
        assert!(findings[0].fix_hint.contains("chmod 600"));
        assert!(findings[0].fix_hint.contains("chmod 700"));
    }

    #[test]
    fn store_permissions_flags_world_readable_dir() {
        let findings = evaluate_store_permissions(
            "secrets.store-permissions",
            "/x/context.db",
            Some(0o600),
            "/x",
            Some(0o755),
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
    }

    #[test]
    fn store_permissions_escalates_to_error_when_world_writable() {
        let findings = evaluate_store_permissions(
            "secrets.store-permissions",
            "/x/context.db",
            Some(0o666),
            "/x",
            Some(0o700),
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Error);
    }

    #[test]
    fn store_permissions_unreadable_metadata_is_not_flagged() {
        // A stat() failure (None) must not be treated as loose — there's no
        // evidence either way, so don't manufacture a false positive.
        let findings = evaluate_store_permissions(
            "secrets.store-permissions",
            "/x/context.db",
            None,
            "/x",
            None,
        );
        assert!(findings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn store_permissions_check_detects_loose_fixture_db_and_repair_tightens_it() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, db_path) = fixture_db();
        std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o644))
            .expect("chmod fixture db to 0644");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod fixture dir to 0755");

        let ctx = ctx_with_profile_dir("jane", db_path.clone(), dir.path().to_path_buf());
        let findings = SecretsStorePermissions.detect(&ctx).expect("detect");
        assert_eq!(findings.len(), 1, "0644 db + 0755 dir must be flagged");
        assert_eq!(findings[0].severity, Severity::Warning);

        // Dry-run: planned, must not touch disk.
        let planned = SecretsStorePermissions
            .repair(&ctx, &findings[0], false)
            .expect("repair dry-run");
        assert!(matches!(planned, RepairOutcome::Planned { .. }));
        assert_eq!(
            unix_perms::mode(&db_path),
            Some(0o644),
            "dry-run must not chmod"
        );

        // Apply: tightens to 0600/0700 and journals before/after.
        let applied = SecretsStorePermissions
            .repair(&ctx, &findings[0], true)
            .expect("repair apply");
        assert!(matches!(applied, RepairOutcome::Applied { .. }));
        assert_eq!(unix_perms::mode(&db_path), Some(0o600));
        assert_eq!(unix_perms::mode(dir.path()), Some(0o700));

        let journal_path = dir.path().join("doctor-repair-journal.jsonl");
        assert!(journal_path.exists());
        let content = std::fs::read_to_string(&journal_path).expect("read journal");
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 1);
        let entry: serde_json::Value = serde_json::from_str(lines[0]).expect("parse journal line");
        assert_eq!(entry["check_id"], json!("secrets.store-permissions"));
        assert_eq!(entry["action"], json!("chmod-store-permissions"));
        assert_eq!(entry["before"]["db_mode"], json!("0644"));
        assert_eq!(entry["after"]["db_mode"], json!("0600"));

        // Re-running detect() now finds nothing — clean.
        let clean = SecretsStorePermissions.detect(&ctx).expect("detect again");
        assert!(
            clean.is_empty(),
            "expected clean after repair, got {clean:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn store_permissions_tight_fixture_db_has_no_finding() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, db_path) = fixture_db();
        std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o600))
            .expect("chmod fixture db to 0600");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod fixture dir to 0700");

        let ctx = ctx_with_profile_dir("jane", db_path, dir.path().to_path_buf());
        let findings = SecretsStorePermissions.detect(&ctx).expect("detect");
        assert!(findings.is_empty(), "expected clean, got {findings:?}");
    }

    // ── Severity ordering / parsing ──────────────────────────────────────

    #[test]
    fn severity_ordering_is_ascending() {
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
        assert!(Severity::Error < Severity::Critical);
    }

    #[test]
    fn severity_from_str_accepts_known_values_case_insensitively() {
        assert_eq!(Severity::from_str("Warning").unwrap(), Severity::Warning);
        assert_eq!(Severity::from_str("CRITICAL").unwrap(), Severity::Critical);
        assert!(Severity::from_str("bogus").is_err());
    }

    // ── Zero-writes guarantee ────────────────────────────────────────────

    #[test]
    fn doctor_ctx_connection_rejects_writes() {
        let (_dir, path) = fixture_db();
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let result = ctx.conn.execute("DELETE FROM hotels", []);
        assert!(result.is_err(), "read-only connection must reject writes");
    }

    #[test]
    fn running_all_checks_does_not_modify_fixture_db() {
        let (_dir, path) = fixture_db();
        {
            let storage = SqliteGraphStorage::open(&path).expect("open");
            let conn = storage.raw_conn().lock().unwrap();
            insert_hotel(&conn, "jane", 1, 2, 3, Some("42"));
        }
        let before = std::fs::read(&path).expect("read before");
        let mtime_before = std::fs::metadata(&path)
            .expect("meta")
            .modified()
            .expect("mtime");

        let ctx = DoctorCtx::open_at("jane", path.clone()).expect("open ctx");
        for check in catalog() {
            let _ = check.detect(&ctx);
        }
        drop(ctx);

        let after = std::fs::read(&path).expect("read after");
        let mtime_after = std::fs::metadata(&path)
            .expect("meta")
            .modified()
            .expect("mtime");
        assert_eq!(before, after, "doctor must not alter DB contents");
        assert_eq!(mtime_before, mtime_after, "doctor must not alter DB mtime");
    }

    // ── --json schema shape ──────────────────────────────────────────────

    #[test]
    fn doctor_report_json_shape_is_stable() {
        let report = DoctorReport {
            ok: false,
            hotel: "jane".to_string(),
            db_path: "/x/context.db".to_string(),
            checks_run: 1,
            findings: vec![Finding {
                check_id: "logs.rotation-missing".to_string(),
                severity: Severity::Warning,
                message: "no rotation".to_string(),
                evidence: json!({"log_path": "/x"}),
                fix_hint: "install rotation".to_string(),
                auto_repairable: false,
            }],
            repairs: vec![RepairEntry {
                check_id: "logs.rotation-missing".to_string(),
                outcome: RepairOutcome::Planned {
                    description: "install newsyslog drop-in".to_string(),
                },
            }],
        };
        let value = serde_json::to_value(&report).expect("serialize");
        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["hotel"], json!("jane"));
        assert_eq!(value["db_path"], json!("/x/context.db"));
        assert_eq!(value["checks_run"], json!(1));
        assert_eq!(
            value["findings"][0]["check_id"],
            json!("logs.rotation-missing")
        );
        assert_eq!(value["findings"][0]["severity"], json!("warning"));
        assert_eq!(value["findings"][0]["auto_repairable"], json!(false));
        assert_eq!(
            value["repairs"][0]["check_id"],
            json!("logs.rotation-missing")
        );
        assert_eq!(value["repairs"][0]["outcome"]["outcome"], json!("planned"));
    }

    // ── DB targeting resolution (slice 2) ─────────────────────────────────

    #[test]
    fn resolve_db_target_db_override_wins_over_everything() {
        let explicit = PathBuf::from("/tmp/some/explicit/context.db");
        let (path, dir) =
            resolve_db_target(Some(explicit.clone()), Some("other-profile")).expect("resolve");
        assert_eq!(path, explicit);
        assert_eq!(dir, PathBuf::from("/tmp/some/explicit"));
    }

    #[test]
    fn resolve_db_target_profile_override_resolves_context_db_not_aiua_context_db() {
        let (path, dir) = resolve_db_target(None, Some("bjork")).expect("resolve");
        assert!(
            path.ends_with("bjork/context.db"),
            "path was {}",
            path.display()
        );
        assert!(!path.to_string_lossy().contains("aiua_context.db"));
        assert_eq!(dir, philotic_dir().join("bjork"));
    }

    // Env-var-dependent tests share one mutex — PHILOTIC_PROFILE is process
    // global, and cargo runs tests in the same binary concurrently.
    static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn resolve_db_target_refuses_with_no_override_and_no_env() {
        let _guard = ENV_GUARD.lock().unwrap();
        let saved = std::env::var("PHILOTIC_PROFILE").ok();
        std::env::remove_var("PHILOTIC_PROFILE");
        let result = resolve_db_target(None, None);
        match saved {
            Some(v) => std::env::set_var("PHILOTIC_PROFILE", v),
            None => std::env::remove_var("PHILOTIC_PROFILE"),
        }
        let err = result.expect_err("must refuse without any target — no CWD fallback");
        let msg = format!("{err:#}");
        assert!(msg.contains("no hotel targeted"), "message was: {msg}");
        assert!(msg.contains("--profile"));
        assert!(msg.contains("--db"));
    }

    #[test]
    fn resolve_db_target_falls_back_to_env_profile_when_no_explicit_override() {
        let _guard = ENV_GUARD.lock().unwrap();
        let saved = std::env::var("PHILOTIC_PROFILE").ok();
        std::env::set_var("PHILOTIC_PROFILE", "env-test-profile");
        let result = resolve_db_target(None, None);
        match saved {
            Some(v) => std::env::set_var("PHILOTIC_PROFILE", v),
            None => std::env::remove_var("PHILOTIC_PROFILE"),
        }
        let (path, _dir) = result.expect("resolve");
        assert!(
            path.ends_with("env-test-profile/context.db"),
            "path was {}",
            path.display()
        );
    }

    // ── graceful missing-store (slice 2) ────────────────────────────────
    //
    // Regression coverage for the wrong-DB incident: opening a store that
    // lacks an expected table (e.g. the auth-only `aiua_context.db`) must
    // produce a clear, check-scoped finding, never a raw SQL error that
    // aborts the whole run.

    /// A minimal on-disk sqlite DB with an unrelated table only — mirrors
    /// the shape of the auth-only `aiua_context.db` that lacks `hotels` and
    /// `vault_secrets` entirely.
    fn bare_db_missing_expected_tables() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("aiua_context.db");
        let conn = Connection::open(&path).expect("open bare db");
        conn.execute("CREATE TABLE operator_auth (id INTEGER PRIMARY KEY)", [])
            .expect("create unrelated table");
        drop(conn);
        (dir, path)
    }

    #[test]
    fn table_exists_true_for_present_and_false_for_absent() {
        let (_dir, path) = fixture_db();
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        assert!(table_exists(&ctx.conn, "vault_secrets").expect("query"));
        assert!(!table_exists(&ctx.conn, "not_a_real_table").expect("query"));
    }

    #[test]
    fn vault_check_on_db_missing_vault_secrets_table_skips_gracefully() {
        let (_dir, path) = bare_db_missing_expected_tables();
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let findings = VaultKeySourceDivergence
            .detect(&ctx)
            .expect("detect must not hard-error on a missing table");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].message.contains("vault_secrets"));
    }

    /// The legacy `vault_secrets` table missing must not swallow secrets
    /// that live exclusively in `graph_nodes` `secret:*` rows — the check
    /// must fall through and evaluate those, not report "table not found,
    /// skipped" while a real, referenced secret goes unchecked.
    #[test]
    fn vault_check_falls_through_to_graph_nodes_when_vault_secrets_table_absent() {
        let (_dir, path) = fixture_db();
        let key = [11u8; 32];
        let (ct, nonce) = encrypt_for_test(&key, "graph-only-secret");
        {
            let storage = SqliteGraphStorage::open(&path).expect("open");
            let conn = storage.raw_conn().lock().unwrap();
            insert_graph_secret(&conn, "secret://graph-only", "api_key", &ct, &nonce);
            insert_node_config_ref(&conn, "config:api_key_ref", "secret://graph-only");
            conn.execute("DROP TABLE vault_secrets", [])
                .expect("drop legacy table");
        }
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        assert!(!table_exists(&ctx.conn, "vault_secrets").expect("query"));

        // Sanity: collect_vault_secrets (what detect() delegates to once it
        // decides not to skip) does see the graph-node secret.
        let secrets = collect_vault_secrets(&ctx.conn).expect("collect");
        assert_eq!(secrets.len(), 1);
        assert!(secrets[0].referenced);

        let findings = VaultKeySourceDivergence
            .detect(&ctx)
            .expect("detect must not hard-error with vault_secrets absent");
        // Whatever this evaluates to (key-source resolution is
        // environment-dependent in a unit test), it must never be the
        // "table not present" skip warning — a live, non-empty secret
        // store (graph_nodes) was available.
        assert!(
            findings.iter().all(|f| !f.message.contains("not present")),
            "must not report vault_secrets as missing when graph_nodes secrets exist: {findings:?}"
        );
    }

    #[test]
    fn ports_check_on_db_missing_hotels_table_skips_gracefully() {
        let (_dir, path) = bare_db_missing_expected_tables();
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let findings = PortsHotelRecordDrift
            .detect(&ctx)
            .expect("detect must not hard-error on a missing table");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].message.contains("hotels"));
    }

    #[test]
    fn orphans_check_on_db_missing_hotels_table_skips_gracefully() {
        let (_dir, path) = bare_db_missing_expected_tables();
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let findings = ProcOrphanInstances
            .detect(&ctx)
            .expect("detect must not hard-error on a missing table");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].message.contains("hotels"));
    }

    // ── repair() ─────────────────────────────────────────────────────────

    fn dummy_finding(check_id: &str, evidence: serde_json::Value) -> Finding {
        Finding {
            check_id: check_id.to_string(),
            severity: Severity::Error,
            message: "test finding".to_string(),
            evidence,
            fix_hint: "test fix hint".to_string(),
            auto_repairable: true,
        }
    }

    fn ctx_with_profile_dir(hotel: &str, db_path: PathBuf, profile_dir: PathBuf) -> DoctorCtx {
        let mut ctx = DoctorCtx::open_at(hotel, db_path).expect("open ctx");
        ctx.profile_dir = profile_dir;
        ctx
    }

    #[test]
    fn ports_drift_repair_is_always_needs_confirm() {
        let (_dir, path) = fixture_db();
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let finding = dummy_finding(
            "ports.hotel-record-drift",
            json!({"expected_default": {"mesh_port": 100, "blob_port": 101, "execution_port": 102}}),
        );
        for apply in [false, true] {
            let outcome = PortsHotelRecordDrift
                .repair(&ctx, &finding, apply)
                .expect("repair");
            assert!(matches!(outcome, RepairOutcome::NeedsConfirm { .. }));
        }
    }

    #[test]
    fn orphans_repair_is_always_needs_confirm() {
        let (_dir, path) = fixture_db();
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let finding = dummy_finding("proc.orphan-instances", json!({"orphan_pids": [999]}));
        for apply in [false, true] {
            let outcome = ProcOrphanInstances
                .repair(&ctx, &finding, apply)
                .expect("repair");
            assert!(matches!(outcome, RepairOutcome::NeedsConfirm { .. }));
        }
    }

    #[test]
    fn vault_repair_is_always_not_repairable_and_never_touches_key_material() {
        let (_dir, path) = fixture_db();
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let finding = dummy_finding("vault.key-source-divergence", json!({}));
        for apply in [false, true] {
            let outcome = VaultKeySourceDivergence
                .repair(&ctx, &finding, apply)
                .expect("repair");
            assert!(matches!(outcome, RepairOutcome::NotRepairable));
        }
    }

    #[test]
    fn ipc_stale_socket_repair_dry_run_is_planned_and_does_not_touch_disk() {
        let (dir, db_path) = fixture_db();
        let sock_dir = tempfile::tempdir().expect("tempdir");
        let sock_path = sock_dir.path().join("stale.sock");
        {
            let listener = std::os::unix::net::UnixListener::bind(&sock_path).expect("bind");
            drop(listener);
        }
        let ctx = ctx_with_profile_dir("jane", db_path, dir.path().to_path_buf());
        let finding = dummy_finding(
            "ipc.stale-socket",
            json!({"path": sock_path.to_string_lossy()}),
        );

        let outcome = IpcStaleSocket
            .repair(&ctx, &finding, false)
            .expect("repair");
        assert!(matches!(outcome, RepairOutcome::Planned { .. }));
        assert!(sock_path.exists(), "dry-run must not touch the socket");
        assert!(!dir.path().join("doctor-repair-journal.jsonl").exists());
    }

    #[test]
    fn ipc_stale_socket_repair_apply_quarantines_never_deletes() {
        let (dir, db_path) = fixture_db();
        let sock_dir = tempfile::tempdir().expect("tempdir");
        let sock_path = sock_dir.path().join("stale.sock");
        {
            let listener = std::os::unix::net::UnixListener::bind(&sock_path).expect("bind");
            drop(listener);
        }
        let ctx = ctx_with_profile_dir("jane", db_path, dir.path().to_path_buf());
        let finding = dummy_finding(
            "ipc.stale-socket",
            json!({"path": sock_path.to_string_lossy()}),
        );

        let outcome = IpcStaleSocket.repair(&ctx, &finding, true).expect("repair");
        assert!(matches!(outcome, RepairOutcome::Applied { .. }));

        // Original path is gone, but only because it was renamed, not rm'd —
        // the quarantined copy must exist right next to it.
        assert!(!sock_path.exists(), "original stale socket must be gone");
        let expected_stale =
            std::path::PathBuf::from(format!("{}.stale", sock_path.to_string_lossy()));
        assert!(
            expected_stale.exists(),
            "quarantined socket must exist at {}",
            expected_stale.display()
        );

        let journal_path = dir.path().join("doctor-repair-journal.jsonl");
        assert!(journal_path.exists(), "journal line must be written");
        let content = std::fs::read_to_string(&journal_path).expect("read journal");
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 1);
        let entry: serde_json::Value = serde_json::from_str(lines[0]).expect("parse journal line");
        assert_eq!(entry["id"], json!(1));
        assert_eq!(entry["check_id"], json!("ipc.stale-socket"));
        assert_eq!(entry["action"], json!("quarantine-socket"));
    }

    #[test]
    fn ipc_stale_socket_repair_apply_is_idempotent_on_second_run() {
        let (dir, db_path) = fixture_db();
        let sock_dir = tempfile::tempdir().expect("tempdir");
        let sock_path = sock_dir.path().join("stale.sock");
        {
            let listener = std::os::unix::net::UnixListener::bind(&sock_path).expect("bind");
            drop(listener);
        }
        let ctx = ctx_with_profile_dir("jane", db_path, dir.path().to_path_buf());
        let finding = dummy_finding(
            "ipc.stale-socket",
            json!({"path": sock_path.to_string_lossy()}),
        );

        let first = IpcStaleSocket
            .repair(&ctx, &finding, true)
            .expect("repair 1");
        assert!(matches!(first, RepairOutcome::Applied { .. }));

        // Running again against the same (now-missing) original path must
        // not panic — it should report Skipped, since there is nothing left
        // to quarantine.
        let second = IpcStaleSocket
            .repair(&ctx, &finding, true)
            .expect("repair 2");
        assert!(matches!(second, RepairOutcome::Skipped { .. }));

        let journal_path = dir.path().join("doctor-repair-journal.jsonl");
        let content = std::fs::read_to_string(&journal_path).expect("read journal");
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 1, "second (skipped) attempt must not journal");
    }

    #[test]
    fn logs_rotation_repair_dry_run_is_planned() {
        let (dir, db_path) = fixture_db();
        let ctx = ctx_with_profile_dir("jane", db_path, dir.path().to_path_buf());
        let finding = dummy_finding("logs.rotation-missing", json!({"log_path": "/x"}));

        let outcome = LogsRotationMissing
            .repair(&ctx, &finding, false)
            .expect("repair");
        match outcome {
            RepairOutcome::Planned { description } => {
                assert!(description.contains("install-log-rotation.sh"));
            }
            other => panic!("expected Planned, got {other:?}"),
        }
        assert!(!dir.path().join("doctor-repair-journal.jsonl").exists());
    }

    #[test]
    fn logs_rotation_repair_apply_never_panics_and_resolves_gracefully() {
        // This is the one repair whose real target lives under /etc and
        // needs sudo — a live `doctor --fix` is deliberately not run in
        // this test suite. It must still either apply (extremely unlikely
        // without a passwordless sudoer on the test machine) or report a
        // graceful Skipped with a manual fallback command; it must never
        // panic or return a hard Err.
        let (dir, db_path) = fixture_db();
        let ctx = ctx_with_profile_dir("jane", db_path, dir.path().to_path_buf());
        let finding = dummy_finding("logs.rotation-missing", json!({"log_path": "/x"}));

        let outcome = LogsRotationMissing
            .repair(&ctx, &finding, true)
            .expect("repair must not hard-error");
        match outcome {
            RepairOutcome::Applied { .. } => {
                // Only plausible with passwordless sudo on this machine —
                // still valid, just journal it.
                let journal_path = dir.path().join("doctor-repair-journal.jsonl");
                assert!(journal_path.exists());
            }
            RepairOutcome::Skipped { reason } => {
                assert!(!reason.is_empty());
            }
            other => panic!("expected Applied or Skipped, got {other:?}"),
        }
    }

    #[test]
    fn run_never_mutates_fixture_db_with_fix_flag_on_non_writable_checks() {
        // repair() calls for ports/orphans/vault must never touch the
        // read-only context DB even when apply=true.
        let (_dir, path) = fixture_db();
        {
            let storage = SqliteGraphStorage::open(&path).expect("open");
            let conn = storage.raw_conn().lock().unwrap();
            insert_hotel(&conn, "jane", 1, 2, 3, Some("42"));
        }
        let before = std::fs::read(&path).expect("read before");

        let ctx = DoctorCtx::open_at("jane", path.clone()).expect("open ctx");
        let ports_finding = dummy_finding(
            "ports.hotel-record-drift",
            json!({"expected_default": {"mesh_port": 1, "blob_port": 2, "execution_port": 3}}),
        );
        let _ = PortsHotelRecordDrift
            .repair(&ctx, &ports_finding, true)
            .expect("repair");
        let orphan_finding = dummy_finding("proc.orphan-instances", json!({"orphan_pids": [1]}));
        let _ = ProcOrphanInstances
            .repair(&ctx, &orphan_finding, true)
            .expect("repair");
        let vault_finding = dummy_finding("vault.key-source-divergence", json!({}));
        let _ = VaultKeySourceDivergence
            .repair(&ctx, &vault_finding, true)
            .expect("repair");
        drop(ctx);

        let after = std::fs::read(&path).expect("read after");
        assert_eq!(
            before, after,
            "NeedsConfirm/NotRepairable must not mutate the DB"
        );
    }

    // ── self-heal circuit observability ─────────────────────────────────

    fn only(sev: &[Severity], findings: &[Finding]) -> bool {
        findings.iter().map(|f| f.severity).eq(sev.iter().copied())
    }

    // heal.queue-depth threshold logic.
    #[test]
    fn heal_queue_depth_thresholds_fire_by_backlog() {
        let id = "heal.queue-depth";
        // Below and exactly at the warn boundary → clean.
        assert!(evaluate_heal_queue_depth(id, 100, 0).is_empty());
        assert!(evaluate_heal_queue_depth(id, 40, 60).is_empty());
        // Just over warn (pending + assigned counted together) → Warning.
        assert!(only(
            &[Severity::Warning],
            &evaluate_heal_queue_depth(id, 60, 41)
        ));
        // Exactly at the critical boundary is still Warning; over it → Critical.
        assert!(only(
            &[Severity::Warning],
            &evaluate_heal_queue_depth(id, 500, 0)
        ));
        assert!(only(
            &[Severity::Critical],
            &evaluate_heal_queue_depth(id, 400, 101)
        ));
    }

    // heal.oldest-pending-age threshold logic.
    #[test]
    fn heal_oldest_pending_thresholds_fire_by_age() {
        let id = "heal.oldest-pending-age";
        let now = 1_000_000;
        // No pending rows → clean.
        assert!(evaluate_heal_oldest_pending(id, None, now).is_empty());
        // Fresh and exactly at the warn boundary → clean.
        assert!(evaluate_heal_oldest_pending(id, Some(now - 600), now).is_empty());
        // Over warn → Warning.
        assert!(only(
            &[Severity::Warning],
            &evaluate_heal_oldest_pending(id, Some(now - 601), now)
        ));
        // Over critical → Critical.
        assert!(only(
            &[Severity::Critical],
            &evaluate_heal_oldest_pending(id, Some(now - 1801), now)
        ));
    }

    // A future oldest-pending timestamp must NOT read as clean — it degrades to
    // a soft Info note, never a silent green.
    #[test]
    fn heal_oldest_pending_future_timestamp_is_soft_note_not_clean() {
        let now = 1_000_000;
        let findings =
            evaluate_heal_oldest_pending("heal.oldest-pending-age", Some(now + 5_000), now);
        assert!(only(&[Severity::Info], &findings));
        assert!(
            !findings.is_empty(),
            "future ts must surface, not read clean"
        );
    }

    // heal.dispatcher-staleness: absent heartbeat degrades to a soft Info note
    // (older dispatcher build / never-cycled), never a hard failure.
    #[test]
    fn heal_dispatcher_absent_heartbeat_is_soft_info() {
        let findings =
            evaluate_heal_dispatcher_staleness("heal.dispatcher-staleness", None, 1_000_000);
        assert!(only(&[Severity::Info], &findings));
        assert_eq!(findings[0].severity, Severity::Info);
        assert!(
            findings[0].severity < Severity::Error,
            "must not be a failure"
        );
    }

    // A fresh heartbeat (within 3× poll) is clean; a stale one is Critical.
    #[test]
    fn heal_dispatcher_staleness_fires_critical_when_wedged() {
        let id = "heal.dispatcher-staleness";
        let now = 1_000_000;
        // Exactly at the stale boundary (90s) is still fresh.
        assert!(evaluate_heal_dispatcher_staleness(id, Some(now - 90), now).is_empty());
        // One second past the boundary → Critical (dispatcher wedged/down).
        assert!(only(
            &[Severity::Critical],
            &evaluate_heal_dispatcher_staleness(id, Some(now - 91), now)
        ));
        // Hours stale → still Critical.
        assert!(only(
            &[Severity::Critical],
            &evaluate_heal_dispatcher_staleness(id, Some(now - 7_200), now)
        ));
    }

    // THE fail-safe: a heartbeat written in the WRONG UNIT (epoch millis read
    // as seconds) lands far in the future → negative age. It must be flagged
    // as a format mismatch, NEVER read as "fresh" (which would silently mask a
    // wedged dispatcher).
    #[test]
    fn heal_dispatcher_future_heartbeat_never_reads_as_fresh() {
        let id = "heal.dispatcher-staleness";
        let now = 1_750_000_000; // ~2025 in epoch seconds
        let millis_value = now * 1000; // what a millis-writing dispatcher would store
        let findings = evaluate_heal_dispatcher_staleness(id, Some(millis_value), now);
        assert!(
            !findings.is_empty(),
            "future/millis heartbeat must not read clean"
        );
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(
            findings[0].message.contains("future"),
            "should name the future/format-mismatch cause: {}",
            findings[0].message
        );
    }

    // Defensive epoch parsing: number, JSON string, bare int all work; garbage
    // yields None (→ treated as absent, a soft note — never a false value).
    #[test]
    fn parse_epoch_secs_is_defensive() {
        assert_eq!(parse_epoch_secs("1750000000"), Some(1_750_000_000));
        assert_eq!(parse_epoch_secs("\"1750000000\""), Some(1_750_000_000));
        assert_eq!(parse_epoch_secs("  1750000000  "), Some(1_750_000_000));
        assert_eq!(parse_epoch_secs("1750000000.0"), Some(1_750_000_000));
        assert_eq!(parse_epoch_secs("not-a-number"), None);
        assert_eq!(parse_epoch_secs("\"nope\""), None);
    }

    // Detect-level graceful degradation: a DB with no heal_queue table (e.g. a
    // hotel that never booted the heal sink) must NOT hard-error — it yields a
    // single soft "table not present, skipped" Warning.
    #[test]
    fn heal_queue_checks_degrade_when_table_absent() {
        let (_dir, path) = fixture_db();
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        for check in [
            Box::new(HealQueueDepth) as Box<dyn Check>,
            Box::new(HealOldestPendingAge) as Box<dyn Check>,
        ] {
            let findings = check.detect(&ctx).expect("detect must not error");
            assert!(only(&[Severity::Warning], &findings), "{:?}", findings);
            assert!(findings[0].message.contains("heal_queue"));
        }
    }

    // Detect-level: node_config present but the heartbeat key absent → the
    // dispatcher check returns a soft Info note, never an error.
    #[test]
    fn heal_dispatcher_detect_absent_key_is_info() {
        let (_dir, path) = fixture_db();
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let findings = HealDispatcherStaleness.detect(&ctx).expect("detect");
        assert!(only(&[Severity::Info], &findings), "{:?}", findings);
    }

    // Detect-level, full path against a real populated heal_queue: an old
    // pending backlog fires both the depth (Critical) and oldest-age
    // (Critical) checks, proving the guards fire end-to-end.
    #[test]
    fn heal_queue_checks_fire_against_real_backlog() {
        use ansible_mesh_core::heal_queue::SqliteHealQueueStorage;
        let (_dir, path) = fixture_db();
        // Create the heal_queue schema on the same DB file.
        drop(SqliteHealQueueStorage::open(&path).expect("create heal_queue schema"));

        let old_ts = now_epoch_secs() - 3_600; // 1h old → past both age tiers
        {
            let w = Connection::open(&path).expect("open writable");
            for i in 0..=HEAL_DEPTH_CRITICAL_ABOVE {
                w.execute(
                    "INSERT INTO heal_queue (id, guest_id, timestamp, raw_text, severity, status) \
                     VALUES (?1, 'g', ?2, 'boom', 'high', 'pending')",
                    params![format!("row-{i}"), old_ts],
                )
                .expect("insert pending row");
            }
        }

        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let depth = HealQueueDepth.detect(&ctx).expect("detect depth");
        assert!(only(&[Severity::Critical], &depth), "{:?}", depth);
        let age = HealOldestPendingAge.detect(&ctx).expect("detect age");
        assert!(only(&[Severity::Critical], &age), "{:?}", age);
    }

    #[test]
    fn read_config_epoch_reads_graph_nodes_config_node() {
        // The heal-dispatcher stamps its heartbeat as a `graph_nodes` row keyed
        // `config:<key>` with `data_json = {"value": "<epoch>"}` — the canonical
        // config store. Doctor previously only read the legacy `node_config`
        // table and so never saw a live heartbeat (dispatcher-staleness stuck on
        // the "can't confirm" note even against a healthy dispatcher).
        let (_dir, path) = fixture_db();
        let ts = now_epoch_secs();
        {
            let w = Connection::open(&path).expect("open writable");
            w.execute(
                "INSERT INTO graph_nodes (node_key, kind, data_json) VALUES (?1, 'config', ?2)",
                params![
                    format!("config:{HEAL_DISPATCHER_HEARTBEAT_KEY}"),
                    format!("{{\"value\":\"{ts}\"}}")
                ],
            )
            .expect("insert config heartbeat node");
        }
        let conn =
            Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open ro");
        let got = read_config_epoch_secs(&conn, HEAL_DISPATCHER_HEARTBEAT_KEY)
            .expect("read config epoch");
        assert_eq!(
            got,
            Some(ts),
            "heartbeat must resolve from graph_nodes config node"
        );
    }

    // ── system.disk-space ────────────────────────────────────────────────

    // Threshold logic at each tier: ample space is clean, then WARNING and
    // CRITICAL by BOTH the available-bytes axis and the used-percent axis.
    #[test]
    fn evaluate_disk_space_fires_by_tier() {
        let id = "system.disk-space";
        // Plenty of space, low utilisation → clean.
        assert!(evaluate_disk_space(id, 100 * 1024 * 1024, 40).is_empty());
        // Exactly at the 15 GiB warn boundary (not below it) at safe % → clean.
        assert!(evaluate_disk_space(id, DISK_AVAIL_WARN_KB, 40).is_empty());
        // Below 15 GiB but above 5 GiB → Warning.
        assert!(only(
            &[Severity::Warning],
            &evaluate_disk_space(id, 14 * 1024 * 1024, 40)
        ));
        // Below 5 GiB → Critical (regardless of a benign percent).
        assert!(only(
            &[Severity::Critical],
            &evaluate_disk_space(id, 4 * 1024 * 1024, 40)
        ));
    }

    // The used-percent axis fires independently of absolute free space.
    #[test]
    fn evaluate_disk_space_used_percent_axis() {
        let id = "system.disk-space";
        let ample = 100 * 1024 * 1024; // 100 GiB free
        // 89% is still clean, 90% trips Warning.
        assert!(evaluate_disk_space(id, ample, 89).is_empty());
        assert!(only(&[Severity::Warning], &evaluate_disk_space(id, ample, 90)));
        // 95% Warning, 96% escalates to Critical.
        assert!(only(&[Severity::Warning], &evaluate_disk_space(id, ample, 95)));
        assert!(only(&[Severity::Critical], &evaluate_disk_space(id, ample, 96)));
    }

    // The df parser handles a real macOS `df -k` report (header + data line),
    // returning (avail_kb, used_pct); junk degrades to None (→ soft note).
    #[test]
    fn parse_df_kb_reads_macos_report() {
        let sample = "Filesystem   1024-blocks      Used Available Capacity iused     ifree %iused  Mounted on\n\
                      /dev/disk3s5   482766932 368216752  80200648    83% 3917534 802006480    0%   /System/Volumes/Data";
        assert_eq!(parse_df_kb(sample), Some((80_200_648, 83)));
        // A 100%-full disk (the incident this guard exists for).
        let full = "Filesystem 1024-blocks Used Available Capacity iused ifree %iused Mounted on\n\
                    /dev/disk1s1 100 100 0 100% 1 1 50% /";
        assert_eq!(parse_df_kb(full), Some((0, 100)));
        // Garbage / truncated output → None, never a panic.
        assert_eq!(parse_df_kb(""), None);
        assert_eq!(parse_df_kb("just a header line\n"), None);
        assert_eq!(parse_df_kb("hdr\n/dev/x not numbers here"), None);
    }

    // Disk cleanup is always an operator decision: repair returns NeedsConfirm
    // in BOTH dry-run and apply and never deletes anything.
    #[test]
    fn disk_space_repair_always_needs_confirm() {
        let finding = evaluate_disk_space("system.disk-space", 1024 * 1024, 99)
            .into_iter()
            .next()
            .expect("critical finding");
        let (_dir, path) = fixture_db();
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        for apply in [false, true] {
            let outcome = SystemDiskSpace.repair(&ctx, &finding, apply).expect("repair");
            assert!(
                matches!(outcome, RepairOutcome::NeedsConfirm { .. }),
                "apply={apply} must NeedsConfirm, got {outcome:?}"
            );
        }
    }
}
