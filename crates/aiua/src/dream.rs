//! Dream Engine — post-session memory consolidation.
//!
//! # What this does
//!
//! Called by the hotel after all guests have drained (Phase 2 of graceful shutdown),
//! before the internal shutdown broadcast fires. For each active agent vault:
//!
//! 1. **Recall** recent session engrams from MuninnDB.
//! 2. **Embed** each engram via the ONNX sidecar at :11435 (Ollama-compat endpoint).
//! 3. **Cluster** by cosine similarity (threshold 0.82) to find related memories.
//! 4. **Consolidate** each cluster ≥ 2 via Ollama (:11434, `gemma4:e4b`) + MuninnDB
//!    `POST /api/consolidate`. The LLM merges the cluster into a single concise engram.
//! 5. **Evolve** non-consolidated engrams via `POST /api/engrams/{id}/evolve` to apply
//!    Hebbian potentiation updates.
//!
//! Every step is non-fatal: a failed vault is logged and skipped; a failed Ollama call
//! drops consolidation for that cluster and falls back to evolve-only.

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

/// Run the dream sweep across all active agent vaults.
///
/// Non-fatal: returns immediately without propagating errors — all failures are
/// logged at warn/debug level. Caller should `await` but not `?` the result.
pub async fn dream_sweep(config: &MuninnConfig, graph: &GraphDomain, hotel_name: &str) {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("DreamsPhase: failed to build HTTP client — {e}");
            return;
        }
    };

    let vault_names = collect_agent_vault_names(graph, hotel_name);
    if vault_names.is_empty() {
        debug!("DreamsPhase: no agent vaults found for hotel {hotel_name}");
        return;
    }

    info!(count = vault_names.len(), "DreamsPhase: starting sweep");

    for vault_name in &vault_names {
        let token = match config.vault_tokens.get(vault_name) {
            Some(t) => t.clone(),
            None => {
                debug!(vault = %vault_name, "DreamsPhase: no token — skipping");
                continue;
            }
        };

        sweep_vault(&client, config, &token, vault_name).await;
    }

    info!("DreamsPhase: complete");
}

// ──── Per-vault sweep ─────────────────────────────────────────────────────────

async fn sweep_vault(
    client: &reqwest::Client,
    config: &MuninnConfig,
    token: &str,
    vault_name: &str,
) {
    // Step 1: Recall recent session engrams.
    let engrams = match recall_recent(client, &config.base_url, token, vault_name, 20).await {
        Ok(e) if !e.is_empty() => e,
        Ok(_) => {
            debug!(vault = %vault_name, "DreamsPhase: no recent engrams — skipping");
            return;
        }
        Err(e) => {
            warn!(vault = %vault_name, error = %e, "DreamsPhase: recall failed — skipping");
            return;
        }
    };

    // Step 2: Embed via ONNX sidecar.
    let embedded = embed_engrams(client, &engrams).await;
    if embedded.is_empty() {
        warn!(vault = %vault_name, "DreamsPhase: all embeddings failed — evolve-only");
        evolve_all(client, &config.base_url, token, vault_name, &engrams).await;
        return;
    }

    // Step 3: Cluster by cosine similarity.
    let clusters = cosine_cluster(&embedded, 0.82);

    // Step 4: Consolidate clusters ≥ 2.
    let mut consolidated_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for cluster in clusters.iter().filter(|c| c.len() >= 2) {
        let ids: Vec<&str> = cluster.iter().map(|e| e.id.as_str()).collect();
        match ollama_merge(client, cluster).await {
            Ok(merged_content) => {
                match consolidate(
                    client,
                    &config.base_url,
                    token,
                    vault_name,
                    &ids,
                    &merged_content,
                )
                .await
                {
                    Ok(_) => {
                        for e in cluster {
                            consolidated_ids.insert(e.id.clone());
                        }
                        info!(
                            vault = %vault_name,
                            count = cluster.len(),
                            "DreamsPhase: consolidated cluster"
                        );
                    }
                    Err(e) => {
                        warn!(vault = %vault_name, error = %e, "DreamsPhase: consolidate failed — falling back to evolve");
                    }
                }
            }
            Err(e) => {
                debug!(vault = %vault_name, error = %e, "DreamsPhase: Ollama merge failed — evolve-only for cluster");
            }
        }
    }

    // Step 5: Evolve non-consolidated engrams.
    let to_evolve: Vec<&EngramSummary> = engrams
        .iter()
        .filter(|e| !consolidated_ids.contains(&e.id))
        .collect();

    for engram in &to_evolve {
        if let Err(e) = evolve_engram(client, &config.base_url, token, vault_name, &engram.id).await
        {
            debug!(vault = %vault_name, id = %engram.id, error = %e, "DreamsPhase: evolve failed (non-fatal)");
        }
    }

    info!(
        vault = %vault_name,
        total = engrams.len(),
        consolidated = consolidated_ids.len(),
        evolved = to_evolve.len(),
        "DreamsPhase: vault sweep complete"
    );
}

