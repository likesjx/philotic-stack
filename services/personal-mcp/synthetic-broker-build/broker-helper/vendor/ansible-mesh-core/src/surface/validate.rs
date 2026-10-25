//! Component and surface-tree validation against `philotic.desktop.v1`.
//!
//! Fail loud, never partially: anything outside the catalog, any catalog function
//! call, any dangling reference or cycle, and anything over a size ceiling is an
//! error, and the surface is not stored or rendered.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use super::catalog::{catalog, Limits, PropKind};
use super::SurfaceError;

/// Validate one component object in isolation (shape, props, value kinds).
pub fn validate_component(component: &Value) -> Result<(), SurfaceError> {
    let catalog = catalog();
    let obj = component
        .as_object()
        .ok_or_else(|| SurfaceError::invalid("component must be a JSON object"))?;
    let id = component_id_of(obj)?;
    let name = obj
        .get("component")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            SurfaceError::invalid(format!("component '{id}' has no 'component' name"))
        })?;
    let spec = catalog.components.get(name).ok_or_else(|| {
        if catalog.excluded.iter().any(|e| e == name) {
            SurfaceError::component_not_allowed(format!(
                "component '{id}': {name} is excluded from {}",
                catalog.catalog_id
            ))
        } else {
            SurfaceError::component_not_allowed(format!(
                "component '{id}': {name} is not in {}",
                catalog.catalog_id
            ))
        }
    })?;
    for required in &spec.required {
        if !obj.contains_key(required) {
            return Err(SurfaceError::invalid(format!(
                "component '{id}' ({name}) is missing required '{required}'"
            )));
        }
    }
    let ctx = Ctx {
        id,
        limits: &catalog.limits,
    };
    for (key, value) in obj {
        if key == "id" || key == "component" {
            continue;
        }
        let kind = spec
            .props
            .get(key)
            .or_else(|| catalog.common_props.get(key))
            .ok_or_else(|| {
                SurfaceError::invalid(format!(
                    "component '{id}' ({name}) has unknown property '{key}'"
                ))
            })?;
        ctx.check(key, kind, value)?;
    }
    Ok(())
}

fn component_id_of(obj: &Map<String, Value>) -> Result<&str, SurfaceError> {
    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| SurfaceError::invalid("component has no string 'id'"))?;
    check_id(id)?;
    Ok(id)
}

/// Component, surface and action ids: 1..=64 chars of `[A-Za-z0-9_.:-]`.
pub fn check_id(id: &str) -> Result<(), SurfaceError> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'));
    if ok {
        Ok(())
    } else {
        Err(SurfaceError::invalid(format!(
            "invalid id '{}': use 1-64 chars of [A-Za-z0-9_.:-]",
            id.chars().take(80).collect::<String>()
        )))
    }
}

struct Ctx<'a> {
    id: &'a str,
    limits: &'a Limits,
}

