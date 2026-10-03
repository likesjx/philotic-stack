//! Philote-authored operator surfaces: A2UI v0.9 messages against the
//! `philotic.desktop.v1` catalog (seam `surface-schema-and-types`).
//!
//! UI is data, never code. A philote emits A2UI messages; the hotel parses them
//! strictly, validates them against the catalog, applies them to a
//! [`SurfaceState`], and mints the action ids that correlate a click back to the
//! surface. Renderers (web, native lowering) only ever see validated state.
//!
//! Wire shapes follow the vendored schemas in `specs/a2ui/v0_9/`; see
//! `docs/architecture/DESKTOP_GENERATIVE_SURFACES_PROPOSAL.md`.

pub mod catalog;
pub mod pointer;
pub mod record;
pub mod validate;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub use catalog::{catalog, CATALOG_JSON};
pub use validate::{check_id, validate_component, validate_tree};

/// A2UI protocol version carried in every message.
pub const A2UI_VERSION: &str = "v0.9";
/// The only catalog philote surfaces may reference.
pub const CATALOG_ID: &str = "philotic.desktop.v1";
/// Key in a Button's `action.event.context` that carries the hotel-minted id.
pub const ACTION_ID_KEY: &str = "action_id";

/// A refused surface message. `code()` is the stable IPC error code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SurfaceError {
    code: &'static str,
    message: String,
}

impl SurfaceError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("INVALID_SURFACE", message)
    }
    pub fn component_not_allowed(message: impl Into<String>) -> Self {
        Self::new("SURFACE_COMPONENT_NOT_ALLOWED", message)
    }
    pub fn function_not_allowed(message: impl Into<String>) -> Self {
        Self::new("SURFACE_FUNCTION_NOT_ALLOWED", message)
    }
    pub fn too_large(message: impl Into<String>) -> Self {
        Self::new("SURFACE_TOO_LARGE", message)
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new("SURFACE_FORBIDDEN", message)
    }
    pub fn deleted(message: impl Into<String>) -> Self {
        Self::new("SURFACE_DELETED", message)
    }
    pub fn code(&self) -> &'static str {
        self.code
    }
}

/// Optional surface theme. v1 accepts only a display name and a primary color;
/// `iconUrl` is refused because it would make the renderer fetch a remote URL.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceTheme {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_display_name: Option<String>,
}

/// One A2UI server→client operation.
#[derive(Debug, Clone, PartialEq)]
pub enum SurfaceOp {
    CreateSurface {
        surface_id: String,
        catalog_id: String,
        theme: Option<SurfaceTheme>,
        send_data_model: bool,
    },
    UpdateComponents {
        surface_id: String,
        components: Vec<Value>,
    },
    /// `value: None` removes the key at `path` (the A2UI "omitted value" rule);
    /// `Some(Value::Null)` sets it to null.
    UpdateDataModel {
        surface_id: String,
        path: Option<String>,
        value: Option<Value>,
    },
    DeleteSurface {
        surface_id: String,
    },
}

impl SurfaceOp {
    pub fn surface_id(&self) -> &str {
        match self {
            Self::CreateSurface { surface_id, .. }
            | Self::UpdateComponents { surface_id, .. }
            | Self::UpdateDataModel { surface_id, .. }
            | Self::DeleteSurface { surface_id } => surface_id,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::CreateSurface { .. } => "createSurface",
            Self::UpdateComponents { .. } => "updateComponents",
            Self::UpdateDataModel { .. } => "updateDataModel",
            Self::DeleteSurface { .. } => "deleteSurface",
        }
    }
}

/// A validated A2UI v0.9 server→client message.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceMessage {
    pub op: SurfaceOp,
}

