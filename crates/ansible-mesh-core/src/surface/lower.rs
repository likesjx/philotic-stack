//! Native lowering: draw a simple surface as chat text plus buttons, for
//! channels with no surface renderer (Telegram inline keyboards today).
//!
//! Only a "native subset" lowers: Text, Column, Row, List, Card, Divider and
//! Button. Inputs (TextField, CheckBox, ChoicePicker, Slider), Modal and Table
//! need the web renderer; asking to lower them is an honest refusal, never a
//! lossy approximation the philote could misreport as "displayed".

use serde_json::Value;

use super::{SurfaceState, ACTION_ID_KEY};

/// Telegram's `callback_data` prefix for surface actions: `sa:<action_id>`.
pub const SURFACE_ACTION_CALLBACK_PREFIX: &str = "sa:";

/// Ceilings that keep a lowered surface inside one chat message.
const MAX_TEXT_BYTES: usize = 3500;
const MAX_BUTTONS: usize = 24;
const MAX_BUTTONS_PER_ROW: usize = 4;
const MAX_LABEL_CHARS: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredButton {
    pub label: String,
    pub action_id: String,
}

impl LoweredButton {
    pub fn callback_data(&self) -> String {
        format!("{SURFACE_ACTION_CALLBACK_PREFIX}{}", self.action_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredSurface {
    pub text: String,
    /// Keyboard rows; a Row of Buttons stays one row.
    pub button_rows: Vec<Vec<LoweredButton>>,
}

impl LoweredSurface {
    /// Telegram `reply_markup`, or `None` without buttons.
    pub fn inline_keyboard(&self) -> Option<Value> {
        if self.button_rows.is_empty() {
            return None;
        }
        let rows: Vec<Value> = self
            .button_rows
            .iter()
            .map(|row| {
                Value::Array(
                    row.iter()
                        .map(|b| serde_json::json!({"text": b.label, "callback_data": b.callback_data()}))
                        .collect(),
                )
            })
            .collect();
        Some(serde_json::json!({ "inline_keyboard": rows }))
    }
}

/// Why a surface cannot be drawn natively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotLowerable(pub String);

/// The action id carried by a surface-action callback, if `data` is one.
pub fn parse_surface_action_callback(data: &str) -> Option<&str> {
    data.strip_prefix(SURFACE_ACTION_CALLBACK_PREFIX)
        .filter(|id| !id.is_empty() && id.len() <= 64)
}

/// Lower a surface to text + buttons, or say why it can't be.
pub fn lower_to_chat(state: &SurfaceState) -> Result<LoweredSurface, NotLowerable> {
    let mut out = Lowering {
        state,
        lines: Vec::new(),
        rows: Vec::new(),
        buttons: 0,
    };
    out.node("root", None, 0)?;
    let mut text = out.lines.join("\n").trim().to_string();
    if text.is_empty() {
        text = "\u{200b}".into();
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(NotLowerable(format!(
            "surface text is {} bytes; chat lowering allows {MAX_TEXT_BYTES}",
            text.len()
        )));
    }
    Ok(LoweredSurface {
        text,
        button_rows: out.rows,
    })
}

struct Lowering<'a> {
    state: &'a SurfaceState,
    lines: Vec<String>,
    rows: Vec<Vec<LoweredButton>>,
    buttons: usize,
}

impl Lowering<'_> {
    fn component(&self, id: &str) -> Result<&Value, NotLowerable> {
        self.state
            .components
            .get(id)
            .ok_or_else(|| NotLowerable(format!("missing component '{id}'")))
    }

    /// Resolve a dynamic value against the data model; `item` is the current
    /// template item for relative paths.
    fn resolve<'v>(&'v self, value: &'v Value, item: Option<&'v Value>) -> Option<&'v Value> {
        match value
            .as_object()
            .and_then(|o| o.get("path"))
            .and_then(Value::as_str)
        {
            Some(path) if path.starts_with('/') => self.state.data_model.pointer(path),
            Some(path) => item?.pointer(&format!("/{path}")),
            None => Some(value),
        }
    }

    fn text_of(&self, value: &Value, item: Option<&Value>) -> String {
        match self.resolve(value, item) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        }
    }

    fn node(&mut self, id: &str, item: Option<&Value>, depth: usize) -> Result<(), NotLowerable> {
        if depth > 32 {
            return Err(NotLowerable("surface nests too deeply to lower".into()));
        }
        let component = self.component(id)?.clone();
        let name = component["component"].as_str().unwrap_or_default();
        match name {
            "Text" => {
                let text = self.text_of(&component["text"], item);
                let line = match component.get("variant").and_then(Value::as_str) {
                    Some("h1" | "h2" | "h3") => format!("\n{}", text.to_uppercase()),
                    Some("h4" | "h5") => format!("\n{text}"),
                    _ => text,
                };
                self.lines.push(line);
            }
            "Divider" => self.lines.push("──────────".into()),
            "Card" => {
                self.lines.push(String::new());
                let child = component["child"].as_str().unwrap_or_default().to_string();
                self.node(&child, item, depth + 1)?;
            }
            "Column" | "List" => self.children(&component, item, depth)?,
            "Row" => {
                let all_buttons = self.child_ids(&component, item)?.iter().all(|(child, _)| {
                    self.state.components.get(child).and_then(|c| c["component"].as_str()) == Some("Button")
                });
                if all_buttons {
                    let mut row = Vec::new();
                    for (child, child_item) in self.child_ids(&component, item)? {
                        row.push(self.button(&child, child_item.as_ref())?);
                    }
                    for chunk in row.chunks(MAX_BUTTONS_PER_ROW) {
                        self.rows.push(chunk.to_vec());
                    }
                } else {
                    self.children(&component, item, depth)?;
                }
            }
            "Button" => {
                let button = self.button(id, item)?;
                self.rows.push(vec![button]);
            }
            other => {
                return Err(NotLowerable(format!(
                    "{other} ('{id}') needs the web renderer; chat lowering supports Text, Column, Row, List, Card, Divider and Button"
                )))
            }
        }
        Ok(())
    }

    /// Children with their template item (if the list is templated).
    fn child_ids(
        &self,
        component: &Value,
        item: Option<&Value>,
    ) -> Result<Vec<(String, Option<Value>)>, NotLowerable> {
        match component.get("children") {
            Some(Value::Array(ids)) => Ok(ids
                .iter()
                .filter_map(Value::as_str)
                .map(|id| (id.to_string(), item.cloned()))
                .collect()),
            Some(Value::Object(template)) => {
                let template_id = template["componentId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let path = template["path"].as_str().unwrap_or_default();
                let items = if path.starts_with('/') {
                    self.state.data_model.pointer(path)
                } else {
                    item.and_then(|i| i.pointer(&format!("/{path}")))
                };
                let items = match items {
                    Some(Value::Array(items)) => items.clone(),
                    Some(Value::Null) | None => Vec::new(),
                    Some(_) => {
                        return Err(NotLowerable(format!("list path '{path}' is not an array")))
                    }
                };
                if items.len() > 50 {
                    return Err(NotLowerable("list has more than 50 items to lower".into()));
                }
                Ok(items
                    .into_iter()
                    .map(|i| (template_id.clone(), Some(i)))
                    .collect())
            }
            _ => Ok(Vec::new()),
        }
    }

    fn children(
        &mut self,
        component: &Value,
        item: Option<&Value>,
        depth: usize,
    ) -> Result<(), NotLowerable> {
        for (child, child_item) in self.child_ids(component, item)? {
            self.node(&child, child_item.as_ref(), depth + 1)?;
        }
        Ok(())
    }

    fn button(&mut self, id: &str, item: Option<&Value>) -> Result<LoweredButton, NotLowerable> {
        let component = self.component(id)?.clone();
        if component["component"].as_str() != Some("Button") {
            return Err(NotLowerable(format!("'{id}' is not a Button")));
        }
        if item.is_some() {
            // A templated button would need a distinct action id per item; the
            // hotel mints one id per component, so chat lowering refuses it.
            return Err(NotLowerable(format!(
                "button '{id}' repeats inside a list template; chat lowering needs one button per action"
            )));
        }
        self.buttons += 1;
        if self.buttons > MAX_BUTTONS {
            return Err(NotLowerable(format!(
                "more than {MAX_BUTTONS} buttons to lower"
            )));
        }
        let action_id = component["action"]["event"]["context"][ACTION_ID_KEY]
            .as_str()
            .ok_or_else(|| NotLowerable(format!("button '{id}' has no hotel-minted action id")))?
            .to_string();
        let child = component["child"].as_str().unwrap_or_default();
        let label = match self.state.components.get(child) {
            Some(c) if c["component"].as_str() == Some("Text") => self.text_of(&c["text"], item),
            _ => component["action"]["event"]["name"]
                .as_str()
                .unwrap_or("OK")
                .to_string(),
        };
        let label: String = label.chars().take(MAX_LABEL_CHARS).collect();
        Ok(LoweredButton {
            label: if label.trim().is_empty() {
                "OK".into()
            } else {
                label
            },
            action_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::{SurfaceMessage, CATALOG_ID};
    use serde_json::json;

    fn state(components: Value, data: Value) -> SurfaceState {
        let create = SurfaceMessage::from_value(
            &json!({"version": "v0.9", "createSurface": {"surfaceId": "s1", "catalogId": CATALOG_ID}}),
        )
        .unwrap();
        let mut state = SurfaceState::create(&create).unwrap();
        state
            .apply(
                &SurfaceMessage::from_value(&json!({"version": "v0.9", "updateComponents": {"surfaceId": "s1", "components": components}}))
                    .unwrap(),
            )
            .unwrap();
        state
            .apply(&SurfaceMessage::from_value(&json!({"version": "v0.9", "updateDataModel": {"surfaceId": "s1", "value": data}})).unwrap())
            .unwrap();
        state
    }

    fn button(id: &str, label_id: &str, action_id: &str) -> Value {
        json!({"id": id, "component": "Button", "child": label_id,
               "action": {"event": {"name": id, "context": {"action_id": action_id}}}})
    }

    #[test]
    fn simple_card_lowers_to_text_and_keyboard() {
        let s = state(
            json!([
                {"id": "root", "component": "Card", "child": "body"},
                {"id": "body", "component": "Column", "children": ["title", "guests", "div", "actions"]},
                {"id": "title", "component": "Text", "text": "mac-jane", "variant": "h2"},
                {"id": "guests", "component": "List", "children": {"componentId": "row", "path": "/guests"}},
                {"id": "row", "component": "Text", "text": {"path": "name"}},
                {"id": "div", "component": "Divider"},
                {"id": "actions", "component": "Row", "children": ["restart", "ignore"]},
                {"id": "l1", "component": "Text", "text": "Restart"},
                {"id": "l2", "component": "Text", "text": {"path": "/ignoreLabel"}},
                button("restart", "l1", "a000000000001"),
                button("ignore", "l2", "a000000000002"),
            ]),
            json!({"guests": [{"name": "beacon"}, {"name": "jane"}], "ignoreLabel": "Ignore"}),
        );
        let lowered = lower_to_chat(&s).unwrap();
        assert_eq!(lowered.text, "MAC-JANE\nbeacon\njane\n──────────");
        assert_eq!(lowered.button_rows.len(), 1);
        assert_eq!(lowered.button_rows[0][1].label, "Ignore");
        let keyboard = lowered.inline_keyboard().unwrap();
        assert_eq!(
            keyboard["inline_keyboard"][0][0]["callback_data"],
            "sa:a000000000001"
        );
        assert!(
            keyboard["inline_keyboard"][0][0]["callback_data"]
                .as_str()
                .unwrap()
                .len()
                <= 64
        );
    }

    #[test]
    fn inputs_and_tables_refuse_to_lower() {
        let s = state(
            json!([
                {"id": "root", "component": "Column", "children": ["f"]},
                {"id": "f", "component": "TextField", "label": "Note"}
            ]),
            json!({}),
        );
        assert!(lower_to_chat(&s).unwrap_err().0.contains("TextField"));
    }

    #[test]
    fn templated_buttons_refuse_to_lower() {
        let s = state(
            json!([
                {"id": "root", "component": "List", "children": {"componentId": "b", "path": "/items"}},
                {"id": "l", "component": "Text", "text": "Go"},
                button("b", "l", "a000000000009"),
            ]),
            json!({"items": [1, 2]}),
        );
        assert!(lower_to_chat(&s).unwrap_err().0.contains("template"));
    }

    #[test]
    fn surface_action_callbacks_parse() {
        assert_eq!(
            parse_surface_action_callback("sa:a0123456789ab"),
            Some("a0123456789ab")
        );
        assert_eq!(parse_surface_action_callback("sa:"), None);
        assert_eq!(parse_surface_action_callback("approve:x"), None);
    }
}
