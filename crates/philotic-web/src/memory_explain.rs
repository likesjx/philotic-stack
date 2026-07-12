//! `phil memory explain "<claim>"` — Memory Transparency Slice M2
//! (`docs/architecture/MEMORY_TRANSPARENCY_PROPOSAL.md`). Fans a claim out
//! across the three memory planes and renders the merged, banded provenance
//! report — the operator-facing counterpart of the `memory.explain` philote
//! tool (`crates/philote/src/memory_explain_tool.rs`).
//!
//! ## Transport (deliberately different from the philote tool's, and named
//! honestly rather than pretending parity):
//!
//! - **Muninn**: direct REST, same as the tool — `POST /api/activate`
//!   against `PHILOTIC_MUNINN_URL` (default `http://127.0.0.1:8475`) /
//!   `PHILOTIC_MUNINN_VAULT` (default `default`). This CLI is not a
//!   materialized agent, so it does not go through `MuninnRestEngine`'s
//!   `VaultResolver` (which derives vault names from an agent/user identity
//!   the CLI does not have) — it queries the configured vault directly.
//! - **Intel graph**: direct REST, same client shape as the philote tool —
//!   `GET /api/mutations` against `PHILOTIC_INTEL_GRAPH_URL` (default
//!   `http://127.0.0.1:8900`), filtered client-side by claim substring.
//! - **LifeGraph**: always reported unavailable from this surface. Unlike
//!   the philote tool (which can read a live session's `life.recall`
//!   prefetch cache), `phil` is a one-shot process with no session — and
//!   `life.recall` itself is mesh-routed/asynchronous with no synchronous
//!   REST transport in this repo (see the tool's module doc). This is a
//!   named structural gap, not a runtime unreachability — reported through
//!   the same "plane unavailable" contract so it is never silently dropped.

use ansible_mesh_core::memory_explain::{
    band_report, envelope_from_decision_details, ExplainEvidenceItem, ExplainPlane,
    ExplainPlaneOutcome, ExplainReport, render_text,
};
use anyhow::Result;
use clap::Subcommand;
use std::time::Duration;

const MUNINN_DEFAULT_URL: &str = "http://127.0.0.1:8475";
const MUNINN_DEFAULT_VAULT: &str = "default";
const INTEL_GRAPH_DEFAULT_URL: &str = "http://127.0.0.1:8900";
const PLANE_TIMEOUT: Duration = Duration::from_secs(8);
const INTEL_GRAPH_MAX_MATCHES: usize = 10;

#[derive(Subcommand, Debug)]
pub enum MemoryAction {
    /// Explain why the system believes a claim: merged Muninn + intel-graph
    /// + LifeGraph provenance, banded into confirmed / inferred /
    /// told-or-seeded / pre-provenance.
    Explain {
        /// The belief/fact/decision to explain, in natural language.
        claim: String,
        /// Maximum Muninn engrams to consider. Defaults to 8.
        #[arg(long, default_value_t = 8)]
        limit: usize,
    },
}

pub async fn run(action: MemoryAction) -> Result<()> {
    match action {
        MemoryAction::Explain { claim, limit } => explain(claim, limit).await,
    }
}