impl Ctx<'_> {
    fn err(&self, key: &str, msg: impl std::fmt::Display) -> SurfaceError {
        SurfaceError::invalid(format!("component '{}' property '{key}': {msg}", self.id))
    }

    fn string(&self, key: &str, value: &str) -> Result<(), SurfaceError> {
        if value.len() > self.limits.max_string_bytes {
            return Err(self.err(
                key,
                format!("string exceeds {} bytes", self.limits.max_string_bytes),
            ));
        }
        Ok(())
    }

    /// A `{"path": ...}` binding. A function call is refused outright: v1 allows
    /// no catalog functions (that is where `openUrl` lives).
    fn binding(&self, key: &str, value: &Value) -> Result<bool, SurfaceError> {
        let Some(obj) = value.as_object() else {
            return Ok(false);
        };
        if obj.contains_key("call") {
            return Err(SurfaceError::function_not_allowed(format!(
                "component '{}' property '{key}': catalog functions are not allowed in v1",
                self.id
            )));
        }
        match (obj.len(), obj.get("path").and_then(Value::as_str)) {
            (1, Some(path)) => {
                check_binding_path(path).map_err(|e| self.err(key, e))?;
                Ok(true)
            }
            _ => Err(self.err(key, "object values must be exactly {\"path\": \"...\"}")),
        }
    }

    fn dynamic(
        &self,
        key: &str,
        value: &Value,
        literal_ok: impl Fn(&Value) -> bool,
        what: &str,
    ) -> Result<(), SurfaceError> {
        if self.binding(key, value)? {
            return Ok(());
        }
        if !literal_ok(value) {
            return Err(self.err(key, format!("expected {what} or a {{\"path\"}} binding")));
        }
        if let Some(s) = value.as_str() {
            self.string(key, s)?;
        }
        Ok(())
    }

    fn check(&self, key: &str, kind: &PropKind, value: &Value) -> Result<(), SurfaceError> {
        match kind {
            PropKind::DynamicString => self.dynamic(key, value, Value::is_string, "a string"),
            PropKind::DynamicNumber => self.dynamic(key, value, Value::is_number, "a number"),
            PropKind::DynamicBoolean => self.dynamic(key, value, Value::is_boolean, "a boolean"),
            PropKind::DynamicStringList => {
                if self.binding(key, value)? {
                    return Ok(());
                }
                let items = value.as_array().ok_or_else(|| {
                    self.err(key, "expected an array of strings or a {\"path\"} binding")
                })?;
                for item in items {
                    let s = item
                        .as_str()
                        .ok_or_else(|| self.err(key, "list items must be strings"))?;
                    self.string(key, s)?;
                }
                Ok(())
            }
            PropKind::ComponentId => {
                let s = value
                    .as_str()
                    .ok_or_else(|| self.err(key, "expected a component id"))?;
                check_id(s)
            }
            PropKind::ChildList => match value {
                Value::Array(items) => items.iter().try_for_each(|item| {
                    let s = item
                        .as_str()
                        .ok_or_else(|| self.err(key, "children must be component ids"))?;
                    check_id(s)
                }),
                Value::Object(obj) => {
                    let (Some(component_id), Some(path), 2) = (
                        obj.get("componentId").and_then(Value::as_str),
                        obj.get("path").and_then(Value::as_str),
                        obj.len(),
                    ) else {
                        return Err(
                            self.err(key, "template must be exactly {\"componentId\", \"path\"}")
                        );
                    };
                    check_id(component_id)?;
                    check_binding_path(path).map_err(|e| self.err(key, e))
                }
                _ => Err(self.err(key, "expected an array of ids or a template")),
            },
            PropKind::Action => self.action(key, value),
            PropKind::LiteralString => {
                let s = value
                    .as_str()
                    .ok_or_else(|| self.err(key, "expected a string literal"))?;
                if key == "validationRegexp" && s.len() > self.limits.max_regex_bytes {
                    return Err(self.err(
                        key,
                        format!("regex exceeds {} bytes", self.limits.max_regex_bytes),
                    ));
                }
                self.string(key, s)
            }
            PropKind::LiteralNumber => value
                .is_number()
                .then_some(())
                .ok_or_else(|| self.err(key, "expected a number literal")),
            PropKind::LiteralBoolean => value
                .is_boolean()
                .then_some(())
                .ok_or_else(|| self.err(key, "expected a boolean literal")),
            PropKind::Enum(allowed) => match value.as_str() {
                Some(s) if allowed.iter().any(|a| a == s) => Ok(()),
                _ => Err(self.err(key, format!("expected one of {allowed:?}"))),
            },
            PropKind::Options => {
                let items = value
                    .as_array()
                    .ok_or_else(|| self.err(key, "expected an array of options"))?;
                for item in items {
                    let obj = item
                        .as_object()
                        .ok_or_else(|| self.err(key, "option must be an object"))?;
                    exact_keys(obj, &["label", "value"], &["label", "value"])
                        .map_err(|e| self.err(key, e))?;
                    self.check(key, &PropKind::DynamicString, &obj["label"])?;
                    self.check(key, &PropKind::LiteralString, &obj["value"])?;
                }
                Ok(())
            }
            PropKind::Checks => {
                let items = value
                    .as_array()
                    .ok_or_else(|| self.err(key, "expected an array of checks"))?;
                for item in items {
                    let obj = item
                        .as_object()
                        .ok_or_else(|| self.err(key, "check must be an object"))?;
                    exact_keys(obj, &["condition", "message"], &["condition", "message"])
                        .map_err(|e| self.err(key, e))?;
                    self.check(key, &PropKind::DynamicBoolean, &obj["condition"])?;
                    self.check(key, &PropKind::LiteralString, &obj["message"])?;
                }
                Ok(())
            }
            PropKind::TableColumns => {
                let items = value
                    .as_array()
                    .ok_or_else(|| self.err(key, "expected an array of columns"))?;
                if items.is_empty() {
                    return Err(self.err(key, "a table needs at least one column"));
                }
                for item in items {
                    let obj = item
                        .as_object()
                        .ok_or_else(|| self.err(key, "column must be an object"))?;
                    exact_keys(obj, &["header", "field"], &["header", "field"])
                        .map_err(|e| self.err(key, e))?;
                    self.check(key, &PropKind::DynamicString, &obj["header"])?;
                    self.check(key, &PropKind::LiteralString, &obj["field"])?;
                }
                Ok(())
            }
            PropKind::TableRows => {
                if self.binding(key, value)? {
                    return Ok(());
                }
                let rows = value.as_array().ok_or_else(|| {
                    self.err(
                        key,
                        "expected an array of row objects or a {\"path\"} binding",
                    )
                })?;
                for row in rows {
                    row.as_object()
                        .ok_or_else(|| self.err(key, "rows must be objects"))?;
                    self.literal_strings(key, row)?;
                }
                Ok(())
            }
            PropKind::Accessibility => {
                let obj = value
                    .as_object()
                    .ok_or_else(|| self.err(key, "expected an object"))?;
                exact_keys(obj, &[], &["label", "description"]).map_err(|e| self.err(key, e))?;
                obj.values()
                    .try_for_each(|v| self.check(key, &PropKind::DynamicString, v))
            }
        }
    }

    /// Only server events: `{"event": {"name", "context"}}`. Local function
    /// actions are refused with every other catalog function.
    fn action(&self, key: &str, value: &Value) -> Result<(), SurfaceError> {
        let obj = value
            .as_object()
            .ok_or_else(|| self.err(key, "expected an action object"))?;
        if obj.contains_key("functionCall") {
            return Err(SurfaceError::function_not_allowed(format!(
                "component '{}': functionCall actions are not allowed in v1",
                self.id
            )));
        }
        exact_keys(obj, &["event"], &["event"]).map_err(|e| self.err(key, e))?;
        let event = obj["event"]
            .as_object()
            .ok_or_else(|| self.err(key, "event must be an object"))?;
        exact_keys(event, &["name"], &["name", "context"]).map_err(|e| self.err(key, e))?;
        let name = event["name"]
            .as_str()
            .ok_or_else(|| self.err(key, "event.name must be a string"))?;
        check_id(name)
            .map_err(|_| self.err(key, "event.name must be 1-64 chars of [A-Za-z0-9_.:-]"))?;
        if let Some(context) = event.get("context") {
            let context = context
                .as_object()
                .ok_or_else(|| self.err(key, "event.context must be an object"))?;
            for value in context.values() {
                if !self.binding(key, value)? {
                    self.literal_strings(key, value)?;
                }
            }
        }
        Ok(())
    }

    /// Walk a literal value bounding every string.
    fn literal_strings(&self, key: &str, value: &Value) -> Result<(), SurfaceError> {
        match value {
            Value::String(s) => self.string(key, s),
            Value::Array(items) => items.iter().try_for_each(|v| self.literal_strings(key, v)),
            Value::Object(map) => {
                if map.contains_key("call") {
                    return Err(SurfaceError::function_not_allowed(format!(
                        "component '{}' property '{key}': catalog functions are not allowed in v1",
                        self.id
                    )));
                }
                map.values().try_for_each(|v| self.literal_strings(key, v))
            }
            _ => Ok(()),
        }
    }
}

