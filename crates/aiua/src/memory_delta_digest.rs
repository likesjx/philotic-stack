//! Memory Delta Digest — Memory Transparency Slice M3 (`memory-delta-digest`).
//!
//! An operator-facing, on-demand digest of what the fleet's Muninn vaults
//! remembered, forgot, and found contradictory in a trailing window (default
//! 24h): counts per category plus a bounded top-N of notable lines, each
//! carrying its provenance summary when the M1 [`ansible_mesh_core::provenance::ProvenanceEnvelope`]
//! is present on the underlying engram, and a textual revert hint so the
//! operator can act on a line without leaving the digest.
//!
//! Reuses M4's (`memory_hygiene.rs`) REST wrappers and per-vault iteration —
//! [`crate::memory_hygiene::fetch_engrams`] and
//! [`crate::memory_hygiene::fetch_contradiction_findings`] — rather than
//! duplicating them, and folds in M4's own last-sweep-run marker
//! ([`crate::memory_hygiene::get_last_sweep_run`]) so the digest answers "did
//! the nightly hygiene pass find anything" in the same read.
//!
//! # What this deliberately does NOT do (reality gap, noted honestly)
//!
//! - **"Evolved" memories are not enumerable.** `POST /api/engrams/{id}/evolve`
//!   is a write-only MuninnDB REST action; the list/read surfaces
//!   (`GET /api/engrams`, `GET /api/engrams/{id}`) return no evolution
//!   history and `ListEngramsRequest` has no `updated`/`evolved` sort or
//!   filter. `evolved` is therefore always `0` in [`DigestCounts`] with the
//!   gap named in [`MemoryDeltaDigest::gaps`] rather than silently omitted —
//!   a future slice needs a MuninnDB REST addition (an evolution/audit log
//!   endpoint) before this category can be real.
//! - **Per-line provenance is a bounded enrichment, not a full-window one.**
//!   `GET /api/engrams` (the list endpoint) does not return `metadata`, so
//!   provenance is only available via a per-id `GET /api/engrams/{id}` call.
//!   Fetching that for every engram in the window would be an unbounded
//!   N+1; instead only the top-N notable lines actually rendered
//!   ([`NOTABLE_LIMIT`] per category) are enriched. Lines beyond that (or
//!   whose fetch fails, or whose engram carries no envelope yet) render as
//!   "pre-provenance" — honest absence, not a guess.
//! - **Forgotten-item provenance may be unavailable even for notable lines.**
//!   `GET /api/deleted` returns no `metadata`, and `GET /api/engrams/{id}`
//!   may not resolve a soft-deleted id depending on the server's read path;
//!   a failed detail fetch degrades to "pre-provenance" rather than erroring
//!   the whole line.
//! - **Deleted-item windowing is client-side and bounded.** `GET /api/deleted`
//!   has no `since`/`before` filter — this module fetches a bounded page
//!   ([`DELETED_FETCH_LIMIT`], matching MuninnDB's own server-side cap of
//!   100) and filters by `deleted_at` client-side. If more than that many
//!   items were soft-deleted in the window, older-in-fetch-order deletions
//!   are missed; a per-vault gap note is added when the fetch returns
//!   exactly the cap (a signal, not a proof, that more may exist).
//!
//! # Delivery (reality gap on the config/prompt side, named per AGENTS.md)
//!
//! The Autopoiesis proposal's Slice A4 (`aria-architect-charter` — the daily
//! cron that produces a morning dev-brief) has **not landed as code or
//! config in this repo**: `ARCHITECTURE_STATUS.md` lists it unstarted, and
//! the "staged charters" mentioned in `AUTOPOIESIS_PROPOSAL.md`'s disposition
//! are role/cron records applied directly to live hotels (mbp-jane,
//! vps-jane), not files tracked here. There is therefore no in-repo
//! charter/prompt artifact for this slice to extend. What this slice ships
//! instead is the smallest coherent honest half: a `memory.delta_digest`
//! philote tool (catalog + tool_exec) that any role's toolset can call —
//! including a future A4 steward charter, whose prompt should call this tool
//! before composing the morning brief. See the `MEMORY_TRANSPARENCY_PROPOSAL.md`
//! M3 disposition for the explicit note.

