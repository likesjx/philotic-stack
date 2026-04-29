//! Inbound and outbound transform engine for MCP endpoint configs.
//!
//! Inbound: maps MCP `tools/call` args to a router envelope content blob.
//! Outbound: maps the router response back to an MCP `tools/call` result string.

use ansible_mesh_core::mcp_endpoint::{
    FieldMapping, McpInboundTransform, McpOutboundTransform, McpToolSpec,
};
use serde_json::Value;

// ── Inbound ───────────────────────────────────────────────────────────────────

/// Result of applying an inbound transform: the JSON payload string to embed
/// in the envelope content, plus the action name for `command`.
pub struct InboundResult {
    pub action: String,
    pub payload: Value,
    pub target_kind: String,
    pub target_id: String,
}

/// Apply the inbound transform for `spec` to the caller-supplied `args`.
///
/// Returns `Err` only for Template variants (Phase 4) or malformed specs.
pub fn apply_inbound(spec: &McpToolSpec, args: &Value) -> Result<InboundResult, String> {
    match &spec.inbound_transform {
        McpInboundTransform::FieldMap {
            action,
            target,
            mappings,
        } => {
            let payload = apply_field_map(args, mappings);
            let (target_kind, target_id) = target_parts(target);
            Ok(InboundResult {
                action: action.clone(),
                payload,
                target_kind,
                target_id,
            })
        }
        McpInboundTransform::Template { .. } => {
            Err("Template inbound transform is not yet supported (Phase 4)".into())
        }
    }
}

/// Apply dot-path field mappings from `args` into a new payload object.
///
/// `from` and `to` are dot-separated key paths (e.g. `"query"`, `"payload.q"`).
/// Missing source keys are silently skipped. Nested destination paths are
/// auto-created.
fn apply_field_map(args: &Value, mappings: &[FieldMapping]) -> Value {
    let mut payload = serde_json::Map::new();

    for mapping in mappings {
        if let Some(value) = get_dot_path(args, &mapping.from) {
            set_dot_path(&mut payload, &mapping.to, value.clone());
        }
    }

    // If no mappings are declared, pass the full args object through.
    if mappings.is_empty() {
        if let Some(obj) = args.as_object() {
            payload = obj.clone();
        }
    }

    Value::Object(payload)
}

fn get_dot_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(value, |acc, key| match acc {
        Value::Array(arr) => key.parse::<usize>().ok().and_then(|i| arr.get(i)),
        _ => acc.get(key),
    })
}

fn set_dot_path(obj: &mut serde_json::Map<String, Value>, path: &str, value: Value) {
    let mut parts = path.splitn(2, '.');
    let key = parts.next().unwrap_or(path);
    if let Some(rest) = parts.next() {
        let child = obj
            .entry(key.to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Value::Object(child_map) = child {
            set_dot_path(child_map, rest, value);
        }
    } else {
        obj.insert(key.to_string(), value);
    }
}

fn target_parts(target: &ansible_mesh_core::mcp_route::McpRouteTarget) -> (String, String) {
    use ansible_mesh_core::mcp_route::McpRouteTarget;
    match target {
        McpRouteTarget::Philote { agent_id } => ("philote".into(), agent_id.clone()),
        McpRouteTarget::Tool { tool_ref } => ("tool".into(), tool_ref.clone()),
        McpRouteTarget::Datasource { datasource_id } => {
            ("datasource".into(), datasource_id.clone())
        }
    }
}

// ── Outbound ──────────────────────────────────────────────────────────────────

/// Apply the outbound transform for `spec` to the raw response string from the
/// router. Returns the transformed string to send as the MCP result.
pub fn apply_outbound(spec: &McpToolSpec, response: &str) -> String {
    match &spec.outbound_transform {
        McpOutboundTransform::PassThrough => response.to_string(),

        McpOutboundTransform::Extract { path } => {
            // Parse the response and extract the dot-path field.
            let parsed: Value = match serde_json::from_str(response) {
                Ok(v) => v,
                Err(_) => return response.to_string(), // not JSON — return as-is
            };
            match get_dot_path(&parsed, path) {
                Some(v) => match v {
                    Value::String(s) => s.clone(),
                    other => serde_json::to_string(other).unwrap_or_else(|_| response.to_string()),
                },
                None => response.to_string(),
            }
        }

        McpOutboundTransform::Template { .. } => {
            // Phase 4 — return raw for now.
            response.to_string()
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::mcp_endpoint::{FieldMapping, McpInboundTransform, McpOutboundTransform, McpToolSpec};
    use ansible_mesh_core::mcp_route::McpRouteTarget;
    use serde_json::json;

    fn make_spec(mappings: Vec<FieldMapping>, outbound: McpOutboundTransform) -> McpToolSpec {
        McpToolSpec {
            name: "test".into(),
            description: "".into(),
            input_schema: json!({}),
            inbound_transform: McpInboundTransform::FieldMap {
                action: "datasource.query".into(),
                target: McpRouteTarget::Datasource {
                    datasource_id: "ds-01".into(),
                },
                mappings,
            },
            outbound_transform: outbound,
            auth: None,
        }
    }

    #[test]
    fn field_map_single_key() {
        let spec = make_spec(
            vec![FieldMapping { from: "query".into(), to: "payload.q".into() }],
            McpOutboundTransform::PassThrough,
        );
        let args = json!({ "query": "hello world" });
        let result = apply_inbound(&spec, &args).unwrap();
        assert_eq!(result.action, "datasource.query");
        assert_eq!(result.payload["payload"]["q"], "hello world");
    }

    #[test]
    fn field_map_no_mappings_passthrough_args() {
        let spec = make_spec(vec![], McpOutboundTransform::PassThrough);
        let args = json!({ "foo": 1, "bar": "baz" });
        let result = apply_inbound(&spec, &args).unwrap();
        assert_eq!(result.payload["foo"], 1);
        assert_eq!(result.payload["bar"], "baz");
    }

    #[test]
    fn field_map_missing_source_skipped() {
        let spec = make_spec(
            vec![
                FieldMapping { from: "present".into(), to: "out.present".into() },
                FieldMapping { from: "missing".into(), to: "out.missing".into() },
            ],
            McpOutboundTransform::PassThrough,
        );
        let args = json!({ "present": 42 });
        let result = apply_inbound(&spec, &args).unwrap();
        assert_eq!(result.payload["out"]["present"], 42);
        assert!(result.payload["out"].get("missing").is_none());
    }

    #[test]
    fn outbound_extract_string_field() {
        let spec = make_spec(vec![], McpOutboundTransform::Extract { path: "results.0.text".into() });
        let response = json!({ "results": [{ "text": "found it" }] }).to_string();
        assert_eq!(apply_outbound(&spec, &response), "found it");
    }

    #[test]
    fn outbound_extract_missing_returns_raw() {
        let spec = make_spec(vec![], McpOutboundTransform::Extract { path: "does.not.exist".into() });
        let response = json!({ "ok": true }).to_string();
        assert_eq!(apply_outbound(&spec, &response), response);
    }

    #[test]
    fn outbound_passthrough() {
        let spec = make_spec(vec![], McpOutboundTransform::PassThrough);
        let response = r#"{"anything":true}"#;
        assert_eq!(apply_outbound(&spec, response), response);
    }
}
