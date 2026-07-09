//! `phil doctor` — read-only self-diagnosis (slice 0).
//!
//! Named, versioned checks against the hotel's context DB (and, for a
//! handful of checks, `launchctl`/`pgrep`/filesystem probes) that reproduce
//! real philotic incidents in seconds instead of ad-hoc SQL + `launchctl
//! print`. Slice 0 is detection only — there is no `--fix` and no check may
//! write to the context DB (opened `SQLITE_OPEN_READ_ONLY`) or mutate any
//! system state. See `PHIL_DOCTOR_EXPLAIN_PROPOSAL.md` for the full design;
//! this module implements only the five slice-0 checks it names.

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

use crate::init::{active_profile, profile_dir};

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

/// A named, versioned diagnostic. Slice 0 is detect-only: no `repair()`.
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
}

// ── DoctorCtx ────────────────────────────────────────────────────────────

pub struct DoctorCtx {
    pub hotel: String,
    pub profile_dir: PathBuf,
    pub db_path: PathBuf,
    pub conn: Connection,
}

impl DoctorCtx {
    pub fn open(hotel: &str) -> Result<Self> {
        let db_path = match active_profile() {
            Some(_) => profile_dir().join("aiua_context.db"),
            None => PathBuf::from("aiua_context.db"),
        };
        Self::open_at(hotel, db_path)
    }

    /// Open against an explicit DB path (used by `open()` for the real
    /// profile DB, and directly by tests against a fixture DB).
    pub fn open_at(hotel: &str, db_path: PathBuf) -> Result<Self> {
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
            profile_dir: profile_dir(),
            db_path,
            conn,
        })
    }
}

// ── Catalog ──────────────────────────────────────────────────────────────

