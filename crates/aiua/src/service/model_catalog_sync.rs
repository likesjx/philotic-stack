//! Background model-catalog discovery job (option A: aiua owns the job).
//!
//! Periodically pulls a provider's live model list, diffs it against the last
//! persisted snapshot, and routes consequential changes (a model the provider
//! **retired**, or a model that flipped to reasoning/"thinking" by default) into
//! the self-heal queue for operator visibility. This is the early-warning that
//! would have flagged the `gemini-2.0-flash` retirement before it wedged turns.
//!
//! Providers: **OpenRouter** (public, no key), **Gemini** (Google
//! generativelanguage, vault-backed key), **OpenAI** and **Anthropic** (vault
//! keys), **Ollama** (local `/api/tags` — per-hotel by design), and **MLX**
//! (operator-declared `mlx_available_models` config list). Each provider is
//! fetched independently; a missing key or unreachable local server skips
//! that provider without failing the pass.
//!
//! Each pass also assembles the MODEL-GRAPH PROJECTION payload
//! (`model_graph.projection`): catalogs + agents + role ladders/bindings in
//! one config node, consumed by the Memgraph ingest on vps-jane (a DERIVED
//! analytical read-model — never routing authority).
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
    CatalogDiffEvent, CatalogDiffKind, DiscoveredModel, diff_catalog, parse_anthropic_models,
    parse_google_models, parse_ollama_models, parse_openai_models, parse_openrouter_models,
};
use ansible_mesh_core::model_routing::{
    DEFAULT_FALLBACK_TIERS, ProviderRouting, RoutingImpact, routing_impact_for_model,
};
use ansible_mesh_core::provider_keys::{provider_key_spec, provider_key_specs};

/// Providers this job knows how to fetch, in sync order. Each gets its own
/// `model_catalog.<provider>` compact node and
/// `model_catalog_discovery.<provider>` diff snapshot.
const PROVIDERS: &[&str] = &["openrouter", "gemini", "openai", "anthropic", "ollama", "mlx"];
/// Config-node key holding the COMPACT queryable catalog guests read over
/// `GetConfig` — the hotel's "possible models" surface. philote's `/models`
/// drill-down and `/model` tool badges consume this instead of each guest
/// fetching OpenRouter itself. Kept separate from [`SNAPSHOT_KEY`] (full
/// `DiscoveredModel` records, diff-only) so the guest payload stays small:
/// one terse object per model —
/// `{"id","name","tools":bool?,"ctx":u32?,"in":f64?,"out":f64?,"think":bool?}`.
pub const CATALOG_KEY: &str = "model_catalog.openrouter";
/// Config-node key holding the assembled model-graph projection payload
/// (catalogs + agents + role ladders/bindings) the Memgraph ingest consumes.
pub const PROJECTION_KEY: &str = "model_graph.projection";
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

/// One discovery pass over every provider: fetch → diff vs persisted
/// snapshot → persist compact catalog → alert, each provider independent (a
/// missing key or unreachable local server skips that provider). Ends by
/// assembling the model-graph projection payload.
pub async fn run_once(
    graph: &GraphDomain,
    heal: Option<&Arc<dyn HealQueueStorage>>,
    http: &reqwest::Client,
) -> Result<()> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .ok();

    let mut catalogs: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
        std::collections::BTreeMap::new();
    for provider in PROVIDERS {
        match fetch_provider_models(graph, http, provider, now_secs).await {
            Ok(Some(discovered)) => {
                if let Err(e) = sync_provider(graph, heal, provider, &discovered) {
                    warn!("model-catalog-sync: [{provider}] sync failed: {e:#}");
                    continue;
                }
                catalogs.insert(provider.to_string(), compact_catalog(&discovered));
            }
            Ok(None) => {
                info!(provider, "model-catalog-sync: skipped (no key/config or local server absent)");
            }
            Err(e) => {
                warn!("model-catalog-sync: [{provider}] fetch failed: {e:#}");
            }
        }
    }

    if let Err(e) = persist_model_graph_projection(graph, &catalogs, now_secs) {
        warn!("model-catalog-sync: projection persist failed: {e:#}");
    }
    Ok(())
}