// ──── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct EngramSummary {
    id: String,
    concept: String,
    content: String,
}

#[derive(Debug, Clone)]
struct EmbeddedEngram {
    id: String,
    concept: String,
    content: String,
    vector: Vec<f32>,
}

// ──── Step 1: Recall ──────────────────────────────────────────────────────────

async fn recall_recent(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    vault_name: &str,
    limit: u32,
) -> Result<Vec<EngramSummary>> {
    let url = format!(
        "{}/api/recall?vault={}&q=session&limit={}",
        base_url.trim_end_matches('/'),
        vault_name,
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
        anyhow::bail!("recall returned {status}: {body}");
    }

    Ok(resp.json::<Vec<EngramSummary>>().await?)
}

// ──── Step 2: Embed (ONNX sidecar :11435) ────────────────────────────────────

#[derive(Serialize)]
struct EmbedRequest<'a> {
    prompt: &'a str,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embedding: Vec<f32>,
}

const ONNX_SIDECAR_URL: &str = "http://localhost:11435/api/embeddings";

async fn embed_engrams(client: &reqwest::Client, engrams: &[EngramSummary]) -> Vec<EmbeddedEngram> {
    let mut out = Vec::with_capacity(engrams.len());
    for engram in engrams {
        let text = format!("{}: {}", engram.concept, engram.content);
        match client
            .post(ONNX_SIDECAR_URL)
            .json(&EmbedRequest { prompt: &text })
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => match resp.json::<EmbedResponse>().await {
                Ok(r) if !r.embedding.is_empty() => {
                    out.push(EmbeddedEngram {
                        id: engram.id.clone(),
                        concept: engram.concept.clone(),
                        content: engram.content.clone(),
                        vector: r.embedding,
                    });
                }
                Ok(_) => debug!(id = %engram.id, "DreamsPhase: empty embedding vector"),
                Err(e) => debug!(id = %engram.id, error = %e, "DreamsPhase: embed parse failed"),
            },
            Ok(resp) => {
                debug!(id = %engram.id, status = %resp.status(), "DreamsPhase: embed non-2xx")
            }
            Err(e) => debug!(id = %engram.id, error = %e, "DreamsPhase: embed request failed"),
        }
    }
    out
}

// ──── Step 3: Cosine cluster ──────────────────────────────────────────────────

/// Greedily cluster `engrams` by cosine similarity.
///
/// Iterates pairs in order; once an engram is assigned to a cluster it is not
/// re-evaluated. Singletons form their own cluster of length 1.
fn cosine_cluster(engrams: &[EmbeddedEngram], threshold: f32) -> Vec<Vec<EmbeddedEngram>> {
    let mut assigned = vec![false; engrams.len()];
    let mut clusters: Vec<Vec<EmbeddedEngram>> = Vec::new();

    for i in 0..engrams.len() {
        if assigned[i] {
            continue;
        }
        let mut cluster = vec![engrams[i].clone()];
        assigned[i] = true;

        for j in (i + 1)..engrams.len() {
            if assigned[j] {
                continue;
            }
            if cosine_similarity(&engrams[i].vector, &engrams[j].vector) >= threshold {
                cluster.push(engrams[j].clone());
                assigned[j] = true;
            }
        }
        clusters.push(cluster);
    }
    clusters
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

// ──── Step 4a: Ollama merge ───────────────────────────────────────────────────

const OLLAMA_CHAT_URL: &str = "http://localhost:11434/v1/chat/completions";
const OLLAMA_MODEL: &str = "gemma4:e4b";

async fn ollama_merge(client: &reqwest::Client, cluster: &[EmbeddedEngram]) -> Result<String> {
    let memories = cluster
        .iter()
        .map(|e| format!("- {}: {}", e.concept, e.content))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        "You are a memory consolidation engine. \
         Merge these related memories into one concise engram. \
         Output only the merged memory text — no commentary, no labels.\n\n\
         Memories:\n{memories}"
    );

    let body = serde_json::json!({
        "model": OLLAMA_MODEL,
        "messages": [
            { "role": "user", "content": prompt }
        ],
        "stream": false,
    });

    let resp = client.post(OLLAMA_CHAT_URL).json(&body).send().await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Ollama returned {status}: {text}");
    }

    let json: serde_json::Value = resp.json().await?;
    let content = json
        .pointer("/choices/0/message/content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Ollama response missing choices[0].message.content"))?
        .trim()
        .to_string();

    if content.is_empty() {
        anyhow::bail!("Ollama returned empty merge content");
    }

    Ok(content)
}

