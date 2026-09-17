//! Operator text corrections. Deliberately not a model-projected tool.
//! The edge gateway supplies the enrolled device identity; callers cannot
//! change labels, trust, provenance, relationships, or arbitrary properties.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NodeEdit {
    pub id: String,
    pub actor: String,
    pub before: BTreeMap<String, Option<String>>,
    pub changes: BTreeMap<String, String>,
}

impl NodeEdit {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.id.starts_with("life:") || self.id.len() > 512 || self.actor.trim().is_empty() {
            return Err("canonical life node id and authenticated actor required");
        }
        if self.changes.is_empty()
            || self.changes.len() > 3
            || self.before.keys().ne(self.changes.keys())
        {
            return Err("supply original values for exactly the changed fields");
        }
        for (key, value) in &self.changes {
            if !["title", "claim_summary", "description"].contains(&key.as_str()) {
                return Err("only title, claim_summary and description are editable");
            }
            if value.len() > 16_384 || (key == "claim_summary" && value.trim().is_empty()) {
                return Err("text too long or summary empty");
            }
            if self.before[key].as_ref().is_some_and(|v| v.len() > 16_384) {
                return Err("original text too long");
            }
        }
        Ok(())
    }
}

// One statement: compare-and-set plus append-only audit. No MERGE, no implicit
// creation, and no numeric internal-id fallback. Ambiguous ids fail closed.
pub const EDIT_QUERY: &str = "MATCH (n {id: $id}) \
    WHERE NOT n:LifeNodeEdit \
    WITH collect(n) AS candidates WHERE size(candidates) = 1 \
    UNWIND candidates AS n \
    WITH n WHERE all(k IN keys($changes) WHERE \
        coalesce(n[k] = $before[k], n[k] IS NULL AND $before[k] IS NULL)) \
    CREATE (a:LifeNodeEdit {id: $audit_id, node_id: $id, actor: $actor, \
        edited_at: $edited_at, before_json: $before_json, after_json: $after_json}) \
    SET n += $changes \
    RETURN n.id AS node_id, a.id AS audit_id";

#[cfg(test)]
mod tests {
    use super::*;
    fn edit() -> NodeEdit {
        NodeEdit {
            id: "life:goal:test".into(),
            actor: "edge:mac".into(),
            before: BTreeMap::from([("title".into(), None)]),
            changes: BTreeMap::from([("title".into(), "New title".into())]),
        }
    }
    #[test]
    fn accepts_text_correction() {
        assert!(edit().validate().is_ok());
    }
    #[test]
    fn requires_exact_original_fields() {
        let mut e = edit();
        e.before.clear();
        assert!(e.validate().is_err());
    }
    #[test]
    fn rejects_authority_fields() {
        for key in ["id", "validation_state", "confidence", "provenance"] {
            let mut e = edit();
            e.before = BTreeMap::from([(key.into(), None)]);
            e.changes = BTreeMap::from([(key.into(), "confirmed".into())]);
            assert!(e.validate().is_err());
        }
    }
    #[test]
    fn rejects_internal_ids_and_empty_actor() {
        let mut e = edit();
        e.id = "123".into();
        assert!(e.validate().is_err());
        e = edit();
        e.actor.clear();
        assert!(e.validate().is_err());
    }
    #[test]
    fn query_keeps_audit_and_update_in_one_statement() {
        assert!(EDIT_QUERY.contains("CREATE (a:LifeNodeEdit"));
        assert!(EDIT_QUERY.contains("SET n += $changes"));
        assert!(!EDIT_QUERY.contains("MERGE"));
    }
}