/// Binding paths are absolute JSON Pointers (`/a/b`) or, inside a list template,
/// relative ones (`name`). Either way the escapes must be well formed.
fn check_binding_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > 512 {
        return Err("binding path must be 1-512 chars".into());
    }
    let absolute = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    super::pointer::tokens(&absolute).map(|_| ())
}

fn exact_keys(obj: &Map<String, Value>, required: &[&str], allowed: &[&str]) -> Result<(), String> {
    for key in required {
        if !obj.contains_key(*key) {
            return Err(format!("missing '{key}'"));
        }
    }
    for key in obj.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("unknown key '{key}'"));
        }
    }
    Ok(())
}

/// Ids a component references: children (static or template), child, trigger,
/// content.
pub fn referenced_ids(component: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for key in ["child", "trigger", "content"] {
        if let Some(id) = component.get(key).and_then(Value::as_str) {
            out.push(id.to_string());
        }
    }
    match component.get("children") {
        Some(Value::Array(items)) => {
            out.extend(items.iter().filter_map(Value::as_str).map(str::to_string))
        }
        Some(Value::Object(obj)) => {
            if let Some(id) = obj.get("componentId").and_then(Value::as_str) {
                out.push(id.to_string());
            }
        }
        _ => {}
    }
    out
}

/// Whole-surface structure: `root` exists, every reference resolves, no cycles,
/// depth and count within limits.
pub fn validate_tree(components: &BTreeMap<String, Value>) -> Result<(), SurfaceError> {
    let limits = &catalog().limits;
    if components.len() > limits.max_components {
        return Err(SurfaceError::too_large(format!(
            "surface has {} components; the limit is {}",
            components.len(),
            limits.max_components
        )));
    }
    if !components.contains_key("root") {
        return Err(SurfaceError::invalid(
            "surface has no component with id 'root'",
        ));
    }
    for (id, component) in components {
        for target in referenced_ids(component) {
            if !components.contains_key(&target) {
                return Err(SurfaceError::invalid(format!(
                    "component '{id}' references missing component '{target}'"
                )));
            }
        }
    }
    // Depth-first from root; a back edge to the current path is a cycle.
    fn walk(
        id: &str,
        depth: usize,
        components: &BTreeMap<String, Value>,
        path: &mut BTreeSet<String>,
        max_depth: usize,
    ) -> Result<(), SurfaceError> {
        if depth > max_depth {
            return Err(SurfaceError::too_large(format!(
                "surface nesting exceeds depth {max_depth} at '{id}'"
            )));
        }
        if !path.insert(id.to_string()) {
            return Err(SurfaceError::invalid(format!(
                "component cycle through '{id}'"
            )));
        }
        for child in referenced_ids(&components[id]) {
            walk(&child, depth + 1, components, path, max_depth)?;
        }
        path.remove(id);
        Ok(())
    }
    walk(
        "root",
        1,
        components,
        &mut BTreeSet::new(),
        limits.max_depth,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tree(items: Vec<Value>) -> BTreeMap<String, Value> {
        items
            .into_iter()
            .map(|c| (c["id"].as_str().unwrap().to_string(), c))
            .collect()
    }

    #[test]
    fn every_allowed_component_has_a_passing_fixture() {
        let fixtures = vec![
            json!({"id": "t", "component": "Text", "text": {"path": "/title"}, "variant": "h2"}),
            json!({"id": "r", "component": "Row", "children": ["a", "b"], "justify": "spaceBetween"}),
            json!({"id": "c", "component": "Column", "children": ["a"], "align": "stretch"}),
            json!({"id": "l", "component": "List", "children": {"componentId": "row", "path": "/guests"}}),
            json!({"id": "k", "component": "Card", "child": "t"}),
            json!({"id": "m", "component": "Modal", "trigger": "b", "content": "k"}),
            json!({"id": "d", "component": "Divider", "axis": "horizontal"}),
            json!({"id": "b", "component": "Button", "child": "t", "variant": "primary",
                   "action": {"event": {"name": "restart_guest", "context": {"guest": {"path": "id"}, "n": 1}}}}),
            json!({"id": "f", "component": "TextField", "label": "Note", "value": {"path": "/note"},
                   "variant": "longText", "validationRegexp": "^.{0,200}$",
                   "checks": [{"condition": {"path": "/ok"}, "message": "required"}]}),
            json!({"id": "x", "component": "CheckBox", "label": "Agree", "value": false}),
            json!({"id": "p", "component": "ChoicePicker", "options": [{"label": "A", "value": "a"}],
                   "value": ["a"], "variant": "mutuallyExclusive", "filterable": true}),
            json!({"id": "s", "component": "Slider", "value": {"path": "/v"}, "min": 0, "max": 10}),
            json!({"id": "tb", "component": "Table", "columns": [{"header": "Guest", "field": "name"}],
                   "rows": {"path": "/guests"}}),
            json!({"id": "acc", "component": "Text", "text": "hi", "weight": 1,
                   "accessibility": {"label": "greeting"}}),
        ];
        let names: BTreeSet<&str> = fixtures
            .iter()
            .filter_map(|f| f["component"].as_str())
            .collect();
        for name in catalog().components.keys() {
            assert!(names.contains(name.as_str()), "no fixture for {name}");
        }
        for f in &fixtures {
            validate_component(f).unwrap_or_else(|e| panic!("{f}: {e}"));
        }
    }

    #[test]
    fn every_excluded_component_is_refused() {
        for name in &catalog().excluded {
            let err =
                validate_component(&json!({"id": "z", "component": name, "url": "https://x"}))
                    .unwrap_err();
            assert_eq!(err.code(), "SURFACE_COMPONENT_NOT_ALLOWED", "{name}");
        }
        let err = validate_component(&json!({"id": "z", "component": "Iframe"})).unwrap_err();
        assert_eq!(err.code(), "SURFACE_COMPONENT_NOT_ALLOWED");
    }

    #[test]
    fn function_calls_are_refused_everywhere() {
        let cases = [
            json!({"id": "t", "component": "Text",
                   "text": {"call": "formatString", "args": {}, "returnType": "string"}}),
            json!({"id": "b", "component": "Button", "child": "t",
                   "action": {"functionCall": {"call": "openUrl", "args": {"url": "https://x"}}}}),
            json!({"id": "f", "component": "TextField", "label": "x",
                   "checks": [{"condition": {"call": "required", "args": {}}, "message": "m"}]}),
            json!({"id": "b", "component": "Button", "child": "t",
                   "action": {"event": {"name": "go", "context": {"u": {"call": "openUrl"}}}}}),
        ];
        for case in cases {
            let err = validate_component(&case).unwrap_err();
            assert_eq!(err.code(), "SURFACE_FUNCTION_NOT_ALLOWED", "{case}");
        }
    }

    #[test]
    fn shape_errors_fail_loud() {
        let bad = [
            json!({"component": "Text", "text": "no id"}),
            json!({"id": "t", "component": "Text"}),
            json!({"id": "t", "component": "Text", "text": "x", "onclick": "alert(1)"}),
            json!({"id": "t", "component": "Text", "text": 5}),
            json!({"id": "t", "component": "Text", "text": "x", "variant": "h9"}),
            json!({"id": "t", "component": "Text", "text": {"path": "/a", "extra": 1}}),
            json!({"id": "bad id", "component": "Divider"}),
            json!({"id": "b", "component": "Button", "child": "t", "action": {"event": {}}}),
            json!({"id": "t", "component": "Text", "text": "x".repeat(5000)}),
            json!({"id": "f", "component": "TextField", "label": "x", "validationRegexp": "a".repeat(300)}),
            json!({"id": "tb", "component": "Table", "columns": [], "rows": []}),
        ];
        for case in bad {
            assert!(validate_component(&case).is_err(), "accepted: {case}");
        }
    }

    #[test]
    fn tree_requires_root_and_resolved_references() {
        let ok = tree(vec![
            json!({"id": "root", "component": "Column", "children": ["title", "list"]}),
            json!({"id": "title", "component": "Text", "text": "Guests"}),
            json!({"id": "list", "component": "List", "children": {"componentId": "row", "path": "/guests"}}),
            json!({"id": "row", "component": "Text", "text": {"path": "name"}}),
        ]);
        validate_tree(&ok).unwrap();

        let no_root = tree(vec![json!({"id": "a", "component": "Divider"})]);
        assert!(validate_tree(&no_root).is_err());

        let dangling = tree(vec![
            json!({"id": "root", "component": "Card", "child": "ghost"}),
        ]);
        assert!(validate_tree(&dangling).is_err());
    }

    #[test]
    fn cycles_and_depth_are_refused() {
        let cycle = tree(vec![
            json!({"id": "root", "component": "Card", "child": "a"}),
            json!({"id": "a", "component": "Card", "child": "root"}),
        ]);
        assert!(validate_tree(&cycle)
            .unwrap_err()
            .to_string()
            .contains("cycle"));

        let mut deep = vec![json!({"id": "root", "component": "Card", "child": "c1"})];
        for i in 1..30 {
            deep.push(
                json!({"id": format!("c{i}"), "component": "Card", "child": format!("c{}", i + 1)}),
            );
        }
        deep.push(json!({"id": "c30", "component": "Divider"}));
        assert_eq!(
            validate_tree(&tree(deep)).unwrap_err().code(),
            "SURFACE_TOO_LARGE"
        );
    }

    #[test]
    fn shared_children_are_not_cycles() {
        let diamond = tree(vec![
            json!({"id": "root", "component": "Row", "children": ["a", "b"]}),
            json!({"id": "a", "component": "Card", "child": "leaf"}),
            json!({"id": "b", "component": "Card", "child": "leaf"}),
            json!({"id": "leaf", "component": "Divider"}),
        ]);
        validate_tree(&diamond).unwrap();
    }
}