async fn explain(claim: String, limit: usize) -> Result<()> {
    let claim = claim.trim().to_string();
    if claim.is_empty() {
        println!("phil memory explain: claim must not be empty");
        return Ok(());
    }
    let limit = limit.clamp(1, 20);

    let muninn_outcome = fetch_muninn_plane(&claim, limit).await;
    let intel_graph_outcome = fetch_intel_graph_plane(&claim).await;
    let lifegraph_outcome = ExplainPlaneOutcome::unavailable(
        ExplainPlane::LifeGraph,
        "phil has no transport to the LifeGraph plane in this slice — life.recall is \
         mesh-routed/asynchronous with no synchronous REST path, and phil is a one-shot \
         process with no session-scoped prefetch cache to read (the philote tool surface \
         reads that cache instead; see memory_explain_tool.rs's module doc)",
    );

    let report = ExplainReport {
        claim: claim.clone(),
        planes: vec![muninn_outcome, intel_graph_outcome, lifegraph_outcome],
    };
    println!("{}", render_text(&band_report(&report)));
    Ok(())
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

async fn fetch_muninn_plane(claim: &str, limit: usize) -> ExplainPlaneOutcome {
    let base = env_or("PHILOTIC_MUNINN_URL", MUNINN_DEFAULT_URL);
    let base = base.trim_end_matches('/').to_string();
    let vault = env_or("PHILOTIC_MUNINN_VAULT", MUNINN_DEFAULT_VAULT);
    let token = std::env::var("PHILOTIC_MUNINN_TOKEN").ok();

    let client = match reqwest::Client::builder().timeout(PLANE_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            return ExplainPlaneOutcome::unavailable(
                ExplainPlane::Muninn,
                format!("failed to build HTTP client: {e}"),
            )
        }
    };

    let url = format!("{base}/api/activate");
    let body = serde_json::json!({
        "vault": vault,
        "context": [claim],
        "max_results": limit,
    });
    let mut req = client.post(&url).json(&body);
    if let Some(t) = &token {
        req = req.bearer_auth(t);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            return ExplainPlaneOutcome::unavailable(
                ExplainPlane::Muninn,
                format!("request to {url} (vault={vault}) failed: {e}"),
            )
        }
    };
    if !resp.status().is_success() {
        return ExplainPlaneOutcome::unavailable(
            ExplainPlane::Muninn,
            format!("{url} (vault={vault}) returned HTTP {}", resp.status()),
        );
    }
    let payload: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return ExplainPlaneOutcome::unavailable(
                ExplainPlane::Muninn,
                format!("failed to parse {url} response: {e}"),
            )
        }
    };
    let activations = payload
        .get("activations")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let items = activations
        .into_iter()
        .map(|a| {
            let concept = a.get("concept").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let content = a.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let id = a.get("id").and_then(|v| v.as_str()).map(str::to_string);
            let recorded_at = a
                .get("updated_at")
                .and_then(|v| v.as_i64())
                .or_else(|| a.get("created_at").and_then(|v| v.as_i64()));
            let metadata = a.get("metadata").cloned().unwrap_or(serde_json::Value::Null);
            ExplainEvidenceItem {
                plane: ExplainPlane::Muninn,
                label: concept,
                detail: content,
                source_ref: id,
                recorded_at,
                envelope: ansible_mesh_core::memory_explain::envelope_from_engram_metadata(&metadata),
            }
        })
        .collect();

    ExplainPlaneOutcome::ok(ExplainPlane::Muninn, items)
}

async fn fetch_intel_graph_plane(claim: &str) -> ExplainPlaneOutcome {
    let base = env_or("PHILOTIC_INTEL_GRAPH_URL", INTEL_GRAPH_DEFAULT_URL);
    let base = base.trim_end_matches('/').to_string();

    let client = match reqwest::Client::builder().timeout(PLANE_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            return ExplainPlaneOutcome::unavailable(
                ExplainPlane::IntelGraph,
                format!("failed to build HTTP client: {e}"),
            )
        }
    };

    let url = format!("{base}/api/mutations?limit=200");
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            return ExplainPlaneOutcome::unavailable(
                ExplainPlane::IntelGraph,
                format!("request to {url} failed: {e}"),
            )
        }
    };
    if !resp.status().is_success() {
        return ExplainPlaneOutcome::unavailable(
            ExplainPlane::IntelGraph,
            format!("{url} returned HTTP {}", resp.status()),
        );
    }
    let mutations: Vec<serde_json::Value> = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return ExplainPlaneOutcome::unavailable(
                ExplainPlane::IntelGraph,
                format!("failed to parse {url} response: {e}"),
            )
        }
    };

    let needle = claim.to_lowercase();
    let mut items = Vec::new();
    for m in mutations {
        let reason = m.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        let target = m.get("target_node").and_then(|v| v.as_str()).unwrap_or("");
        let action = m.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let haystack = format!("{reason} {target} {action}").to_lowercase();
        if !haystack.contains(&needle) {
            continue;
        }
        let agent = m.get("agent").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let details = m.get("details").cloned().unwrap_or(serde_json::Value::Null);
        let recorded_at = m
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp());

        items.push(ExplainEvidenceItem {
            plane: ExplainPlane::IntelGraph,
            label: format!("{action} on {target}"),
            detail: reason.to_string(),
            source_ref: m.get("id").and_then(|v| v.as_str()).map(str::to_string),
            recorded_at,
            envelope: envelope_from_decision_details(&details, agent),
        });
        if items.len() >= INTEL_GRAPH_MAX_MATCHES {
            break;
        }
    }
    ExplainPlaneOutcome::ok(ExplainPlane::IntelGraph, items)
}
