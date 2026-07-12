//! Memory Hygiene sweep — Memory Transparency Slice M4 (`memory.hygiene`).
//!
//! A scheduled, **non-destructive** per-hotel Muninn sweep: for every active
//! agent vault, (a) list open contradiction pairs (`GET /api/contradictions`)
//! and (b) list old-but-still-active engrams as a staleness proxy
//! (`GET /api/engrams?sort=created&before=...`). Findings above a threshold
//! are filed as ONE aggregated `autonomy_audit` record on lane
//! [`ansible_mesh_core::autonomy::LANE_MEMORY_HYGIENE`] — annotation/flagging
//! only, never forget or consolidate-merge.
//!
//! # What this deliberately does NOT do (reality gap, noted honestly)
//!
//! MuninnDB's public REST surface does not expose a "never accessed"
//! signal: `EngramItem` has no `last_accessed` field, and `sort=accessed`
//! orders **most-recently-accessed first** with no accessible timestamp —
//! useful for ranking, useless for a client-side staleness cutoff. Staleness
//! here is therefore an **age proxy** (`created_at` older than the
//! configured threshold, still in an active lifecycle state — the server
//! already excludes soft-deleted/archived engrams by default), not a true
//! access-recency signal. A future slice that wants real access-based
//! staleness needs a MuninnDB REST addition, not a client-side workaround.
//!
//! Likewise `graph_memory_true_up` (the Memory Cultivation True-Up proposal's
//! reconciliation pass) is MCP-only on the graph-intelligence server and its
//! logic lives in the `philote` guest process — it is not callable in-process
//! from `aiua`, so it is out of scope here; see `AGENTS.md`/slice notes.
//!
//! # Two kinds of durable record — do not conflate them
//!
//! - **`autonomy_audit`** (via [`ansible_mesh_core::autonomy::AutonomyAuditRecord`]):
//!   written only when the sweep *files* — i.e. when [`HygieneReport::should_file`]
//!   crosses threshold. This is the budgeted, kill-switch-gated autonomous
//!   *action* ledger; a clean night must not consume the lane's daily budget
//!   for doing nothing.
//! - **last-run marker** (via [`record_sweep_run`] / [`get_last_sweep_run`],
//!   stored as a hotel-scoped config value — no grant, no budget): written on
//!   *every* sweep, filed or not. This is what satisfies "write an audit
//!   record per sweep run — what was scanned, what was filed" literally,
//!   without pretending an unfiled clean sweep was an autonomous action.
//!
//! # Scheduling
//!
//! Wired as a `CronJob` (see [`CRON_TARGET_ROLE`]) whose fire is intercepted
//! by `CronTicker::fire` before guest delivery — the sweep runs in-process in
//! the hotel daemon rather than materializing a guest. Registration is
//! opt-in per hotel via [`ENV_ENABLED`] (env kill switch convention, matching
//! the rest of the Autopoiesis lane machinery); the filing *action* is
//! additionally gated by the `memory.hygiene` `AutonomyGrant` (kill switch,
//! daily budget, ProposalOnly-is-the-action posture — mirrors Slice A3's
//! `fleet.heal_slices` heal-work-item filing).
//!
//! **Mesh note:** `CronJobSync` replicates a hotel's `CronJob` *definitions*
//! to every mesh-connected peer unconditionally (see `handle_cron_job_sync`
//! in `aiua::main`) — job registration is not itself a per-hotel opt-in once
//! a mesh is involved. `CronTicker` re-checks each hotel's own
//! `PHILOTIC_MEMORY_HYGIENE_ENABLED` at fire time
//! (`MemoryHygieneCronContext::enabled_locally`) so one hotel opting in never
//! silently sweeps its peers.

use ansible_mesh_core::autonomy::{
    AutonomyAuditRecord, AutonomyLane, LANE_MEMORY_HYGIENE, lane_enabled, try_consume_daily_action,
};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::provenance::{ProvenanceEnvelope, TrustTier};
use memory_core::MuninnConfig;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Config-value key prefix for the per-hotel last-sweep-run marker (see
/// module docs, "Two kinds of durable record").
const CONFIG_KEY_LAST_RUN_PREFIX: &str = "memory_hygiene:last_run:";

/// Reserved `CronJob::target_role` recognized by `CronTicker::fire` as an
/// internal sweep rather than a guest-inbox delivery. Not a real role
/// namespace (`role:{agent}:{role}`) — the `internal:` prefix keeps it out
/// of `resolve_target_role_record`'s parsing.
pub const CRON_TARGET_ROLE: &str = "internal:memory_hygiene";

/// Env var gating whether the hotel registers/keeps the nightly sweep cron
/// job at all. Operator opt-in per hotel — disabled unless explicitly set to
/// a truthy value. Distinct from the AutonomyGrant kill switch
/// (`PHILOTIC_AUTONOMY_DISABLE_MEMORY_HYGIENE`), which gates the *filing*
/// action even once the sweep is scheduled.
pub const ENV_ENABLED: &str = "PHILOTIC_MEMORY_HYGIENE_ENABLED";
/// Env override for the nightly cron schedule (7-field `cron` crate syntax).
pub const ENV_SCHEDULE: &str = "PHILOTIC_MEMORY_HYGIENE_SCHEDULE";
/// Default: nightly at 03:00 UTC.
pub const DEFAULT_SCHEDULE: &str = "0 0 3 * * * *";
/// Env override for the staleness age cutoff, in days.
pub const ENV_STALE_DAYS: &str = "PHILOTIC_MEMORY_HYGIENE_STALE_DAYS";
pub const DEFAULT_STALE_DAYS: i64 = 30;
/// Env override for the minimum contradiction-pair count that triggers filing.
pub const ENV_CONTRADICTION_THRESHOLD: &str = "PHILOTIC_MEMORY_HYGIENE_CONTRADICTION_THRESHOLD";
pub const DEFAULT_CONTRADICTION_THRESHOLD: usize = 1;
/// Env override for the minimum stale-engram count (per vault) that triggers filing.
pub const ENV_STALE_THRESHOLD: &str = "PHILOTIC_MEMORY_HYGIENE_STALE_THRESHOLD";
pub const DEFAULT_STALE_THRESHOLD: usize = 5;
/// Cap on stale engrams fetched per vault per sweep (bounds the REST call and
/// the evidence blob).
const STALE_FETCH_LIMIT: u32 = 50;

/// Deterministic id for the auto-registered per-hotel cron job — stable
/// across restarts so `ensure_scheduled` is idempotent and never double-registers.
pub fn cron_job_id(hotel_name: &str) -> String {
    format!("memory-hygiene:{hotel_name}")
}

/// True when the operator has opted this hotel into the nightly sweep.
pub fn sweep_enabled(env: impl Fn(&str) -> Option<String>) -> bool {
    match env(ENV_ENABLED) {
        None => false,
        Some(v) => {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes"
        }
    }
}

// ── Thresholds (pure, unit-testable) ───────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HygieneThresholds {
    pub stale_days: i64,
    pub contradiction_threshold: usize,
    pub stale_threshold: usize,
}