impl SurfaceMessage {
    /// Parse and validate one message: `{"version": "v0.9", <op>: {...}}` with
    /// exactly one operation key and no unknown fields anywhere in the envelope.
    pub fn from_value(value: &Value) -> Result<Self, SurfaceError> {
        let limits = &catalog().limits;
        let bytes = serde_json::to_vec(value)
            .map(|b| b.len())
            .unwrap_or(usize::MAX);
        if bytes > limits.max_message_bytes {
            return Err(SurfaceError::too_large(format!(
                "message is {bytes} bytes; the limit is {}",
                limits.max_message_bytes
            )));
        }
        let obj = value
            .as_object()
            .ok_or_else(|| SurfaceError::invalid("message must be a JSON object"))?;
        match obj.get("version").and_then(Value::as_str) {
            Some(A2UI_VERSION) => {}
            other => {
                return Err(SurfaceError::invalid(format!(
                    "version must be \"{A2UI_VERSION}\", got {other:?}"
                )))
            }
        }
        let ops: Vec<(&String, &Value)> = obj.iter().filter(|(k, _)| *k != "version").collect();
        let [(kind, body)] = ops.as_slice() else {
            return Err(SurfaceError::invalid(
                "message must carry exactly one of createSurface, updateComponents, updateDataModel, deleteSurface",
            ));
        };
        let body = body
            .as_object()
            .ok_or_else(|| SurfaceError::invalid(format!("{kind} must be an object")))?;
        let surface_id = body
            .get("surfaceId")
            .and_then(Value::as_str)
            .ok_or_else(|| SurfaceError::invalid(format!("{kind} needs a string surfaceId")))?
            .to_string();
        check_id(&surface_id)?;
        let allow = |allowed: &[&str]| -> Result<(), SurfaceError> {
            match body.keys().find(|k| !allowed.contains(&k.as_str())) {
                Some(extra) => Err(SurfaceError::invalid(format!(
                    "{kind} has unknown field '{extra}'"
                ))),
                None => Ok(()),
            }
        };
        let op = match kind.as_str() {
            "createSurface" => {
                allow(&["surfaceId", "catalogId", "theme", "sendDataModel"])?;
                let catalog_id = body
                    .get("catalogId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| SurfaceError::invalid("createSurface needs a catalogId"))?;
                if catalog_id != CATALOG_ID {
                    return Err(SurfaceError::invalid(format!(
                        "catalogId must be \"{CATALOG_ID}\", got \"{catalog_id}\""
                    )));
                }
                let theme = body.get("theme").map(parse_theme).transpose()?;
                let send_data_model = match body.get("sendDataModel") {
                    None => false,
                    Some(Value::Bool(b)) => *b,
                    Some(_) => {
                        return Err(SurfaceError::invalid("sendDataModel must be a boolean"))
                    }
                };
                SurfaceOp::CreateSurface {
                    surface_id,
                    catalog_id: catalog_id.to_string(),
                    theme,
                    send_data_model,
                }
            }
            "updateComponents" => {
                allow(&["surfaceId", "components"])?;
                let components = body
                    .get("components")
                    .and_then(Value::as_array)
                    .filter(|c| !c.is_empty())
                    .ok_or_else(|| {
                        SurfaceError::invalid("updateComponents needs a non-empty components array")
                    })?
                    .clone();
                for component in &components {
                    validate_component(component)?;
                }
                SurfaceOp::UpdateComponents {
                    surface_id,
                    components,
                }
            }
            "updateDataModel" => {
                allow(&["surfaceId", "path", "value"])?;
                let path = match body.get("path") {
                    None => None,
                    Some(Value::String(p)) => {
                        pointer::tokens(p).map_err(SurfaceError::invalid)?;
                        Some(p.clone())
                    }
                    Some(_) => {
                        return Err(SurfaceError::invalid(
                            "updateDataModel path must be a string",
                        ))
                    }
                };
                SurfaceOp::UpdateDataModel {
                    surface_id,
                    path,
                    value: body.get("value").cloned(),
                }
            }
            "deleteSurface" => {
                allow(&["surfaceId"])?;
                SurfaceOp::DeleteSurface { surface_id }
            }
            other => {
                return Err(SurfaceError::invalid(format!(
                    "unknown message type '{other}'"
                )))
            }
        };
        Ok(Self { op })
    }

    /// Back to the A2UI wire shape.
    pub fn to_value(&self) -> Value {
        let body = match &self.op {
            SurfaceOp::CreateSurface {
                surface_id,
                catalog_id,
                theme,
                send_data_model,
            } => {
                let mut body = json!({"surfaceId": surface_id, "catalogId": catalog_id});
                if let Some(theme) = theme {
                    body["theme"] = serde_json::to_value(theme).unwrap_or_default();
                }
                if *send_data_model {
                    body["sendDataModel"] = Value::Bool(true);
                }
                body
            }
            SurfaceOp::UpdateComponents {
                surface_id,
                components,
            } => json!({"surfaceId": surface_id, "components": components}),
            SurfaceOp::UpdateDataModel {
                surface_id,
                path,
                value,
            } => {
                let mut body = json!({"surfaceId": surface_id});
                if let Some(path) = path {
                    body["path"] = Value::String(path.clone());
                }
                if let Some(value) = value {
                    body["value"] = value.clone();
                }
                body
            }
            SurfaceOp::DeleteSurface { surface_id } => json!({"surfaceId": surface_id}),
        };
        let mut message = Map::new();
        message.insert("version".into(), Value::String(A2UI_VERSION.into()));
        message.insert(self.op.kind().into(), body);
        Value::Object(message)
    }
}

fn parse_theme(value: &Value) -> Result<SurfaceTheme, SurfaceError> {
    let obj = value
        .as_object()
        .ok_or_else(|| SurfaceError::invalid("theme must be an object"))?;
    if let Some(extra) = obj
        .keys()
        .find(|k| !matches!(k.as_str(), "primaryColor" | "agentDisplayName"))
    {
        return Err(SurfaceError::invalid(format!(
            "theme.{extra} is not allowed (v1 accepts primaryColor and agentDisplayName)"
        )));
    }
    let theme: SurfaceTheme = serde_json::from_value(value.clone())
        .map_err(|e| SurfaceError::invalid(format!("theme: {e}")))?;
    if let Some(color) = &theme.primary_color {
        let hex = color.strip_prefix('#').unwrap_or("");
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(SurfaceError::invalid("theme.primaryColor must be #RRGGBB"));
        }
    }
    if theme
        .agent_display_name
        .as_ref()
        .is_some_and(|n| n.len() > 64)
    {
        return Err(SurfaceError::invalid(
            "theme.agentDisplayName exceeds 64 bytes",
        ));
    }
    Ok(theme)
}