use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::provenance::ProvenanceEnvelope;
use memory_core::MuninnConfig;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Default trailing window, in hours, when a caller does not specify one.
pub const DEFAULT_WINDOW_HOURS: u64 = 24;
/// Bounded number of rendered/enriched lines per category. Counts in
/// [`DigestCounts`] reflect the full (fetch-capped) window; `lines` is a
/// top-N sample, not the whole window — see module doc.
const NOTABLE_LIMIT: usize = 5;
/// Cap on newly-created engrams fetched per vault per digest.
const CREATED_FETCH_LIMIT: u32 = 50;
/// Cap on soft-deleted engrams fetched per vault per digest — mirrors
/// MuninnDB's own server-side cap on `GET /api/deleted` (see module doc).
const DELETED_FETCH_LIMIT: u32 = 100;

// ── Categorization ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigestCategory {
    Remembered,
    Forgotten,
    Contradictory,
}

impl DigestCategory {
    pub fn label(&self) -> &'static str {
        match self {
            DigestCategory::Remembered => "Remembered",
            DigestCategory::Forgotten => "Forgotten",
            DigestCategory::Contradictory => "Contradictory",
        }
    }
}

/// Compact provenance summary attached to a digest line when the underlying
/// engram carries a non-empty [`ProvenanceEnvelope`] (M1). Absent means
/// "pre-provenance" — the write predates M1 adoption on this path, or the
/// writing component has not adopted the envelope yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceLine {
    pub author: String,
    pub trust: String,
}

impl From<&ProvenanceEnvelope> for ProvenanceLine {
    fn from(envelope: &ProvenanceEnvelope) -> Self {
        Self {
            author: envelope.author.clone(),
            trust: envelope.trust.as_str().to_string(),
        }
    }
}

/// One rendered/reversible row of the digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestLine {
    pub category: DigestCategory,
    pub vault: String,
    pub id: String,
    pub concept: String,
    /// Set for `Contradictory` lines — the paired engram id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary_concept: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ProvenanceLine>,
    /// Textual instruction for how the operator undoes/acts on this line.
    /// Full one-tap reversal is out of scope for this slice (named gap) —
    /// this is the "which tool/command" pointer, not an executable action.
    pub revert_hint: String,
}

impl DigestLine {
    pub fn render(&self) -> String {
        let provenance = self
            .provenance
            .as_ref()
            .map(|p| format!("{}/{}", p.author, p.trust))
            .unwrap_or_else(|| "pre-provenance".to_string());

        match (&self.secondary_id, &self.secondary_concept) {
            (Some(sid), Some(sconcept)) => format!(
                "  - '{}' ({}) <-> '{}' ({}) [vault={}] — revert: {}",
                self.concept, self.id, sconcept, sid, self.vault, self.revert_hint
            ),
            _ => format!(
                "  - [{}] {} (vault={}) — {} — revert: {}",
                self.id, self.concept, self.vault, provenance, self.revert_hint
            ),
        }
    }
}

/// Category totals for the window. `evolved` is always `0` today — see the
/// module doc's reality gap; it is a named absence, not a fabricated count.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestCounts {
    pub remembered: usize,
    pub evolved: usize,
    pub forgotten: usize,
    pub contradictory: usize,
    pub vaults_scanned: usize,
}

/// The full digest for one hotel's window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryDeltaDigest {
    pub hotel_name: String,
    pub window_hours: u64,
    pub window_start: String,
    pub window_end: String,
    pub counts: DigestCounts,
    pub lines: Vec<DigestLine>,
    pub hygiene_last_run: Option<crate::memory_hygiene::LastRunRecord>,
    pub gaps: Vec<String>,
    pub errors: Vec<String>,
}

