//! Deterministic-first handling of inbound MCP tool calls.
//!
//! When `membrane-mcp` dispatches a `tools/call` at a philote it forwards the
//! tool's [`McpHandlerPolicy`] (declared at `mcp.provision` time), the
//! advertised `input_schema`, and the caller's raw arguments in
//! `raw_transport_event`. This module is the pure, unit-testable half of the
//! ladder: parse the call, validate the arguments against the schema, render
//! reflex argument templates, and build the prompt used when the ladder falls
//! through to a model turn. The side-effecting half (running reflexes and
//! emitting replies) lives in `runtime/mcp_handling.rs`.
//!
//! Two payload shapes reach a philote and both are accepted here:
//!
//! - config-driven endpoints: `content = {"action", "payload", "target_kind",
//!   "target_id"}` and `raw_transport_event = {"transport":"mcp", "tool",
//!   "handler", "input_schema", "args", ...}`
//! - legacy route-table endpoints: `content = {"tool", "args"}` and
//!   `raw_transport_event = {"transport":"mcp", "tool", ...}`

use crate::protocol::InboundTaskPayload;
use ansible_mesh_core::mcp_endpoint::McpHandlerPolicy;
use serde_json::{Map, Value, json};

/// A parsed inbound MCP tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct McpCall {
    /// Tool name as advertised in `tools/list`.
    pub tool: String,
    /// Envelope action (`inbound_transform.action`) when known.
    pub action: Option<String>,
    /// Raw caller arguments, exactly as received by the membrane. Validated
    /// against `input_schema`.
    pub args: Value,
    /// Transformed payload (after the tool's field mappings). Reflex argument
    /// templates are rendered against this.
    pub payload: Value,
    /// JSON Schema advertised for the tool (`{}` when unknown).
    pub input_schema: Value,
    /// Handler policy declared at provisioning time, if any.
    pub policy: Option<McpHandlerPolicy>,
}

fn raw(task: &InboundTaskPayload) -> Option<&Value> {
    task.raw_transport_event.as_ref()
}

/// True when the task arrived through an MCP membrane.
pub fn is_mcp_call(task: &InboundTaskPayload) -> bool {
    raw(task)
        .and_then(|r| r.get("transport"))
        .and_then(Value::as_str)
        .map(|t| t == "mcp")
        .unwrap_or(false)
        || task.transport.as_deref() == Some("mcp")
}

fn content_json(task: &InboundTaskPayload) -> Option<Value> {
    task.content
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
}

/// The caller's arguments for an MCP call, whichever payload shape carried
/// them. Config-driven endpoints put the transformed payload under
/// `content.payload`; legacy route-table endpoints put the raw args under
/// `content.args`. Returns an empty object when neither is present.
pub fn extract_args(task: &InboundTaskPayload) -> Value {
    if let Some(args) = raw(task).and_then(|r| r.get("args")) {
        if args.is_object() {
            return args.clone();
        }
    }
    let content = content_json(task).unwrap_or(Value::Null);
    if let Some(payload) = content.get("payload") {
        if payload.is_object() {
            return payload.clone();
        }
    }
    if let Some(args) = content.get("args") {
        if args.is_object() {
            return args.clone();
        }
    }
    Value::Object(Map::new())
}

/// Parse an MCP call out of an inbound task. `None` when the task did not
/// come through an MCP membrane.
pub fn parse_call(task: &InboundTaskPayload) -> Option<McpCall> {
    if !is_mcp_call(task) {
        return None;
    }
    let raw_event = raw(task).cloned().unwrap_or(Value::Null);
    let content = content_json(task).unwrap_or(Value::Null);

    let tool = raw_event
        .get("tool")
        .and_then(Value::as_str)
        .or_else(|| content.get("tool").and_then(Value::as_str))
        .or(task.command.as_deref())
        .unwrap_or("unknown")
        .to_string();

    let action = content
        .get("action")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| task.command.clone());

    let payload = content
        .get("payload")
        .filter(|v| v.is_object())
        .cloned()
        .or_else(|| content.get("args").filter(|v| v.is_object()).cloned())
        .unwrap_or_else(|| Value::Object(Map::new()));

    let args = raw_event
        .get("args")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| payload.clone());

    let input_schema = raw_event
        .get("input_schema")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let policy = raw_event
        .get("handler")
        .filter(|v| !v.is_null())
        .and_then(|v| serde_json::from_value::<McpHandlerPolicy>(v.clone()).ok());

    Some(McpCall {
        tool,
        action,
        args,
        payload,
        input_schema,
        policy,
    })
}