/// The current state of one surface: what a renderer draws.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceState {
    pub surface_id: String,
    pub catalog_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<SurfaceTheme>,
    #[serde(default)]
    pub send_data_model: bool,
    /// Components keyed by id (the A2UI adjacency list).
    pub components: BTreeMap<String, Value>,
    #[serde(default = "empty_object")]
    pub data_model: Value,
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

/// What applying a message did to the surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    Updated,
    Deleted,
}

impl SurfaceState {
    /// Start a surface from `createSurface`.
    pub fn create(message: &SurfaceMessage) -> Result<Self, SurfaceError> {
        let SurfaceOp::CreateSurface {
            surface_id,
            catalog_id,
            theme,
            send_data_model,
        } = &message.op
        else {
            return Err(SurfaceError::invalid(format!(
                "a surface must start with createSurface, got {}",
                message.op.kind()
            )));
        };
        Ok(Self {
            surface_id: surface_id.clone(),
            catalog_id: catalog_id.clone(),
            theme: theme.clone(),
            send_data_model: *send_data_model,
            components: BTreeMap::new(),
            data_model: empty_object(),
        })
    }

    /// Apply one message. On error the state is unchanged.
    pub fn apply(&mut self, message: &SurfaceMessage) -> Result<Applied, SurfaceError> {
        if message.op.surface_id() != self.surface_id {
            return Err(SurfaceError::invalid(format!(
                "message for surface '{}' applied to '{}'",
                message.op.surface_id(),
                self.surface_id
            )));
        }
        match &message.op {
            SurfaceOp::CreateSurface { .. } => Err(SurfaceError::invalid(format!(
                "surface '{}' already exists; delete it before creating it again",
                self.surface_id
            ))),
            SurfaceOp::UpdateComponents { components, .. } => {
                let mut next = self.components.clone();
                for component in components {
                    let id = component["id"].as_str().unwrap_or_default().to_string();
                    next.insert(id, component.clone());
                }
                validate_tree(&next)?;
                self.components = next;
                Ok(Applied::Updated)
            }
            SurfaceOp::UpdateDataModel { path, value, .. } => {
                let mut next = self.data_model.clone();
                let path = path.as_deref().unwrap_or("/");
                match value {
                    Some(value) => pointer::set(&mut next, path, value.clone()),
                    None => pointer::remove(&mut next, path),
                }
                .map_err(SurfaceError::invalid)?;
                let limits = &catalog().limits;
                let bytes = serde_json::to_vec(&next)
                    .map(|b| b.len())
                    .unwrap_or(usize::MAX);
                if bytes > limits.max_data_model_bytes {
                    return Err(SurfaceError::too_large(format!(
                        "data model would be {bytes} bytes; the limit is {}",
                        limits.max_data_model_bytes
                    )));
                }
                self.data_model = next;
                Ok(Applied::Updated)
            }
            SurfaceOp::DeleteSurface { .. } => Ok(Applied::Deleted),
        }
    }

