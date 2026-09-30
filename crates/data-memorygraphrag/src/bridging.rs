//! Bridge an incoming claim to what it names.
//!
//! A lived fact arrives as prose plus a label. Left alone it lands as an
//! island, and the gardener's only remedy is a `SCOPED_TO` spoke to the
//! operator's anchor role — a hub with spokes, not a graph. Live 2026-09-17:
//! "Jared dropped Daxton off for his class" was written with no edge to
//! `life:person:daxton` until the operator asked for one by hand, and a
//! practice Event naming Moonlight III had no edge to the CreativeWork.
//!
//! This module is the deterministic half of that work: given the claim's
//! text and the live nodes it could be talking about, it returns the edges
//! the ontology would allow. Endpoint-validated pairs (`INVOLVES`,
//! `OCCURS_AT`, `ABOUT`, …) are safe to write; anything else is advisory,
//! for the turn or the sweep to decide.

use crate::cypher::AGENDA_EDGE_RULES;

/// A live node an incoming claim might be about.
#[derive(Debug, Clone, PartialEq)]
pub struct BridgeTarget {
    pub id: String,
    pub label: String,
    /// `title` property when the node has one; the id slug is used otherwise.
    pub title: Option<String>,
}

/// An edge the ontology allows between the incoming claim and a live node.
#[derive(Debug, Clone, PartialEq)]
pub struct BridgeEdge {
    pub target_id: String,
    pub target_label: String,
    pub rel_type: String,
    /// The word in the claim that named the target.
    pub matched: String,
    /// True when the rel type is endpoint-validated for this label pair and
    /// therefore safe to write without asking.
    pub validated: bool,
}

/// Words that name nothing in particular. A target whose only distinctive
/// token is one of these is not "named" by a claim that happens to use it.
const GENERIC: &[&str] = &[
    "practice",
    "session",
    "morning",
    "evening",
    "class",
    "routine",
    "event",
    "update",
    "reminder",
    "check",
    "checkin",
    "appointment",
    "meeting",
    "call",
    "plan",
    "review",
    "sweep",
    "daily",
    "weekly",
    "today",
    "tonight",
    "tomorrow",
    "yesterday",
    "loop",
    "task",
    "note",
    "item",
    "work",
    "time",
    "schedule",
    "trip",
    "visit",
    "record",
    "graph",
    "lifegraph",
    "operator",
    "jared",
];

fn normalize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// The tokens that make a target recognizable in prose: its title when it has
/// one, else its id's last segment, minus dates, numbers and generic words.
fn distinctive_tokens(target: &BridgeTarget) -> Vec<String> {
    let source = target
        .title
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            target
                .id
                .rsplit(':')
                .next()
                .unwrap_or(target.id.as_str())
                .replace(['_', '-'], " ")
        });
    normalize(&source)
        .into_iter()
        .filter(|t| {
            t.len() >= 5 && !t.chars().all(|c| c.is_ascii_digit()) && !GENERIC.contains(&t.as_str())
        })
        .collect()
}

/// The endpoint-validated rel type for this label pair, if the ontology has
/// one. The first matching rule wins, mirroring the order in
/// `AGENDA_EDGE_RULES`.
pub fn validated_rel_type(source_label: &str, target_label: &str) -> Option<&'static str> {
    AGENDA_EDGE_RULES
        .iter()
        .find(|r| {
            r.source_labels.contains(&source_label) && r.target_labels.contains(&target_label)
        })
        .map(|r| r.rel_type)
}

/// Every edge the incoming claim earns against the live nodes it names.
/// Validated pairs first, then advisory ones; at most `limit` of each.
pub fn bridges_for(
    source_label: &str,
    source_id: &str,
    summary: &str,
    targets: &[BridgeTarget],
    limit: usize,
) -> Vec<BridgeEdge> {
    let words: Vec<String> = normalize(summary);
    if words.is_empty() {
        return Vec::new();
    }
    let mut edges: Vec<BridgeEdge> = Vec::new();
    for target in targets {
        if target.id == source_id || target.label == "Role" {
            continue;
        }
        let tokens = distinctive_tokens(target);
        let Some(matched) = tokens.iter().find(|t| words.contains(t)) else {
            continue;
        };
        let validated = validated_rel_type(source_label, &target.label);
        edges.push(BridgeEdge {
            target_id: target.id.clone(),
            target_label: target.label.clone(),
            rel_type: validated.unwrap_or("RELATES_TO").to_string(),
            matched: matched.clone(),
            validated: validated.is_some(),
        });
    }
    edges.sort_by(|a, b| {
        b.validated
            .cmp(&a.validated)
            .then_with(|| b.matched.len().cmp(&a.matched.len()))
            .then_with(|| a.target_id.cmp(&b.target_id))
    });
    edges.dedup_by(|a, b| a.target_id == b.target_id);
    edges.truncate(limit);
    edges
}