pub fn catalog() -> Vec<Box<dyn Check>> {
    vec![
        Box::new(PortsHotelRecordDrift),
        Box::new(ProcOrphanInstances),
        Box::new(IpcStaleSocket),
        Box::new(VaultKeySourceDivergence),
        Box::new(LogsRotationMissing),
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
        let launchd = launchd_pid(&ctx.hotel);
        let active = hotel_active_pid(&ctx.conn, &ctx.hotel)?;
        let running = running_aiua_pids(&ctx.hotel);
        Ok(evaluate_orphans(self.id(), launchd, active, &running))
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

fn count_decrypt_failures(key: &[u8], secrets: &[(String, String)]) -> usize {
    secrets
        .iter()
        .filter(|(ciphertext, nonce)| !try_decrypt(key, ciphertext, nonce))
        .count()
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

fn evaluate_vault_divergence(
    check_id: &'static str,
    sources: &[(&'static str, Option<Vec<u8>>)],
    secrets: &[(String, String)],
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
                 Keychain entry found) while {} vault_secrets row(s) exist",
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

    let failed = count_decrypt_failures(key, secrets);
    if failed == 0 {
        return Vec::new();
    }

    vec![Finding {
        check_id: check_id.to_string(),
        severity: Severity::Critical,
        message: format!(
            "{failed}/{} vault secrets do not decrypt under the effective key source ('{source_name}')",
            secrets.len()
        ),
        evidence: json!({
            "effective_source": source_name,
            "effective_fingerprint": fingerprint(key),
            "sources": fingerprints,
            "failed": failed,
            "total": secrets.len(),
        }),
        fix_hint: "residual divergence after the env->file->keychain precedence fix (b87b581) \
                   — re-encrypt the affected secrets via the RotateSecret IPC path under the \
                   now-effective key; doctor never touches key material"
            .to_string(),
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
        let secrets = vault_secrets_from_db(&ctx.conn)?;
        let sources = resolve_vault_sources();
        Ok(evaluate_vault_divergence(self.id(), &sources, &secrets))
    }
}

fn vault_secrets_from_db(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare("SELECT ciphertext_b64, nonce_b64 FROM vault_secrets")?;
    let secrets = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(secrets)
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
}

// ── CLI entry ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct DoctorReport {
    ok: bool,
    hotel: String,
    checks_run: usize,
    findings: Vec<Finding>,
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    hotel: String,
    json: bool,
    severity_min: String,
    only: Vec<String>,
    skip: Vec<String>,
    list_checks: bool,
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

    let ctx = match DoctorCtx::open(&hotel) {
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

    let mut all_findings = Vec::new();
    for check in &selected {
        match check.detect(&ctx) {
            Ok(findings) => all_findings.extend(findings),
            Err(e) => {
                eprintln!("phil doctor: check '{}' failed: {e:#}", check.id());
                std::process::exit(2);
            }
        }
    }

    let above_threshold = all_findings
        .iter()
        .filter(|f| f.severity >= threshold)
        .count();
    let ok = above_threshold == 0;

    if json {
        let report = DoctorReport {
            ok,
            hotel: hotel.clone(),
            checks_run: selected.len(),
            findings: all_findings,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(
            &hotel,
            &ctx.db_path,
            selected.len(),
            &selected_ids,
            &all_findings,
            threshold,
        );
    }

    std::process::exit(if ok { 0 } else { 1 });
}

fn print_human(
    hotel: &str,
    db_path: &std::path::Path,
    checks_run: usize,
    check_ids: &[&'static str],
    findings: &[Finding],
    threshold: Severity,
) {
    println!(
        "phil doctor — hotel {hotel} ({}), offline, {checks_run} checks\n",
        db_path.display()
    );
    for id in check_ids {
        let for_check: Vec<&Finding> = findings.iter().filter(|f| f.check_id == *id).collect();
        if for_check.is_empty() {
            println!("  \u{2713} {id}");
            continue;
        }
        for f in for_check {
            let marker = if f.severity >= threshold {
                "\u{2717}"
            } else {
                "!"
            };
            println!("  {marker} {:<28} [{}] {}", id, f.severity, f.message);
            if !f.fix_hint.is_empty() {
                println!("      fix: {}", f.fix_hint);
            }
        }
    }
    let above = findings.iter().filter(|f| f.severity >= threshold).count();
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
        assert_eq!(probe_socket(path.to_str().unwrap()), SocketProbe::Stale);
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
        let secrets = vec![(ct, nonce)];
        let findings = evaluate_vault_divergence("vault.key-source-divergence", &sources, &secrets);
        assert!(
            findings.is_empty(),
            "expected no findings, got {findings:?}"
        );
    }

    #[test]
    fn vault_evaluate_flags_divergence_when_effective_key_cannot_decrypt() {
        let real_key = [3u8; 32];
        let wrong_key = [4u8; 32];
        let (ct, nonce) = encrypt_for_test(&real_key, "hello");
        let sources = vec![
            ("env", Some(wrong_key.to_vec())),
            ("file", None),
            ("keychain", None),
        ];
        let secrets = vec![(ct, nonce)];
        let findings = evaluate_vault_divergence("vault.key-source-divergence", &sources, &secrets);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Critical);
        assert!(!findings[0].auto_repairable);
    }

    #[test]
    fn vault_evaluate_no_source_but_secrets_exist_is_warning() {
        let sources = vec![("env", None), ("file", None), ("keychain", None)];
        let secrets = vec![("ct".to_string(), "n".to_string())];
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
        // Read via the same read-only path detect() uses, but drive the
        // pure evaluator with fabricated sources — this deliberately avoids
        // calling resolve_vault_sources() (real env/file/Keychain), which
        // would make the test depend on this machine's actual vault state.
        let ctx = DoctorCtx::open_at("jane", path).expect("open ctx");
        let secrets = vault_secrets_from_db(&ctx.conn).expect("read secrets");
        assert_eq!(secrets, vec![(ct, nonce)]);

        let sources = vec![
            ("env", Some(key.to_vec())),
            ("file", None),
            ("keychain", None),
        ];
        let clean = evaluate_vault_divergence("vault.key-source-divergence", &sources, &secrets);
        assert!(clean.is_empty());

        let wrong_sources = vec![
            ("env", Some([9u8; 32].to_vec())),
            ("file", None),
            ("keychain", None),
        ];
        let divergent =
            evaluate_vault_divergence("vault.key-source-divergence", &wrong_sources, &secrets);
        assert_eq!(divergent.len(), 1);
        assert_eq!(divergent[0].severity, Severity::Critical);
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
            checks_run: 1,
            findings: vec![Finding {
                check_id: "logs.rotation-missing".to_string(),
                severity: Severity::Warning,
                message: "no rotation".to_string(),
                evidence: json!({"log_path": "/x"}),
                fix_hint: "install rotation".to_string(),
                auto_repairable: false,
            }],
        };
        let value = serde_json::to_value(&report).expect("serialize");
        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["hotel"], json!("jane"));
        assert_eq!(value["checks_run"], json!(1));
        assert_eq!(
            value["findings"][0]["check_id"],
            json!("logs.rotation-missing")
        );
        assert_eq!(value["findings"][0]["severity"], json!("warning"));
        assert_eq!(value["findings"][0]["auto_repairable"], json!(false));
    }
}
