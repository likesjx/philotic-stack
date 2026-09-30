//! Memory sleep — Cortex-side maintenance of philote Muninn vaults.
//!
//! Phase 2 M6 (2026-09-16). The previous "dream" sweep never worked in
//! production: it recalled through a REST route Muninn does not expose
//! (`GET /api/recall`), "evolved" engrams by posting a Hebbian delta to an
//! endpoint that REPLACES content, discovered vaults with a guest-config scheme
//! that matches no real guests, depended on a local Ollama + embedding sidecar,
//! and ran on every hotel — where observer replicas reject writes (421).
//!
//! # What this does now
//!
//! Runs **only on the Cortex hotel** (the hotel with no `muninn_write_route`),
//! so every mutation goes through the primary and replicates. For each memory
//! vault this hotel holds a token for (`default`, `fleet_knowledge`, `user_*`,
//! `self_*` — observer hotels' agent vaults arrive via M4 forwarding):
//!
//! 1. **List** active engrams (`GET /api/engrams`, bounded).
//! 2. **Plan** deterministically, no model:
//!    - exact duplicates (same normalized concept + content) → one
//!      `POST /api/consolidate` per group, keeping the newest content (Muninn
//!      supersedes the rest with lineage);
//!    - diagnostic traffic (smoke tests, canaries, probes) → soft forget.
//! 3. **Report** contradictions (`GET /api/contradictions`) and tombstones
//!    (`GET /api/deleted`) — resolving those needs judgment, so they are
//!    counted, not mutated. REST has no hard delete; tombstones are reported.
//! 4. **Execute** only when `PHILOTIC_MEMORY_SLEEP_MUTATE` is truthy; otherwise
//!    the run is a proposal (logged + recorded) — the autonomy posture moves
//!    from proposal to execution per hotel by operator choice.
//!
//! Never run the `muninn dream` CLI on a cluster node: it opens Pebble without
//! the replication log, so its changes never reach the observers.

use ansible_mesh_core::domain::GraphDomain;
use anyhow::Result;
use memory_core::MuninnConfig;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

// ──── Nightly cron scheduling ─────────────────────────────────────────────────
//
// The shutdown-drain sweep alone leaves long-lived hotels accumulating
// near-duplicate engrams for days (audit finding): a hotel that never
// restarts never consolidates. Mirroring `memory_hygiene`, the sweep can
// also run on a nightly in-process cron job — same sentinel-role
// interception in `CronTicker::fire`, same per-hotel operator opt-in
// re-checked at fire time (CronJobSync replicates job definitions to every
// mesh peer regardless of that peer's own opt-in).

/// Sentinel `target_role` intercepted by `CronTicker::fire` — never resolves
/// to a guest inbox; the `internal:` prefix keeps it out of
/// `resolve_target_role_record`'s `role:{agent}:{role}` parsing.
pub const CRON_TARGET_ROLE: &str = "internal:dream_sweep";

/// Env var gating whether the hotel registers/runs the nightly sweep.
/// Operator opt-in per hotel — disabled unless explicitly truthy. The
/// shutdown-drain sweep is unaffected by this flag.
pub const ENV_ENABLED: &str = "PHILOTIC_DREAM_SWEEP_ENABLED";

/// Env override for the nightly cron schedule (7-field `cron` crate syntax).
pub const ENV_SCHEDULE: &str = "PHILOTIC_DREAM_SWEEP_SCHEDULE";

/// Default: nightly at 03:30 UTC — offset from memory-hygiene's 03:00 so the
/// two sweeps never hammer Muninn concurrently.
pub const DEFAULT_SCHEDULE: &str = "0 30 3 * * * *";

