//! Shared model-routing coherence helpers.
//!
//! Three concerns keep model routing honest end-to-end and are collected here so
//! the hotel (config-time validation, catalog-alert wiring) and the agent loop
//! (default fallback ladder) read from one source of truth:
//!
//! 1. [`DEFAULT_FALLBACK_TIERS`] — the canonical fallback ladder philote uses when
//!    a role configures none. Living in core lets the hotel's startup validation
//!    check the *same* ladder philote would actually run.
//! 2. [`validate_fallback_ladders`] — pure check that every tier a ladder names
//!    resolves to a live controller role. Escalating into a tier with no seeded
//!    controller targets a void (the failure mode behind the gemini thinking-hang
//!    incident); this surfaces it at config time instead of mid-turn.
//! 3. [`routing_impact_for_model`] — maps a catalog retirement / thinking-flip
//!    onto the ladders (and default_model) that route through it, so a catalog
//!    alert becomes a *routing* alert naming the affected ladder.

use std::collections::BTreeSet;

/// Default tier ordering used when a role's `TurnLoopConfig` configures no
/// `fallback_tiers`. Tier 0 is attempted first; on retriable failure the loop
/// advances to the next tier.
///
/// Ordering rationale (operator directive, 2026-07): gemini (`model`) is the
/// primary; `model.openrouter` is the first fallback because OpenRouter
/// controllers are live on all hotels and cloud reliability beats local;
/// `model.ollama` is the local last resort — ollama is unstable and must never
/// be the first fallback.
///
/// NOTE: `model.ollama` is intentionally listed even though the hotel does not
/// auto-seed an ollama controller — validation surfaces the gap loudly rather
/// than silently escalating into a void. Both philote and the hotel's config
/// validation read this constant so their notion of "the default ladder" cannot
/// diverge.
pub const DEFAULT_FALLBACK_TIERS: &[&str] = &["model", "model.openrouter", "model.ollama"];

// ─────────────────────────────────────────────────────────────────────────────
// (a) Fallback-ladder validation
// ─────────────────────────────────────────────────────────────────────────────

/// A ladder tier that names a controller role with no live guest to serve it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreachableTier {
    /// Human label for the ladder (e.g. `role:agent-x:orchestrator` or
    /// `default fallback ladder`).
    pub ladder_label: String,
    /// The controller role that has no seeded+active guest.
    pub tier_role: String,
}

/// Validate that every tier named by every ladder resolves to a controller role
/// backed by a seeded+active guest.
///
/// `active_guest_roles` is the set of `role` values from the hotel's active guest
/// records — this is the exact key the task router matches `target_role` against
/// (exact match, no provider fallback), so a tier is reachable iff its role is in
/// this set. Findings are de-duplicated per `(ladder_label, tier_role)`.
pub fn validate_fallback_ladders(
    active_guest_roles: &BTreeSet<String>,
    ladders: &[(String, Vec<String>)],
) -> Vec<UnreachableTier> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for (label, tiers) in ladders {
        for tier in tiers {
            if active_guest_roles.contains(tier) {
                continue;
            }
            if seen.insert((label.clone(), tier.clone())) {
                out.push(UnreachableTier {
                    ladder_label: label.clone(),
                    tier_role: tier.clone(),
                });
            }
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// (c) Catalog-alert → routing-impact mapping
// ─────────────────────────────────────────────────────────────────────────────

/// Per-provider routing facts needed to decide whether a catalog change touches
/// live routing: the controller roles that dispatch to the provider and the
/// model the provider currently defaults to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRouting {
    pub provider: String,
    /// Controller roles that route text turns to this provider (e.g.
    /// `["model", "model.gemini"]`).
    pub allowed_roles: Vec<String>,
    /// The provider's currently configured default_model, if any.
    pub default_model: Option<String>,
}

/// A catalog change (retirement / thinking-flip) that lands on live routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingImpact {
    pub provider: String,
    /// The configured default_model that matched the changed model.
    pub matched_model: String,
    /// Ladders that route through this provider's controller role(s).
    pub affected_ladders: Vec<String>,
}

/// True when two model refs denote the same model despite prefix differences.
///
/// OpenRouter reports prefixed ids (`google/gemini-2.0-flash-exp`,
/// `openai/gpt-4.1-mini`) while a configured default_model may be bare
/// (`gemini-2.0-flash-exp`) or itself prefixed (`openai/gpt-4.1-mini`). Compare
/// exactly, then fall back to comparing the final path segment.
pub fn model_ref_matches(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let last = |s: &str| s.rsplit('/').next().unwrap_or(s).to_string();
    last(a) == last(b)
}