impl MemoryDeltaDigest {
    /// Compact, human-readable rendering — this is what `memory.delta_digest`
    /// hands back to the calling model/operator. Pure function of the struct
    /// contents; no I/O.
    pub fn render(&self) -> String {
        let mut out = Vec::new();
        out.push(format!(
            "Memory Delta Digest — {} — window {} .. {} ({}h)",
            self.hotel_name, self.window_start, self.window_end, self.window_hours
        ));
        out.push(format!(
            "remembered={} evolved={} forgotten={} contradictory={} (vaults scanned: {})",
            self.counts.remembered,
            self.counts.evolved,
            self.counts.forgotten,
            self.counts.contradictory,
            self.counts.vaults_scanned
        ));
        match &self.hygiene_last_run {
            Some(run) => out.push(format!(
                "memory.hygiene last sweep: scanned_at={} vaults={} contradictions={} stale={} filed={}{}",
                run.scanned_at,
                run.vaults_scanned,
                run.contradictions,
                run.stale,
                run.filed,
                run.audit_id
                    .as_deref()
                    .map(|id| format!(" (audit {id})"))
                    .unwrap_or_default()
            )),
            None => out.push(
                "memory.hygiene: no sweep has run yet on this hotel (or the lane is not enabled)"
                    .to_string(),
            ),
        }

        if self.lines.is_empty() && self.counts.remembered == 0 && self.counts.forgotten == 0 {
            out.push("\nNo notable memory activity this window.".to_string());
        } else {
            for cat in [
                DigestCategory::Remembered,
                DigestCategory::Forgotten,
                DigestCategory::Contradictory,
            ] {
                let cat_lines: Vec<&DigestLine> =
                    self.lines.iter().filter(|l| l.category == cat).collect();
                if cat_lines.is_empty() {
                    continue;
                }
                out.push(format!("\n{}:", cat.label()));
                for line in cat_lines {
                    out.push(line.render());
                }
            }
        }

        if !self.gaps.is_empty() {
            out.push("\nReality gaps:".to_string());
            for gap in &self.gaps {
                out.push(format!("  - {gap}"));
            }
        }
        if !self.errors.is_empty() {
            out.push("\nErrors during collection:".to_string());
            for e in &self.errors {
                out.push(format!("  - {e}"));
            }
        }
        out.join("\n")
    }
}

/// Standing gap note for the `evolved` category — always present (see module
/// doc). Kept as a `const` so the wording is identical everywhere it appears.
const EVOLVED_GAP: &str = "evolved: MuninnDB REST has no queryable evolution history \
    (POST /api/engrams/{id}/evolve is write-only; GET /api/engrams and GET /api/engrams/{id} \
    return no updated-at listing) — this category is always 0 until a MuninnDB REST addition \
    exists, not because nothing was evolved.";

// ── Pure categorization (fixture-testable) ──────────────────────────────────

/// Build `Remembered` digest lines from a bounded page of newly-created
/// engrams. `provenance` is looked up by id from `provenance_by_id` — the
/// caller does the (bounded, I/O-bearing) detail fetch; this function stays
/// pure so it is directly unit-testable against fixture data.
pub(crate) fn build_remembered_lines(
    vault: &str,
    items: &[crate::memory_hygiene::EngramItem],
    provenance_by_id: &std::collections::HashMap<String, ProvenanceLine>,
) -> Vec<DigestLine> {
    items
        .iter()
        .take(NOTABLE_LIMIT)
        .map(|item| DigestLine {
            category: DigestCategory::Remembered,
            vault: vault.to_string(),
            id: item.id.clone(),
            concept: item.concept.clone(),
            secondary_id: None,
            secondary_concept: None,
            provenance: provenance_by_id.get(&item.id).cloned(),
            revert_hint: format!("muninn_forget {} (soft-delete; recoverable)", item.id),
        })
        .collect()
}

/// Build `Forgotten` digest lines from soft-deleted engrams already filtered
/// to the window by the caller.
pub(crate) fn build_forgotten_lines(
    vault: &str,
    items: &[DeletedItem],
    provenance_by_id: &std::collections::HashMap<String, ProvenanceLine>,
) -> Vec<DigestLine> {
    items
        .iter()
        .take(NOTABLE_LIMIT)
        .map(|item| DigestLine {
            category: DigestCategory::Forgotten,
            vault: vault.to_string(),
            id: item.id.clone(),
            concept: item.concept.clone(),
            secondary_id: None,
            secondary_concept: None,
            provenance: provenance_by_id.get(&item.id).cloned(),
            revert_hint: format!("muninn_restore {}", item.id),
        })
        .collect()
}