/// The category Roles a claim belongs to: a Role whose own name appears in
/// the claim ("organ" → `life:role:organist` via the role's tokens). The
/// gardener anchors an orphan to one of these instead of parking every node
/// on the operator's chief-of-staff role.
pub fn category_roles_for(summary: &str, roles: &[BridgeTarget]) -> Vec<String> {
    let words = normalize(summary);
    let mut out: Vec<String> = Vec::new();
    for role in roles.iter().filter(|r| r.label == "Role") {
        let tokens = distinctive_tokens(role);
        // A role's stem carries it: "organist" is named by "organ".
        if tokens.iter().any(|t| {
            words.iter().any(|w| {
                w == t
                    || (t.len() >= 6
                        && w.len() >= 5
                        && (t.starts_with(w.as_str()) || w.starts_with(t.as_str())))
            })
        }) && !out.contains(&role.id)
        {
            out.push(role.id.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(id: &str, label: &str) -> BridgeTarget {
        BridgeTarget {
            id: id.into(),
            label: label.into(),
            title: None,
        }
    }

    /// The live 2026-09-17 shapes: a drop-off Event naming Daxton, a practice
    /// Event naming a piece, and a claim that names nobody.
    #[test]
    fn a_claim_earns_edges_to_what_it_names() {
        let targets = vec![
            target("life:person:daxton", "Person"),
            target("life:person:zerin_maluy_likes", "Person"),
            target(
                "life:creative_work:beethoven-moonlight-mvt3",
                "CreativeWork",
            ),
            target("life:place:workplace_att_lenox", "Place"),
        ];
        let edges = bridges_for(
            "Event",
            "life:event:daxton_class_dropoff_20260917",
            "Jared dropped Daxton off for his class on September 17, 2026, running a bit late.",
            &targets,
            5,
        );
        assert_eq!(edges.len(), 1, "{edges:?}");
        assert_eq!(edges[0].target_id, "life:person:daxton");
        assert_eq!(edges[0].rel_type, "INVOLVES");
        assert!(edges[0].validated, "Event -> Person is endpoint-validated");

        // An Event naming a piece: the ontology has no validated Event ->
        // CreativeWork edge, so it is advisory.
        let edges = bridges_for(
            "Event",
            "life:event:music_practice_20260916_tonight",
            "Jared practiced organ (two hymns) and piano: Moonlight III, nocturnes, Waltz 64/2.",
            &targets,
            5,
        );
        assert_eq!(
            edges[0].target_id,
            "life:creative_work:beethoven-moonlight-mvt3"
        );
        assert!(!edges[0].validated);
        assert_eq!(edges[0].rel_type, "RELATES_TO");

        // A loop about a subscription IS validated (ABOUT).
        let subs = vec![target(
            "life:subscription:bronze_david_bars",
            "Subscription",
        )];
        let edges = bridges_for(
            "OpenLoop",
            "life:open_loop:cancel_bronze_david_bars",
            "Cancel the Bronze David Bars subscription.",
            &subs,
            5,
        );
        assert_eq!(edges[0].rel_type, "ABOUT");
        assert!(edges[0].validated);

        // Names nobody: no edges, and generic words never bridge.
        assert!(
            bridges_for(
                "Event",
                "life:event:x",
                "A quiet evening at home.",
                &targets,
                5
            )
            .is_empty()
        );
        let generic = vec![target("life:routine:morning_practice_session", "Routine")];
        assert!(
            bridges_for(
                "Event",
                "life:event:y",
                "Jared had a practice session this morning.",
                &generic,
                5
            )
            .is_empty(),
            "generic words must not bridge"
        );
    }

    #[test]
    fn category_roles_come_from_the_claim() {
        let roles = vec![
            target("life:role:organist", "Role"),
            target("life:role:pianist", "Role"),
            target("life:role:health-and-wellness", "Role"),
            target("life:role:chief-of-staff", "Role"),
        ];
        let got = category_roles_for(
            "Jared practiced the organ tonight before sacrament meeting.",
            &roles,
        );
        assert_eq!(got, vec!["life:role:organist".to_string()]);
        let got = category_roles_for(
            "Prostate assay results came back well within range.",
            &roles,
        );
        assert!(got.is_empty(), "{got:?}");
        assert!(category_roles_for("Nothing in particular.", &roles).is_empty());
    }
}