impl Default for HygieneThresholds {
    fn default() -> Self {
        Self {
            stale_days: DEFAULT_STALE_DAYS,
            contradiction_threshold: DEFAULT_CONTRADICTION_THRESHOLD,
            stale_threshold: DEFAULT_STALE_THRESHOLD,
        }
    }
}

impl HygieneThresholds {
    pub fn from_env(env: impl Fn(&str) -> Option<String>) -> Self {
        let stale_days = env(ENV_STALE_DAYS)
            .and_then(|v| v.trim().parse::<i64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_STALE_DAYS);
        let contradiction_threshold = env(ENV_CONTRADICTION_THRESHOLD)
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_CONTRADICTION_THRESHOLD);
        let stale_threshold = env(ENV_STALE_THRESHOLD)
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_STALE_THRESHOLD);
        Self {
            stale_days,
            contradiction_threshold,
            stale_threshold,
        }
    }
}

// ── Findings ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct ContradictionFinding {
    pub vault: String,
    pub id_a: String,
    pub concept_a: String,
    pub id_b: String,
    pub concept_b: String,
    pub detected_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StaleFinding {
    pub vault: String,
    pub id: String,
    pub concept: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct VaultSweepResult {
    pub vault: String,
    pub contradictions: Vec<ContradictionFinding>,
    pub stale: Vec<StaleFinding>,
    /// `Some(message)` when a REST call for this vault failed. Sweep
    /// continues to the next vault — one unreachable vault must not abort
    /// the hotel-wide report.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct HygieneReport {
    pub hotel_name: String,
    pub vaults: Vec<VaultSweepResult>,
}

impl HygieneReport {
    pub fn total_contradictions(&self) -> usize {
        self.vaults.iter().map(|v| v.contradictions.len()).sum()
    }

    pub fn total_stale(&self) -> usize {
        self.vaults.iter().map(|v| v.stale.len()).sum()
    }

    pub fn vaults_scanned(&self) -> usize {
        self.vaults.len()
    }

    /// Judgment-worthy findings exist and should be filed as one aggregated
    /// annotation. Pure — no I/O, easy to unit test against fixture reports.
    pub fn should_file(&self, thresholds: &HygieneThresholds) -> bool {
        self.total_contradictions() >= thresholds.contradiction_threshold
            || self
                .vaults
                .iter()
                .any(|v| v.stale.len() >= thresholds.stale_threshold)
    }

    /// Bounded human-readable evidence blob for the audit record. Bounding
    /// itself happens in `AutonomyAuditRecord::new` (`bound_evidence`) — this
    /// just orders findings so truncation drops the least-interesting tail
    /// (per-vault stale counts) before the contradiction pairs.
    pub fn evidence_summary(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!(
            "memory.hygiene sweep — hotel={} vaults_scanned={} contradictions={} stale={}",
            self.hotel_name,
            self.vaults_scanned(),
            self.total_contradictions(),
            self.total_stale()
        ));
        for v in &self.vaults {
            if let Some(err) = &v.error {
                lines.push(format!("  vault={} ERROR: {err}", v.vault));
                continue;
            }
            if v.contradictions.is_empty() && v.stale.is_empty() {
                continue;
            }
            lines.push(format!(
                "  vault={} contradictions={} stale={}",
                v.vault,
                v.contradictions.len(),
                v.stale.len()
            ));
            for c in &v.contradictions {
                lines.push(format!(
                    "    contradiction: '{}' ({}) <-> '{}' ({})",
                    c.concept_a, c.id_a, c.concept_b, c.id_b
                ));
            }
            for s in v.stale.iter().take(10) {
                lines.push(format!(
                    "    stale: '{}' ({}) created_at={}",
                    s.concept, s.id, s.created_at
                ));
            }
            if v.stale.len() > 10 {
                lines.push(format!("    ... and {} more stale", v.stale.len() - 10));
            }
        }
        lines.join("\n")
    }

    pub fn action_summary(&self) -> String {
        format!(
            "memory.hygiene sweep on {}: {} vault(s) scanned, {} contradiction pair(s), \
             {} stale/aging memor{} flagged for review",
            self.hotel_name,
            self.vaults_scanned(),
            self.total_contradictions(),
            self.total_stale(),
            if self.total_stale() == 1 { "y" } else { "ies" }
        )
    }

    /// Evidence pointer strings for a [`ansible_mesh_core::provenance::ProvenanceEnvelope`]:
    /// one `engram:<vault>:<id>` per contradiction/stale finding, bounded the
    /// same way `evidence_summary` is (list order — contradictions first).
    /// Distinct from `evidence_summary`, which is the bounded human-readable
    /// prose blob stored on `AutonomyAuditRecord::evidence`.
    pub fn evidence_pointers(&self) -> Vec<String> {
        let mut pointers = Vec::new();
        for v in &self.vaults {
            for c in &v.contradictions {
                pointers.push(format!("engram:{}:{}", v.vault, c.id_a));
                pointers.push(format!("engram:{}:{}", v.vault, c.id_b));
            }
            for s in &v.stale {
                pointers.push(format!("engram:{}:{}", v.vault, s.id));
            }
        }
        pointers
    }
}

