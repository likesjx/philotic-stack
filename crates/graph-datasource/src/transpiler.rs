use anyhow::{Result, bail};
use regex::Regex;
use serde_json::{Value, json};

#[derive(Debug)]
pub enum TranslatedQuery {
    SelectNodes {
        label: Option<String>,
        id_filter: Option<String>,
    },
    InsertNode {
        id: String,
        label: String,
        properties: Value,
    },
    InsertEdge {
        id: String,
        source_id: String,
        target_id: String,
        label: String,
        properties: Value,
    },
    DeleteNode {
        id: String,
    },
    DeleteEdge {
        id: String,
    },
}

fn parse_cypher_props(props_str: &str) -> Result<Value> {
    let re_keys = Regex::new(r"(\w+)\s*:")?;
    let fixed_props = re_keys.replace_all(props_str, "\"$1\":");
    let json_props = fixed_props.replace("'", "\"");
    let properties: Value = serde_json::from_str(&json_props)?;
    Ok(properties)
}

pub fn transpile_cypher(query: &str) -> Result<TranslatedQuery> {
    let query = query.trim();

    if query.to_uppercase().starts_with("MATCH") {
        // Delete node by id:
        //   MATCH (n {id: 'X'}) DELETE n
        //   MATCH (n:Label {id: 'X'}) DETACH DELETE n
        let re_delete_node = Regex::new(
            r"(?i)MATCH\s*\(\s*\w+(?::[\w]+)?\s*(?P<props>\{[^}]*\})\s*\)\s*(?:DETACH\s+)?DELETE\s+\w+",
        )?;
        if let Some(caps) = re_delete_node.captures(query) {
            let props = parse_cypher_props(caps.name("props").unwrap().as_str())?;
            let id = props
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| anyhow::anyhow!("DELETE node requires an 'id' property"))?;
            return Ok(TranslatedQuery::DeleteNode { id });
        }

        // Delete edge by id:
        //   MATCH ()-[r {id: 'X'}]-() DELETE r
        //   MATCH ()-[r {id: 'X'}]->() DELETE r
        let re_delete_edge = Regex::new(
            r"(?i)MATCH\s*\([^)]*\)\s*-\[\s*\w+(?::[\w]+)?\s*(?P<props>\{[^}]+\})\s*\]-?>?\s*\([^)]*\)\s*(?:DETACH\s+)?DELETE\s+\w+",
        )?;
        if let Some(caps) = re_delete_edge.captures(query) {
            let props = parse_cypher_props(caps.name("props").unwrap().as_str())?;
            let id = props
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| anyhow::anyhow!("DELETE edge requires an 'id' property"))?;
            return Ok(TranslatedQuery::DeleteEdge { id });
        }

        // MATCH with label, optional inline props:
        //   MATCH (n:Label) RETURN n
        //   MATCH (n:Label {id: 'X'}) RETURN n
        let re_match_labeled = Regex::new(
            r"(?i)MATCH\s*\(\s*\w+\s*:\s*(?P<label>\w+)\s*(?P<props>\{[^}]*\})?\s*\)\s*RETURN\s+\w+",
        )?;
        if let Some(caps) = re_match_labeled.captures(query) {
            let label = caps.name("label").map(|m| m.as_str().to_string());
            let id_filter = caps
                .name("props")
                .and_then(|p| parse_cypher_props(p.as_str()).ok())
                .and_then(|props| {
                    props
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                });
            return Ok(TranslatedQuery::SelectNodes { label, id_filter });
        }

        // MATCH all nodes (no label):  MATCH (n) RETURN n
        let re_match_all = Regex::new(r"(?i)MATCH\s*\(\s*\w+\s*\)\s*RETURN\s+\w+")?;
        if re_match_all.is_match(query) {
            return Ok(TranslatedQuery::SelectNodes {
                label: None,
                id_filter: None,
            });
        }
    }

    if query.to_uppercase().starts_with("CREATE") {
        let re_create_rel = Regex::new(
            r"(?i)CREATE\s*\((?P<s_var>\w+):(?P<s_label>\w+)\s*(?P<s_props>\{.*\})\)-\[:(?P<r_label>\w+)\s*(?P<r_props>\{.*\})?\]->\((?P<t_var>\w+):(?P<t_label>\w+)\s*(?P<t_props>\{.*\})\)",
        )?;
        if let Some(caps) = re_create_rel.captures(query) {
            let s_props = parse_cypher_props(caps.name("s_props").unwrap().as_str())?;
            let t_props = parse_cypher_props(caps.name("t_props").unwrap().as_str())?;
            let r_props = if let Some(p) = caps.name("r_props") {
                parse_cypher_props(p.as_str())?
            } else {
                json!({})
            };

            let source_id = s_props
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_default();
            let target_id = t_props
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_default();
            let r_label = caps.name("r_label").unwrap().as_str().to_string();
            let r_id = r_props
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

            return Ok(TranslatedQuery::InsertEdge {
                id: r_id,
                source_id,
                target_id,
                label: r_label,
                properties: r_props,
            });
        }

        let re_create_node =
            Regex::new(r"(?i)CREATE\s*\((?P<var>\w+):(?P<label>\w+)\s*(?P<props>\{.*\})\)")?;
        if let Some(caps) = re_create_node.captures(query) {
            let label = caps.name("label").unwrap().as_str().to_string();
            let props_str = caps.name("props").unwrap().as_str();
            let properties = parse_cypher_props(props_str)?;
            let id = properties
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

            return Ok(TranslatedQuery::InsertNode {
                id,
                label,
                properties,
            });
        }
    }

    bail!("unsupported or invalid cypher query: {}", query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_all_nodes_no_label() {
        let q = transpile_cypher("MATCH (n) RETURN n").unwrap();
        assert!(matches!(
            q,
            TranslatedQuery::SelectNodes {
                label: None,
                id_filter: None
            }
        ));
    }

    #[test]
    fn match_label_only() {
        let q = transpile_cypher("MATCH (n:Task) RETURN n").unwrap();
        match q {
            TranslatedQuery::SelectNodes { label, id_filter } => {
                assert_eq!(label.as_deref(), Some("Task"));
                assert!(id_filter.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn match_label_with_id_filter() {
        let q = transpile_cypher("MATCH (n:Task {id: 'task-1'}) RETURN n").unwrap();
        match q {
            TranslatedQuery::SelectNodes { label, id_filter } => {
                assert_eq!(label.as_deref(), Some("Task"));
                assert_eq!(id_filter.as_deref(), Some("task-1"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn delete_node_without_label() {
        let q = transpile_cypher("MATCH (n {id: 'node-abc'}) DELETE n").unwrap();
        match q {
            TranslatedQuery::DeleteNode { id } => assert_eq!(id, "node-abc"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn delete_node_with_label_and_detach() {
        let q = transpile_cypher("MATCH (n:Task {id: 'task-1'}) DETACH DELETE n").unwrap();
        match q {
            TranslatedQuery::DeleteNode { id } => assert_eq!(id, "task-1"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn delete_edge_undirected() {
        let q = transpile_cypher("MATCH ()-[r {id: 'edge-xyz'}]-() DELETE r").unwrap();
        match q {
            TranslatedQuery::DeleteEdge { id } => assert_eq!(id, "edge-xyz"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn delete_edge_directed() {
        let q = transpile_cypher("MATCH ()-[r {id: 'edge-xyz'}]->() DELETE r").unwrap();
        match q {
            TranslatedQuery::DeleteEdge { id } => assert_eq!(id, "edge-xyz"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn create_node() {
        let q = transpile_cypher("CREATE (n:Task {id: 'task-1', name: 'Setup graph'})").unwrap();
        match q {
            TranslatedQuery::InsertNode { id, label, .. } => {
                assert_eq!(id, "task-1");
                assert_eq!(label, "Task");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn create_edge() {
        let q =
            transpile_cypher("CREATE (a:Task {id: 'task-1'})-[:BLOCKS]->(b:Task {id: 'task-2'})")
                .unwrap();
        match q {
            TranslatedQuery::InsertEdge {
                source_id,
                target_id,
                label,
                ..
            } => {
                assert_eq!(source_id, "task-1");
                assert_eq!(target_id, "task-2");
                assert_eq!(label, "BLOCKS");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn unsupported_query_errors() {
        assert!(transpile_cypher("SELECT * FROM nodes").is_err());
        assert!(transpile_cypher("MERGE (n:Task {id: 'x'})").is_err());
    }
}