    /// The surface as A2UI messages a renderer can replay from scratch.
    pub fn to_messages(&self) -> Vec<Value> {
        let mut out = vec![SurfaceMessage {
            op: SurfaceOp::CreateSurface {
                surface_id: self.surface_id.clone(),
                catalog_id: self.catalog_id.clone(),
                theme: self.theme.clone(),
                send_data_model: self.send_data_model,
            },
        }
        .to_value()];
        if !self.components.is_empty() {
            out.push(
                SurfaceMessage {
                    op: SurfaceOp::UpdateComponents {
                        surface_id: self.surface_id.clone(),
                        components: self.components.values().cloned().collect(),
                    },
                }
                .to_value(),
            );
        }
        if self.data_model.as_object().is_none_or(|m| !m.is_empty()) {
            out.push(
                SurfaceMessage {
                    op: SurfaceOp::UpdateDataModel {
                        surface_id: self.surface_id.clone(),
                        path: None,
                        value: Some(self.data_model.clone()),
                    },
                }
                .to_value(),
            );
        }
        out
    }

    /// Every action id currently on the surface, with its event name.
    pub fn action_ids(&self) -> Vec<(String, String)> {
        self.components
            .values()
            .filter_map(|c| {
                let event = c.get("action")?.get("event")?;
                Some((
                    event
                        .get("context")?
                        .get(ACTION_ID_KEY)?
                        .as_str()?
                        .to_string(),
                    event.get("name")?.as_str()?.to_string(),
                ))
            })
            .collect()
    }
}

/// A compact action id: `a` + 12 hex chars (48 random bits). Short enough for
/// Telegram's 64-byte `callback_data` together with a surface reference.
pub fn new_action_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 6];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("a{}", hex::encode(bytes))
}

/// Overwrite `context.action_id` on every Button event with a hotel-minted id.
/// Model-supplied ids are never trusted: they could collide with, or forge, the
/// correlation that ties a click to its surface or approval. Returns the minted
/// `(action_id, event_name, component_id)` triples.
pub fn assign_action_ids(
    components: &mut [Value],
    mut mint: impl FnMut() -> String,
) -> Vec<(String, String, String)> {
    let mut minted = Vec::new();
    for component in components {
        let component_id = component["id"].as_str().unwrap_or_default().to_string();
        let Some(event) = component
            .get_mut("action")
            .and_then(|a| a.get_mut("event"))
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        let name = event
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let context = event
            .entry("context")
            .or_insert_with(|| Value::Object(Map::new()));
        if !context.is_object() {
            *context = Value::Object(Map::new());
        }
        let id = mint();
        context[ACTION_ID_KEY] = Value::String(id.clone());
        minted.push((id, name, component_id));
    }
    minted
}

/// An A2UI v0.9 client→server `action`: what a click or form submit sends back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SurfaceAction {
    pub name: String,
    pub surface_id: String,
    pub source_component_id: String,
    pub timestamp: String,
    #[serde(default)]
    pub context: Map<String, Value>,
}