// ── REST wire shapes ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ContradictionsResponse {
    contradictions: Vec<ContradictionItem>,
}

#[derive(Debug, Deserialize)]
struct ContradictionItem {
    id_a: String,
    concept_a: String,
    id_b: String,
    concept_b: String,
    detected_at: i64,
}

#[derive(Debug, Deserialize)]
struct ListEngramsResponse {
    engrams: Vec<EngramItem>,
}

#[derive(Debug, Deserialize)]
struct EngramItem {
    id: String,
    concept: String,
    created_at: i64,
}

async fn fetch_contradictions(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
) -> anyhow::Result<Vec<ContradictionItem>> {
    let url = format!("{}/api/contradictions", base_url.trim_end_matches('/'));
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("contradictions returned {status}: {body}");
    }
    Ok(resp.json::<ContradictionsResponse>().await?.contradictions)
}

async fn fetch_stale_candidates(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    before_rfc3339: &str,
    limit: u32,
) -> anyhow::Result<Vec<EngramItem>> {
    let url = format!(
        "{}/api/engrams?sort=created&before={}&limit={}",
        base_url.trim_end_matches('/'),
        urlencoding_light(before_rfc3339),
        limit,
    );
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("list engrams returned {status}: {body}");
    }
    Ok(resp.json::<ListEngramsResponse>().await?.engrams)
}

/// Minimal RFC3339 query-param escaping — the only characters an RFC3339
/// timestamp contains that need encoding in a query string are `:` and `+`.
/// Avoids pulling in a full percent-encoding dependency for one call site.
fn urlencoding_light(s: &str) -> String {
    s.replace(':', "%3A").replace('+', "%2B")
}

// ── Sweep ──────────────────────────────────────────────────────────────────────

/// Sweep every active agent vault in `hotel_name` for contradictions and
/// aging-but-active memories. Read-only — no Muninn write calls. Per-vault
/// failures are captured on the `VaultSweepResult` and do not abort the rest
/// of the sweep.
pub async fn sweep(
    client: &reqwest::Client,
    config: &MuninnConfig,
    graph: &GraphDomain,
    hotel_name: &str,
    thresholds: &HygieneThresholds,
    now: chrono::DateTime<chrono::Utc>,
) -> HygieneReport {
    let vault_names = crate::dream::collect_agent_vault_names(graph, hotel_name);
    let before = (now - chrono::Duration::days(thresholds.stale_days)).to_rfc3339();

    let mut report = HygieneReport {
        hotel_name: hotel_name.to_string(),
        vaults: Vec::with_capacity(vault_names.len()),
    };

    for vault_name in &vault_names {
        let token = match config
            .vault_tokens
            .get(vault_name)
            .or(config.default_token.as_ref())
        {
            Some(t) => t.clone(),
            None => {
                debug!(vault = %vault_name, "memory.hygiene: no token — skipping");
                continue;
            }
        };

        let mut result = VaultSweepResult {
            vault: vault_name.clone(),
            ..Default::default()
        };

        match fetch_contradictions(client, &config.base_url, &token).await {
            Ok(items) => {
                result.contradictions = items
                    .into_iter()
                    .map(|c| ContradictionFinding {
                        vault: vault_name.clone(),
                        id_a: c.id_a,
                        concept_a: c.concept_a,
                        id_b: c.id_b,
                        concept_b: c.concept_b,
                        detected_at: c.detected_at,
                    })
                    .collect();
            }
            Err(e) => {
                warn!(vault = %vault_name, error = %e, "memory.hygiene: contradictions fetch failed");
                result.error = Some(format!("contradictions: {e}"));
            }
        }

        match fetch_stale_candidates(client, &config.base_url, &token, &before, STALE_FETCH_LIMIT)
            .await
        {
            Ok(items) => {
                result.stale = items
                    .into_iter()
                    .map(|e| StaleFinding {
                        vault: vault_name.clone(),
                        id: e.id,
                        concept: e.concept,
                        created_at: e.created_at,
                    })
                    .collect();
            }
            Err(e) => {
                warn!(vault = %vault_name, error = %e, "memory.hygiene: stale-candidate fetch failed");
                let existing = result.error.take();
                result.error = Some(match existing {
                    Some(prev) => format!("{prev}; stale: {e}"),
                    None => format!("stale: {e}"),
                });
            }
        }

        report.vaults.push(result);
    }

    report
}