/// Fetch one provider's live model list. `Ok(None)` = provider intentionally
/// skipped (no key configured, local server absent, no MLX list declared).
async fn fetch_provider_models(
    graph: &GraphDomain,
    http: &reqwest::Client,
    provider: &str,
    now_secs: Option<u64>,
) -> Result<Option<Vec<DiscoveredModel>>> {
    match provider {
        "openrouter" => {
            let base = provider_base(graph, "openrouter");
            let body = fetch_text(http.get(format!("{base}/v1/models"))).await?;
            Ok(Some(parse_openrouter_models(&body, now_secs)?))
        }
        "gemini" => {
            let Some(key) = provider_api_key(graph, "gemini") else {
                return Ok(None);
            };
            let base = provider_base(graph, "gemini");
            let body = fetch_text(
                http.get(format!("{base}/v1beta/models"))
                    .query(&[("key", key.as_str()), ("pageSize", "1000")]),
            )
            .await?;
            Ok(Some(parse_google_models(&body, now_secs)?))
        }
        "openai" => {
            let Some(key) = provider_api_key(graph, "openai") else {
                return Ok(None);
            };
            let base = provider_base(graph, "openai");
            let body = fetch_text(
                http.get(format!("{base}/v1/models"))
                    .header("Authorization", format!("Bearer {key}")),
            )
            .await?;
            Ok(Some(parse_openai_models(&body, now_secs)?))
        }
        "anthropic" => {
            let Some(key) = provider_api_key(graph, "anthropic") else {
                return Ok(None);
            };
            let base = provider_base(graph, "anthropic");
            let body = fetch_text(
                http.get(format!("{base}/v1/models?limit=1000"))
                    .header("x-api-key", key)
                    .header("anthropic-version", "2023-06-01"),
            )
            .await?;
            Ok(Some(parse_anthropic_models(&body, now_secs)?))
        }
        "ollama" => {
            let base = config_string(graph, "ollama_base_url")
                .unwrap_or_else(|| "http://localhost:11434".to_string());
            let base = base.trim_end_matches('/');
            // A hotel without a local Ollama is normal — connection refusal
            // is a skip, not an error.
            let response = match http.get(format!("{base}/api/tags")).send().await {
                Ok(r) => r,
                Err(_) => return Ok(None),
            };
            let body = response
                .error_for_status()
                .context("ollama /api/tags returned an error status")?
                .text()
                .await
                .context("read ollama /api/tags body")?;
            Ok(Some(parse_ollama_models(&body, now_secs)?))
        }
        "mlx" => {
            // MLX has no listing endpoint; the operator declares installed
            // models via the `mlx_available_models` config key (JSON array).
            let Some(raw) = graph.get_config_value("mlx_available_models")? else {
                return Ok(None);
            };
            let ids: Vec<String> = serde_json::from_str(&raw).unwrap_or_default();
            if ids.is_empty() {
                return Ok(None);
            }
            Ok(Some(
                ids.into_iter()
                    .map(|id| DiscoveredModel {
                        provider: "mlx".to_string(),
                        endpoint_family: "mlx-local".to_string(),
                        model_ref: id.clone(),
                        provider_model_ref: id,
                        display_name: None,
                        context_window_tokens: None,
                        input_cost_per_million: Some(0.0),
                        output_cost_per_million: Some(0.0),
                        modalities: vec!["text".to_string()],
                        reasoning_default: None,
                        supports_tools: None,
                        declared_task_kinds: vec!["text.generate".to_string()],
                        lifecycle_hint: None,
                        source_url: "config:mlx_available_models".to_string(),
                        fetched_at_secs: now_secs,
                    })
                    .collect(),
            ))
        }
        other => anyhow::bail!("unknown catalog provider '{other}'"),
    }
}

/// GET a URL and return the body text, folding HTTP-status errors in.
async fn fetch_text(request: reqwest::RequestBuilder) -> Result<String> {
    request
        .send()
        .await
        .context("fetch model list")?
        .error_for_status()
        .context("model list returned an error status")?
        .text()
        .await
        .context("read model list body")
}