impl SurfaceAction {
    /// Parse `{"version": "v0.9", "action": {...}}`, or a bare action object.
    pub fn from_value(value: &Value) -> Result<Self, SurfaceError> {
        let body = match value.get("version") {
            Some(Value::String(v)) if v == A2UI_VERSION => {
                let obj = value.as_object().map(|o| o.len()).unwrap_or(0);
                if obj != 2 {
                    return Err(SurfaceError::invalid(
                        "action message must be {version, action}",
                    ));
                }
                value
                    .get("action")
                    .ok_or_else(|| SurfaceError::invalid("message has no action"))?
            }
            Some(_) => {
                return Err(SurfaceError::invalid(format!(
                    "version must be \"{A2UI_VERSION}\""
                )))
            }
            None => value,
        };
        let action: Self = serde_json::from_value(body.clone())
            .map_err(|e| SurfaceError::invalid(format!("action: {e}")))?;
        check_id(&action.surface_id)?;
        check_id(&action.source_component_id)?;
        check_id(&action.name)?;
        Ok(action)
    }

    /// The hotel-minted id the click carries back, if any.
    pub fn action_id(&self) -> Option<&str> {
        self.context.get(ACTION_ID_KEY).and_then(Value::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(value: Value) -> SurfaceMessage {
        SurfaceMessage::from_value(&value).unwrap_or_else(|e| panic!("{value}: {e}"))
    }

    fn create(id: &str) -> SurfaceMessage {
        msg(json!({"version": "v0.9", "createSurface": {"surfaceId": id, "catalogId": CATALOG_ID}}))
    }

    #[test]
    fn messages_round_trip_through_the_wire_shape() {
        let cases = [
            json!({"version": "v0.9", "createSurface": {"surfaceId": "s1", "catalogId": CATALOG_ID,
                   "theme": {"primaryColor": "#336699"}, "sendDataModel": true}}),
            json!({"version": "v0.9", "updateComponents": {"surfaceId": "s1",
                   "components": [{"id": "root", "component": "Text", "text": "hi"}]}}),
            json!({"version": "v0.9", "updateDataModel": {"surfaceId": "s1", "path": "/a", "value": null}}),
            json!({"version": "v0.9", "updateDataModel": {"surfaceId": "s1", "path": "/a"}}),
            json!({"version": "v0.9", "deleteSurface": {"surfaceId": "s1"}}),
        ];
        for case in cases {
            assert_eq!(msg(case.clone()).to_value(), case);
        }
    }

    #[test]
    fn envelope_errors_fail_loud() {
        let bad = [
            json!({"createSurface": {"surfaceId": "s", "catalogId": CATALOG_ID}}),
            json!({"version": "v0.8", "createSurface": {"surfaceId": "s", "catalogId": CATALOG_ID}}),
            json!({"version": "v0.9"}),
            json!({"version": "v0.9", "deleteSurface": {"surfaceId": "s"}, "createSurface": {}}),
            json!({"version": "v0.9", "createSurface": {"surfaceId": "s", "catalogId": "basic"}}),
            json!({"version": "v0.9", "createSurface": {"surfaceId": "s", "catalogId": CATALOG_ID,
                   "theme": {"iconUrl": "https://evil"}}}),
            json!({"version": "v0.9", "deleteSurface": {"surfaceId": "s", "extra": 1}}),
            json!({"version": "v0.9", "updateComponents": {"surfaceId": "s", "components": []}}),
            json!({"version": "v0.9", "updateDataModel": {"surfaceId": "s", "path": "no-slash"}}),
            json!({"version": "v0.9", "beginRendering": {"surfaceId": "s"}}),
        ];
        for case in bad {
            assert!(
                SurfaceMessage::from_value(&case).is_err(),
                "accepted: {case}"
            );
        }
    }

    #[test]
    fn oversized_messages_are_refused() {
        let text = "x".repeat(4000);
        let components: Vec<Value> = (0..20)
            .map(|i| json!({"id": format!("t{i}"), "component": "Text", "text": text}))
            .collect();
        let err = SurfaceMessage::from_value(
            &json!({"version": "v0.9", "updateComponents": {"surfaceId": "s", "components": components}}),
        )
        .unwrap_err();
        assert_eq!(err.code(), "SURFACE_TOO_LARGE");
    }

    #[test]
    fn state_applies_components_and_data_model() {
        let mut state = SurfaceState::create(&create("s1")).unwrap();
        state
            .apply(&msg(
                json!({"version": "v0.9", "updateComponents": {"surfaceId": "s1", "components": [
                    {"id": "root", "component": "Column", "children": ["title"]},
                    {"id": "title", "component": "Text", "text": {"path": "/title"}}
                ]}}),
            ))
            .unwrap();
        state
            .apply(&msg(json!({"version": "v0.9", "updateDataModel": {"surfaceId": "s1", "path": "/title", "value": "Guests"}})))
            .unwrap();
        assert_eq!(state.data_model, json!({"title": "Guests"}));

        // A later update replaces a component by id and must keep the tree valid.
        state
            .apply(&msg(
                json!({"version": "v0.9", "updateComponents": {"surfaceId": "s1", "components": [
                    {"id": "title", "component": "Text", "text": "Static", "variant": "h1"}
                ]}}),
            ))
            .unwrap();
        assert_eq!(state.components["title"]["variant"], "h1");

        // Removing via an omitted value.
        state
            .apply(&msg(json!({"version": "v0.9", "updateDataModel": {"surfaceId": "s1", "path": "/title"}})))
            .unwrap();
        assert_eq!(state.data_model, json!({}));

        let replay = state.to_messages();
        assert_eq!(replay.len(), 2);
        assert!(replay[0].get("createSurface").is_some());
    }

    #[test]
    fn failed_apply_leaves_state_unchanged() {
        let mut state = SurfaceState::create(&create("s1")).unwrap();
        let before = state.clone();
        let err = state
            .apply(&msg(
                json!({"version": "v0.9", "updateComponents": {"surfaceId": "s1", "components": [
                    {"id": "root", "component": "Card", "child": "missing"}
                ]}}),
            ))
            .unwrap_err();
        assert!(err.to_string().contains("missing"));
        assert_eq!(state, before);
        assert!(
            state.apply(&create("s1")).is_err(),
            "double create must fail"
        );
        assert!(
            state.apply(&create("other")).is_err(),
            "wrong surface must fail"
        );
        assert_eq!(
            state.apply(&msg(
                json!({"version": "v0.9", "deleteSurface": {"surfaceId": "s1"}})
            )),
            Ok(Applied::Deleted)
        );
    }

    #[test]
    fn surfaces_must_start_with_create() {
        let update = msg(json!({"version": "v0.9", "deleteSurface": {"surfaceId": "s1"}}));
        assert!(SurfaceState::create(&update).is_err());
    }

    #[test]
    fn action_ids_are_minted_server_side_and_overwrite_model_ids() {
        let mut components = vec![
            json!({"id": "go", "component": "Button", "child": "t",
                   "action": {"event": {"name": "approve", "context": {"action_id": "forged", "n": 1}}}}),
            json!({"id": "plain", "component": "Button", "child": "t",
                   "action": {"event": {"name": "dismiss"}}}),
            json!({"id": "t", "component": "Text", "text": "x"}),
        ];
        let mut n = 0;
        let minted = assign_action_ids(&mut components, || {
            n += 1;
            format!("a{n:012}")
        });
        assert_eq!(minted.len(), 2);
        assert_eq!(
            components[0]["action"]["event"]["context"]["action_id"],
            "a000000000001"
        );
        assert_eq!(components[0]["action"]["event"]["context"]["n"], 1);
        assert_eq!(
            components[1]["action"]["event"]["context"]["action_id"],
            "a000000000002"
        );
        assert_eq!(
            minted[1],
            ("a000000000002".into(), "dismiss".into(), "plain".into())
        );

        let id = new_action_id();
        assert_eq!(id.len(), 13);
        assert!(id.starts_with('a'));
        assert_ne!(id, new_action_id());
    }

    #[test]
    fn client_actions_parse_and_carry_the_action_id() {
        let wire = json!({"version": "v0.9", "action": {
            "name": "approve", "surfaceId": "s1", "sourceComponentId": "go",
            "timestamp": "2026-10-02T12:00:00Z", "context": {"action_id": "a0123456789ab"}}});
        let action = SurfaceAction::from_value(&wire).unwrap();
        assert_eq!(action.action_id(), Some("a0123456789ab"));
        let bare = SurfaceAction::from_value(&wire["action"]).unwrap();
        assert_eq!(bare, action);

        let mut extra = wire.clone();
        extra["action"]["evil"] = json!(1);
        assert!(SurfaceAction::from_value(&extra).is_err());
        assert!(
            SurfaceAction::from_value(&json!({"version": "v0.9", "action": {"name": "x"}}))
                .is_err()
        );
    }
}