// ── Per-run marker (every sweep, filed or not) ─────────────────────────────────

/// What a sweep run scanned and (if anything) filed. Persisted per hotel via
/// the generic hotel-scoped config-value store — deliberately *not* an
/// `autonomy_audit` record; see the module doc's "Two kinds of durable
/// record" note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastRunRecord {
    pub hotel_name: String,
    pub scanned_at: u64,
    pub vaults_scanned: usize,
    pub contradictions: usize,
    pub stale: usize,
    pub filed: bool,
    pub audit_id: Option<String>,
}

/// Record that a sweep ran, regardless of whether it filed anything.
/// Overwrites the previous marker for this hotel — this is a "most recent
/// run" pointer, not a history; the append-only trail for filed findings
/// lives in `autonomy_audit` records (`list_autonomy_audits_by_lane`).
pub fn record_sweep_run(
    graph: &GraphDomain,
    report: &HygieneReport,
    now: u64,
    filed_audit_id: Option<&str>,
) -> anyhow::Result<()> {
    let record = LastRunRecord {
        hotel_name: report.hotel_name.clone(),
        scanned_at: now,
        vaults_scanned: report.vaults_scanned(),
        contradictions: report.total_contradictions(),
        stale: report.total_stale(),
        filed: filed_audit_id.is_some(),
        audit_id: filed_audit_id.map(str::to_string),
    };
    let key = format!("{CONFIG_KEY_LAST_RUN_PREFIX}{}", report.hotel_name);
    graph.set_config_value(&key, &serde_json::to_string(&record)?)
}

/// Read back the most recent sweep-run marker for `hotel_name`, if any sweep
/// has run yet.
///
/// Not yet called from production code — reserved for a future operator
/// status surface (dashboard/`phil` CLI) or the M3 memory-delta digest.
/// Exercised directly by tests today.
#[allow(dead_code)]
pub fn get_last_sweep_run(
    graph: &GraphDomain,
    hotel_name: &str,
) -> anyhow::Result<Option<LastRunRecord>> {
    let key = format!("{CONFIG_KEY_LAST_RUN_PREFIX}{hotel_name}");
    graph
        .get_config_value(&key)?
        .map(|raw| serde_json::from_str(&raw).map_err(anyhow::Error::from))
        .transpose()
}

// ── Filing (annotation only — never forget/consolidate) ────────────────────────

/// File one aggregated `autonomy_audit` record on the `memory.hygiene` lane
/// when the sweep's findings clear the configured thresholds.
///
/// Mirrors Slice A3's `handle_file_heal_work_item` pipeline: kill switch →
/// daily budget → audit record. Unlike A3 there is no separate durable
/// "work item" table — the audit record itself is both the filing and the
/// per-run log entry (requirement: "write an audit record per sweep run,
/// what was scanned, what was filed"). Only ever annotates; never calls
/// `forget` or `consolidate`.
///
/// Returns `Some(audit_id)` when a record was written, `None` when refused
/// (kill switch, frozen lane, exhausted budget) or when nothing crossed the
/// filing threshold.
pub fn file_if_warranted(
    graph: &GraphDomain,
    report: &HygieneReport,
    thresholds: &HygieneThresholds,
    now: u64,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let lane = AutonomyLane::new(LANE_MEMORY_HYGIENE);

    if !report.should_file(thresholds) {
        debug!(
            hotel = %report.hotel_name,
            "memory.hygiene: sweep clean — nothing crosses the filing threshold"
        );
        return None;
    }

    // Kill switch overrides everything, always (Autonomy Contract rule 3).
    if !lane_enabled(&lane, env) {
        debug!("memory.hygiene: filing skipped — lane kill switch set");
        return None;
    }

    let mut grant = match graph.get_or_create_autonomy_grant(LANE_MEMORY_HYGIENE, now) {
        Ok(g) => g,
        Err(e) => {
            warn!("memory.hygiene: failed to load autonomy grant: {e:#}");
            return None;
        }
    };
    if !try_consume_daily_action(&mut grant, now) {
        let reason = if grant.frozen_until_operator_review {
            "lane_frozen"
        } else {
            "daily_budget_exhausted"
        };
        debug!(reason, "memory.hygiene: filing refused by autonomy grant");
        return None;
    }
    if let Err(e) = graph.upsert_autonomy_grant(&grant) {
        warn!("memory.hygiene: failed to persist autonomy grant: {e:#}");
        return None;
    }

    let audit_id = format!("memory_hygiene:{}:{now}", report.hotel_name);
    // Memory Transparency Slice M1: component-authored provenance for the
    // M4 hygiene filing — evidence pointers are per-engram (contradiction
    // pairs + stale findings), directly observed by the sweep itself.
    let provenance = ProvenanceEnvelope::from_component("memory-hygiene-sweep")
        .with_source(format!("memory_hygiene:sweep:{}:{now}", report.hotel_name))
        .with_trust(TrustTier::Observed)
        .with_evidence(report.evidence_pointers())
        .with_reversal(
            "review flagged engrams via muninn_contradictions / muninn_consolidate; \
             annotation-only, no automatic forget or merge",
        );
    let audit = AutonomyAuditRecord::new(
        audit_id.clone(),
        lane,
        report.action_summary(),
        &report.evidence_summary(),
        "review flagged engrams via muninn_contradictions / muninn_consolidate; \
         no automatic forget or merge has occurred — this record is annotation-only",
        grant.posture,
        now,
    )
    .with_provenance(provenance);
    if let Err(e) = graph.record_autonomy_audit(&audit) {
        warn!("memory.hygiene: failed to record audit: {e:#}");
        return None;
    }

    info!(
        hotel = %report.hotel_name,
        audit_id = %audit_id,
        contradictions = report.total_contradictions(),
        stale = report.total_stale(),
        "memory.hygiene: findings filed"
    );
    Some(audit_id)
}

