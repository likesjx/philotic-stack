//! Model routing oracle: deterministic, health-aware provider ranking.
//!
//! Sits beside [`crate::model_routing`] (ladder validation / catalog impact)
//! and answers a different question: given the live [`ModelProfileRecord`]s
//! and what a task actually needs, which providers are currently the best
//! fit, in order? The motivating incident: Gemini returned 400s on every
//! tool-turn for three days while a healthy OpenRouter controller sat
//! unconsulted — a static ladder can go stale, the oracle cannot.
//!
//! Three pure pieces live here so the hotel (IPC `QueryModelRoute` handler),
//! the model-router (health observation), and tests all share one truth:
//!
//! 1. [`rank_models`] — hard filters (status, context fit, trust ceiling,
//!    capability flags) then a weighted score (health, latency fit, tier
//!    fit). Fully deterministic given `now_secs`.
//! 2. [`apply_model_outcome`] — the health state machine: EMA error/latency,
//!    degrade after N consecutive failures, recover on first success. The
//!    cool-off recovery half lives in `rank_models` (a degraded record whose
//!    last update is older than the cool-off window becomes a probe
//!    candidate again, at a score penalty).
//! 3. Provider seed facts — [`seed_capabilities_for_provider`] and
//!    [`controller_role_for_provider`] so profile creation and role mapping
//!    stay honest and in one place.
//!
//! Operator intent always wins: the oracle is only consulted *beneath* the
//! configured `fallback_tiers` ladder, when that ladder is exhausted.

use crate::graph::ModelProfileRecord;

/// Consecutive failures before a profile is stamped `degraded`.
/// Override with `PHILOTIC_MODEL_DEGRADE_THRESHOLD`.
pub const DEFAULT_DEGRADE_THRESHOLD: u32 = 3;

/// Seconds a `degraded` profile stays hard-excluded from ranking before the
/// oracle re-admits it as a probe candidate (at a score penalty).
/// Override with `PHILOTIC_MODEL_DEGRADE_COOLOFF_SECS`.
pub const DEFAULT_DEGRADE_COOLOFF_SECS: u64 = 300;

/// EMA smoothing for latency_p50_ms (matches the field docs on
/// [`ModelProfileRecord`]).
const LATENCY_ALPHA: f64 = 0.25;
/// EMA smoothing for error_rate (α=0.1 per the field docs).
const ERROR_ALPHA: f32 = 0.1;

/// `PHILOTIC_MODEL_DEGRADE_THRESHOLD`, defaulting to
/// [`DEFAULT_DEGRADE_THRESHOLD`]. Zero/garbage falls back to the default.
pub fn degrade_threshold_from_env() -> u32 {
    std::env::var("PHILOTIC_MODEL_DEGRADE_THRESHOLD")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_DEGRADE_THRESHOLD)
}

