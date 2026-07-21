//! Memory Hygiene sweep — Memory Transparency Slice M4 (`memory.hygiene`).
//!
//! A scheduled, **non-destructive** per-hotel Muninn sweep: for every vault
//! this hotel can discover ([`discover_vaults`] — MuninnDB's own vault
//! listing, filtered by the hotel's `vault_registry` tokens; see "Vault
//! discovery" below), (a) list open contradiction pairs
//! (`GET /api/contradictions`) and (b) list old-but-still-active engrams as a
//! staleness proxy (`GET /api/engrams?sort=created&before=...`). Findings
//! above a threshold are filed as ONE aggregated `autonomy_audit` record on
//! lane [`ansible_mesh_core::autonomy::LANE_MEMORY_HYGIENE`] —
//! annotation/flagging only, never forget or consolidate-merge.
//!
//! # Vault discovery (DEF-067)
//!
//! Discovery used to derive vault names from materialized guest configs
//! (`dream::collect_agent_vault_names`, keyed on a guest config's top-level
//! `agent_id` field). No real guest config carries that field at the top
//! level — it lives under `env.PHILOTIC_AGENT_ID` — so that scheme matched
//! zero guests on every real hotel and the sweep silently ran as an empty
//! no-op that read as "clean" (`vaults=0 contradictions=0 stale=0` every
//! night since the sweep was first enabled). [`discover_vaults`] instead
//! intersects the hotel's `vault_registry` tokens with MuninnDB's own
//! `GET /api/vaults` ground truth, and surfaces an explicit
//! [`HygieneReport::discovery_warning`] (logged at WARN, carried onto the
//! last-run marker) whenever that intersection is empty — an empty sweep
//! must never look identical to a clean one.
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

use std::collections::HashSet;

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
    /// `Some(reason)` when vault *discovery* itself found nothing to sweep —
    /// distinct from a clean sweep of N vaults that simply had no findings.
    /// Set by [`discover_vaults`]; carried through to the last-run marker
    /// ([`LastRunRecord::discovery_warning`]) so an empty sweep is never
    /// silently indistinguishable from a genuinely clean one. `None` whenever
    /// at least one vault was discovered and scanned.
    pub discovery_warning: Option<String>,
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
pub(crate) struct ContradictionItem {
    pub(crate) id_a: String,
    pub(crate) concept_a: String,
    pub(crate) id_b: String,
    pub(crate) concept_b: String,
    pub(crate) detected_at: i64,
}

#[derive(Debug, Deserialize)]
struct ListEngramsResponse {
    engrams: Vec<EngramItem>,
}

/// One row of `GET /api/engrams` list output. Reused by the M3 memory-delta
/// digest (`memory_delta_digest.rs`) as well as this module's own staleness
/// sweep — the list endpoint does not return `metadata`/`updated_at`, so
/// neither consumer can read provenance or track evolution from this shape
/// alone (see `memory_delta_digest`'s module doc for the per-engram detail
/// fetch that fills the provenance gap for a bounded top-N of notable lines).
#[derive(Debug, Deserialize)]
pub(crate) struct EngramItem {
    pub(crate) id: String,
    pub(crate) concept: String,
    pub(crate) created_at: i64,
}