/// Build `Contradictory` digest lines from this hotel's contradiction
/// findings (shared vocabulary with `memory_hygiene::ContradictionFinding`).
pub(crate) fn build_contradiction_lines(
    findings: &[crate::memory_hygiene::ContradictionFinding],
) -> Vec<DigestLine> {
    findings
        .iter()
        .take(NOTABLE_LIMIT)
        .map(|c| DigestLine {
            category: DigestCategory::Contradictory,
            vault: c.vault.clone(),
            id: c.id_a.clone(),
            concept: c.concept_a.clone(),
            secondary_id: Some(c.id_b.clone()),
            secondary_concept: Some(c.concept_b.clone()),
            provenance: None,
            revert_hint: "review via muninn_contradictions; resolving requires the admin-only \
                          POST /api/admin/contradictions/resolve — no auto-resolution has occurred"
                .to_string(),
        })
        .collect()
}

/// Parse a `ProvenanceEnvelope` out of a full engram detail JSON blob
/// (`GET /api/engrams/{id}` response shape: `{ ..., "metadata": { "provenance": {...} } }`,
/// per `philote::memory_integration::merge_provenance_into_metadata`). Pure —
/// no I/O — so it is directly unit-testable against fixture JSON. Returns
/// `None` for missing/malformed provenance or an envelope carrying no more
/// information than a fresh default ([`ProvenanceEnvelope::is_empty_shell`]).
pub(crate) fn parse_provenance(detail: &serde_json::Value) -> Option<ProvenanceLine> {
    let raw = detail.get("metadata")?.get("provenance")?;
    let envelope: ProvenanceEnvelope = serde_json::from_value(raw.clone()).ok()?;
    if envelope.is_empty_shell() {
        return None;
    }
    Some(ProvenanceLine::from(&envelope))
}

// ── REST wire shapes (new to this module — GET /api/deleted) ──────────────

#[derive(Debug, Deserialize)]
struct ListDeletedResponse {
    deleted: Vec<DeletedItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DeletedItem {
    pub(crate) id: String,
    pub(crate) concept: String,
    pub(crate) deleted_at: i64,
}

async fn fetch_deleted(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    limit: u32,
) -> anyhow::Result<Vec<DeletedItem>> {
    let url = format!(
        "{}/api/deleted?limit={}",
        base_url.trim_end_matches('/'),
        limit
    );
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("list deleted returned {status}: {body}");
    }
    Ok(resp.json::<ListDeletedResponse>().await?.deleted)
}

/// Fetch full engram detail (`GET /api/engrams/{id}`) for provenance
/// enrichment. Best-effort: any failure (network, 404 for a soft-deleted id
/// the read path does not resolve, etc.) degrades to `None` — the caller
/// renders "pre-provenance" rather than surfacing an enrichment error as a
/// collection error.
async fn fetch_engram_detail(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    id: &str,
) -> Option<serde_json::Value> {
    let url = format!("{}/api/engrams/{}", base_url.trim_end_matches('/'), id);
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<serde_json::Value>().await.ok()
}

/// Bounded provenance lookup for a batch of ids — one `GET /api/engrams/{id}`
/// per id, capped at [`NOTABLE_LIMIT`] by the caller already having sliced
/// its input. Never errors; missing entries in the returned map render as
/// "pre-provenance" downstream.
async fn enrich_provenance(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    ids: &[String],
) -> std::collections::HashMap<String, ProvenanceLine> {
    let mut map = std::collections::HashMap::new();
    for id in ids {
        if let Some(detail) = fetch_engram_detail(client, base_url, token, id).await {
            if let Some(line) = parse_provenance(&detail) {
                map.insert(id.clone(), line);
            }
        }
    }
    map
}

// ── Collection (I/O) ────────────────────────────────────────────────────────