/// `PHILOTIC_MODEL_DEGRADE_COOLOFF_SECS`, defaulting to
/// [`DEFAULT_DEGRADE_COOLOFF_SECS`].
pub fn degrade_cooloff_secs_from_env() -> u64 {
    std::env::var("PHILOTIC_MODEL_DEGRADE_COOLOFF_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_DEGRADE_COOLOFF_SECS)
}

/// Kill switch: `PHILOTIC_DISABLE_ROUTING_ORACLE=1` disables oracle-driven
/// rerouting everywhere (philote consult + hotel handler both check it).
pub fn routing_oracle_disabled() -> bool {
    matches!(
        std::env::var("PHILOTIC_DISABLE_ROUTING_ORACLE")
            .ok()
            .as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Route need
// ─────────────────────────────────────────────────────────────────────────────

/// How urgently the caller needs an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatencyClass {
    /// A user is waiting on this turn: prefer fast, low-latency providers.
    Interactive,
    /// Batch / background work: heavy, slower providers are fine.
    Background,
}

impl LatencyClass {
    /// Parse from the wire form; anything unrecognised is `Interactive`
    /// (the conservative choice — a background task routed fast is fine,
    /// an interactive turn routed slow is not).
    pub fn parse(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "background" => LatencyClass::Background,
            _ => LatencyClass::Interactive,
        }
    }
}

/// What the task at hand needs from a model — derived from the request, not
/// configured. Drives both the hard filters and the score weights.
#[derive(Debug, Clone)]
pub struct RouteNeed {
    /// The cognitive request class from the model request payload
    /// ("cognitive", "transform", "synthesis", "embedding", ...).
    pub request_class: String,
    /// The turn presents tools the model may call.
    pub needs_tools: bool,
    /// The turn requires a structured (JSON-contract) response.
    pub needs_structured: bool,
    /// Rough prompt+context size; 0 = unknown (context filter is skipped).
    pub approx_context_tokens: u32,
    pub latency_class: LatencyClass,
    /// Most-permissive trust tier the data may route to
    /// ("local_trusted" < "local_experimental" < "remote_cloud").
    pub trust_ceiling: String,
}

/// One ranked candidate out of [`rank_models`].
#[derive(Debug, Clone, PartialEq)]
pub struct RankedModel {
    pub provider: String,
    pub model_ref: String,
    pub node_id: String,
    /// Composite score in [0, 1]; higher is better.
    pub score: f32,
    /// Human-readable scoring notes ("degraded_probe", "stale_health", ...).
    pub reasons: Vec<String>,
}

/// Map a request class onto the task kind a profile must support.
pub fn task_kind_for_request_class(request_class: &str) -> &'static str {
    match request_class {
        "embedding" => "text.embed",
        "synthesis" => "voice.synthesize",
        _ => "text.generate",
    }
}

/// Trust tier ordering: lower rank = more trusted placement. Unknown tiers
/// rank as `remote_cloud` so they are excluded under a tight ceiling rather
/// than sneaking sensitive data to an unclassified provider.
pub fn trust_tier_rank(tier: &str) -> u8 {
    match tier {
        "local_trusted" => 0,
        "local_experimental" => 1,
        _ => 2, // remote_cloud and unknown
    }
}

/// The controller role that dispatches to `provider` on a hotel.
///
/// Mirrors the guest seeding in aiua: the gemini-backed controller owns the
/// bare "model" role, the ONNX controller owns "model.local", everything
/// else follows the `model.<provider>` convention.
pub fn controller_role_for_provider(provider: &str) -> String {
    match provider {
        "gemini" => "model".to_string(),
        "onnx" => "model.local".to_string(),
        other => format!("model.{other}"),
    }
}