/// Determine whether a changed (retired / thinking-flipped) model touches live
/// routing: it does when the model is some provider's configured default_model.
/// Each impacted provider yields one [`RoutingImpact`] naming the ladders that
/// route through its controller role(s). An empty `affected_ladders` still
/// signals impact via the default_model itself.
pub fn routing_impact_for_model(
    changed_model_ref: &str,
    providers: &[ProviderRouting],
    ladders: &[(String, Vec<String>)],
) -> Vec<RoutingImpact> {
    let mut impacts = Vec::new();
    for provider in providers {
        let Some(default_model) = provider.default_model.as_deref() else {
            continue;
        };
        if !model_ref_matches(default_model, changed_model_ref) {
            continue;
        }
        let role_set: BTreeSet<&str> = provider.allowed_roles.iter().map(String::as_str).collect();
        let affected_ladders: Vec<String> = ladders
            .iter()
            .filter(|(_, tiers)| tiers.iter().any(|tier| role_set.contains(tier.as_str())))
            .map(|(label, _)| label.clone())
            .collect();
        impacts.push(RoutingImpact {
            provider: provider.provider.clone(),
            matched_model: default_model.to_string(),
            affected_ladders,
        });
    }
    impacts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn ladder(label: &str, tiers: &[&str]) -> (String, Vec<String>) {
        (
            label.to_string(),
            tiers.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn validation_flags_unseeded_tier() {
        // model + model.openrouter are live; model.ollama is not (the real gap
        // in the shipped DEFAULT_FALLBACK_TIERS).
        let active = roles(&["model", "model.openrouter", "model.elevenlabs"]);
        let ladders = vec![ladder("default fallback ladder", DEFAULT_FALLBACK_TIERS)];
        let findings = validate_fallback_ladders(&active, &ladders);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].tier_role, "model.ollama");
        assert_eq!(findings[0].ladder_label, "default fallback ladder");
    }

    #[test]
    fn default_ladder_puts_openrouter_before_ollama() {
        // Operator directive: ollama is unstable — openrouter (live on all
        // hotels) must be the first fallback after the gemini primary, with
        // ollama demoted to local last resort.
        assert_eq!(
            DEFAULT_FALLBACK_TIERS,
            &["model", "model.openrouter", "model.ollama"]
        );
        let openrouter_idx = DEFAULT_FALLBACK_TIERS
            .iter()
            .position(|t| *t == "model.openrouter")
            .expect("default ladder must include model.openrouter");
        let ollama_idx = DEFAULT_FALLBACK_TIERS
            .iter()
            .position(|t| *t == "model.ollama")
            .expect("default ladder must include model.ollama");
        assert!(openrouter_idx < ollama_idx);
        assert_eq!(DEFAULT_FALLBACK_TIERS[0], "model");
    }

    #[test]
    fn validation_accepts_default_ladder_when_openrouter_and_ollama_live() {
        // Phase-3 hotel validation must accept the new default order when the
        // controllers are seeded (openrouter controllers are live on all
        // three hotels).
        let active = roles(&["model", "model.openrouter", "model.ollama"]);
        let ladders = vec![ladder("default fallback ladder", DEFAULT_FALLBACK_TIERS)];
        assert!(validate_fallback_ladders(&active, &ladders).is_empty());
    }

    #[test]
    fn validation_passes_when_all_tiers_live() {
        let active = roles(&["model", "model.local"]);
        let ladders = vec![ladder("role:x:orchestrator", &["model", "model.local"])];
        assert!(validate_fallback_ladders(&active, &ladders).is_empty());
    }

    #[test]
    fn validation_dedupes_same_tier_in_same_ladder() {
        let active = roles(&["model"]);
        let ladders = vec![ladder("l", &["model.ollama", "model.ollama"])];
        let findings = validate_fallback_ladders(&active, &ladders);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn model_ref_matches_across_prefixes() {
        assert!(model_ref_matches(
            "gemini-2.0-flash-exp",
            "google/gemini-2.0-flash-exp"
        ));
        assert!(model_ref_matches(
            "openai/gpt-4.1-mini",
            "openai/gpt-4.1-mini"
        ));
        assert!(model_ref_matches("gpt-4.1-mini", "openai/gpt-4.1-mini"));
        assert!(!model_ref_matches("gpt-4.1-mini", "openai/gpt-4o"));
    }

    #[test]
    fn routing_impact_maps_retired_default_to_ladder() {
        // openrouter retires the model that is gemini's configured default; the
        // orchestrator ladder routes tier 0 through "model" (gemini).
        let providers = vec![ProviderRouting {
            provider: "gemini".into(),
            allowed_roles: vec!["model".into(), "model.gemini".into()],
            default_model: Some("gemini-2.0-flash-exp".into()),
        }];
        let ladders = vec![
            ladder("role:jane:orchestrator", &["model", "model.local"]),
            ladder("role:aria:voice", &["model.elevenlabs"]),
        ];
        let impacts = routing_impact_for_model("google/gemini-2.0-flash-exp", &providers, &ladders);
        assert_eq!(impacts.len(), 1);
        assert_eq!(impacts[0].provider, "gemini");
        assert_eq!(impacts[0].affected_ladders, vec!["role:jane:orchestrator"]);
    }

    #[test]
    fn routing_impact_empty_when_model_not_a_default() {
        let providers = vec![ProviderRouting {
            provider: "gemini".into(),
            allowed_roles: vec!["model".into()],
            default_model: Some("gemini-2.0-flash-exp".into()),
        }];
        let ladders = vec![ladder("role:x:orchestrator", &["model"])];
        // A different, non-default model retiring has no routing impact.
        assert!(routing_impact_for_model("google/gemini-1.0-pro", &providers, &ladders).is_empty());
    }

    #[test]
    fn routing_impact_default_model_with_no_ladder_still_reports() {
        let providers = vec![ProviderRouting {
            provider: "openrouter".into(),
            allowed_roles: vec!["model.openrouter".into()],
            default_model: Some("openai/gpt-4.1-mini".into()),
        }];
        // No ladder references model.openrouter, but the default_model itself is
        // impacted — impact is still reported (with empty affected_ladders).
        let ladders = vec![ladder("role:x:orchestrator", &["model", "model.local"])];
        let impacts = routing_impact_for_model("openai/gpt-4.1-mini", &providers, &ladders);
        assert_eq!(impacts.len(), 1);
        assert!(impacts[0].affected_ladders.is_empty());
    }
}
