//! Background model-catalog discovery job (option A: aiua owns the job).
//!
//! Periodically pulls a provider's live model list, diffs it against the last
//! persisted snapshot, and routes consequential changes (a model the provider
//! **retired**, or a model that flipped to reasoning/"thinking" by default) into
//! the self-heal queue for operator visibility. This is the early-warning that
//! would have flagged the `gemini-2.0-flash` retirement before it wedged turns.
//!
//! First cut: **OpenRouter** (`/api/v1/models`, public — no key). OpenRouter
//! aggregates Google/OpenAI/Anthropic models, so this already gives cross-
//! provider coverage. The Google-direct fetch (which needs the vault-backed key)
//! is a trivial add-on once this pattern is proven.
//!
//! Authority split (per MODEL_GRAPH_CATALOG_PROPOSAL): this writes catalog facts
//! + provenance and raises alerts. It does not touch live availability,
//! reachability, or per-turn routing.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{info, warn};

use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::heal_queue::{HealQueueStorage, SqliteHealQueueStorage};
use ansible_mesh_core::model_catalog_discovery::{
    CatalogDiffEvent, CatalogDiffKind, DiscoveredModel, diff_catalog, parse_openrouter_models,
};

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/models";
/// Config-node key holding the last OpenRouter discovery snapshot, so diffs
/// survive hotel restarts (the deprecation that bit us happened *during*
/// downtime — an in-memory `prev` would have missed it).
const SNAPSHOT_KEY: &str = "model_catalog_discovery.openrouter";
const GUEST_ID: &str = "model-catalog-sync";
const SYNC_INTERVAL_SECS: u64 = 6 * 60 * 60;
const INITIAL_DELAY_SECS: u64 = 45;

/// Spawn the periodic discovery loop. Bare interval loop (ends on process exit),
/// matching the network-poll loop style in `main.rs`.
pub fn spawn_loop(graph: Arc<GraphDomain>, db_path: String) {
    tokio::spawn(async move {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let heal: Option<Arc<dyn HealQueueStorage>> = match SqliteHealQueueStorage::open(&db_path) {
            Ok(h) => Some(Arc::new(h)),
            Err(e) => {
                warn!("model-catalog-sync: heal_queue unavailable ({e:#}); will log only");
                None
            }
        };

        tokio::time::sleep(Duration::from_secs(INITIAL_DELAY_SECS)).await;
        let mut interval = tokio::time::interval(Duration::from_secs(SYNC_INTERVAL_SECS));
        loop {
            interval.tick().await;
            if let Err(e) = run_once(&graph, heal.as_ref(), &http).await {
                warn!("model-catalog-sync: run failed: {e:#}");
            }
        }
    });
}

/// One discovery pass: fetch → diff vs persisted snapshot → persist → alert.
pub async fn run_once(
    graph: &GraphDomain,
    heal: Option<&Arc<dyn HealQueueStorage>>,
    http: &reqwest::Client,
) -> Result<()> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .ok();

    let body = http
        .get(OPENROUTER_URL)
        .send()
        .await
        .context("fetch OpenRouter model list")?
        .error_for_status()
        .context("OpenRouter model list returned an error status")?
        .text()
        .await
        .context("read OpenRouter model list body")?;

    let discovered = parse_openrouter_models(&body, now_secs)?;

    let prev: Vec<DiscoveredModel> = match graph.get_config_value(SNAPSHOT_KEY)? {
        Some(json) => serde_json::from_str(&json).unwrap_or_default(),
        None => Vec::new(),
    };
    let first_run = prev.is_empty();

    let diffs = diff_catalog(&prev, &discovered);

    // Persist the new snapshot before alerting so a restart mid-run can't cause
    // the same change to alert twice.
    graph.set_config_value(SNAPSHOT_KEY, &serde_json::to_string(&discovered)?)?;

    let mut alerts = 0usize;
    // Skip alerts on the first run — with no baseline everything is "Added" and
    // would flood the queue. We just record the baseline.
    if !first_run {
        for ev in &diffs {
            let Some(text) = heal_text(ev) else { continue };
            match heal {
                Some(hq) => {
                    if let Err(e) = hq.push_error(GUEST_ID, &text) {
                        warn!("model-catalog-sync: heal push failed: {e}");
                    } else {
                        alerts += 1;
                    }
                }
                None => warn!("model-catalog-sync alert (no heal queue): {text}"),
            }
        }
    }

    info!(
        provider = "openrouter",
        models = discovered.len(),
        diffs = diffs.len(),
        alerts,
        first_run,
        "model-catalog-sync: catalog synced"
    );
    Ok(())
}

/// Which diffs are worth a self-heal entry. Additions and context changes are
/// informational (logged only); retirements and thinking-flips break routing.
fn heal_text(ev: &CatalogDiffEvent) -> Option<String> {
    match ev.kind {
        CatalogDiffKind::Removed => Some(format!(
            "model-catalog: provider '{}' RETIRED model '{}' — any routing or config referencing it will now fail",
            ev.provider, ev.model_ref
        )),
        CatalogDiffKind::ReasoningChanged if ev.reasoning_default == Some(true) => Some(format!(
            "model-catalog: model '{}/{}' now reasons by default (thinking) — verify the controller handles thinking streams before routing text turns to it",
            ev.provider, ev.model_ref
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: CatalogDiffKind, reasoning: Option<bool>) -> CatalogDiffEvent {
        CatalogDiffEvent {
            kind,
            provider: "gemini".into(),
            model_ref: "gemini-2.0-flash".into(),
            detail: String::new(),
            reasoning_default: reasoning,
        }
    }

    #[test]
    fn retirements_and_thinking_flips_alert_additions_do_not() {
        assert!(heal_text(&ev(CatalogDiffKind::Removed, None)).is_some());
        assert!(heal_text(&ev(CatalogDiffKind::ReasoningChanged, Some(true))).is_some());
        // reasoning turned OFF is not a routing hazard.
        assert!(heal_text(&ev(CatalogDiffKind::ReasoningChanged, Some(false))).is_none());
        assert!(heal_text(&ev(CatalogDiffKind::Added, Some(true))).is_none());
        assert!(heal_text(&ev(CatalogDiffKind::ContextChanged, None)).is_none());
    }
}