/// Best-effort mirror of a fresh filing into the intel graph via
/// `POST /api/decide` — same reviewable-breadcrumb pattern as Slice A3's
/// `push_intel_graph_record` (there is no proposal-create REST route). Never
/// blocks or fails the filing; the hotel-graph audit record above is the
/// durable one.
pub async fn push_intel_graph_record(
    http: &reqwest::Client,
    base_url: &str,
    report: &HygieneReport,
    audit_id: &str,
) {
    // Memory Transparency Slice M1: mirror the same provenance attached to
    // the hotel-graph `autonomy_audit` record (see `file_if_warranted`
    // above) onto the intel-graph decision record — evidence pointers are
    // the same per-engram findings, reversal is the same review path.
    let body = serde_json::json!({
        "target_node": "doc:MEMORY_TRANSPARENCY_PROPOSAL",
        "action": "memory_hygiene_finding_filed",
        "to_value": audit_id,
        "reason": format!(
            "{} — filed as hotel-graph autonomy_audit {audit_id}",
            report.action_summary()
        ),
        "agent": "memory-hygiene-sweep",
        "evidence": report.evidence_pointers(),
        "reversal": "review flagged engrams via muninn_contradictions / muninn_consolidate; \
             annotation-only, no automatic forget or merge",
        "trust": "observed",
    });
    let result = http
        .post(format!("{}/api/decide", base_url.trim_end_matches('/')))
        .timeout(std::time::Duration::from_secs(5))
        .json(&body)
        .send()
        .await;
    match result {
        Ok(resp) if resp.status().is_success() => {
            info!(audit_id, "memory.hygiene: finding mirrored to intel graph");
        }
        Ok(resp) => {
            debug!(
                audit_id,
                status = %resp.status(),
                "memory.hygiene: intel graph mirror rejected (best-effort, ignoring)"
            );
        }
        Err(e) => {
            debug!(
                audit_id,
                "memory.hygiene: intel graph unreachable, skipping mirror (best-effort): {e}"
            );
        }
    }
}

// ── Scheduled entry point (called from CronTicker::fire) ──────────────────────

/// Run one scheduled sweep for `hotel_name`. No-op (logged) when Muninn is
/// not configured on this hotel. Never panics or propagates — cron fires are
/// fire-and-forget from the ticker's perspective.
pub async fn run_scheduled_sweep(
    graph: &GraphDomain,
    muninn_config: Option<&MuninnConfig>,
    hotel_name: &str,
    intel_graph_url: Option<&str>,
    now_secs: u64,
) {
    let Some(config) = muninn_config else {
        debug!("memory.hygiene: sweep fired but MuninnDB is not configured — skipping");
        return;
    };

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("memory.hygiene: failed to build HTTP client — {e}");
            return;
        }
    };

    let thresholds = HygieneThresholds::from_env(|k| std::env::var(k).ok());
    let now_dt =
        chrono::DateTime::from_timestamp(now_secs as i64, 0).unwrap_or_else(chrono::Utc::now);

    let report = sweep(&client, config, graph, hotel_name, &thresholds, now_dt).await;
    info!(
        hotel = %hotel_name,
        vaults = report.vaults_scanned(),
        contradictions = report.total_contradictions(),
        stale = report.total_stale(),
        "memory.hygiene: sweep complete"
    );

    let env = |k: &str| std::env::var(k).ok();
    let audit_id = file_if_warranted(graph, &report, &thresholds, now_secs, &env);
    if let Some(audit_id) = &audit_id {
        if let Some(url) = intel_graph_url {
            push_intel_graph_record(&client, url, &report, audit_id).await;
        }
    }

    // Every run — filed or clean — gets a last-run marker (see module doc's
    // "Two kinds of durable record"). Best-effort: a storage failure here
    // must not undo a filing that already happened above.
    if let Err(e) = record_sweep_run(graph, &report, now_secs, audit_id.as_deref()) {
        warn!(hotel = %hotel_name, "memory.hygiene: failed to record last-run marker: {e:#}");
    }
}

// ── Cron registration (idempotent, operator opt-in) ─────────────────────────────

