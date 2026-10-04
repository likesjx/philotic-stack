//! Hotel-owned surface records (seam `surface-persistence-rehydrate`).
//!
//! A surface is a `ui_surface` graph node owned by the hotel. The owning philote
//! changes it by sending A2UI message batches; the hotel mints the surface id and
//! every action id, applies the batch atomically, and refuses writes from any
//! agent other than the owner, so one philote cannot repaint another's surface
//! (or its approval card).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    assign_action_ids, Applied, SurfaceError, SurfaceMessage, SurfaceOp, SurfaceState,
    ACTION_ID_KEY,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceStatus {
    Active,
    Deleted,
}

/// One actionable element on a surface, as minted by the hotel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceActionRef {
    pub action_id: String,
    pub event_name: String,
    pub component_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SurfaceRecord {
    pub surface_id: String,
    /// Base agent id of the authoring philote (role incarnations collapse to it).
    pub owner_agent_id: String,
    pub source_hotel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    pub state: SurfaceState,
    /// Messages applied so far; renderers use it to drop stale updates.
    pub seq: u64,
    pub status: SurfaceStatus,
    pub created_at: u64,
    pub updated_at: u64,
}

impl SurfaceRecord {
    /// Every actionable element currently on the surface.
    pub fn actions(&self) -> Vec<SurfaceActionRef> {
        self.state
            .components
            .values()
            .filter_map(|c| {
                let event = c.get("action")?.get("event")?;
                Some(SurfaceActionRef {
                    action_id: event
                        .get("context")?
                        .get(ACTION_ID_KEY)?
                        .as_str()?
                        .to_string(),
                    event_name: event.get("name")?.as_str()?.to_string(),
                    component_id: c.get("id")?.as_str()?.to_string(),
                })
            })
            .collect()
    }

    /// The action on this surface that carries `action_id`, if any.
    pub fn find_action(&self, action_id: &str) -> Option<SurfaceActionRef> {
        self.actions()
            .into_iter()
            .find(|a| a.action_id == action_id)
    }
}

/// `agent-x:role` and `agent-x` are the same author.
pub fn base_agent_id(guest_id: &str) -> &str {
    guest_id.split(':').next().unwrap_or(guest_id)
}

/// A fresh hotel-minted surface id: `s` + lowercase ULID (27 chars).
pub fn new_surface_id() -> String {
    format!("s{}", ulid::Ulid::new().to_string().to_ascii_lowercase())
}

/// Who is writing and where.
#[derive(Debug, Clone)]
pub struct ApplyContext {
    /// Caller's guest id; ownership compares its base agent id.
    pub caller_guest_id: String,
    pub source_hotel: String,
    pub title: Option<String>,
    pub session_id: Option<String>,
    pub chat_id: Option<String>,
    pub transport: Option<String>,
    pub now: u64,
}

