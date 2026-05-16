use anyhow::{Result, bail};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Debug)]
pub enum TranslatedQuery {
    SelectNodes {
        label: Option<String>,
        filters: HashMap<String, Value>,
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
        let re_match_node = Regex::new(
            r"(?i)MATCH\s*\((?P<var>\w+):(?P<label>\w+)\s*(?P<props>\{.*\})?\)\s*RETURN\s+(?P<ret>\w+)",
        )?;
        if let Some(caps) = re_match_node.captures(query) {
            let label = caps.name("label").map(|m| m.as_str().to_string());
            return Ok(TranslatedQuery::SelectNodes {
                label,
                filters: HashMap::new(),
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