/// Resolve a provider's API key: process env first (ephemeral/CI), then the
/// vault-backed config ref. `None` = provider not configured on this hotel.
fn provider_api_key(graph: &GraphDomain, provider: &str) -> Option<String> {
    let spec = provider_key_spec(provider)?;
    if let Ok(v) = std::env::var(spec.env_api_key) {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    let secret_ref = config_string(graph, spec.api_key_ref_key)?;
    crate::vault::resolve_secret(
        graph,
        &secret_ref,
        &crate::vault::SecretAccess {
            role: "model".to_string(),
            guest_id: GUEST_ID.to_string(),
        },
    )
    .ok()
    .flatten()
}

/// Provider base URL: configured override, else the spec default. Trailing
/// slashes and `/v1` are normalized off so path joins stay predictable.
fn provider_base(graph: &GraphDomain, provider: &str) -> String {
    let spec = provider_key_spec(provider);
    let configured = spec
        .and_then(|s| s.base_url_key)
        .and_then(|key| config_string(graph, key));
    let base = configured
        .or_else(|| {
            spec.and_then(|s| s.default_base_url)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "https://openrouter.ai/api".to_string());
    base.trim_end_matches('/')
        .trim_end_matches("/v1")
        .trim_end_matches('/')
        .to_string()
}

/// Diff one provider's snapshot, persist compact catalog + snapshot, alert.
fn sync_provider(
    graph: &GraphDomain,
    heal: Option<&Arc<dyn HealQueueStorage>>,
    provider: &str,
    discovered: &[DiscoveredModel],
) -> Result<()> {
    let snapshot_key = format!("model_catalog_discovery.{provider}");
    let catalog_key = format!("model_catalog.{provider}");

    let prev: Vec<DiscoveredModel> = match graph.get_config_value(&snapshot_key)? {
        Some(json) => serde_json::from_str(&json).unwrap_or_default(),
        None => Vec::new(),
    };
    let first_run = prev.is_empty();
    let diffs = diff_catalog(&prev, discovered);

    // Persist the guest-facing compact catalog FIRST — even a first run makes
    // "possible models" immediately queryable — then the diff snapshot before
    // alerting so a restart mid-run can't cause the same change to alert twice.
    graph.set_config_value(&catalog_key, &serde_json::to_string(&compact_catalog(discovered))?)?;
    graph.set_config_value(&snapshot_key, &serde_json::to_string(discovered)?)?;

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
        provider,
        models = discovered.len(),
        diffs = diffs.len(),
        alerts,
        first_run,
        "model-catalog-sync: catalog synced"
    );
    Ok(())
}

/// Assemble and persist the model-graph projection payload: every provider's
/// compact catalog plus this hotel's agents and role ladders/bindings, in one
/// config node the Memgraph ingest (vps-jane, co-located with the LifeGraph
/// runner) consumes over `GetConfig`. Derived read-model input only — nothing
/// on the dispatch path reads this.
fn persist_model_graph_projection(
    graph: &GraphDomain,
    catalogs: &std::collections::BTreeMap<String, Vec<serde_json::Value>>,
    now_secs: Option<u64>,
) -> Result<()> {
    let hotel =
        std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string());
    let agents: Vec<String> = graph
        .list_agent_identities()
        .unwrap_or_default()
        .into_iter()
        .map(|a| a.agent_id)
        .collect();
    let roles: Vec<serde_json::Value> = graph
        .list_all_role_incarnations()
        .unwrap_or_default()
        .into_iter()
        .map(|rec| {
            let ladder = if rec.turn_loop_config.fallback_tiers.is_empty() {
                DEFAULT_FALLBACK_TIERS
                    .iter()
                    .map(|t| t.to_string())
                    .collect()
            } else {
                rec.turn_loop_config.fallback_tiers.clone()
            };
            serde_json::json!({
                "agent_id": rec.agent_id,
                "role_name": rec.role_name,
                "guest_id": rec.guest_id,
                "ladder": ladder,
                "bindings": rec.turn_loop_config.model_bindings,
                "content_policy": rec.content_policy,
            })
        })
        .collect();
    let payload = serde_json::json!({
        "hotel": hotel,
        "generated_at": now_secs,
        "providers": catalogs,
        "agents": agents,
        "roles": roles,
    });
    graph.set_config_value(PROJECTION_KEY, &serde_json::to_string(&payload)?)
}