// ── Input validation ──────────────────────────────────────────────────────────

fn json_type_matches(expected: &str, value: &Value) -> bool {
    match expected {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    }
}

/// Validate `args` against the subset of JSON Schema the membrane advertises:
/// top-level `required` keys and the primitive `type` of each declared
/// property (a `type` given as an array accepts any listed type). Unknown
/// keys are allowed. Returns a caller-facing message on the first violation.
pub fn validate_args(schema: &Value, args: &Value) -> Result<(), String> {
    if !schema.is_object() {
        return Ok(());
    }
    let Some(obj) = args.as_object() else {
        return Err("arguments must be a JSON object".into());
    };

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for key in required.iter().filter_map(Value::as_str) {
            match obj.get(key) {
                None | Some(Value::Null) => {
                    return Err(format!("missing required argument '{key}'"));
                }
                _ => {}
            }
        }
    }

    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        for (key, prop) in props {
            let Some(value) = obj.get(key) else { continue };
            if value.is_null() {
                continue;
            }
            let ok = match prop.get("type") {
                Some(Value::String(t)) => json_type_matches(t, value),
                Some(Value::Array(types)) => types
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|t| json_type_matches(t, value)),
                _ => true,
            };
            if !ok {
                let expected = prop
                    .get("type")
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "\"any\"".into());
                return Err(format!(
                    "argument '{key}' has the wrong type (expected {expected})"
                ));
            }
            if let Some(allowed) = prop.get("enum").and_then(Value::as_array) {
                if !allowed.contains(value) {
                    return Err(format!("argument '{key}' is not one of the allowed values"));
                }
            }
        }
    }

    Ok(())
}

// ── Reflex argument templates ─────────────────────────────────────────────────

fn dot_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(value);
    }
    path.split('.').try_fold(value, |acc, key| match acc {
        Value::Array(arr) => key.parse::<usize>().ok().and_then(|i| arr.get(i)),
        _ => acc.get(key),
    })
}