/// Apply a batch of A2UI messages to a surface, atomically.
///
/// - `existing: None` creates a surface: the first message must be
///   `createSurface`, and `new_surface_id` becomes its id.
/// - Every message's `surfaceId` is normalised to the record's id, so the
///   philote never has to know (or guess) the hotel-minted id.
/// - Button action ids are minted with `mint`; model-supplied ones are replaced.
/// - Any error leaves nothing changed (the caller simply does not store).
pub fn apply_surface_messages(
    existing: Option<SurfaceRecord>,
    new_surface_id: impl FnOnce() -> String,
    ctx: &ApplyContext,
    messages: &[Value],
    mint: impl FnMut() -> String,
) -> Result<SurfaceRecord, SurfaceError> {
    if messages.is_empty() {
        return Err(SurfaceError::invalid("no surface messages to apply"));
    }
    let owner = base_agent_id(&ctx.caller_guest_id).to_string();
    let surface_id = match &existing {
        Some(record) => {
            if record.owner_agent_id != owner {
                return Err(SurfaceError::forbidden(format!(
                    "surface '{}' belongs to {}; {} may not change it",
                    record.surface_id, record.owner_agent_id, owner
                )));
            }
            if record.status == SurfaceStatus::Deleted {
                return Err(SurfaceError::deleted(format!(
                    "surface '{}' was deleted",
                    record.surface_id
                )));
            }
            record.surface_id.clone()
        }
        None => new_surface_id(),
    };

    let mut parsed = Vec::with_capacity(messages.len());
    for message in messages {
        let mut message = message.clone();
        normalise_surface_id(&mut message, &surface_id);
        parsed.push(SurfaceMessage::from_value(&message)?);
    }
    // Mint action ids before applying so the stored tree carries them.
    let mut mint = mint;
    for message in &mut parsed {
        if let SurfaceOp::UpdateComponents { components, .. } = &mut message.op {
            assign_action_ids(components, &mut mint);
        }
    }

    let mut iter = parsed.iter();
    let (mut record, mut applied) = match existing {
        Some(record) => (record, 0u64),
        None => {
            let first = iter.next().expect("messages is non-empty");
            let state = SurfaceState::create(first)?;
            let record = SurfaceRecord {
                surface_id: surface_id.clone(),
                owner_agent_id: owner,
                source_hotel: ctx.source_hotel.clone(),
                title: None,
                session_id: None,
                chat_id: None,
                transport: None,
                state,
                seq: 0,
                status: SurfaceStatus::Active,
                created_at: ctx.now,
                updated_at: ctx.now,
            };
            (record, 1u64)
        }
    };
    for message in iter {
        if record.status == SurfaceStatus::Deleted {
            return Err(SurfaceError::deleted(
                "messages after deleteSurface in the same batch",
            ));
        }
        if record.state.apply(message)? == Applied::Deleted {
            record.status = SurfaceStatus::Deleted;
        }
        applied += 1;
    }
    if record.status == SurfaceStatus::Active && !record.state.components.contains_key("root") {
        return Err(SurfaceError::invalid(
            "surface has no 'root' component; send createSurface and updateComponents together",
        ));
    }
    record.seq += applied;
    record.updated_at = ctx.now;
    if ctx.title.is_some() {
        record.title = ctx.title.clone();
    }
    for (slot, value) in [
        (&mut record.session_id, &ctx.session_id),
        (&mut record.chat_id, &ctx.chat_id),
        (&mut record.transport, &ctx.transport),
    ] {
        if value.is_some() {
            *slot = value.clone();
        }
    }
    Ok(record)
}