/// Collect the memory-delta digest for `hotel_name` over the trailing
/// `window_hours`. Read-only across every plane it touches — no Muninn
/// writes, no autonomy grant (this is a query, not an autonomous action; the
/// Autonomy Contract's auditable/reversible/budgeted rules apply to writes,
/// not to reads that merely render existing state). Per-vault failures are
/// recorded in `errors` and do not abort the rest of the collection.
pub async fn collect(
    client: &reqwest::Client,
    config: &MuninnConfig,
    graph: &GraphDomain,
    hotel_name: &str,
    window_hours: u64,
    now: chrono::DateTime<chrono::Utc>,
) -> MemoryDeltaDigest {
    let vault_names = crate::dream::collect_agent_vault_names(graph, hotel_name);
    let window_start_dt = now - chrono::Duration::hours(window_hours as i64);
    let window_start = window_start_dt.to_rfc3339();
    let window_end = now.to_rfc3339();
    let window_start_epoch = window_start_dt.timestamp();

    let mut counts = DigestCounts {
        vaults_scanned: vault_names.len(),
        ..Default::default()
    };
    let mut remembered_lines = Vec::new();
    let mut forgotten_lines = Vec::new();
    let mut contradiction_lines = Vec::new();
    let mut gaps = vec![EVOLVED_GAP.to_string()];
    let mut errors = Vec::new();

    for vault_name in &vault_names {
        let token = match config
            .vault_tokens
            .get(vault_name)
            .or(config.default_token.as_ref())
        {
            Some(t) => t.clone(),
            None => {
                debug!(vault = %vault_name, "memory.delta_digest: no token — skipping");
                continue;
            }
        };

        match crate::memory_hygiene::fetch_engrams(
            client,
            &config.base_url,
            &token,
            "created",
            Some(&window_start),
            None,
            CREATED_FETCH_LIMIT,
        )
        .await
        {
            Ok(items) => {
                counts.remembered += items.len();
                if items.len() as u32 >= CREATED_FETCH_LIMIT {
                    gaps.push(format!(
                        "vault={vault_name}: remembered fetch hit the {CREATED_FETCH_LIMIT}-item \
                         cap — more may exist in-window than counted"
                    ));
                }
                let notable = &items[..items.len().min(NOTABLE_LIMIT)];
                let ids: Vec<String> = notable.iter().map(|i| i.id.clone()).collect();
                let provenance = enrich_provenance(client, &config.base_url, &token, &ids).await;
                remembered_lines.extend(build_remembered_lines(vault_name, notable, &provenance));
            }
            Err(e) => {
                warn!(vault = %vault_name, error = %e, "memory.delta_digest: remembered fetch failed");
                errors.push(format!("vault={vault_name}: remembered fetch failed: {e}"));
            }
        }

        match fetch_deleted(client, &config.base_url, &token, DELETED_FETCH_LIMIT).await {
            Ok(items) => {
                if items.len() as u32 >= DELETED_FETCH_LIMIT {
                    gaps.push(format!(
                        "vault={vault_name}: deleted fetch hit the {DELETED_FETCH_LIMIT}-item \
                         server cap — GET /api/deleted has no since/before filter, so older \
                         deletions may be crowding out in-window ones"
                    ));
                }
                let in_window: Vec<DeletedItem> = items
                    .into_iter()
                    .filter(|d| d.deleted_at >= window_start_epoch)
                    .collect();
                counts.forgotten += in_window.len();
                let notable = &in_window[..in_window.len().min(NOTABLE_LIMIT)];
                let ids: Vec<String> = notable.iter().map(|i| i.id.clone()).collect();
                let provenance = enrich_provenance(client, &config.base_url, &token, &ids).await;
                forgotten_lines.extend(build_forgotten_lines(vault_name, notable, &provenance));
            }
            Err(e) => {
                warn!(vault = %vault_name, error = %e, "memory.delta_digest: deleted fetch failed");
                errors.push(format!("vault={vault_name}: deleted fetch failed: {e}"));
            }
        }

        match crate::memory_hygiene::fetch_contradiction_findings(
            client,
            &config.base_url,
            &token,
            vault_name,
        )
        .await
        {
            Ok(findings) => {
                counts.contradictory += findings.len();
                contradiction_lines.extend(build_contradiction_lines(&findings));
            }
            Err(e) => {
                warn!(vault = %vault_name, error = %e, "memory.delta_digest: contradictions fetch failed");
                errors.push(format!(
                    "vault={vault_name}: contradictions fetch failed: {e}"
                ));
            }
        }
    }

    let hygiene_last_run = crate::memory_hygiene::get_last_sweep_run(graph, hotel_name)
        .unwrap_or_else(|e| {
            warn!(hotel = %hotel_name, "memory.delta_digest: failed to read last hygiene run: {e:#}");
            None
        });

    remembered_lines.truncate(NOTABLE_LIMIT * vault_names.len().max(1));
    forgotten_lines.truncate(NOTABLE_LIMIT * vault_names.len().max(1));
    contradiction_lines.truncate(NOTABLE_LIMIT * vault_names.len().max(1));

    let mut lines = Vec::new();
    lines.extend(remembered_lines);
    lines.extend(forgotten_lines);
    lines.extend(contradiction_lines);

    MemoryDeltaDigest {
        hotel_name: hotel_name.to_string(),
        window_hours,
        window_start,
        window_end,
        counts,
        lines,
        hygiene_last_run,
        gaps,
        errors,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_hygiene::{ContradictionFinding, EngramItem};

    fn engram(id: &str, concept: &str) -> EngramItem {
        EngramItem {
            id: id.to_string(),
            concept: concept.to_string(),
            created_at: 1_750_000_000,
        }
    }

    fn deleted(id: &str, concept: &str, deleted_at: i64) -> DeletedItem {
        DeletedItem {
            id: id.to_string(),
            concept: concept.to_string(),
            deleted_at,
        }
    }

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

    // ── build_*_lines ────────────────────────────────────────────────────

    #[test]
    fn remembered_lines_carry_provenance_when_present() {
        let items = vec![engram("e1", "prefers dark mode"), engram("e2", "likes tea")];
        let mut provenance = std::collections::HashMap::new();
        provenance.insert(
            "e1".to_string(),
            ProvenanceLine {
                author: "philote:bjork/orchestrator".to_string(),
                trust: "told".to_string(),
            },
        );
        let lines = build_remembered_lines("self_bjork", &items, &provenance);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].category, DigestCategory::Remembered);
        assert_eq!(
            lines[0].provenance.as_ref().unwrap().author,
            "philote:bjork/orchestrator"
        );
        assert!(lines[1].provenance.is_none(), "e2 has no provenance entry");
        assert!(lines[0].revert_hint.contains("muninn_forget e1"));
    }

    #[test]
    fn remembered_lines_bounded_to_notable_limit() {
        let items: Vec<_> = (0..20)
            .map(|i| engram(&format!("e{i}"), &format!("concept-{i}")))
            .collect();
        let lines = build_remembered_lines("v", &items, &std::collections::HashMap::new());
        assert_eq!(lines.len(), NOTABLE_LIMIT);
    }

    #[test]
    fn forgotten_lines_revert_hint_points_to_restore() {
        let items = vec![deleted("d1", "old fact", 1_750_000_100)];
        let lines = build_forgotten_lines("v", &items, &std::collections::HashMap::new());
        assert_eq!(lines[0].category, DigestCategory::Forgotten);
        assert!(lines[0].revert_hint.contains("muninn_restore d1"));
    }

    #[test]
    fn contradiction_lines_carry_both_ids_and_concepts() {
        let findings = vec![contradiction("self_bjork", "prefers-x", "prefers-not-x")];
        let lines = build_contradiction_lines(&findings);
        assert_eq!(lines[0].category, DigestCategory::Contradictory);
        assert_eq!(lines[0].concept, "prefers-x");
        assert_eq!(lines[0].secondary_concept.as_deref(), Some("prefers-not-x"));
        assert!(lines[0].revert_hint.contains("muninn_contradictions"));
        assert!(lines[0].provenance.is_none());
    }

    // ── parse_provenance ─────────────────────────────────────────────────

    #[test]
    fn parse_provenance_extracts_envelope_from_metadata() {
        let detail = serde_json::json!({
            "id": "e1",
            "concept": "prefers dark mode",
            "metadata": {
                "provenance": {
                    "source": "turn:abc",
                    "author": "philote:bjork/orchestrator",
                    "trust": "told",
                    "evidence": ["session:s1"],
                }
            }
        });
        let line = parse_provenance(&detail).expect("provenance present");
        assert_eq!(line.author, "philote:bjork/orchestrator");
        assert_eq!(line.trust, "told");
    }

    #[test]
    fn parse_provenance_none_when_metadata_missing_provenance_key() {
        let detail = serde_json::json!({ "id": "e1", "metadata": {} });
        assert!(parse_provenance(&detail).is_none());
    }

    #[test]
    fn parse_provenance_none_when_envelope_is_empty_shell() {
        // A malformed/degenerate envelope with no source/author/evidence/reversal
        // is indistinguishable from "never adopted" — Standing Rule 1's "naked
        // write" — and should render as pre-provenance too.
        let detail = serde_json::json!({
            "metadata": { "provenance": {} }
        });
        assert!(parse_provenance(&detail).is_none());
    }

    #[test]
    fn parse_provenance_none_when_metadata_not_object() {
        let detail = serde_json::json!({ "id": "e1", "metadata": "not-an-object" });
        assert!(parse_provenance(&detail).is_none());
    }

    // ── render ───────────────────────────────────────────────────────────

    fn sample_digest() -> MemoryDeltaDigest {
        MemoryDeltaDigest {
            hotel_name: "mac-jane".to_string(),
            window_hours: 24,
            window_start: "2026-07-10T00:00:00+00:00".to_string(),
            window_end: "2026-07-11T00:00:00+00:00".to_string(),
            counts: DigestCounts {
                remembered: 3,
                evolved: 0,
                forgotten: 1,
                contradictory: 1,
                vaults_scanned: 2,
            },
            lines: vec![
                DigestLine {
                    category: DigestCategory::Remembered,
                    vault: "self_bjork".to_string(),
                    id: "e1".to_string(),
                    concept: "prefers dark mode".to_string(),
                    secondary_id: None,
                    secondary_concept: None,
                    provenance: Some(ProvenanceLine {
                        author: "philote:bjork/orchestrator".to_string(),
                        trust: "told".to_string(),
                    }),
                    revert_hint: "muninn_forget e1 (soft-delete; recoverable)".to_string(),
                },
                DigestLine {
                    category: DigestCategory::Contradictory,
                    vault: "self_bjork".to_string(),
                    id: "a-id".to_string(),
                    concept: "prefers-x".to_string(),
                    secondary_id: Some("b-id".to_string()),
                    secondary_concept: Some("prefers-not-x".to_string()),
                    provenance: None,
                    revert_hint: "review via muninn_contradictions".to_string(),
                },
            ],
            hygiene_last_run: None,
            gaps: vec![EVOLVED_GAP.to_string()],
            errors: vec![],
        }
    }

    #[test]
    fn render_includes_hotel_window_and_counts() {
        let rendered = sample_digest().render();
        assert!(rendered.contains("mac-jane"));
        assert!(rendered.contains("remembered=3"));
        assert!(rendered.contains("evolved=0"));
        assert!(rendered.contains("forgotten=1"));
        assert!(rendered.contains("contradictory=1"));
    }

    #[test]
    fn render_includes_provenance_and_pre_provenance_lines() {
        let rendered = sample_digest().render();
        assert!(rendered.contains("philote:bjork/orchestrator/told"));
        assert!(rendered.contains("prefers-x"));
        assert!(rendered.contains("<->"));
    }

    #[test]
    fn render_includes_gaps_and_no_sweep_marker() {
        let rendered = sample_digest().render();
        assert!(rendered.contains("Reality gaps:"));
        assert!(rendered.contains("evolved:"));
        assert!(rendered.contains("no sweep has run yet"));
    }

    #[test]
    fn render_includes_hygiene_last_run_when_present() {
        let mut digest = sample_digest();
        digest.hygiene_last_run = Some(crate::memory_hygiene::LastRunRecord {
            hotel_name: "mac-jane".to_string(),
            scanned_at: 1_750_000_000,
            vaults_scanned: 2,
            contradictions: 1,
            stale: 3,
            filed: true,
            audit_id: Some("memory_hygiene:mac-jane:1750000000".to_string()),
        });
        let rendered = digest.render();
        assert!(rendered.contains("memory.hygiene last sweep"));
        assert!(rendered.contains("memory_hygiene:mac-jane:1750000000"));
    }

    #[test]
    fn render_reports_clean_window_when_nothing_notable() {
        let mut digest = sample_digest();
        digest.lines.clear();
        digest.counts = DigestCounts {
            vaults_scanned: 1,
            ..Default::default()
        };
        let rendered = digest.render();
        assert!(rendered.contains("No notable memory activity"));
    }
}