/// Render a reflex `args` template against the call payload.
///
/// - a string exactly equal to `"${payload}"` becomes the whole payload
/// - a string exactly equal to `"${payload.a.b}"` becomes that value (any
///   JSON type), or `null` when absent
/// - other strings get `${payload.a.b}` occurrences interpolated as text
/// - objects and arrays are rendered recursively; everything else is copied
pub fn render_template(template: &Value, payload: &Value) -> Value {
    match template {
        Value::String(s) => render_string(s, payload),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| render_template(item, payload))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), render_template(v, payload)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn render_string(s: &str, payload: &Value) -> Value {
    // Whole-value substitution keeps the JSON type of the referenced field.
    if let Some(inner) = s.strip_prefix("${").and_then(|r| r.strip_suffix('}')) {
        if !inner.contains("${") {
            if inner == "payload" {
                return payload.clone();
            }
            if let Some(path) = inner.strip_prefix("payload.") {
                return dot_path(payload, path).cloned().unwrap_or(Value::Null);
            }
        }
    }

    // Interpolation: substitute every ${payload.x} occurrence as text.
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let expr = &after[..end];
        let replacement = if expr == "payload" {
            Some(payload.clone())
        } else {
            expr.strip_prefix("payload.")
                .and_then(|path| dot_path(payload, path).cloned())
        };
        match replacement {
            Some(Value::String(text)) => out.push_str(&text),
            Some(Value::Null) | None => {}
            Some(other) => out.push_str(&other.to_string()),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Value::String(out)
}

// ── Model fallback prompt ─────────────────────────────────────────────────────

/// The turn content handed to the cognitive loop when the deterministic
/// ladder falls through. Makes the model aware it is answering a machine
/// caller, what the tool contract is, and (when the provisioner supplied
/// them) how to shape the answer.
pub fn model_prompt(call: &McpCall, instructions: Option<&str>) -> String {
    let mut prompt = String::new();
    prompt.push_str(&format!(
        "[MCP tool call] An external MCP client invoked the tool `{}` on this agent's endpoint.\n",
        call.tool
    ));
    prompt.push_str(
        "Your entire reply is returned verbatim to that client as the tool result — \
         answer the call directly, do not greet, narrate, or ask questions.\n",
    );
    if let Some(desc) = call.input_schema.get("description").and_then(Value::as_str) {
        prompt.push_str(&format!("Tool description: {desc}\n"));
    }
    prompt.push_str(&format!(
        "Arguments (JSON): {}\n",
        serde_json::to_string(&call.args).unwrap_or_else(|_| "{}".into())
    ));
    if let Some(text) = instructions.map(str::trim).filter(|t| !t.is_empty()) {
        prompt.push_str(&format!(
            "Handling instructions from the endpoint owner: {text}\n"
        ));
    }
    prompt
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::mcp_endpoint::{McpHandlerFallback, McpHandlerStep};

    fn task(content: Value, raw_event: Value) -> InboundTaskPayload {
        InboundTaskPayload {
            content: Some(content.to_string()),
            raw_transport_event: Some(raw_event),
            command: content
                .get("action")
                .and_then(Value::as_str)
                .map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn non_mcp_task_is_not_a_call() {
        let t = InboundTaskPayload {
            content: Some("hello".into()),
            raw_transport_event: Some(json!({"update_id": 1})),
            ..Default::default()
        };
        assert!(!is_mcp_call(&t));
        assert!(parse_call(&t).is_none());
    }

    #[test]
    fn parses_config_driven_shape_with_policy() {
        let t = task(
            json!({"action": "notes.search", "payload": {"q": "tea"},
                   "target_kind": "philote", "target_id": "agent-x"}),
            json!({
                "transport": "mcp", "tool": "search_notes",
                "target_kind": "philote", "target_id": "agent-x",
                "input_schema": {"type": "object", "required": ["query"],
                                 "properties": {"query": {"type": "string"}}},
                "args": {"query": "tea"},
                "handler": {"steps": [{"kind": "static", "result": {"ok": true}}],
                            "fallback": {"kind": "error", "message": "no"}}
            }),
        );
        let call = parse_call(&t).expect("mcp call");
        assert_eq!(call.tool, "search_notes");
        assert_eq!(call.action.as_deref(), Some("notes.search"));
        assert_eq!(call.args, json!({"query": "tea"}));
        assert_eq!(call.payload, json!({"q": "tea"}));
        let policy = call.policy.expect("policy");
        assert_eq!(policy.steps.len(), 1);
        assert!(matches!(policy.steps[0], McpHandlerStep::Static { .. }));
        assert!(matches!(policy.fallback, McpHandlerFallback::Error { .. }));
    }

    #[test]
    fn parses_legacy_shape_without_policy() {
        let t = task(
            json!({"tool": "context.capture", "args": {"content": "x"}}),
            json!({"transport": "mcp", "tool": "context.capture"}),
        );
        let call = parse_call(&t).expect("mcp call");
        assert_eq!(call.tool, "context.capture");
        assert_eq!(call.args, json!({"content": "x"}));
        assert_eq!(call.payload, json!({"content": "x"}));
        assert!(call.policy.is_none());
        assert_eq!(call.input_schema, json!({}));
    }

    #[test]
    fn extract_args_accepts_both_shapes() {
        let legacy = task(
            json!({"tool": "context.capture", "args": {"content": "legacy"}}),
            json!({"transport": "mcp", "tool": "context.capture"}),
        );
        assert_eq!(extract_args(&legacy)["content"], "legacy");

        let config = task(
            json!({"action": "context.capture", "payload": {"content": "config"}}),
            json!({"transport": "mcp", "tool": "context.capture"}),
        );
        assert_eq!(extract_args(&config)["content"], "config");

        let with_raw_args = task(
            json!({"action": "context.capture", "payload": {"content": "mapped"}}),
            json!({"transport": "mcp", "tool": "context.capture",
                   "args": {"content": "raw"}}),
        );
        assert_eq!(extract_args(&with_raw_args)["content"], "raw");
    }

    #[test]
    fn validate_required_and_types() {
        let schema = json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {"type": "string"},
                "limit": {"type": "integer"},
                "scope": {"type": "string", "enum": ["self", "user"]},
                "flag": {"type": ["boolean", "null"]}
            }
        });
        assert!(validate_args(&schema, &json!({"query": "x"})).is_ok());
        assert!(validate_args(&schema, &json!({"query": "x", "limit": 3, "flag": true})).is_ok());
        assert_eq!(
            validate_args(&schema, &json!({})).unwrap_err(),
            "missing required argument 'query'"
        );
        assert!(
            validate_args(&schema, &json!({"query": 5}))
                .unwrap_err()
                .contains("wrong type")
        );
        assert!(
            validate_args(&schema, &json!({"query": "x", "limit": "3"}))
                .unwrap_err()
                .contains("'limit'")
        );
        assert!(
            validate_args(&schema, &json!({"query": "x", "scope": "everyone"}))
                .unwrap_err()
                .contains("allowed values")
        );
        assert!(validate_args(&schema, &json!("not an object")).is_err());
        // No schema → nothing to enforce.
        assert!(validate_args(&Value::Null, &json!({})).is_ok());
    }

    #[test]
    fn template_substitution_keeps_types_and_interpolates() {
        let payload = json!({"query": "tea", "limit": 7, "tags": ["a", "b"], "nested": {"k": "v"}});
        let template = json!({
            "q": "${payload.query}",
            "n": "${payload.limit}",
            "all": "${payload}",
            "tags": "${payload.tags}",
            "text": "search for ${payload.query} in ${payload.nested.k} (${payload.missing})",
            "first": "${payload.tags.0}",
            "fixed": 5,
            "list": ["${payload.query}", "literal"]
        });
        let out = render_template(&template, &payload);
        assert_eq!(out["q"], "tea");
        assert_eq!(out["n"], 7);
        assert_eq!(out["all"], payload);
        assert_eq!(out["tags"], json!(["a", "b"]));
        assert_eq!(out["text"], "search for tea in v ()");
        assert_eq!(out["first"], "a");
        assert_eq!(out["fixed"], 5);
        assert_eq!(out["list"], json!(["tea", "literal"]));
        assert_eq!(
            render_template(&json!("${payload.missing}"), &payload),
            Value::Null
        );
    }

    #[test]
    fn model_prompt_names_tool_args_and_instructions() {
        let call = McpCall {
            tool: "ask_agent".into(),
            action: Some("ask".into()),
            args: json!({"question": "status?"}),
            payload: json!({"question": "status?"}),
            input_schema: json!({"description": "Ask the agent a question"}),
            policy: None,
        };
        let prompt = model_prompt(&call, Some("Reply in one sentence."));
        assert!(prompt.contains("`ask_agent`"));
        assert!(prompt.contains("Ask the agent a question"));
        assert!(prompt.contains("\"question\":\"status?\""));
        assert!(prompt.contains("Reply in one sentence."));
        let bare = model_prompt(&call, Some("   "));
        assert!(!bare.contains("Handling instructions"));
    }
}