fn normalise_surface_id(message: &mut Value, surface_id: &str) {
    if let Some(obj) = message.as_object_mut() {
        for (key, body) in obj.iter_mut() {
            if key != "version" {
                if let Some(body) = body.as_object_mut() {
                    body.insert("surfaceId".into(), Value::String(surface_id.to_string()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::CATALOG_ID;
    use serde_json::json;

    fn ctx(caller: &str) -> ApplyContext {
        ApplyContext {
            caller_guest_id: caller.into(),
            source_hotel: "mac-jane".into(),
            title: Some("Guests".into()),
            session_id: Some("sess-1".into()),
            chat_id: Some("555".into()),
            transport: Some("telegram".into()),
            now: 100,
        }
    }

    fn create_batch() -> Vec<Value> {
        vec![
            json!({"version": "v0.9", "createSurface": {"surfaceId": "draft", "catalogId": CATALOG_ID}}),
            json!({"version": "v0.9", "updateComponents": {"surfaceId": "draft", "components": [
                {"id": "root", "component": "Column", "children": ["go"]},
                {"id": "label", "component": "Text", "text": "Restart"},
                {"id": "go", "component": "Button", "child": "label",
                 "action": {"event": {"name": "restart", "context": {"action_id": "forged"}}}}
            ]}}),
        ]
    }

    fn counter() -> impl FnMut() -> String {
        let mut n = 0;
        move || {
            n += 1;
            format!("a{n:012}")
        }
    }

    #[test]
    fn create_mints_ids_and_normalises_surface_ids() {
        let record = apply_surface_messages(
            None,
            || "s-new".into(),
            &ctx("agent-beacon:brain"),
            &create_batch(),
            counter(),
        )
        .unwrap();
        assert_eq!(record.surface_id, "s-new");
        assert_eq!(record.state.surface_id, "s-new");
        assert_eq!(record.owner_agent_id, "agent-beacon");
        assert_eq!(record.seq, 2);
        assert_eq!(record.status, SurfaceStatus::Active);
        assert_eq!(record.title.as_deref(), Some("Guests"));
        assert_eq!(
            record.actions(),
            vec![SurfaceActionRef {
                action_id: "a000000000001".into(),
                event_name: "restart".into(),
                component_id: "go".into(),
            }]
        );
        assert!(record.find_action("forged").is_none());
    }

    #[test]
    fn create_without_root_is_refused() {
        let batch = vec![create_batch().remove(0)];
        let err = apply_surface_messages(
            None,
            || "s1".into(),
            &ctx("agent-beacon"),
            &batch,
            counter(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("root"));
    }

    #[test]
    fn only_the_owner_may_change_a_surface() {
        let record = apply_surface_messages(
            None,
            || "s1".into(),
            &ctx("agent-beacon"),
            &create_batch(),
            counter(),
        )
        .unwrap();
        let update = vec![
            json!({"version": "v0.9", "updateDataModel": {"surfaceId": "s1", "path": "/x", "value": 1}}),
        ];
        let err = apply_surface_messages(
            Some(record.clone()),
            || unreachable!(),
            &ctx("agent-jane"),
            &update,
            counter(),
        )
        .unwrap_err();
        assert_eq!(err.code(), "SURFACE_FORBIDDEN");
        // A role incarnation of the owner is the owner.
        let next = apply_surface_messages(
            Some(record),
            || unreachable!(),
            &ctx("agent-beacon:ops"),
            &update,
            counter(),
        )
        .unwrap();
        assert_eq!(next.seq, 3);
        assert_eq!(next.state.data_model, json!({"x": 1}));
    }

    #[test]
    fn a_bad_message_rejects_the_whole_batch() {
        let record = apply_surface_messages(
            None,
            || "s1".into(),
            &ctx("agent-beacon"),
            &create_batch(),
            counter(),
        )
        .unwrap();
        let batch = vec![
            json!({"version": "v0.9", "updateDataModel": {"surfaceId": "s1", "path": "/x", "value": 1}}),
            json!({"version": "v0.9", "updateComponents": {"surfaceId": "s1", "components": [
                {"id": "pic", "component": "Image", "url": "https://x"}]}}),
        ];
        let err = apply_surface_messages(
            Some(record.clone()),
            || unreachable!(),
            &ctx("agent-beacon"),
            &batch,
            counter(),
        )
        .unwrap_err();
        assert_eq!(err.code(), "SURFACE_COMPONENT_NOT_ALLOWED");
    }

    #[test]
    fn deleted_surfaces_stay_deleted() {
        let record = apply_surface_messages(
            None,
            || "s1".into(),
            &ctx("agent-beacon"),
            &create_batch(),
            counter(),
        )
        .unwrap();
        let delete = vec![json!({"version": "v0.9", "deleteSurface": {"surfaceId": "whatever"}})];
        let deleted = apply_surface_messages(
            Some(record),
            || unreachable!(),
            &ctx("agent-beacon"),
            &delete,
            counter(),
        )
        .unwrap();
        assert_eq!(deleted.status, SurfaceStatus::Deleted);
        let err = apply_surface_messages(
            Some(deleted),
            || unreachable!(),
            &ctx("agent-beacon"),
            &delete,
            counter(),
        )
        .unwrap_err();
        assert_eq!(err.code(), "SURFACE_DELETED");
    }

    #[test]
    fn record_round_trips_through_json() {
        let record = apply_surface_messages(
            None,
            || "s1".into(),
            &ctx("agent-beacon"),
            &create_batch(),
            counter(),
        )
        .unwrap();
        let back: SurfaceRecord =
            serde_json::from_value(serde_json::to_value(&record).unwrap()).unwrap();
        assert_eq!(back, record);
    }

    #[test]
    fn surface_ids_are_hotel_minted_ulids() {
        let id = new_surface_id();
        assert_eq!(id.len(), 27);
        assert!(id.starts_with('s'));
        crate::surface::check_id(&id).unwrap();
        assert_ne!(id, new_surface_id());
    }
}
