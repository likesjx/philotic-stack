//! Schema-aware repair of model-authored tool arguments before dispatch.
//!
//! Live 2026-09-15 22:55 EDT and again 2026-09-16 09:49 EDT (bjork, mac-jane,
//! Gemini): every `life.observe` that carried the R1 `evidence.properties`
//! map arrived with that map serialized as a JSON *string* —
//! `"properties": "{\"title\":\"…\",\"status\":\"confirmed\"}"` — so the runner
//! answered `invalid type: string …, expected a map`, the automatic retry sent
//! the identical string, and nothing was written (DEF-146). The tool schema
//! declares the field as an object; the model simply stringified the nested
//! value, which some providers do for any nested object or array.
//!
//! This pass walks the tool's declared `input_schema` beside the arguments and,
//! wherever the schema says object/array but the value is a string that parses
//! to exactly that shape, replaces the string with the parsed value. Nothing
//! else is touched: a string field stays a string even if it looks like JSON,
//! unknown keys are left alone, and a string that does not parse is passed
//! through for the runner to reject with its own message.

use serde_json::Value;

/// Repair stringified objects/arrays in `arguments` according to `schema`
/// (a JSON Schema fragment). Returns how many values were replaced.
pub(crate) fn coerce_stringified_json(arguments: &mut Value, schema: &Value) -> usize {
    let mut replaced = 0;
    coerce_value(arguments, schema, &mut replaced);
    replaced
}

fn schema_declares(schema: &Value, kind: &str) -> bool {
    match schema.get("type") {
        Some(Value::String(t)) => t == kind,
        Some(Value::Array(ts)) => ts.iter().any(|t| t.as_str() == Some(kind)),
        _ => match kind {
            "object" => schema.get("properties").is_some(),
            "array" => schema.get("items").is_some(),
            _ => false,
        },
    }
}

fn coerce_value(value: &mut Value, schema: &Value, replaced: &mut usize) {
    // A stringified value where the schema wants a container.
    if let Value::String(text) = value {
        let trimmed = text.trim();
        let wants_object = schema_declares(schema, "object");
        let wants_array = schema_declares(schema, "array");
        let candidate =
            (wants_object && trimmed.starts_with('{')) || (wants_array && trimmed.starts_with('['));
        let parsed = if candidate {
            serde_json::from_str::<Value>(trimmed).ok()
        } else {
            None
        };
        if let Some(parsed) = parsed {
            let shape_ok =
                (wants_object && parsed.is_object()) || (wants_array && parsed.is_array());
            if shape_ok {
                *value = parsed;
                *replaced += 1;
            }
        }
    }
    match value {
        Value::Object(map) => {
            let Some(props) = schema.get("properties").and_then(Value::as_object) else {
                return;
            };
            for (key, child) in map.iter_mut() {
                if let Some(child_schema) = props.get(key) {
                    coerce_value(child, child_schema, replaced);
                }
            }
        }
        Value::Array(items) => {
            if let Some(item_schema) = schema.get("items") {
                for item in items.iter_mut() {
                    coerce_value(item, item_schema, replaced);
                }
            }
        }
        _ => {}
    }
}

/// Repair a model tool call against the philote's own catalog entry for that
/// tool. Tools without a catalog schema (MCP upstream, HTTP integrations)
/// pass through untouched — their schemas are not known here.
pub(crate) fn coerce_tool_call_arguments(tool_call: &mut crate::r#loop::ToolCall) -> usize {
    let Some(def) = crate::catalog::tool_catalog().get(tool_call.tool_name.as_str()) else {
        return 0;
    };
    let replaced = coerce_stringified_json(&mut tool_call.arguments, &def.input_schema);
    if replaced > 0 {
        tracing::info!(
            tool = %tool_call.tool_name,
            replaced,
            "repaired stringified JSON object/array arguments against the tool schema"
        );
    }
    replaced
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn observe_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "evidence": {
                    "type": "object",
                    "properties": {
                        "claim_summary": { "type": "string" },
                        "properties": { "type": "object", "additionalProperties": true },
                        "source_refs": {
                            "type": "array",
                            "items": { "type": "object", "properties": { "source_id": { "type": "string" } } }
                        }
                    }
                },
                "edges": { "type": "array", "items": { "type": "object" } }
            }
        })
    }

    #[test]
    fn stringified_nested_properties_map_becomes_an_object() {
        // The live 2026-09-16 09:50 EDT shape, verbatim.
        let mut args = json!({
            "evidence": {
                "claim_summary": "Jared plans to arrive by 8:00 AM",
                "properties": "{\"title\":\"Sunday Organ Warmup - September 20, 2026\",\"status\":\"proposed\"}",
                "source_refs": [{"source_id": "membrane:telegram"}]
            },
            "edges": "[{\"rel_type\":\"SCOPED_TO\",\"target_id\":\"life:role:musician\"}]"
        });
        let replaced = coerce_stringified_json(&mut args, &observe_schema());
        assert_eq!(replaced, 2);
        assert_eq!(
            args["evidence"]["properties"]["status"],
            json!("proposed"),
            "properties must be a map after repair: {args}"
        );
        assert_eq!(args["edges"][0]["rel_type"], json!("SCOPED_TO"));
        // A genuine string field stays a string even though it is prose.
        assert!(args["evidence"]["claim_summary"].is_string());
    }

    #[test]
    fn string_fields_that_look_like_json_are_left_alone() {
        let schema = json!({"type":"object","properties":{"note":{"type":"string"}}});
        let mut args = json!({"note": "{\"not\":\"a map field\"}"});
        assert_eq!(coerce_stringified_json(&mut args, &schema), 0);
        assert!(args["note"].is_string());
    }

    #[test]
    fn unparseable_or_wrong_shape_strings_pass_through() {
        let schema = json!({"type":"object","properties":{"properties":{"type":"object"}}});
        let mut args = json!({"properties": "{not json"});
        assert_eq!(coerce_stringified_json(&mut args, &schema), 0);
        assert!(args["properties"].is_string());
        let mut args = json!({"properties": "[1,2]"});
        assert_eq!(coerce_stringified_json(&mut args, &schema), 0);
        assert!(args["properties"].is_string());
    }

    #[test]
    fn live_observe_call_is_repaired_through_the_catalog() {
        let mut call = crate::r#loop::ToolCall {
            tool_name: "life.observe".into(),
            arguments: json!({
                "evidence": {
                    "claim_ref": {"id": "life:event:organ_warmup_20260920", "label": "Event"},
                    "claim_summary": "warm up on the organ",
                    "properties": "{\"title\":\"Sunday Organ Warmup\",\"status\":\"proposed\"}"
                }
            }),
        };
        assert_eq!(coerce_tool_call_arguments(&mut call), 1);
        assert!(call.arguments["evidence"]["properties"].is_object());
    }
}