/// Honest capability seed per provider: (supports_tools, supports_structured).
///
/// Cloud chat providers (anthropic/gemini/openai/openrouter) all expose
/// native tool calling and structured output. Local/experimental backends
/// (ollama, mlx, onnx) and the audio provider (elevenlabs) do not reliably —
/// seed them `false` so a tools-required turn never routes there.
pub fn seed_capabilities_for_provider(provider: &str) -> (bool, bool) {
    match provider {
        "anthropic" | "gemini" | "openai" | "openrouter" => (true, true),
        _ => (false, false),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Ranking
// ─────────────────────────────────────────────────────────────────────────────

const WEIGHT_HEALTH: f32 = 0.5;
const WEIGHT_LATENCY: f32 = 0.3;
const WEIGHT_TIER: f32 = 0.2;
/// Score multiplier for a degraded provider re-admitted after cool-off.
const DEGRADED_PROBE_PENALTY: f32 = 0.5;

/// Rank `candidates` for `need` using the default cool-off window.
pub fn rank_models(
    candidates: &[ModelProfileRecord],
    need: &RouteNeed,
    now_secs: u64,
) -> Vec<RankedModel> {
    rank_models_with(candidates, need, now_secs, DEFAULT_DEGRADE_COOLOFF_SECS)
}

/// Rank `candidates` for `need`. Pure and deterministic: same inputs, same
/// output. Hard filters first, then a weighted score; ties break on
/// (provider, model_ref) so ordering is stable.
pub fn rank_models_with(
    candidates: &[ModelProfileRecord],
    need: &RouteNeed,
    now_secs: u64,
    degrade_cooloff_secs: u64,
) -> Vec<RankedModel> {
    let task_kind = task_kind_for_request_class(&need.request_class);
    let ceiling_rank = trust_tier_rank(&need.trust_ceiling);

    let mut ranked: Vec<RankedModel> = candidates
        .iter()
        .filter_map(|profile| {
            let mut reasons: Vec<String> = Vec::new();

            // ── Hard filters ────────────────────────────────────────────
            match profile.status.as_str() {
                "retired" | "unavailable" => return None,
                "degraded" => {
                    // Cool-off recovery: a degraded record that has not been
                    // updated within the cool-off window becomes a probe
                    // candidate again; a freshly degraded one stays frozen.
                    let age = now_secs.saturating_sub(profile.updated_secs);
                    if age < degrade_cooloff_secs {
                        return None;
                    }
                    reasons.push("degraded_probe".to_string());
                }
                _ => {}
            }

            if !profile.task_kinds.is_empty() && !profile.task_kinds.iter().any(|t| t == task_kind)
            {
                return None;
            }
            if trust_tier_rank(&profile.trust_tier) > ceiling_rank {
                return None;
            }
            if need.approx_context_tokens > 0
                && profile.max_context_tokens > 0
                && need.approx_context_tokens > profile.max_context_tokens
            {
                return None;
            }
            if need.needs_tools && !profile.supports_tools {
                return None;
            }
            if need.needs_structured && !profile.supports_structured {
                return None;
            }

            // ── Weighted score ──────────────────────────────────────────
            // Health: error-rate EMA discounted by how stale the last
            // success is. A provider that has not succeeded recently is
            // suspect even if its EMA looks clean.
            let error_health = (1.0 - profile.error_rate).clamp(0.0, 1.0);
            let recency = if profile.last_healthy_secs == 0 {
                reasons.push("never_observed_healthy".to_string());
                0.8
            } else {
                let since = now_secs.saturating_sub(profile.last_healthy_secs);
                if since <= 600 {
                    1.0
                } else if since <= 3600 {
                    0.9
                } else {
                    reasons.push("stale_health".to_string());
                    0.7
                }
            };
            let health = error_health * recency;

            // Latency fit: unknown latency scores neutral; Interactive
            // penalises slow providers much harder than Background.
            let latency = if profile.latency_p50_ms == 0 {
                0.7
            } else {
                let half_life_ms: f64 = match need.latency_class {
                    LatencyClass::Interactive => 2_000.0,
                    LatencyClass::Background => 20_000.0,
                };
                (1.0 / (1.0 + profile.latency_p50_ms as f64 / half_life_ms)) as f32
            };

            // Tier fit: Interactive prefers the fast tier; Background
            // tolerates (mildly prefers) heavy.
            let tier = match (need.latency_class, profile.model_tier.as_str()) {
                (LatencyClass::Interactive, "fast") => 1.0,
                (LatencyClass::Interactive, "heavy") => 0.5,
                (LatencyClass::Background, "heavy") => 1.0,
                (LatencyClass::Background, "fast") => 0.85,
                _ => 0.75,
            };

            let mut score = WEIGHT_HEALTH * health + WEIGHT_LATENCY * latency + WEIGHT_TIER * tier;
            if reasons.iter().any(|r| r == "degraded_probe") {
                score *= DEGRADED_PROBE_PENALTY;
            }

            Some(RankedModel {
                provider: profile.provider.clone(),
                model_ref: profile.model_ref.clone(),
                node_id: profile.node_id.clone(),
                score,
                reasons,
            })
        })
        .collect();

    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.provider.cmp(&b.provider))
            .then_with(|| a.model_ref.cmp(&b.model_ref))
    });
    ranked
}

