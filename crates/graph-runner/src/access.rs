use crate::graph::Identity;

/// Evaluate whether a visibility tag list permits access for the given identity.
///
/// Rules:
/// - "public"          → visible to everyone
/// - "hotel-public"    → visible to everyone within the hotel (same semantics for now)
/// - "role:<name>"     → visible if identity carries that role
/// - "identity:<id>"   → visible if identity.id matches exactly
///
/// An empty visibility list means "fall back to the graph's default_visibility",
/// which the caller resolves before calling this function.
pub fn visibility_permits(tags: &[String], identity: &Identity) -> bool {
    for tag in tags {
        match tag.as_str() {
            "public" | "hotel-public" => return true,
            other => {
                if let Some(role) = other.strip_prefix("role:") {
                    if identity.roles.iter().any(|r| r == role) {
                        return true;
                    }
                } else if let Some(id) = other.strip_prefix("identity:") {
                    if identity.id == id {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Resolve effective visibility for a node whose stored list may be empty.
/// Falls back to the graph-level default_visibility when the list is empty.
pub fn resolve_node_visibility(
    node_visibility: &[String],
    graph_default: &str,
    identity: &Identity,
) -> bool {
    if node_visibility.is_empty() {
        return default_visibility_permits(graph_default, identity);
    }
    visibility_permits(node_visibility, identity)
}

/// Resolve effective visibility for an edge.
/// The edge is visible only if BOTH endpoints are visible AND the edge's own
/// visibility permits. Endpoint visibility must be pre-evaluated by the caller.
pub fn resolve_edge_visibility(
    edge_visibility: &[String],
    graph_default: &str,
    from_visible: bool,
    to_visible: bool,
    identity: &Identity,
) -> bool {
    if !from_visible || !to_visible {
        return false;
    }
    if edge_visibility.is_empty() {
        return default_visibility_permits(graph_default, identity);
    }
    visibility_permits(edge_visibility, identity)
}

fn default_visibility_permits(default_visibility: &str, _identity: &Identity) -> bool {
    matches!(default_visibility, "public" | "hotel-public")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Identity;

    fn alice() -> Identity {
        Identity::new("alice").with_roles(vec!["researcher".into()])
    }

    fn bob() -> Identity {
        Identity::new("bob").with_roles(vec!["viewer".into()])
    }

    #[test]
    fn public_tag_permits_anyone() {
        assert!(visibility_permits(&["public".into()], &alice()));
        assert!(visibility_permits(&["public".into()], &bob()));
    }

    #[test]
    fn hotel_public_permits_anyone() {
        assert!(visibility_permits(&["hotel-public".into()], &alice()));
        assert!(visibility_permits(&["hotel-public".into()], &bob()));
    }

    #[test]
    fn role_tag_permits_matching_role_only() {
        let tags = vec!["role:researcher".into()];
        assert!(visibility_permits(&tags, &alice()));
        assert!(!visibility_permits(&tags, &bob()));
    }

    #[test]
    fn identity_tag_permits_exact_match_only() {
        let tags = vec!["identity:alice".into()];
        assert!(visibility_permits(&tags, &alice()));
        assert!(!visibility_permits(&tags, &bob()));
    }

    #[test]
    fn empty_tags_with_public_default_permits() {
        assert!(resolve_node_visibility(&[], "public", &bob()));
    }

    #[test]
    fn empty_tags_with_private_default_denies() {
        assert!(!resolve_node_visibility(&[], "private", &bob()));
    }

    #[test]
    fn edge_denied_when_endpoint_hidden() {
        let tags = vec!["public".into()];
        assert!(!resolve_edge_visibility(&tags, "public", false, true, &alice()));
        assert!(!resolve_edge_visibility(&tags, "public", true, false, &alice()));
    }

    #[test]
    fn edge_permitted_when_both_endpoints_visible() {
        let tags = vec!["public".into()];
        assert!(resolve_edge_visibility(&tags, "public", true, true, &alice()));
    }

    #[test]
    fn multiple_tags_any_match_permits() {
        let tags = vec!["role:admin".into(), "identity:alice".into()];
        assert!(visibility_permits(&tags, &alice())); // matches identity:alice
        assert!(!visibility_permits(&tags, &bob()));  // matches neither
    }
}
