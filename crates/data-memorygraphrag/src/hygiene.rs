// Life Graph hygiene sweep: auto-retire stale proposals + exact-duplicate
// collapse (audit-roadmap slice 3a). Runs on an internal runner timer
// (see `main.rs`), never on the request path — a failed or slow sweep must
// never affect `life.observe`/`life.recall` latency or availability.
//
// Design invariants:
//   - Confirmed / retired / conflicted nodes are NEVER mutated by this sweep.
//     Auto-retire only touches `validation_state = 'proposed'`; duplicate
//     collapse only retires a duplicate whose *current* validation_state is
//     `proposed` or `inferred` (re-checked in the write-side Cypher WHERE,
//     not just the Rust-side plan, as defense against a stale read).
//   - All node-affecting Cypher is parameterized (`.param`), never string
//     interpolation, for every value. Label names are the one exception —
//     interpolated into the MATCH pattern, but only ever a literal drawn
//     from `SWEEP_LABELS` below, never caller input (same pattern already
//     used for `n:{label}` writes elsewhere in this crate).
//   - Every sweep is capped at `max_writes_from_env()` total node writes
//     (retire + collapse combined) so a pathological backlog degrades to
//     "did some of it, logged capped=true" instead of a multi-minute
//     Memgraph write storm.

use std::collections::HashMap;

use anyhow::Result;
use neo4rs::{Graph, query};
use tracing::info;

/// Labels the hygiene sweep is scoped to. Deliberately a narrow pilot lane
/// (not all `KNOWN_LABELS`): OpenLoop/Event/Signal are the highest-churn,
/// lowest-stakes labels — Goal/Commitment/Decision etc. stay untouched until
/// the sweep has a live track record.
pub const SWEEP_LABELS: &[&str] = &["OpenLoop", "Event", "Signal"];

/// Property value stamped on auto-retired nodes so they're distinguishable
/// from operator- or model-driven retirement.
pub const RETIRED_BY_TAG: &str = "life-hygiene";

pub const STALE_DAYS_ENV: &str = "PHILOTIC_LIFE_HYGIENE_STALE_DAYS";
pub const DEFAULT_STALE_DAYS: i64 = 45;

pub const MAX_WRITES_ENV: &str = "PHILOTIC_LIFE_HYGIENE_MAX_WRITES";
pub const DEFAULT_MAX_WRITES: usize = 200;

pub const HYGIENE_ENABLED_ENV: &str = "PHILOTIC_LIFE_HYGIENE_ENABLED";
pub const HYGIENE_INTERVAL_HOURS_ENV: &str = "PHILOTIC_LIFE_HYGIENE_INTERVAL_HOURS";
pub const DEFAULT_INTERVAL_HOURS: u64 = 24;

/// Per-label fetch ceiling for candidate reads (both the stale-retire scan
/// and the duplicate-candidate scan). Independent of `max_writes` — this
/// just bounds how much we pull into Rust before capping writes; generous
/// enough that a real backlog is fully visible to the planner.
const CANDIDATE_FETCH_LIMIT: i64 = 1000;

// ── Env parsing (pure, testable) ────────────────────────────────────────────