/// Project the full discovery snapshot into the compact guest-facing catalog
/// (see [`CATALOG_KEY`]). Unreported fields are omitted, not null.
fn compact_catalog(models: &[DiscoveredModel]) -> Vec<serde_json::Value> {
    models
        .iter()
        .map(|m| {
            let mut entry = serde_json::json!({ "id": m.model_ref });
            if let Some(name) = &m.display_name {
                entry["name"] = serde_json::json!(name);
            }
            if let Some(tools) = m.supports_tools {
                entry["tools"] = serde_json::json!(tools);
            }
            if let Some(ctx) = m.context_window_tokens {
                entry["ctx"] = serde_json::json!(ctx);
            }
            if let Some(cost) = m.input_cost_per_million {
                entry["in"] = serde_json::json!(cost);
            }
            if let Some(cost) = m.output_cost_per_million {
                entry["out"] = serde_json::json!(cost);
            }
            if let Some(think) = m.reasoning_default {
                entry["think"] = serde_json::json!(think);
            }
            entry
        })
        .collect()
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

    #[test]
    fn compact_catalog_projects_tools_and_context() {
        let model = DiscoveredModel {
            provider: "openrouter".into(),
            endpoint_family: "openrouter-hosted".into(),
            model_ref: "sao10k/l3.1-euryale-70b".into(),
            provider_model_ref: "sao10k/l3.1-euryale-70b".into(),
            display_name: Some("Euryale 70B".into()),
            context_window_tokens: Some(16384),
            input_cost_per_million: Some(0.7),
            output_cost_per_million: Some(0.8),
            modalities: vec!["text".into()],
            reasoning_default: None,
            supports_tools: Some(true),
            declared_task_kinds: vec!["text.generate".into()],
            lifecycle_hint: None,
            source_url: "https://openrouter.ai/api/v1/models".into(),
            fetched_at_secs: Some(1),
        };
        let compact = compact_catalog(&[model]);
        assert_eq!(compact.len(), 1);
        assert_eq!(compact[0]["id"], "sao10k/l3.1-euryale-70b");
        assert_eq!(compact[0]["name"], "Euryale 70B");
        assert_eq!(compact[0]["tools"], true);
        assert_eq!(compact[0]["ctx"], 16384);
        assert!(
            compact[0].get("think").is_none(),
            "unreported fields are omitted"
        );
    }

    #[test]
    fn projection_payload_includes_roles_and_catalogs() {
        use ansible_mesh_core::graph::{RoleIncarnationRecord, RoleReadinessState, TurnLoopConfig};
        use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;

        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(std::sync::Arc::new(graph_store.adapter()));
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-jane".into(),
                role_name: "vixen".into(),
                guest_id: "agent-jane:vixen".into(),
                toolset_profile: "orchestrator".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig {
                    fallback_tiers: vec!["model.openrouter".into()],
                    model_bindings: [(
                        "model.openrouter".to_string(),
                        "sao10k/l3.1-euryale-70b".to_string(),
                    )]
                    .into_iter()
                    .collect(),
                    ..Default::default()
                },
                home_node: None,
                ..Default::default()
            })
            .expect("seed role");

        let mut catalogs = std::collections::BTreeMap::new();
        catalogs.insert(
            "openrouter".to_string(),
            vec![serde_json::json!({"id": "sao10k/l3.1-euryale-70b", "tools": true})],
        );
        persist_model_graph_projection(&graph, &catalogs, Some(42)).expect("persist projection");

        let raw = graph
            .get_config_value(PROJECTION_KEY)
            .expect("read projection")
            .expect("projection present");
        let payload: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
        assert_eq!(payload["generated_at"], 42);
        assert_eq!(payload["providers"]["openrouter"][0]["id"], "sao10k/l3.1-euryale-70b");
        let role = &payload["roles"][0];
        assert_eq!(role["role_name"], "vixen");
        assert_eq!(role["ladder"][0], "model.openrouter");
        assert_eq!(
            role["bindings"]["model.openrouter"],
            "sao10k/l3.1-euryale-70b"
        );
    }
}