// ─────────────────────────────────────────────────────────────────────────────
// Health state machine
// ─────────────────────────────────────────────────────────────────────────────

/// Fold one dispatch outcome into a profile: EMA latency (α=0.25) and error
/// rate (α=0.1); degrade after `degrade_threshold` consecutive failures (or
/// when the error EMA crosses 0.5, preserving the pre-oracle behaviour);
/// recover to healthy on the first success. Cool-off recovery is the
/// oracle's side (see [`rank_models_with`]), so a degraded provider that
/// stays silent still gets probed again.
pub fn apply_model_outcome(
    profile: &mut ModelProfileRecord,
    latency_ms: u64,
    success: bool,
    now_secs: u64,
    degrade_threshold: u32,
) {
    profile.latency_p50_ms = ((1.0 - LATENCY_ALPHA) * profile.latency_p50_ms as f64
        + LATENCY_ALPHA * latency_ms as f64) as u64;
    let outcome = if success { 0.0f32 } else { 1.0f32 };
    profile.error_rate = (1.0 - ERROR_ALPHA) * profile.error_rate + ERROR_ALPHA * outcome;

    if success {
        profile.consecutive_failures = 0;
        profile.last_healthy_secs = now_secs;
        profile.status = "healthy".to_string();
    } else {
        profile.consecutive_failures = profile.consecutive_failures.saturating_add(1);
        if profile.consecutive_failures >= degrade_threshold.max(1) || profile.error_rate > 0.5 {
            profile.status = "degraded".to_string();
        }
    }
    profile.updated_secs = now_secs;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(provider: &str) -> ModelProfileRecord {
        ModelProfileRecord {
            model_ref: provider.to_string(),
            node_id: "test-aiua-01".to_string(),
            provider: provider.to_string(),
            task_kinds: vec!["text.generate".to_string()],
            trust_tier: "remote_cloud".to_string(),
            latency_p50_ms: 1000,
            last_healthy_secs: NOW - 60,
            updated_secs: NOW - 60,
            ..Default::default()
        }
    }

    fn need() -> RouteNeed {
        RouteNeed {
            request_class: "cognitive".to_string(),
            needs_tools: true,
            needs_structured: true,
            approx_context_tokens: 0,
            latency_class: LatencyClass::Interactive,
            trust_ceiling: "remote_cloud".to_string(),
        }
    }

    const NOW: u64 = 1_800_000_000;

    // ── rank_models filter matrix ────────────────────────────────────────

    #[test]
    fn healthy_provider_ranks() {
        let ranked = rank_models(&[profile("anthropic")], &need(), NOW);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].provider, "anthropic");
        assert!(ranked[0].score > 0.0);
    }

    #[test]
    fn unavailable_and_retired_are_excluded() {
        for status in ["unavailable", "retired"] {
            let mut p = profile("gemini");
            p.status = status.to_string();
            assert!(rank_models(&[p], &need(), NOW).is_empty(), "{status}");
        }
    }

    #[test]
    fn freshly_degraded_is_frozen_out() {
        let mut p = profile("gemini");
        p.status = "degraded".to_string();
        p.updated_secs = NOW - 10; // inside cool-off
        assert!(rank_models(&[p], &need(), NOW).is_empty());
    }

    #[test]
    fn degraded_past_cooloff_is_probe_candidate_with_penalty() {
        let mut degraded = profile("gemini");
        degraded.status = "degraded".to_string();
        degraded.updated_secs = NOW - DEFAULT_DEGRADE_COOLOFF_SECS - 1;
        let healthy = profile("anthropic");
        let ranked = rank_models(&[degraded, healthy], &need(), NOW);
        assert_eq!(ranked.len(), 2);
        // Healthy provider outranks the probe.
        assert_eq!(ranked[0].provider, "anthropic");
        assert_eq!(ranked[1].provider, "gemini");
        assert!(ranked[1].reasons.contains(&"degraded_probe".to_string()));
        assert!(ranked[1].score < ranked[0].score * 0.75);
    }

    #[test]
    fn trust_ceiling_excludes_remote_providers() {
        let remote = profile("anthropic"); // remote_cloud
        let mut local = profile("ollama");
        local.trust_tier = "local_trusted".to_string();
        local.supports_tools = true;
        local.supports_structured = true;
        let mut n = need();
        n.trust_ceiling = "local_trusted".to_string();
        let ranked = rank_models(&[remote, local], &n, NOW);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].provider, "ollama");
    }

    #[test]
    fn unknown_trust_tier_treated_as_remote() {
        let mut p = profile("mystery");
        p.trust_tier = String::new();
        let mut n = need();
        n.trust_ceiling = "local_experimental".to_string();
        assert!(rank_models(&[p], &n, NOW).is_empty());
    }

    #[test]
    fn context_window_filter_applies_only_when_both_known() {
        let mut small = profile("gemini");
        small.max_context_tokens = 10_000;
        let mut unknown = profile("anthropic");
        unknown.max_context_tokens = 0;
        let mut n = need();
        n.approx_context_tokens = 50_000;
        let ranked = rank_models(&[small, unknown], &n, NOW);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].provider, "anthropic");
    }

    #[test]
    fn capability_flags_filter_tool_turns() {
        let mut no_tools = profile("ollama");
        no_tools.supports_tools = false;
        no_tools.supports_structured = true;
        let ranked = rank_models(&[no_tools.clone()], &need(), NOW);
        assert!(ranked.is_empty(), "tools-required turn must skip ollama");

        // A turn that doesn't need tools may still use it.
        let mut n = need();
        n.needs_tools = false;
        let ranked = rank_models(&[no_tools], &n, NOW);
        assert_eq!(ranked.len(), 1);
    }

    #[test]
    fn task_kind_mismatch_excludes() {
        let mut voice = profile("elevenlabs");
        voice.task_kinds = vec!["voice.synthesize".to_string()];
        assert!(rank_models(&[voice], &need(), NOW).is_empty());
    }

    // ── scoring ──────────────────────────────────────────────────────────

    #[test]
    fn lower_error_rate_wins() {
        let mut flaky = profile("gemini");
        flaky.error_rate = 0.4;
        let clean = profile("anthropic");
        let ranked = rank_models(&[flaky, clean], &need(), NOW);
        assert_eq!(ranked[0].provider, "anthropic");
    }

    #[test]
    fn interactive_prefers_fast_tier_and_low_latency() {
        let mut fast = profile("anthropic");
        fast.model_tier = "fast".to_string();
        fast.latency_p50_ms = 400;
        let mut heavy = profile("openai");
        heavy.model_tier = "heavy".to_string();
        heavy.latency_p50_ms = 6_000;
        let ranked = rank_models(&[heavy.clone(), fast.clone()], &need(), NOW);
        assert_eq!(ranked[0].provider, "anthropic");

        // Background flips the tier preference (latency neutralised).
        fast.latency_p50_ms = 1_000;
        heavy.latency_p50_ms = 1_000;
        let mut n = need();
        n.latency_class = LatencyClass::Background;
        let ranked = rank_models(&[fast, heavy], &n, NOW);
        assert_eq!(ranked[0].provider, "openai");
    }

    #[test]
    fn stale_last_healthy_is_discounted() {
        let mut stale = profile("gemini");
        stale.last_healthy_secs = NOW - 100_000;
        let fresh = profile("anthropic");
        let ranked = rank_models(&[stale, fresh], &need(), NOW);
        assert_eq!(ranked[0].provider, "anthropic");
        assert!(ranked[1].reasons.contains(&"stale_health".to_string()));
    }

    #[test]
    fn ranking_is_deterministic_on_ties() {
        let a = profile("gemini");
        let b = profile("anthropic");
        let r1 = rank_models(&[a.clone(), b.clone()], &need(), NOW);
        let r2 = rank_models(&[b, a], &need(), NOW);
        assert_eq!(r1, r2);
        assert_eq!(r1[0].provider, "anthropic"); // tie → provider name order
    }

    // ── health state machine ─────────────────────────────────────────────

    #[test]
    fn degrades_after_three_consecutive_failures() {
        let mut p = profile("gemini");
        apply_model_outcome(&mut p, 500, false, NOW, DEFAULT_DEGRADE_THRESHOLD);
        assert_eq!(p.status, "healthy");
        assert_eq!(p.consecutive_failures, 1);
        apply_model_outcome(&mut p, 500, false, NOW + 1, DEFAULT_DEGRADE_THRESHOLD);
        assert_eq!(p.status, "healthy");
        apply_model_outcome(&mut p, 500, false, NOW + 2, DEFAULT_DEGRADE_THRESHOLD);
        assert_eq!(p.status, "degraded");
        assert_eq!(p.consecutive_failures, 3);
        // Error EMA moved up but nowhere near the 0.5 legacy threshold —
        // the consecutive counter is what tripped the transition.
        assert!(p.error_rate < 0.5);
    }

    #[test]
    fn first_success_recovers_and_resets_counter() {
        let mut p = profile("gemini");
        for i in 0..5 {
            apply_model_outcome(&mut p, 500, false, NOW + i, 3);
        }
        assert_eq!(p.status, "degraded");
        apply_model_outcome(&mut p, 500, true, NOW + 10, 3);
        assert_eq!(p.status, "healthy");
        assert_eq!(p.consecutive_failures, 0);
        assert_eq!(p.last_healthy_secs, NOW + 10);
    }

    #[test]
    fn error_ema_uses_alpha_point_one() {
        let mut p = profile("gemini");
        p.error_rate = 0.0;
        apply_model_outcome(&mut p, 500, false, NOW, 3);
        assert!((p.error_rate - 0.1).abs() < 1e-6);
        apply_model_outcome(&mut p, 500, true, NOW + 1, 3);
        assert!((p.error_rate - 0.09).abs() < 1e-6);
    }

    #[test]
    fn intermittent_failures_never_trip_consecutive_threshold() {
        let mut p = profile("gemini");
        for i in 0..10 {
            apply_model_outcome(&mut p, 500, i % 2 == 0, NOW + i, 3);
        }
        assert_eq!(p.status, "healthy");
        assert!(p.consecutive_failures <= 1);
    }

    // ── seed facts ───────────────────────────────────────────────────────

    #[test]
    fn capability_seeds_are_honest_per_provider() {
        for cloud in ["anthropic", "gemini", "openai", "openrouter"] {
            assert_eq!(seed_capabilities_for_provider(cloud), (true, true));
        }
        for local in ["ollama", "mlx", "onnx", "elevenlabs"] {
            assert_eq!(seed_capabilities_for_provider(local), (false, false));
        }
    }

    #[test]
    fn controller_role_mapping_matches_hotel_seeding() {
        assert_eq!(controller_role_for_provider("gemini"), "model");
        assert_eq!(controller_role_for_provider("onnx"), "model.local");
        assert_eq!(controller_role_for_provider("anthropic"), "model.anthropic");
        assert_eq!(
            controller_role_for_provider("openrouter"),
            "model.openrouter"
        );
    }

    #[test]
    fn legacy_profile_json_deserializes_with_permissive_defaults() {
        // A record written before the oracle fields existed.
        let legacy = serde_json::json!({
            "model_ref": "gemini",
            "node_id": "n1",
            "provider": "gemini",
            "latency_p50_ms": 900,
            "error_rate": 0.05,
            "status": "healthy"
        });
        let p: ModelProfileRecord = serde_json::from_value(legacy).unwrap();
        assert!(p.supports_tools);
        assert!(p.supports_structured);
        assert_eq!(p.model_tier, "");
        assert_eq!(p.consecutive_failures, 0);
    }
}
