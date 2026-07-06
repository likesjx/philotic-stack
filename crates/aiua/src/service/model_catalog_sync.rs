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
use ansible_mesh_core::model_routing::{
    DEFAULT_FALLBACK_TIERS, ProviderRouting, RoutingImpact, routing_impact_for_model,
};
use ansible_mesh_core::provider_keys::provider_key_specs;

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
        // Load the live routing picture once so a catalog change can be re-cast as
        // a *routing* alert naming the affected ladder, not just catalog news.
        let providers = provider_routings(graph);
        let ladders = fallback_ladders(graph);
        for ev in &diffs {
            let Some(text) = heal_text(ev) else { continue };
            let mut texts = vec![text];
            // Routing-impact overlay: if this retired / thinking-flipped model is
            // some provider's configured default_model, name the ladders that
            // route through it (distinct entry — routing impact, not catalog news).
            if routing_relevant(ev) {
                for impact in routing_impact_for_model(&ev.model_ref, &providers, &ladders) {
                    texts.push(routing_impact_text(ev, &impact));
                }
            }
            for text in texts {
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

/// The catalog changes that can break routing: a model the provider retired, or
/// one that flipped to thinking-by-default. Mirrors [`heal_text`]'s gate.
fn routing_relevant(ev: &CatalogDiffEvent) -> bool {
    matches!(ev.kind, CatalogDiffKind::Removed)
        || (matches!(ev.kind, CatalogDiffKind::ReasoningChanged)
            && ev.reasoning_default == Some(true))
}

/// A distinct heal entry that names the routing surface a catalog change lands on
/// (default_model + any fallback ladder routing through the provider), so the
/// operator sees the routing consequence, not just the catalog fact.
fn routing_impact_text(ev: &CatalogDiffEvent, impact: &RoutingImpact) -> String {
    let verb = match ev.kind {
        CatalogDiffKind::Removed => "RETIRED",
        _ => "flipped to thinking-by-default for",
    };
    let ladders = if impact.affected_ladders.is_empty() {
        "no configured fallback ladder (default_model only)".to_string()
    } else {
        format!(
            "fallback ladder(s): [{}]",
            impact.affected_ladders.join(", ")
        )
    };
    format!(
        "model-catalog ROUTING IMPACT: provider '{}' {verb} '{}', which is the configured default_model for '{}' — affects {}. Turns routing there will escalate into the next tier (or a void) until reconfigured.",
        ev.provider, ev.model_ref, impact.matched_model, ladders
    )
}

/// Read a config-stored string value, tolerating both JSON-quoted and raw forms
/// (config writers store strings via `serde_json::to_string`, i.e. quoted).
fn config_string(graph: &GraphDomain, key: &str) -> Option<String> {
    let raw = graph.get_config_value(key).ok().flatten()?;
    let value = serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or(raw);
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Build the per-provider routing facts (controller roles + configured
/// default_model) from the provider key specs and stored config.
fn provider_routings(graph: &GraphDomain) -> Vec<ProviderRouting> {
    provider_key_specs()
        .iter()
        .map(|spec| {
            let default_model = spec
                .default_model_key
                .and_then(|key| config_string(graph, key))
                .or_else(|| spec.default_model.map(str::to_string));
            ProviderRouting {
                provider: spec.provider.to_string(),
                allowed_roles: spec.allowed_roles.iter().map(|r| r.to_string()).collect(),
                default_model,
            }
        })
        .collect()
}

/// All fallback ladders in play: every role incarnation's configured tiers plus
/// the default ladder every role without configured tiers actually runs.
fn fallback_ladders(graph: &GraphDomain) -> Vec<(String, Vec<String>)> {
    let mut ladders: Vec<(String, Vec<String>)> = Vec::new();
    ladders.push((
        "default fallback ladder".to_string(),
        DEFAULT_FALLBACK_TIERS
            .iter()
            .map(|t| t.to_string())
            .collect(),
    ));
    if let Ok(incarnations) = graph.list_all_role_incarnations() {
        for rec in incarnations {
            let tiers = &rec.turn_loop_config.fallback_tiers;
            if tiers.is_empty() {
                continue;
            }
            ladders.push((
                format!("role:{}:{}", rec.agent_id, rec.role_name),
                tiers.clone(),
            ));
        }
    }
    ladders
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

    #[test]
    fn routing_relevant_matches_heal_text_gate() {
        assert!(routing_relevant(&ev(CatalogDiffKind::Removed, None)));
        assert!(routing_relevant(&ev(
            CatalogDiffKind::ReasoningChanged,
            Some(true)
        )));
        assert!(!routing_relevant(&ev(
            CatalogDiffKind::ReasoningChanged,
            Some(false)
        )));
        assert!(!routing_relevant(&ev(CatalogDiffKind::Added, None)));
    }

    #[test]
    fn routing_impact_text_names_affected_ladder() {
        // An OpenRouter-prefixed retirement of gemini's default_model resolves onto
        // the orchestrator ladder that routes tier 0 through "model".
        let mut event = ev(CatalogDiffKind::Removed, None);
        event.provider = "openrouter".into();
        event.model_ref = "google/gemini-2.0-flash-exp".into();

        let providers = vec![ProviderRouting {
            provider: "gemini".into(),
            allowed_roles: vec!["model".into(), "model.gemini".into()],
            default_model: Some("gemini-2.0-flash-exp".into()),
        }];
        let ladders = vec![(
            "role:jane:orchestrator".to_string(),
            vec!["model".to_string(), "model.local".to_string()],
        )];

        let impacts = routing_impact_for_model(&event.model_ref, &providers, &ladders);
        assert_eq!(impacts.len(), 1);
        let text = routing_impact_text(&event, &impacts[0]);
        assert!(text.contains("ROUTING IMPACT"));
        assert!(text.contains("role:jane:orchestrator"));
        assert!(text.contains("gemini-2.0-flash-exp"));
    }
}