/// Parse a truthy env value: `"1"`, `"true"`, `"yes"` (case-insensitive,
/// trimmed). Anything else — including unset/empty/garbage — is `false`.
/// Hygiene defaults OFF; an operator must opt in explicitly.
pub fn parse_enabled(raw: Option<&str>) -> bool {
    matches!(
        raw.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

pub fn hygiene_enabled_from_env() -> bool {
    parse_enabled(std::env::var(HYGIENE_ENABLED_ENV).ok().as_deref())
}

/// Parse a positive integer env override; invalid/zero/missing falls back to
/// `default`.
fn parse_positive_u64(raw: Option<&str>, default: u64) -> u64 {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

pub fn stale_days_from_env() -> i64 {
    parse_positive_u64(
        std::env::var(STALE_DAYS_ENV).ok().as_deref(),
        DEFAULT_STALE_DAYS as u64,
    ) as i64
}

pub fn max_writes_from_env() -> usize {
    parse_positive_u64(
        std::env::var(MAX_WRITES_ENV).ok().as_deref(),
        DEFAULT_MAX_WRITES as u64,
    ) as usize
}

pub fn interval_hours_from_env() -> u64 {
    parse_positive_u64(
        std::env::var(HYGIENE_INTERVAL_HOURS_ENV).ok().as_deref(),
        DEFAULT_INTERVAL_HOURS,
    )
}

// ── Normalization + keeper selection (pure, testable) ───────────────────────

/// Normalize a `claim_summary` for exact-duplicate grouping: lowercase,
/// tokenize on non-alphanumeric boundaries, drop empty tokens, rejoin with a
/// single space. This is rephrasing-STABLE for whitespace/punctuation/case
/// only (e.g. "Call pharmacy." vs "call  pharmacy" normalize identically) —
/// it is NOT a semantic dedupe; two genuinely different claims that happen to
/// share tokens after stripping punctuation will still collide, which is why
/// this feeds an exact-duplicate collapse, not a similarity threshold.
pub fn normalize_claim_summary(raw: &str) -> String {
    raw.split(|c: char| !c.is_alphanumeric())
        .filter(|tok| !tok.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// A duplicate-collapse candidate node, as read from Memgraph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateCandidate {
    pub id: String,
    pub claim_summary: String,
    pub observed_at: Option<String>,
    pub validation_state: String,
}

/// A validation_state that duplicate-collapse (or auto-retire) is allowed to
/// overwrite. Confirmed/retired/conflicted nodes are never write targets.
fn is_retirable_state(state: &str) -> bool {
    matches!(state, "proposed" | "inferred")
}

/// Choose the keeper among a group of same-label, same-normalized-claim
/// candidates: a `confirmed` member always wins (operator-validated fact
/// outranks everything); otherwise the newest `observed_at` wins (missing
/// `observed_at` sorts as oldest — ISO 8601 strings compare lexicographically
/// in chronological order, the same convention `provider.rs` already uses
/// for `ORDER BY observed_at`). Ties keep the first-encountered candidate.
pub fn select_keeper<'a>(
    candidates: impl IntoIterator<Item = &'a DuplicateCandidate>,
) -> Option<&'a DuplicateCandidate> {
    let mut confirmed: Option<&DuplicateCandidate> = None;
    let mut newest: Option<&DuplicateCandidate> = None;
    for c in candidates {
        if confirmed.is_none() && c.validation_state == "confirmed" {
            confirmed = Some(c);
        }
        newest = Some(match newest {
            None => c,
            Some(current) => {
                let current_key = current.observed_at.as_deref().unwrap_or("");
                let candidate_key = c.observed_at.as_deref().unwrap_or("");
                if candidate_key > current_key {
                    c
                } else {
                    current
                }
            }
        });
    }
    confirmed.or(newest)
}

/// One planned duplicate-collapse group: retire `duplicate_ids` and link each
/// `(keeper)-[:SUPERSEDES]->(dup)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollapseGroup {
    pub keeper_id: String,
    pub duplicate_ids: Vec<String>,
}

/// Group same-label candidates by normalized `claim_summary`, and for every
/// group with more than one member, pick a keeper and plan retirement of the
/// rest. A candidate that loses keeper selection but is itself
/// confirmed/retired/conflicted is dropped from `duplicate_ids` — it is never
/// touched, even though it "lost" — that state means either it's already
/// inert (retired) or represents a genuine conflict a human should resolve,
/// not silent auto-collapse. A group left with zero retirable duplicates
/// after that filter produces no `CollapseGroup` at all.
///
/// Deterministic: groups are visited in sorted normalized-key order.
pub fn plan_duplicate_collapse(candidates: &[DuplicateCandidate]) -> Vec<CollapseGroup> {
    let mut groups: HashMap<String, Vec<&DuplicateCandidate>> = HashMap::new();
    for c in candidates {
        groups
            .entry(normalize_claim_summary(&c.claim_summary))
            .or_default()
            .push(c);
    }

    let mut keys: Vec<&String> = groups.keys().collect();
    keys.sort();

    let mut plans = Vec::new();
    for key in keys {
        let members = &groups[key];
        if members.len() < 2 {
            continue;
        }
        let Some(keeper) = select_keeper(members.iter().copied()) else {
            continue;
        };
        let duplicate_ids: Vec<String> = members
            .iter()
            .filter(|c| c.id != keeper.id)
            .filter(|c| is_retirable_state(&c.validation_state))
            .map(|c| c.id.clone())
            .collect();
        if !duplicate_ids.is_empty() {
            plans.push(CollapseGroup {
                keeper_id: keeper.id.clone(),
                duplicate_ids,
            });
        }
    }
    plans
}

// ── Write cap (pure, testable) ───────────────────────────────────────────────

/// Cap a planned list of write ops at `max_writes`, truncating in order.
/// Returns `(applied, capped)` — `capped` is `true` iff any ops were dropped.
pub fn apply_write_cap<T>(mut ops: Vec<T>, max_writes: usize) -> (Vec<T>, bool) {
    if ops.len() > max_writes {
        ops.truncate(max_writes);
        (ops, true)
    } else {
        (ops, false)
    }
}

// ── Sweep summary + DB-touching sweep ────────────────────────────────────────

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepSummary {
    pub retired_stale: usize,
    pub collapsed_duplicates: usize,
    pub capped: bool,
}

/// One planned node write, tagged with which phase produced it (for
/// summary counting after the shared cap is applied).
enum PlannedWrite {
    /// Auto-retire a stale `proposed` node: (label, id).
    Stale { label: &'static str, id: String },
    /// Collapse a duplicate into its keeper: (keeper_id, dup_id).
    Duplicate { keeper_id: String, dup_id: String },
}

/// Run one hygiene sweep against `graph`: auto-retire stale proposed nodes,
/// then collapse exact duplicates, across [`SWEEP_LABELS`]. Read-then-plan
/// happens entirely in Rust (testable, deterministic); only the capped,
/// already-decided write list touches Memgraph. Every write is parameterized;
/// the duplicate-collapse write re-checks `validation_state IN
/// ['proposed','inferred']` server-side as a second guard against retiring
/// anything confirmed/retired/conflicted.
pub async fn sweep(graph: &Graph) -> Result<SweepSummary> {
    let stale_days = stale_days_from_env();
    let max_writes = max_writes_from_env();
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(stale_days)).to_rfc3339();

    let mut planned: Vec<PlannedWrite> = Vec::new();

    // Phase 1: auto-retire stale proposed nodes.
    for &label in SWEEP_LABELS {
        let cypher = format!(
            "MATCH (n:{label}) \
             WHERE n.validation_state = 'proposed' AND n.observed_at < $cutoff \
             RETURN n.id AS id \
             LIMIT $limit"
        );
        let mut rows = graph
            .execute(
                query(&cypher)
                    .param("cutoff", cutoff.as_str())
                    .param("limit", CANDIDATE_FETCH_LIMIT),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            if let Ok(id) = row.get::<String>("id") {
                planned.push(PlannedWrite::Stale { label, id });
            }
        }
    }

    // Phase 2: exact-duplicate collapse, per label.
    for &label in SWEEP_LABELS {
        let cypher = format!(
            "MATCH (n:{label}) \
             WHERE n.validation_state <> 'retired' \
             RETURN n.id AS id, n.claim_summary AS claim_summary, \
                    n.observed_at AS observed_at, n.validation_state AS validation_state \
             LIMIT $limit"
        );
        let mut rows = graph
            .execute(query(&cypher).param("limit", CANDIDATE_FETCH_LIMIT))
            .await?;
        let mut candidates = Vec::new();
        while let Some(row) = rows.next().await? {
            let Ok(id) = row.get::<String>("id") else {
                continue;
            };
            let claim_summary = row.get::<String>("claim_summary").unwrap_or_default();
            if claim_summary.is_empty() {
                continue;
            }
            let observed_at = row.get::<String>("observed_at").ok();
            let validation_state = row
                .get::<String>("validation_state")
                .unwrap_or_else(|_| "inferred".to_string());
            candidates.push(DuplicateCandidate {
                id,
                claim_summary,
                observed_at,
                validation_state,
            });
        }
        for group in plan_duplicate_collapse(&candidates) {
            for dup_id in group.duplicate_ids {
                planned.push(PlannedWrite::Duplicate {
                    keeper_id: group.keeper_id.clone(),
                    dup_id,
                });
            }
        }
    }

    let (planned, capped) = apply_write_cap(planned, max_writes);

    let mut summary = SweepSummary {
        capped,
        ..Default::default()
    };

    for write in planned {
        match write {
            PlannedWrite::Stale { label, id } => {
                // retired_at closes ontology gap G2: without a stamp, the
                // steward's recently_retired self-audit could only sort by
                // observed_at, which reflects when the fact was SEEN, not
                // when it was retired.
                let cypher = format!(
                    "MATCH (n:{label} {{id: $id}}) \
                     WHERE n.validation_state = 'proposed' \
                     SET n.validation_state = 'retired', n.retired_by = $retired_by, \
                     n.retired_at = $retired_at"
                );
                let retired_at = chrono::Utc::now().to_rfc3339();
                let mut rows = graph
                    .execute(
                        query(&cypher)
                            .param("id", id.as_str())
                            .param("retired_by", RETIRED_BY_TAG)
                            .param("retired_at", retired_at.as_str()),
                    )
                    .await?;
                rows.next().await?;
                summary.retired_stale += 1;
            }
            PlannedWrite::Duplicate { keeper_id, dup_id } => {
                let cypher = "MATCH (dup {id: $dup_id}) \
                     WHERE dup.validation_state IN ['proposed', 'inferred'] \
                     MATCH (keeper {id: $keeper_id}) \
                     SET dup.validation_state = 'retired', dup.retired_by = $retired_by, \
                     dup.retired_at = $retired_at \
                     MERGE (keeper)-[:SUPERSEDES]->(dup)";
                let retired_at = chrono::Utc::now().to_rfc3339();
                let mut rows = graph
                    .execute(
                        query(cypher)
                            .param("dup_id", dup_id.as_str())
                            .param("keeper_id", keeper_id.as_str())
                            .param("retired_by", RETIRED_BY_TAG)
                            .param("retired_at", retired_at.as_str()),
                    )
                    .await?;
                rows.next().await?;
                summary.collapsed_duplicates += 1;
            }
        }
    }

    info!(
        retired_stale = summary.retired_stale,
        collapsed_duplicates = summary.collapsed_duplicates,
        capped = summary.capped,
        "life-graph hygiene sweep completed"
    );

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        id: &str,
        summary: &str,
        observed_at: Option<&str>,
        state: &str,
    ) -> DuplicateCandidate {
        DuplicateCandidate {
            id: id.to_string(),
            claim_summary: summary.to_string(),
            observed_at: observed_at.map(str::to_string),
            validation_state: state.to_string(),
        }
    }

    // ── normalize_claim_summary ──────────────────────────────────────────

    #[test]
    fn normalize_is_case_and_punctuation_insensitive() {
        assert_eq!(
            normalize_claim_summary("Call pharmacy."),
            normalize_claim_summary("call  pharmacy")
        );
        assert_eq!(normalize_claim_summary("Call pharmacy."), "call pharmacy");
    }

    #[test]
    fn normalize_collapses_repeated_and_leading_whitespace() {
        assert_eq!(
            normalize_claim_summary("  Call   the   pharmacy!!  "),
            "call the pharmacy"
        );
    }

    #[test]
    fn normalize_treats_hyphenated_and_spaced_forms_the_same() {
        // Rephrasing-stable across punctuation-only differences.
        assert_eq!(
            normalize_claim_summary("follow-up: call pharmacy"),
            normalize_claim_summary("follow up call pharmacy")
        );
    }

    #[test]
    fn normalize_does_not_collapse_genuinely_different_claims() {
        assert_ne!(
            normalize_claim_summary("call pharmacy"),
            normalize_claim_summary("call dentist")
        );
    }

    // ── select_keeper ─────────────────────────────────────────────────────

    #[test]
    fn keeper_selection_confirmed_beats_newest() {
        let candidates = vec![
            candidate(
                "a",
                "call pharmacy",
                Some("2026-07-15T00:00:00Z"),
                "proposed",
            ),
            candidate(
                "b",
                "call pharmacy",
                Some("2026-01-01T00:00:00Z"),
                "confirmed",
            ),
        ];
        let keeper = select_keeper(&candidates).expect("keeper");
        assert_eq!(keeper.id, "b", "confirmed must win even though it's older");
    }

    #[test]
    fn keeper_selection_newest_beats_older_when_none_confirmed() {
        let candidates = vec![
            candidate(
                "a",
                "call pharmacy",
                Some("2026-01-01T00:00:00Z"),
                "proposed",
            ),
            candidate(
                "b",
                "call pharmacy",
                Some("2026-07-15T00:00:00Z"),
                "inferred",
            ),
            candidate(
                "c",
                "call pharmacy",
                Some("2026-03-01T00:00:00Z"),
                "proposed",
            ),
        ];
        let keeper = select_keeper(&candidates).expect("keeper");
        assert_eq!(keeper.id, "b", "newest observed_at must win");
    }

    #[test]
    fn keeper_selection_missing_observed_at_sorts_oldest() {
        let candidates = vec![
            candidate("a", "call pharmacy", None, "proposed"),
            candidate(
                "b",
                "call pharmacy",
                Some("2026-01-01T00:00:00Z"),
                "proposed",
            ),
        ];
        let keeper = select_keeper(&candidates).expect("keeper");
        assert_eq!(keeper.id, "b", "a real timestamp must beat a missing one");
    }

    #[test]
    fn keeper_selection_empty_candidates_returns_none() {
        assert!(select_keeper(&[]).is_none());
    }

    // ── plan_duplicate_collapse ───────────────────────────────────────────

    #[test]
    fn collapse_plan_retires_all_but_the_keeper() {
        let candidates = vec![
            candidate(
                "a",
                "Call pharmacy.",
                Some("2026-01-01T00:00:00Z"),
                "proposed",
            ),
            candidate(
                "b",
                "call  pharmacy",
                Some("2026-07-15T00:00:00Z"),
                "proposed",
            ),
            candidate(
                "c",
                "unrelated claim",
                Some("2026-07-15T00:00:00Z"),
                "proposed",
            ),
        ];
        let plans = plan_duplicate_collapse(&candidates);
        assert_eq!(plans.len(), 1, "only the pharmacy group has >1 member");
        assert_eq!(plans[0].keeper_id, "b");
        assert_eq!(plans[0].duplicate_ids, vec!["a".to_string()]);
    }

    #[test]
    fn collapse_plan_never_retires_a_confirmed_or_conflicted_loser() {
        // Two "confirmed" members of the same normalized group: select_keeper
        // picks the first-found confirmed one, but the OTHER confirmed member
        // must be dropped from duplicate_ids, never scheduled for retirement.
        let candidates = vec![
            candidate(
                "a",
                "call pharmacy",
                Some("2026-01-01T00:00:00Z"),
                "confirmed",
            ),
            candidate(
                "b",
                "call pharmacy",
                Some("2026-07-15T00:00:00Z"),
                "confirmed",
            ),
            candidate(
                "c",
                "call pharmacy",
                Some("2026-07-15T00:00:00Z"),
                "conflicted",
            ),
        ];
        let plans = plan_duplicate_collapse(&candidates);
        assert!(
            plans.is_empty(),
            "no retirable (proposed/inferred) duplicates in this group: {plans:?}"
        );
    }

    #[test]
    fn collapse_plan_skips_singleton_groups() {
        let candidates = vec![candidate("a", "call pharmacy", None, "proposed")];
        assert!(plan_duplicate_collapse(&candidates).is_empty());
    }

    #[test]
    fn collapse_plan_is_deterministic_across_input_order() {
        let forward = vec![
            candidate(
                "a",
                "call pharmacy",
                Some("2026-01-01T00:00:00Z"),
                "proposed",
            ),
            candidate(
                "b",
                "call pharmacy",
                Some("2026-07-15T00:00:00Z"),
                "proposed",
            ),
        ];
        let mut reversed = forward.clone();
        reversed.reverse();
        assert_eq!(
            plan_duplicate_collapse(&forward),
            plan_duplicate_collapse(&reversed)
        );
    }

    // ── env gating ────────────────────────────────────────────────────────

    #[test]
    fn parse_enabled_accepts_known_truthy_forms_case_insensitive() {
        for v in ["1", "true", "TRUE", "True", "yes", "YES", "  yes  "] {
            assert!(parse_enabled(Some(v)), "{v:?} must be truthy");
        }
    }

    #[test]
    fn parse_enabled_rejects_everything_else() {
        for v in ["0", "false", "no", "", "   ", "on", "enabled"] {
            assert!(!parse_enabled(Some(v)), "{v:?} must not be truthy");
        }
        assert!(!parse_enabled(None), "unset must default OFF");
    }

    // ── apply_write_cap ───────────────────────────────────────────────────

    #[test]
    fn write_cap_truncates_and_reports_capped() {
        let ops: Vec<i32> = (0..10).collect();
        let (applied, capped) = apply_write_cap(ops, 3);
        assert_eq!(applied, vec![0, 1, 2]);
        assert!(capped);
    }

    #[test]
    fn write_cap_passes_through_when_under_the_limit() {
        let ops = vec!["a", "b"];
        let (applied, capped) = apply_write_cap(ops, 10);
        assert_eq!(applied, vec!["a", "b"]);
        assert!(!capped);
    }

    #[test]
    fn write_cap_boundary_exact_limit_is_not_capped() {
        let ops: Vec<i32> = (0..5).collect();
        let (applied, capped) = apply_write_cap(ops, 5);
        assert_eq!(applied.len(), 5);
        assert!(!capped);
    }
}
