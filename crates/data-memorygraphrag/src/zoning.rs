// Life Graph soft-zoning: the domain Role seed (V005).
//
// One shared graph, soft boundaries. Every steward agent gets a domain Role
// node it can anchor OWNS/SHAPES/SETS/SPAWNS/RELATES_TO edges to. The seed is
// pure data + deterministic Cypher so it is unit-testable and can be applied
// by the runner at deploy time (or manually via mgconsole, one statement per
// call — Bolt does not support multi-statement batches).
//
// Reconciliation with pre-existing Role nodes (audited 2026-07-06):
//   - "human"                    -> Human domain (coach: health + workouts)
//   - "life:role:musician"       -> Musician domain (bjork)
//   - "life:role:ai_architect"   -> Architect domain (aria)
//   - husband / father / friend / brother_son / toastmasters-sergeant-at-arms
//     are the operator's personal life roles: no obvious domain match, left
//     untouched (the seed only MERGEs by id — it never deletes or relabels).

/// A steward domain in the soft-zoning map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainRoleSeed {
    /// Stable Role node id (MERGE key).
    pub role_node_id: &'static str,
    /// Human-readable role name; only filled in when missing on the node.
    pub name: &'static str,
    /// Queryable zone slug — the property the OWNS/steward queries filter on.
    pub domain_slug: &'static str,
    /// Canonical steward agent slug (e.g. "astrid").
    pub steward_agent: &'static str,
    /// Short description of the zone's scope.
    pub domain_scope: &'static str,
    /// True when the node id already existed in the live graph before V005.
    pub reuses_existing_node: bool,
}

/// The operator-approved domain map. Order is stable and part of the contract.
pub const DOMAIN_ROLE_SEEDS: &[DomainRoleSeed] = &[
    DomainRoleSeed {
        role_node_id: "life:role:librarian",
        name: "Librarian",
        domain_slug: "librarian",
        steward_agent: "astrid",
        domain_scope: "notes + Obsidian",
        reuses_existing_node: false,
    },
    DomainRoleSeed {
        role_node_id: "life:role:communications",
        name: "Communications",
        domain_slug: "communications",
        steward_agent: "ariel",
        domain_scope: "contacts + relationships",
        reuses_existing_node: false,
    },
    DomainRoleSeed {
        role_node_id: "life:role:companion",
        name: "Companion",
        domain_slug: "companion",
        steward_agent: "jane",
        domain_scope: "companionship",
        reuses_existing_node: false,
    },
    DomainRoleSeed {
        role_node_id: "life:role:ai_architect",
        name: "Architect",
        domain_slug: "architect",
        steward_agent: "aria",
        domain_scope: "philotic + AI research",
        reuses_existing_node: true,
    },
    DomainRoleSeed {
        role_node_id: "life:role:chief-of-staff",
        name: "ChiefOfStaff",
        domain_slug: "chief_of_staff",
        steward_agent: "beacon",
        domain_scope: "goals + open loops + progress",
        reuses_existing_node: false,
    },
    DomainRoleSeed {
        role_node_id: "life:role:musician",
        name: "Musician",
        domain_slug: "musician",
        steward_agent: "bjork",
        domain_scope: "music",
        reuses_existing_node: true,
    },
    DomainRoleSeed {
        role_node_id: "human",
        name: "Human",
        domain_slug: "human",
        steward_agent: "coach",
        domain_scope: "health + workouts",
        reuses_existing_node: true,
    },
];

/// Identifier stamped on every node the V005 seed touches.
pub const ZONING_SEED_VERSION: &str = "V005";

fn escape_cypher_literal(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Compile one idempotent MERGE statement per domain Role.
///
/// - MERGE by id: never creates duplicates, never deletes anything.
/// - `name` is only set when the node has no name yet (coalesce), so the
///   pre-existing "Human" node keeps its display name.
/// - `domain_slug` / `steward_agent` / `domain_scope` are re-asserted on every
///   run (SET), so re-applying the seed converges to the same state.
pub fn compile_domain_role_seed_statements() -> Vec<String> {
    DOMAIN_ROLE_SEEDS
        .iter()
        .map(|seed| {
            format!(
                concat!(
                    "MERGE (r:Role {{id: \"{id}\"}}) ",
                    "SET r.name = coalesce(r.name, \"{name}\"), ",
                    "r.domain_slug = \"{domain_slug}\", ",
                    "r.steward_agent = \"{steward_agent}\", ",
                    "r.domain_scope = \"{domain_scope}\", ",
                    "r.zoning_seed = \"{seed_version}\";",
                ),
                id = escape_cypher_literal(seed.role_node_id),
                name = escape_cypher_literal(seed.name),
                domain_slug = escape_cypher_literal(seed.domain_slug),
                steward_agent = escape_cypher_literal(seed.steward_agent),
                domain_scope = escape_cypher_literal(seed.domain_scope),
                seed_version = ZONING_SEED_VERSION,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn seed_covers_the_seven_domains() {
        let slugs: Vec<&str> = DOMAIN_ROLE_SEEDS.iter().map(|s| s.domain_slug).collect();
        assert_eq!(
            slugs,
            vec![
                "librarian",
                "communications",
                "companion",
                "architect",
                "chief_of_staff",
                "musician",
                "human",
            ]
        );
    }

    #[test]
    fn seed_ids_and_slugs_are_unique() {
        let ids: BTreeSet<&str> = DOMAIN_ROLE_SEEDS.iter().map(|s| s.role_node_id).collect();
        let slugs: BTreeSet<&str> = DOMAIN_ROLE_SEEDS.iter().map(|s| s.domain_slug).collect();
        assert_eq!(ids.len(), DOMAIN_ROLE_SEEDS.len());
        assert_eq!(slugs.len(), DOMAIN_ROLE_SEEDS.len());
    }

    #[test]
    fn seed_reuses_the_three_matched_existing_nodes() {
        let reused: Vec<&str> = DOMAIN_ROLE_SEEDS
            .iter()
            .filter(|s| s.reuses_existing_node)
            .map(|s| s.role_node_id)
            .collect();
        assert_eq!(
            reused,
            vec!["life:role:ai_architect", "life:role:musician", "human"]
        );
    }

    #[test]
    fn seed_statements_are_idempotent_merge_only() {
        let statements = compile_domain_role_seed_statements();
        assert_eq!(statements.len(), DOMAIN_ROLE_SEEDS.len());
        for stmt in &statements {
            assert!(
                stmt.starts_with("MERGE (r:Role {id: "),
                "statement must MERGE by Role id: {stmt}"
            );
            assert!(
                !stmt.contains("CREATE ") && !stmt.contains("DELETE") && !stmt.contains("REMOVE"),
                "seed must never create blindly, delete, or remove: {stmt}"
            );
            assert!(
                stmt.contains("coalesce(r.name,"),
                "existing display names must be preserved: {stmt}"
            );
            assert!(stmt.contains("r.zoning_seed = \"V005\""));
        }
    }

    #[test]
    fn seed_statements_repeat_to_identical_cypher() {
        // Deterministic compilation is what makes re-application idempotent
        // end-to-end: same statements, MERGE by id, SET converges.
        assert_eq!(
            compile_domain_role_seed_statements(),
            compile_domain_role_seed_statements()
        );
    }
}