/// Ensure the nightly `memory.hygiene` cron job is registered when the
/// operator has opted this hotel in via [`ENV_ENABLED`]. Idempotent: does
/// nothing if a job with the deterministic id already exists (so an operator
/// who hand-edits the schedule via `RegisterCronJob` is never clobbered on
/// restart). Does not disable an existing job when the env flag is unset —
/// removal is an explicit operator action, not an implicit side effect of a
/// missing env var on one boot.
pub fn ensure_scheduled(
    graph: &GraphDomain,
    hotel_name: &str,
    now_ms: u64,
    env: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<()> {
    if !sweep_enabled(&env) {
        debug!(
            hotel = %hotel_name,
            "memory.hygiene: not enabled for this hotel (PHILOTIC_MEMORY_HYGIENE_ENABLED unset)"
        );
        return Ok(());
    }

    let job_id = cron_job_id(hotel_name);
    if graph.get_cron_job(&job_id)?.is_some() {
        debug!(hotel = %hotel_name, "memory.hygiene: cron job already registered");
        return Ok(());
    }

    let schedule = env(ENV_SCHEDULE)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_SCHEDULE.to_string());
    let next_fire_at = ansible_mesh_core::cron::next_fire_after(&schedule, now_ms)?;

    let job = ansible_mesh_core::cron::CronJob {
        id: job_id.clone(),
        schedule,
        target_role: CRON_TARGET_ROLE.to_string(),
        target_node_id: None,
        payload: "{}".to_string(),
        guaranteed: false,
        enabled: true,
        last_fired_epoch: None,
        next_fire_at,
        created_at: now_ms,
        created_by: ansible_mesh_core::cron::CronJobSource::Operator,
        silent_ok: true,
        session_target: ansible_mesh_core::cron::CronSessionTarget::Isolated,
    };
    graph.upsert_cron_job(&job)?;
    info!(hotel = %hotel_name, job_id = %job_id, next_fire_at, "memory.hygiene: nightly sweep cron job registered");
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn contradiction(vault: &str, a: &str, b: &str) -> ContradictionFinding {
        ContradictionFinding {
            vault: vault.to_string(),
            id_a: format!("{a}-id"),
            concept_a: a.to_string(),
            id_b: format!("{b}-id"),
            concept_b: b.to_string(),
            detected_at: 1_750_000_000,
        }
    }

    fn stale(vault: &str, id: &str) -> StaleFinding {
        StaleFinding {
            vault: vault.to_string(),
            id: id.to_string(),
            concept: format!("concept-{id}"),
            created_at: 1_700_000_000,
        }
    }

    #[test]
    fn sweep_enabled_requires_explicit_truthy_value() {
        assert!(!sweep_enabled(|_| None));
        assert!(!sweep_enabled(
            |k| (k == ENV_ENABLED).then(|| "0".to_string())
        ));
        assert!(!sweep_enabled(
            |k| (k == ENV_ENABLED).then(|| "false".to_string())
        ));
        assert!(sweep_enabled(
            |k| (k == ENV_ENABLED).then(|| "1".to_string())
        ));
        assert!(sweep_enabled(
            |k| (k == ENV_ENABLED).then(|| "true".to_string())
        ));
        assert!(sweep_enabled(
            |k| (k == ENV_ENABLED).then(|| "YES".to_string())
        ));
    }

    #[test]
    fn thresholds_from_env_fall_back_on_garbage_or_zero() {
        let t = HygieneThresholds::from_env(|k| match k {
            ENV_STALE_DAYS => Some("14".to_string()),
            ENV_CONTRADICTION_THRESHOLD => Some("0".to_string()), // rejected, falls back
            ENV_STALE_THRESHOLD => Some("not-a-number".to_string()),
            _ => None,
        });
        assert_eq!(t.stale_days, 14);
        assert_eq!(t.contradiction_threshold, DEFAULT_CONTRADICTION_THRESHOLD);
        assert_eq!(t.stale_threshold, DEFAULT_STALE_THRESHOLD);
    }

    #[test]
    fn clean_sweep_does_not_cross_filing_threshold() {
        let report = HygieneReport {
            hotel_name: "test-hotel".to_string(),
            vaults: vec![VaultSweepResult {
                vault: "self_agent-1".to_string(),
                ..Default::default()
            }],
        };
        assert!(!report.should_file(&HygieneThresholds::default()));
    }

    #[test]
    fn single_contradiction_crosses_default_threshold() {
        let report = HygieneReport {
            hotel_name: "test-hotel".to_string(),
            vaults: vec![VaultSweepResult {
                vault: "self_agent-1".to_string(),
                contradictions: vec![contradiction("self_agent-1", "a", "b")],
                ..Default::default()
            }],
        };
        assert!(report.should_file(&HygieneThresholds::default()));
    }

    #[test]
    fn stale_cluster_below_threshold_does_not_file() {
        let report = HygieneReport {
            hotel_name: "test-hotel".to_string(),
            vaults: vec![VaultSweepResult {
                vault: "self_agent-1".to_string(),
                stale: vec![stale("self_agent-1", "1"), stale("self_agent-1", "2")],
                ..Default::default()
            }],
        };
        let thresholds = HygieneThresholds {
            stale_threshold: 5,
            ..HygieneThresholds::default()
        };
        assert!(!report.should_file(&thresholds));
    }

    #[test]
    fn stale_cluster_at_threshold_files() {
        let stale_items: Vec<_> = (0..5)
            .map(|i| stale("self_agent-1", &i.to_string()))
            .collect();
        let report = HygieneReport {
            hotel_name: "test-hotel".to_string(),
            vaults: vec![VaultSweepResult {
                vault: "self_agent-1".to_string(),
                stale: stale_items,
                ..Default::default()
            }],
        };
        let thresholds = HygieneThresholds {
            stale_threshold: 5,
            ..HygieneThresholds::default()
        };
        assert!(report.should_file(&thresholds));
    }

    #[test]
    fn evidence_summary_includes_vault_and_finding_detail() {
        let report = HygieneReport {
            hotel_name: "mac-jane".to_string(),
            vaults: vec![VaultSweepResult {
                vault: "self_bjork".to_string(),
                contradictions: vec![contradiction("self_bjork", "prefers-x", "prefers-not-x")],
                stale: vec![stale("self_bjork", "abc123")],
                error: None,
            }],
        };
        let summary = report.evidence_summary();
        assert!(summary.contains("mac-jane"));
        assert!(summary.contains("self_bjork"));
        assert!(summary.contains("prefers-x"));
        assert!(summary.contains("prefers-not-x"));
        assert!(summary.contains("abc123"));
    }

    #[test]
    fn evidence_summary_surfaces_per_vault_errors() {
        let report = HygieneReport {
            hotel_name: "vps-jane".to_string(),
            vaults: vec![VaultSweepResult {
                vault: "self_beacon".to_string(),
                error: Some("contradictions: connection refused".to_string()),
                ..Default::default()
            }],
        };
        assert!(report.evidence_summary().contains("connection refused"));
    }

    #[test]
    fn action_summary_pluralizes_stale_count() {
        let mut report = HygieneReport {
            hotel_name: "h".to_string(),
            vaults: vec![VaultSweepResult {
                vault: "v".to_string(),
                stale: vec![stale("v", "1")],
                ..Default::default()
            }],
        };
        assert!(report.action_summary().contains("1 stale/aging memory"));
        report.vaults[0].stale.push(stale("v", "2"));
        assert!(report.action_summary().contains("2 stale/aging memories"));
    }

    #[test]
    fn urlencoding_light_escapes_colon_and_plus() {
        assert_eq!(
            urlencoding_light("2026-07-11T00:00:00+00:00"),
            "2026-07-11T00%3A00%3A00%2B00%3A00"
        );
    }

    // ── file_if_warranted ────────────────────────────────────────────────────

    fn open_domain() -> GraphDomain {
        let storage =
            ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open(":memory:").expect("open");
        GraphDomain::new(std::sync::Arc::new(storage.adapter()))
    }

    const T0: u64 = 1_750_000_000;
    const NO_ENV: &dyn Fn(&str) -> Option<String> = &|_| None;

    fn dirty_report() -> HygieneReport {
        HygieneReport {
            hotel_name: "test-hotel".to_string(),
            vaults: vec![VaultSweepResult {
                vault: "self_agent-1".to_string(),
                contradictions: vec![contradiction("self_agent-1", "a", "b")],
                ..Default::default()
            }],
        }
    }

    #[test]
    fn clean_report_never_touches_the_grant() {
        let graph = open_domain();
        let clean = HygieneReport {
            hotel_name: "test-hotel".to_string(),
            vaults: vec![VaultSweepResult {
                vault: "self_agent-1".to_string(),
                ..Default::default()
            }],
        };
        let id = file_if_warranted(&graph, &clean, &HygieneThresholds::default(), T0, NO_ENV);
        assert!(id.is_none());
        assert!(
            graph
                .get_autonomy_grant(LANE_MEMORY_HYGIENE)
                .expect("lookup")
                .is_none(),
            "a clean sweep must not even create a grant"
        );
    }

    #[test]
    fn warranted_report_files_one_audit_record() {
        let graph = open_domain();
        let report = dirty_report();
        let audit_id =
            file_if_warranted(&graph, &report, &HygieneThresholds::default(), T0, NO_ENV)
                .expect("should file");

        let audit = graph
            .get_autonomy_audit(&audit_id)
            .expect("lookup")
            .expect("audit exists");
        assert_eq!(audit.lane.as_str(), LANE_MEMORY_HYGIENE);
        assert!(audit.evidence.contains("self_agent-1"));
        assert!(!audit.reversal_hint.is_empty());

        // Memory Transparency Slice M1: the audit record's `provenance`
        // field is populated (not just present-but-None) — this is the
        // proof-of-adoption for the M4 hygiene filing write path.
        let provenance = audit
            .provenance
            .expect("M4 filing must attach a provenance envelope");
        assert_eq!(provenance.author, "memory-hygiene-sweep");
        assert_eq!(provenance.trust, ansible_mesh_core::provenance::TrustTier::Observed);
        assert!(!provenance.evidence.is_empty());
        assert!(provenance.reversal.is_some());

        let grant = graph
            .get_autonomy_grant(LANE_MEMORY_HYGIENE)
            .expect("lookup")
            .expect("grant exists");
        assert_eq!(grant.actions_today, 1);
    }

    #[test]
    fn kill_switch_refuses_filing() {
        let graph = open_domain();
        let report = dirty_report();
        let env =
            |k: &str| (k == "PHILOTIC_AUTONOMY_DISABLE_MEMORY_HYGIENE").then(|| "1".to_string());
        let id = file_if_warranted(&graph, &report, &HygieneThresholds::default(), T0, &env);
        assert!(id.is_none());
        assert!(
            graph
                .get_autonomy_grant(LANE_MEMORY_HYGIENE)
                .expect("lookup")
                .is_none()
        );
    }

    #[test]
    fn frozen_grant_refuses_filing() {
        let graph = open_domain();
        let mut grant = graph
            .get_or_create_autonomy_grant(LANE_MEMORY_HYGIENE, T0)
            .expect("grant");
        grant.frozen_until_operator_review = true;
        graph.upsert_autonomy_grant(&grant).expect("upsert");

        let report = dirty_report();
        let id = file_if_warranted(
            &graph,
            &report,
            &HygieneThresholds::default(),
            T0 + 1,
            NO_ENV,
        );
        assert!(id.is_none());
    }

    #[test]
    fn exhausted_daily_budget_refuses_filing() {
        let graph = open_domain();
        let mut grant = graph
            .get_or_create_autonomy_grant(LANE_MEMORY_HYGIENE, T0)
            .expect("grant");
        grant.budget.max_actions_per_day = 0;
        graph.upsert_autonomy_grant(&grant).expect("upsert");

        let report = dirty_report();
        let id = file_if_warranted(
            &graph,
            &report,
            &HygieneThresholds::default(),
            T0 + 1,
            NO_ENV,
        );
        assert!(id.is_none());
    }

    // ── record_sweep_run / get_last_sweep_run ───────────────────────────────

    #[test]
    fn clean_run_still_gets_a_last_run_marker() {
        let graph = open_domain();
        let clean = HygieneReport {
            hotel_name: "test-hotel".to_string(),
            vaults: vec![VaultSweepResult {
                vault: "self_agent-1".to_string(),
                ..Default::default()
            }],
        };
        // Deliberately does NOT cross the filing threshold — clean sweeps
        // must not write an autonomy_audit record...
        assert!(
            file_if_warranted(&graph, &clean, &HygieneThresholds::default(), T0, NO_ENV).is_none()
        );
        // ...but the run itself is still durably recorded.
        record_sweep_run(&graph, &clean, T0, None).expect("record");
        let marker = get_last_sweep_run(&graph, "test-hotel")
            .expect("lookup")
            .expect("marker exists");
        assert_eq!(marker.vaults_scanned, 1);
        assert_eq!(marker.contradictions, 0);
        assert!(!marker.filed);
        assert!(marker.audit_id.is_none());
    }

    #[test]
    fn filed_run_marker_carries_the_audit_id() {
        let graph = open_domain();
        let report = dirty_report();
        let audit_id =
            file_if_warranted(&graph, &report, &HygieneThresholds::default(), T0, NO_ENV)
                .expect("should file");
        record_sweep_run(&graph, &report, T0, Some(&audit_id)).expect("record");

        let marker = get_last_sweep_run(&graph, "test-hotel")
            .expect("lookup")
            .expect("marker exists");
        assert!(marker.filed);
        assert_eq!(marker.audit_id.as_deref(), Some(audit_id.as_str()));
        assert_eq!(marker.contradictions, 1);
    }

    #[test]
    fn last_sweep_run_overwrites_not_appends() {
        let graph = open_domain();
        let report = dirty_report();
        record_sweep_run(&graph, &report, T0, None).expect("first record");
        record_sweep_run(&graph, &report, T0 + 3600, Some("audit-2")).expect("second record");

        let marker = get_last_sweep_run(&graph, "test-hotel")
            .expect("lookup")
            .expect("marker exists");
        assert_eq!(marker.scanned_at, T0 + 3600);
        assert_eq!(marker.audit_id.as_deref(), Some("audit-2"));
    }

    #[test]
    fn no_sweep_run_yet_returns_none() {
        let graph = open_domain();
        assert!(
            get_last_sweep_run(&graph, "never-swept")
                .expect("lookup")
                .is_none()
        );
    }

    // ── ensure_scheduled ────────────────────────────────────────────────────

    #[test]
    fn ensure_scheduled_noop_when_not_opted_in() {
        let graph = open_domain();
        ensure_scheduled(&graph, "test-hotel", T0 * 1000, |_| None).expect("ok");
        assert!(
            graph
                .get_cron_job(&cron_job_id("test-hotel"))
                .expect("lookup")
                .is_none()
        );
    }

    #[test]
    fn ensure_scheduled_registers_nightly_job_once() {
        let graph = open_domain();
        let env = |k: &str| (k == ENV_ENABLED).then(|| "1".to_string());
        let now_ms = T0 * 1000;
        ensure_scheduled(&graph, "test-hotel", now_ms, env).expect("ok");

        let job = graph
            .get_cron_job(&cron_job_id("test-hotel"))
            .expect("lookup")
            .expect("job registered");
        assert_eq!(job.target_role, CRON_TARGET_ROLE);
        assert!(job.enabled);
        assert!(job.next_fire_at > now_ms);

        // Idempotent: a second call (e.g. hotel restart) does not clobber a
        // job whose schedule the operator may have hand-edited since.
        let mut edited = job.clone();
        edited.schedule = "0 30 4 * * * *".to_string();
        graph.upsert_cron_job(&edited).expect("upsert edited");
        ensure_scheduled(&graph, "test-hotel", now_ms + 1000, env).expect("ok");
        let after = graph
            .get_cron_job(&cron_job_id("test-hotel"))
            .expect("lookup")
            .expect("still present");
        assert_eq!(after.schedule, "0 30 4 * * * *", "operator edit preserved");
    }

    #[test]
    fn ensure_scheduled_honors_schedule_override() {
        let graph = open_domain();
        let env = |k: &str| match k {
            ENV_ENABLED => Some("1".to_string()),
            ENV_SCHEDULE => Some("0 0 5 * * * *".to_string()),
            _ => None,
        };
        ensure_scheduled(&graph, "test-hotel", T0 * 1000, env).expect("ok");
        let job = graph
            .get_cron_job(&cron_job_id("test-hotel"))
            .expect("lookup")
            .expect("job registered");
        assert_eq!(job.schedule, "0 0 5 * * * *");
    }
}