/// Deterministic id for the auto-registered per-hotel cron job — stable
/// across restarts so `ensure_scheduled` is idempotent.
pub fn cron_job_id(hotel_name: &str) -> String {
    format!("dream-sweep:{hotel_name}")
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

/// Idempotent, operator-opt-in registration of the nightly dream-sweep cron
/// job. No-op unless [`ENV_ENABLED`] is truthy for this hotel process; never
/// overwrites an operator-edited schedule.
pub fn ensure_scheduled(
    graph: &GraphDomain,
    hotel_name: &str,
    now_ms: u64,
    env: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<()> {
    if !sweep_enabled(&env) {
        debug!(
            hotel = %hotel_name,
            "dream-sweep: not enabled for this hotel (PHILOTIC_DREAM_SWEEP_ENABLED unset)"
        );
        return Ok(());
    }

    let job_id = cron_job_id(hotel_name);
    if graph.get_cron_job(&job_id)?.is_some() {
        debug!(hotel = %hotel_name, "dream-sweep: cron job already registered");
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
    info!(hotel = %hotel_name, job_id = %job_id, next_fire_at, "dream-sweep: nightly consolidation cron job registered");
    Ok(())
}

// ──── Public entry point ──────────────────────────────────────────────────────

/// Env var: when truthy, the sleep cycle executes its plan (consolidate exact
/// duplicates, forget diagnostic traffic). Unset = proposal only.
pub const ENV_MUTATE: &str = "PHILOTIC_MEMORY_SLEEP_MUTATE";

/// Config-key prefix for the last sleep run summary (per hotel).
pub const CONFIG_KEY_LAST_RUN_PREFIX: &str = "memory_sleep:last_run:";

/// Upper bound on engrams listed per vault per run.
const MAX_ENGRAMS_PER_VAULT: usize = 2_000;
const LIST_PAGE: usize = 200;

/// Run the sleep cycle across this hotel's memory vaults.
///
/// Non-fatal: failures are logged and the run continues with the next vault.
pub async fn dream_sweep(config: &MuninnConfig, graph: &GraphDomain, hotel_name: &str) {
    if let Some(route) = config.shared_write_route.as_deref() {
        debug!(
            hotel = %hotel_name,
            cortex = %route,
            "MemorySleep: not the Cortex hotel (writes route to {route}) — skipping"
        );
        return;
    }
    let mutate = mutate_enabled(|k| std::env::var(k).ok());

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("MemorySleep: failed to build HTTP client — {e}");
            return;
        }
    };

    let vaults = sleep_vault_names(config);
    info!(hotel = %hotel_name, vaults = vaults.len(), mutate, "MemorySleep: starting");
    let mut summary = SleepRunSummary {
        hotel_name: hotel_name.to_string(),
        mutate,
        ..Default::default()
    };
    for vault in &vaults {
        let Some(token) = config.vault_tokens.get(vault) else {
            continue;
        };
        match sleep_vault(&client, &config.base_url, token, vault, mutate).await {
            Ok(outcome) => summary.absorb(vault, outcome),
            Err(e) => {
                warn!(vault = %vault, error = %e, "MemorySleep: vault failed — continuing");
                summary.failed_vaults.push(vault.clone());
            }
        }
    }
    summary.finished_at = now_secs();
    info!(
        hotel = %hotel_name,
        vaults = summary.vaults_scanned,
        engrams = summary.engrams_scanned,
        duplicate_groups = summary.duplicate_groups,
        diagnostic = summary.diagnostic,
        contradictions = summary.contradictions,
        tombstones = summary.tombstones,
        consolidated = summary.consolidated,
        forgotten = summary.forgotten,
        mutate,
        "MemorySleep: complete"
    );
    let key = format!("{CONFIG_KEY_LAST_RUN_PREFIX}{hotel_name}");
    match serde_json::to_string(&summary) {
        Ok(json) => {
            if let Err(e) = graph.set_config_value(&key, &json) {
                warn!(error = %e, "MemorySleep: could not record run summary");
            }
        }
        Err(e) => warn!(error = %e, "MemorySleep: could not serialize run summary"),
    }
}

/// True when the operator has allowed the sleep cycle to change the store.
pub fn mutate_enabled(env: impl Fn(&str) -> Option<String>) -> bool {
    env(ENV_MUTATE)
        .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

/// Memory vaults the sleep cycle maintains: every routable memory vault this
/// hotel holds a Muninn token for (non-memory registry entries such as API
/// keys are excluded by name).
pub fn sleep_vault_names(config: &MuninnConfig) -> Vec<String> {
    let mut names: Vec<String> = config
        .vault_tokens
        .keys()
        .filter(|name| memory_core::is_cortex_routable_vault(name))
        .cloned()
        .collect();
    names.sort();
    names
}

// ──── Planning (pure) ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct EngramItem {
    pub id: String,
    #[serde(default)]
    pub concept: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub created_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SleepPlan {
    /// Groups of ≥2 exact duplicates: `(ids newest-first, merged_content)`.
    pub consolidate: Vec<(Vec<String>, String)>,
    /// Diagnostic traffic to soft-forget.
    pub forget: Vec<String>,
}

fn normalize_for_dedup(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Plan one vault's maintenance. Pure and deterministic.
pub fn plan_vault_sleep(engrams: &[EngramItem]) -> SleepPlan {
    let mut plan = SleepPlan::default();
    let mut groups: std::collections::BTreeMap<(String, String), Vec<&EngramItem>> =
        Default::default();
    for engram in engrams {
        if memory_core::write_hygiene::is_diagnostic_capture(
            &engram.concept,
            &engram.content,
            &engram.tags,
        ) {
            plan.forget.push(engram.id.clone());
            continue;
        }
        let key = (
            normalize_for_dedup(&engram.concept),
            normalize_for_dedup(&engram.content),
        );
        if key.1.is_empty() {
            continue;
        }
        groups.entry(key).or_default().push(engram);
    }
    for (_, mut members) in groups {
        if members.len() < 2 {
            continue;
        }
        members.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        let merged = members[0].content.clone();
        // Muninn consolidates at most 50 ids per call.
        for chunk in members.chunks(50) {
            if chunk.len() < 2 {
                continue;
            }
            plan.consolidate
                .push((chunk.iter().map(|e| e.id.clone()).collect(), merged.clone()));
        }
    }
    plan
}

// ──── Per-vault execution ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq)]
pub struct VaultSleepOutcome {
    pub engrams_scanned: usize,
    pub duplicate_groups: usize,
    pub diagnostic: usize,
    pub contradictions: usize,
    pub tombstones: usize,
    pub consolidated: usize,
    pub forgotten: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SleepRunSummary {
    pub hotel_name: String,
    pub mutate: bool,
    pub finished_at: u64,
    pub vaults_scanned: usize,
    pub engrams_scanned: usize,
    pub duplicate_groups: usize,
    pub diagnostic: usize,
    pub contradictions: usize,
    pub tombstones: usize,
    pub consolidated: usize,
    pub forgotten: usize,
    #[serde(default)]
    pub failed_vaults: Vec<String>,
}

impl SleepRunSummary {
    fn absorb(&mut self, _vault: &str, o: VaultSleepOutcome) {
        self.vaults_scanned += 1;
        self.engrams_scanned += o.engrams_scanned;
        self.duplicate_groups += o.duplicate_groups;
        self.diagnostic += o.diagnostic;
        self.contradictions += o.contradictions;
        self.tombstones += o.tombstones;
        self.consolidated += o.consolidated;
        self.forgotten += o.forgotten;
    }
}

async fn sleep_vault(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    vault: &str,
    mutate: bool,
) -> Result<VaultSleepOutcome> {
    let engrams = list_engrams(client, base_url, token, vault).await?;
    let plan = plan_vault_sleep(&engrams);
    let mut outcome = VaultSleepOutcome {
        engrams_scanned: engrams.len(),
        duplicate_groups: plan.consolidate.len(),
        diagnostic: plan.forget.len(),
        contradictions: count_json_array(
            client,
            base_url,
            token,
            vault,
            "/api/contradictions",
            "contradictions",
        )
        .await,
        tombstones: count_json_array(
            client,
            base_url,
            token,
            vault,
            "/api/deleted?limit=100",
            "deleted",
        )
        .await,
        ..Default::default()
    };

    if !mutate {
        if outcome.duplicate_groups > 0 || outcome.diagnostic > 0 {
            info!(
                vault = %vault,
                duplicate_groups = outcome.duplicate_groups,
                diagnostic = outcome.diagnostic,
                "MemorySleep: proposal only ({ENV_MUTATE} unset) — no changes made"
            );
        }
        return Ok(outcome);
    }

    for (ids, merged) in &plan.consolidate {
        let url = format!("{}/api/consolidate", base_url.trim_end_matches('/'));
        let resp = client
            .post(&url)
            .bearer_auth(token)
            .json(&serde_json::json!({ "vault": vault, "ids": ids, "merged_content": merged }))
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => outcome.consolidated += ids.len(),
            Ok(r) => {
                warn!(vault = %vault, status = %r.status(), "MemorySleep: consolidate refused")
            }
            Err(e) => warn!(vault = %vault, error = %e, "MemorySleep: consolidate failed"),
        }
    }
    for id in &plan.forget {
        let url = format!(
            "{}/api/engrams/{}?vault={}",
            base_url.trim_end_matches('/'),
            id,
            vault
        );
        match client.delete(&url).bearer_auth(token).send().await {
            Ok(r) if r.status().is_success() => outcome.forgotten += 1,
            Ok(r) => {
                warn!(vault = %vault, id = %id, status = %r.status(), "MemorySleep: forget refused")
            }
            Err(e) => warn!(vault = %vault, id = %id, error = %e, "MemorySleep: forget failed"),
        }
    }
    Ok(outcome)
}

async fn list_engrams(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    vault: &str,
) -> Result<Vec<EngramItem>> {
    #[derive(Deserialize)]
    struct Page {
        #[serde(default)]
        engrams: Vec<EngramItem>,
    }
    let mut out = Vec::new();
    let mut offset = 0usize;
    while out.len() < MAX_ENGRAMS_PER_VAULT {
        let url = format!(
            "{}/api/engrams?vault={}&limit={}&offset={}",
            base_url.trim_end_matches('/'),
            vault,
            LIST_PAGE,
            offset
        );
        let resp = client.get(&url).bearer_auth(token).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("list engrams returned {}", resp.status());
        }
        let page: Page = resp.json().await?;
        let n = page.engrams.len();
        out.extend(page.engrams);
        if n < LIST_PAGE {
            break;
        }
        offset += n;
    }
    Ok(out)
}

/// Length of a JSON array found at `field` (or the top level) of a GET
/// response; 0 when the endpoint is unavailable. Report-only.
async fn count_json_array(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    vault: &str,
    path_and_query: &str,
    field: &str,
) -> usize {
    let sep = if path_and_query.contains('?') {
        '&'
    } else {
        '?'
    };
    let url = format!(
        "{}{}{}vault={}",
        base_url.trim_end_matches('/'),
        path_and_query,
        sep,
        vault
    );
    let Ok(resp) = client.get(&url).bearer_auth(token).send().await else {
        return 0;
    };
    if !resp.status().is_success() {
        return 0;
    }
    let Ok(json) = resp.json::<serde_json::Value>().await else {
        return 0;
    };
    json.get(field)
        .and_then(|v| v.as_array())
        .or_else(|| json.as_array())
        .map(|a| a.len())
        .unwrap_or(0)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ──── Helpers ─────────────────────────────────────────────────────────────────

/// Derive `self_{agent_id}` vault names for all active guests in the hotel.
///
/// Retained for `memory_delta_digest`; the sleep cycle uses
/// [`sleep_vault_names`] (the token registry), which matches real vaults.
pub(crate) fn collect_agent_vault_names(graph: &GraphDomain, hotel_name: &str) -> Vec<String> {
    graph
        .list_guests(hotel_name, true)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|g| {
            let cfg: serde_json::Value = serde_json::from_str(&g.config_json).ok()?;
            let agent_id = cfg.get("agent_id")?.as_str()?;
            Some(format!("self_{agent_id}"))
        })
        .collect()
}

// ──── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, concept: &str, content: &str, at: i64) -> EngramItem {
        EngramItem {
            id: id.into(),
            concept: concept.into(),
            content: content.into(),
            tags: vec![],
            created_at: at,
        }
    }

    #[test]
    fn plan_consolidates_exact_duplicates_keeping_newest_and_forgets_probes() {
        let engrams = vec![
            item("01A", "rehearsal", "Choir rehearsal moved to Thursday.", 10),
            item(
                "01B",
                "Rehearsal",
                "choir rehearsal  moved to thursday.",
                30,
            ),
            item("01C", "rehearsal", "Choir rehearsal moved to Thursday.", 20),
            item(
                "01D",
                "perplexity.note: Cross-hotel routing test",
                "routing test ping",
                5,
            ),
            item("01E", "rehearsal", "Choir rehearsal moved to Friday.", 40),
        ];
        let plan = plan_vault_sleep(&engrams);
        assert_eq!(plan.forget, vec!["01D".to_string()]);
        assert_eq!(plan.consolidate.len(), 1);
        let (ids, merged) = &plan.consolidate[0];
        assert_eq!(
            ids,
            &vec!["01B".to_string(), "01C".to_string(), "01A".to_string()]
        );
        assert_eq!(merged, "choir rehearsal  moved to thursday.");
    }

    #[test]
    fn mutation_requires_explicit_opt_in() {
        assert!(!mutate_enabled(|_| None));
        assert!(!mutate_enabled(|k| (k == ENV_MUTATE).then(|| "0".into())));
        assert!(mutate_enabled(|k| (k == ENV_MUTATE).then(|| "true".into())));
        // The nightly-sweep opt-in alone does not allow changes.
        assert!(!mutate_enabled(|k| (k == ENV_ENABLED).then(|| "1".into())));
    }

    #[test]
    fn plan_leaves_distinct_memories_alone() {
        let engrams = vec![
            item("01A", "pref", "Aisle seat", 1),
            item("01B", "pref", "Window seat", 2),
            item("01C", "empty", "   ", 3),
            item("01D", "empty", "", 4),
        ];
        assert_eq!(plan_vault_sleep(&engrams), SleepPlan::default());
    }

    #[test]
    fn sleep_vaults_come_from_the_token_registry_and_exclude_non_memory_entries() {
        let config = memory_core::MuninnConfig::local("default")
            .with_vault_token("self_agent-bjork-01", "t1")
            .with_vault_token("user_likesjx", "t2")
            .with_vault_token("openai_api_key", "t3")
            .with_vault_token("session_01abc", "t4");
        assert_eq!(
            sleep_vault_names(&config),
            vec![
                "self_agent-bjork-01".to_string(),
                "user_likesjx".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn sleep_skips_non_cortex_hotels() {
        // An observer hotel must never attempt maintenance writes. With a write
        // route set the sweep returns before building a client or listing.
        let mut config = memory_core::MuninnConfig::local("default");
        config.base_url = "http://127.0.0.1:9".into();
        config.shared_write_route = Some("vps-jane-aiua-01".into());
        let storage = ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open_in_memory()
            .expect("storage");
        let graph = GraphDomain::new(std::sync::Arc::new(storage.adapter()));
        dream_sweep(&config, &graph, "mac-jane").await;
        assert!(
            graph
                .get_config_value(&format!("{CONFIG_KEY_LAST_RUN_PREFIX}mac-jane"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn nightly_sweep_enabled_requires_explicit_truthy_value() {
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
            |k| (k == ENV_ENABLED).then(|| "TRUE".to_string())
        ));
    }

    #[test]
    fn nightly_default_schedule_parses_and_is_offset_from_hygiene() {
        // Both sweeps hit Muninn; keep them staggered.
        assert_ne!(DEFAULT_SCHEDULE, crate::memory_hygiene::DEFAULT_SCHEDULE);
        let next = ansible_mesh_core::cron::next_fire_after(DEFAULT_SCHEDULE, 1_750_000_000_000)
            .expect("default schedule parses");
        assert!(next > 1_750_000_000_000);
    }

    #[test]
    fn cron_sentinel_role_stays_internal() {
        assert!(CRON_TARGET_ROLE.starts_with("internal:"));
        assert_ne!(CRON_TARGET_ROLE, crate::memory_hygiene::CRON_TARGET_ROLE);
        assert_eq!(cron_job_id("mac-jane"), "dream-sweep:mac-jane");
    }
}