pub(crate) async fn fetch_contradictions(
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

/// Map raw `GET /api/contradictions` items onto this module's
/// [`ContradictionFinding`] vocabulary. Shared by this module's `sweep()` and
/// by the M3 memory-delta digest, so the mapping only lives in one place.
pub(crate) async fn fetch_contradiction_findings(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    vault_name: &str,
) -> anyhow::Result<Vec<ContradictionFinding>> {
    let items = fetch_contradictions(client, base_url, token).await?;
    Ok(items
        .into_iter()
        .map(|c| ContradictionFinding {
            vault: vault_name.to_string(),
            id_a: c.id_a,
            concept_a: c.concept_a,
            id_b: c.id_b,
            concept_b: c.concept_b,
            detected_at: c.detected_at,
        })
        .collect())
}

/// Generic `GET /api/engrams` list call. `since`/`before` are RFC3339 and
/// optional (server treats an absent filter as unbounded on that side).
/// Shared by this module's staleness sweep (`before`, `sort=created`) and the
/// M3 memory-delta digest's "remembered in the window" query (`since`,
/// `sort=created`) — one endpoint wrapper, two callers.
pub(crate) async fn fetch_engrams(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    sort: &str,
    since_rfc3339: Option<&str>,
    before_rfc3339: Option<&str>,
    limit: u32,
) -> anyhow::Result<Vec<EngramItem>> {
    let mut url = format!(
        "{}/api/engrams?sort={}&limit={}",
        base_url.trim_end_matches('/'),
        sort,
        limit,
    );
    if let Some(since) = since_rfc3339 {
        url.push_str(&format!("&since={}", urlencoding_light(since)));
    }
    if let Some(before) = before_rfc3339 {
        url.push_str(&format!("&before={}", urlencoding_light(before)));
    }
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

async fn fetch_stale_candidates(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    before_rfc3339: &str,
    limit: u32,
) -> anyhow::Result<Vec<EngramItem>> {
    fetch_engrams(
        client,
        base_url,
        token,
        "created",
        None,
        Some(before_rfc3339),
        limit,
    )
    .await
}

/// Minimal RFC3339 query-param escaping — the only characters an RFC3339
/// timestamp contains that need encoding in a query string are `:` and `+`.
/// Avoids pulling in a full percent-encoding dependency for one call site.
pub(crate) fn urlencoding_light(s: &str) -> String {
    s.replace(':', "%3A").replace('+', "%2B")
}

// ── Vault discovery ─────────────────────────────────────────────────────────────

/// `GET /api/vaults` — MuninnDB's own ground-truth vault listing. Same call
/// `muninn_provision::provision_muninn_vaults` makes (after an admin login,
/// for the create-vs-exists check); as a read-only list it needs no session —
/// verified live against a loopback Lobe (`curl 127.0.0.1:8475/api/vaults`
/// with no `Authorization` header returns 200 with the vault name array).
pub(crate) async fn fetch_muninn_vault_names(
    client: &reqwest::Client,
    base_url: &str,
) -> anyhow::Result<Vec<String>> {
    let url = format!("{}/api/vaults", base_url.trim_end_matches('/'));
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("list vaults returned {status}: {body}");
    }
    Ok(resp.json::<Vec<String>>().await?)
}

/// Discover which Muninn vaults this hotel can sweep this run.
///
/// Ground truth is MuninnDB's own [`fetch_muninn_vault_names`] listing; the
/// hotel's `vault_registry` — surfaced here as `config.vault_tokens`, already
/// filtered to `muninn_vault_token`-kind secrets and resolved at boot by
/// `memory::load_muninn_config` — is the filter: we can only sweep a vault we
/// hold a bearer token for, and MuninnDB may know about vaults (`default`,
/// other tenants' vaults) this hotel has no business reading.
///
/// Deliberately does **not** derive vault names from materialized guest
/// configs (the scheme `dream::collect_agent_vault_names` uses for the
/// unrelated Dreams consolidation pass). That scheme keys off a guest
/// config's top-level `agent_id` field, but every real guest config in this
/// codebase carries the agent id nested under `env.PHILOTIC_AGENT_ID`
/// (`crates/aiua/src/main.rs::agent_guests_for_profile`), not at the top
/// level — so it matches zero guests on every real hotel and silently
/// starves any sweep that depends on it. Confirmed live on mac-jane: zero
/// `materialized_guests` rows carry a top-level `agent_id`, while
/// `config:vault_registry` holds valid `muninn_vault_token` entries for
/// `self_agent-bjork-01`, `self_agent-coach`, and `user_likesjx`.
///
/// Returns the sweepable vault names and, when that list is empty, `Some`
/// with a human-readable reason — the caller logs this at WARN and carries
/// it onto [`HygieneReport::discovery_warning`] so an empty sweep is never
/// silently indistinguishable from a clean one.
pub(crate) async fn discover_vaults(
    client: &reqwest::Client,
    config: &MuninnConfig,
) -> (Vec<String>, Option<String>) {
    let registry_vaults: Vec<String> = config.vault_tokens.keys().cloned().collect();

    let live_vaults = match fetch_muninn_vault_names(client, &config.base_url).await {
        Ok(v) => Some(v),
        Err(e) => {
            warn!(
                error = %e,
                "memory.hygiene: MuninnDB /api/vaults unreachable — sweeping the vault_registry \
                 token list without ground-truth cross-check"
            );
            None
        }
    };

    resolve_vaults_to_sweep(&registry_vaults, live_vaults.as_deref())
}

/// Pure decision logic behind [`discover_vaults`], split out so it is
/// unit-testable against fixture inputs without a live HTTP call — the
/// network call itself (`fetch_muninn_vault_names`) has no local mock-server
/// dependency available in this crate, matching how the rest of this module
/// tests REST-adjacent logic (e.g. `evidence_summary`) against fixtures
/// rather than mocking the wire calls.
///
/// `registry_vaults` is every vault name this hotel holds a
/// `muninn_vault_token` for (i.e. `config.vault_tokens.keys()`).
/// `live_vaults` is MuninnDB's own `GET /api/vaults` listing when reachable,
/// `None` when the call failed (network error, non-2xx, bad body).
///
/// - `registry_vaults` empty → nothing to sweep, full stop (empty-with-warn).
/// - `live_vaults` reachable → sweep the intersection; a registry vault
///   absent from the live listing is a stale-registry signal, logged and
///   dropped rather than swept blind. If the intersection is empty, that is
///   also empty-with-warn (registry entirely stale).
/// - `live_vaults` unreachable → fall back to trusting the registry token
///   list as-is (best-effort continuity; the caller already warned about the
///   unreachable ground truth).
pub(crate) fn resolve_vaults_to_sweep(
    registry_vaults: &[String],
    live_vaults: Option<&[String]>,
) -> (Vec<String>, Option<String>) {
    let mut candidates: Vec<String> = registry_vaults.to_vec();
    candidates.sort();

    if candidates.is_empty() {
        let reason = "vault_registry has no muninn_vault_token entries — nothing to sweep \
             (check vault provisioning / `phil graph`; a hotel with agents materialized \
             should hold one muninn_vault_token per agent vault)"
            .to_string();
        return (Vec::new(), Some(reason));
    }

    let Some(live_vaults) = live_vaults else {
        // MuninnDB unreachable this run — the caller already logged why;
        // sweep what the registry says we hold tokens for rather than
        // discovering nothing just because the ground-truth check failed.
        return (candidates, None);
    };

    let live: HashSet<&str> = live_vaults.iter().map(String::as_str).collect();
    let (present, missing): (Vec<String>, Vec<String>) = candidates
        .into_iter()
        .partition(|v| live.contains(v.as_str()));
    for vault in &missing {
        warn!(
            vault = %vault,
            "memory.hygiene: vault_registry holds a token for this vault but MuninnDB \
             does not report it in /api/vaults — stale registry entry? skipping"
        );
    }
    if present.is_empty() {
        let reason = format!(
            "MuninnDB /api/vaults reports {} vault(s), none matching the {} \
             vault_registry token(s) held by this hotel — registry looks stale",
            live_vaults.len(),
            missing.len()
        );
        (Vec::new(), Some(reason))
    } else {
        (present, None)
    }
}

// ── Sweep ──────────────────────────────────────────────────────────────────────

/// Sweep every Muninn vault this hotel can discover ([`discover_vaults`]) for
/// contradictions and aging-but-active memories. Read-only — no Muninn write
/// calls. Per-vault failures are captured on the `VaultSweepResult` and do
/// not abort the rest of the sweep.
pub async fn sweep(
    client: &reqwest::Client,
    config: &MuninnConfig,
    hotel_name: &str,
    thresholds: &HygieneThresholds,
    now: chrono::DateTime<chrono::Utc>,
) -> HygieneReport {
    let (vault_names, discovery_warning) = discover_vaults(client, config).await;
    if let Some(reason) = &discovery_warning {
        warn!(
            hotel = %hotel_name,
            reason = %reason,
            "memory.hygiene: sweep discovered zero vaults — this is an empty discovery, not a clean sweep"
        );
    }
    let before = (now - chrono::Duration::days(thresholds.stale_days)).to_rfc3339();

    let mut report = HygieneReport {
        hotel_name: hotel_name.to_string(),
        vaults: Vec::with_capacity(vault_names.len()),
        discovery_warning,
    };

    for vault_name in &vault_names {
        let token = match config
            .vault_tokens
            .get(vault_name)
            .or(config.default_token.as_ref())
        {
            Some(t) => t.clone(),
            None => {
                // discover_vaults only returns names drawn from
                // config.vault_tokens, so this should be unreachable in
                // practice; kept as a defensive skip rather than an unwrap.
                warn!(vault = %vault_name, "memory.hygiene: discovered vault has no token — skipping (unexpected)");
                continue;
            }
        };

        let mut result = VaultSweepResult {
            vault: vault_name.clone(),
            ..Default::default()
        };

        match fetch_contradiction_findings(client, &config.base_url, &token, vault_name).await {
            Ok(findings) => {
                result.contradictions = findings;
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
    /// Carried from [`HygieneReport::discovery_warning`]. `Some(reason)`
    /// means `vaults_scanned == 0` because vault *discovery* found nothing,
    /// not because the hotel genuinely has no findings — a dashboard/digest
    /// consumer must not read a zero-vault marker as "clean" without
    /// checking this field. `#[serde(default)]` so markers written before
    /// this field existed still deserialize.
    #[serde(default)]
    pub discovery_warning: Option<String>,
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
        discovery_warning: report.discovery_warning.clone(),
    };
    let key = format!("{CONFIG_KEY_LAST_RUN_PREFIX}{}", report.hotel_name);
    graph.set_config_value(&key, &serde_json::to_string(&record)?)
}

/// Read back the most recent sweep-run marker for `hotel_name`, if any sweep
/// has run yet.
///
/// Consumed by the M3 memory-delta digest (`memory_delta_digest.rs`) to
/// surface "what did the last hygiene sweep find" alongside the digest's own
/// created/deleted/contradiction window. Also available for a future
/// dashboard/`phil` CLI status surface.
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
///
/// `heal_queue` is optional and wired for Piece 3 of the A9 outcome-stamping
/// follow-up slice: when a fresh filing happens, an unresolved, throttled
/// pending-outcome notice is pushed alongside the `autonomy_audit` record so
/// the finding surfaces via the existing heal-queue channel (not just
/// `phil autonomy pending`) — the same breadcrumb the A3 heal-filing site
/// pushes, but deliberately left unresolved instead of immediately
/// `.resolve()`d, since this one is *awaiting* an operator stamp.
pub async fn run_scheduled_sweep(
    graph: &GraphDomain,
    muninn_config: Option<&MuninnConfig>,
    hotel_name: &str,
    intel_graph_url: Option<&str>,
    heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
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

    let report = sweep(&client, config, hotel_name, &thresholds, now_dt).await;
    info!(
        hotel = %hotel_name,
        vaults = report.vaults_scanned(),
        contradictions = report.total_contradictions(),
        stale = report.total_stale(),
        discovery_warning = report.discovery_warning.as_deref().unwrap_or(""),
        "memory.hygiene: sweep complete"
    );

    let env = |k: &str| std::env::var(k).ok();
    let audit_id = file_if_warranted(graph, &report, &thresholds, now_secs, &env);
    if let Some(audit_id) = &audit_id {
        if let Some(url) = intel_graph_url {
            push_intel_graph_record(&client, url, &report, audit_id).await;
        }
        // A9 Piece 3: push the pending-outcome breadcrumb for this fresh
        // filing. Best-effort and non-blocking — the `autonomy_audit` record
        // above is the durable one; this is visibility only.
        if let Some(hq) = heal_queue {
            let notice = ansible_mesh_core::autonomy::pending_outcome_notice(
                audit_id,
                LANE_MEMORY_HYGIENE,
                &report.action_summary(),
            );
            match hq.push_classified(
                LANE_MEMORY_HYGIENE,
                &notice,
                "info",
                "autonomy_outcome_pending",
            ) {
                Ok(Some(id)) => info!(
                    id,
                    audit_id, "memory.hygiene: pending-outcome notice pushed to heal queue"
                ),
                Ok(None) => debug!(
                    audit_id,
                    "memory.hygiene: pending-outcome notice collapsed (flood window)"
                ),
                Err(e) => warn!("memory.hygiene: pending-outcome notice push failed: {e:#}"),
            }
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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

    // ── resolve_vaults_to_sweep (vault discovery decision logic) ────────────

    fn vaults(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn multi_vault_registry_confirmed_live_sweeps_all() {
        let registry = vaults(&["self_agent-bjork-01", "self_agent-coach", "user_likesjx"]);
        let live = vaults(&[
            "default",
            "self_agent-bjork-01",
            "self_agent-coach",
            "user_likesjx",
        ]);
        let (swept, warning) = resolve_vaults_to_sweep(&registry, Some(&live));
        assert_eq!(
            swept,
            vec!["self_agent-bjork-01", "self_agent-coach", "user_likesjx"]
        );
        assert!(warning.is_none());
    }

    #[test]
    fn empty_registry_is_empty_with_warn_not_silently_clean() {
        let (swept, warning) = resolve_vaults_to_sweep(&[], Some(&vaults(&["default"])));
        assert!(swept.is_empty());
        assert!(
            warning
                .as_deref()
                .is_some_and(|w| w.contains("no muninn_vault_token entries"))
        );
    }

    #[test]
    fn zero_vault_regression_no_guest_derived_names_used() {
        // Regression for the original bug: discovery must never depend on
        // materialized guest configs (a guest config's top-level `agent_id`
        // field, which no real guest config carries). An empty registry
        // yields empty-with-warn regardless of what MuninnDB reports live.
        let live = vaults(&["self_agent-bjork-01", "self_agent-coach", "user_likesjx"]);
        let (swept, warning) = resolve_vaults_to_sweep(&[], Some(&live));
        assert!(swept.is_empty(), "empty registry must never sweep anything");
        assert!(warning.is_some(), "empty discovery must carry a reason");
    }

    #[test]
    fn stale_registry_entries_are_dropped_not_swept_blind() {
        let registry = vaults(&["self_agent-bjork-01", "self_agent-retired"]);
        let live = vaults(&["self_agent-bjork-01", "user_likesjx"]);
        let (swept, warning) = resolve_vaults_to_sweep(&registry, Some(&live));
        assert_eq!(swept, vec!["self_agent-bjork-01"]);
        assert!(warning.is_none(), "at least one vault survived — not empty");
    }

    #[test]
    fn registry_entirely_stale_against_live_is_empty_with_warn() {
        let registry = vaults(&["self_agent-retired"]);
        let live = vaults(&["self_agent-bjork-01"]);
        let (swept, warning) = resolve_vaults_to_sweep(&registry, Some(&live));
        assert!(swept.is_empty());
        assert!(
            warning
                .as_deref()
                .is_some_and(|w| w.contains("registry looks stale"))
        );
    }

    #[test]
    fn muninn_unreachable_falls_back_to_registry_token_list() {
        let registry = vaults(&["self_agent-bjork-01", "self_agent-coach"]);
        let (swept, warning) = resolve_vaults_to_sweep(&registry, None);
        assert_eq!(swept, vec!["self_agent-bjork-01", "self_agent-coach"]);
        assert!(
            warning.is_none(),
            "non-empty registry fallback is not an empty-discovery condition"
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
            ..Default::default()
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
            ..Default::default()
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
        assert_eq!(
            provenance.trust,
            ansible_mesh_core::provenance::TrustTier::Observed
        );
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
            ..Default::default()
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
        assert!(
            marker.discovery_warning.is_none(),
            "a genuinely clean sweep (vaults found, no findings) must not carry a discovery warning"
        );
    }

    #[test]
    fn empty_discovery_warning_propagates_to_the_last_run_marker() {
        // Regression for "never silently empty": a report whose vault
        // *discovery* found nothing must persist that reason on the marker,
        // not just log it — a dashboard/digest reading the marker later has
        // no access to the log line.
        let graph = open_domain();
        let empty = HygieneReport {
            hotel_name: "test-hotel".to_string(),
            vaults: Vec::new(),
            discovery_warning: Some("vault_registry has no muninn_vault_token entries".to_string()),
        };
        assert!(
            file_if_warranted(&graph, &empty, &HygieneThresholds::default(), T0, NO_ENV).is_none()
        );
        record_sweep_run(&graph, &empty, T0, None).expect("record");
        let marker = get_last_sweep_run(&graph, "test-hotel")
            .expect("lookup")
            .expect("marker exists");
        assert_eq!(marker.vaults_scanned, 0);
        assert!(!marker.filed);
        assert_eq!(
            marker.discovery_warning.as_deref(),
            Some("vault_registry has no muninn_vault_token entries"),
            "empty-discovery reason must survive onto the persisted marker"
        );
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