// ──── Step 4b: MuninnDB consolidate ──────────────────────────────────────────

#[derive(Serialize)]
struct ConsolidateRequest<'a> {
    vault: &'a str,
    ids: &'a [&'a str],
    merged_content: &'a str,
}

async fn consolidate(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    vault_name: &str,
    ids: &[&str],
    merged_content: &str,
) -> Result<()> {
    let url = format!("{}/api/consolidate", base_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .json(&ConsolidateRequest {
            vault: vault_name,
            ids,
            merged_content,
        })
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("consolidate returned {status}: {body}");
    }
    Ok(())
}

// ──── Step 5: Evolve ─────────────────────────────────────────────────────────

#[derive(Serialize)]
struct EvolveRequest<'a> {
    vault: &'a str,
    delta: f32,
}

async fn evolve_engram(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    vault_name: &str,
    engram_id: &str,
) -> Result<()> {
    let url = format!(
        "{}/api/engrams/{}/evolve",
        base_url.trim_end_matches('/'),
        engram_id,
    );
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .json(&EvolveRequest {
            vault: vault_name,
            delta: 0.1,
        })
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        anyhow::bail!("evolve returned {status}");
    }
    Ok(())
}

async fn evolve_all(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    vault_name: &str,
    engrams: &[EngramSummary],
) {
    for engram in engrams {
        if let Err(e) = evolve_engram(client, base_url, token, vault_name, &engram.id).await {
            debug!(vault = %vault_name, id = %engram.id, error = %e, "DreamsPhase: evolve failed (non-fatal)");
        }
    }
}

// ──── Helpers ─────────────────────────────────────────────────────────────────

/// Derive `self_{agent_id}` vault names for all active guests in the hotel.
///
/// `pub(crate)` so the memory.hygiene sweep (`crate::memory_hygiene`) can
/// reuse the same vault-derivation logic instead of duplicating it.
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

    fn vec2(x: f32, y: f32) -> Vec<f32> {
        vec![x, y]
    }

    #[test]
    fn cosine_similarity_identical_vectors() {
        let v = vec2(1.0, 0.0);
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let a = vec2(1.0, 0.0);
        let b = vec2(0.0, 1.0);
        assert!((cosine_similarity(&a, &b)).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_length_mismatch_returns_zero() {
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 0.0]), 0.0);
    }

    #[test]
    fn cosine_cluster_groups_similar() {
        let e = |id: &str, v: Vec<f32>| EmbeddedEngram {
            id: id.to_string(),
            concept: id.to_string(),
            content: id.to_string(),
            vector: v,
        };
        // a and b are identical (sim=1.0), c is orthogonal to both.
        let engrams = vec![
            e("a", vec2(1.0, 0.0)),
            e("b", vec2(1.0, 0.0)),
            e("c", vec2(0.0, 1.0)),
        ];
        let clusters = cosine_cluster(&engrams, 0.82);

        // a+b cluster together; c is singleton.
        assert_eq!(clusters.len(), 2);
        let big = clusters.iter().find(|c| c.len() == 2).expect("a+b cluster");
        assert!(big.iter().any(|e| e.id == "a"));
        assert!(big.iter().any(|e| e.id == "b"));
        let singleton = clusters.iter().find(|c| c.len() == 1).expect("c singleton");
        assert_eq!(singleton[0].id, "c");
    }

    #[test]
    fn cosine_cluster_all_singletons_below_threshold() {
        let e = |id: &str, v: Vec<f32>| EmbeddedEngram {
            id: id.to_string(),
            concept: id.to_string(),
            content: id.to_string(),
            vector: v,
        };
        let engrams = vec![e("a", vec2(1.0, 0.0)), e("b", vec2(0.0, 1.0))];
        let clusters = cosine_cluster(&engrams, 0.82);
        assert_eq!(clusters.len(), 2);
        assert!(clusters.iter().all(|c| c.len() == 1));
    }

    #[test]
    fn cosine_cluster_empty_input() {
        assert!(cosine_cluster(&[], 0.82).is_empty());
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
