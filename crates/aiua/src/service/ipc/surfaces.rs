//! A2UI surfaces: apply surface messages with attribution (S1a).
//!
//! Moved verbatim from `ipc/mod.rs` (IPC_DISPATCH_SPLIT); only item
//! visibility was widened so the parent module can reach it.

use super::*;

/// Where a surface write came from, stamped on the record.
pub(in crate::service) struct SurfaceAttribution {
    pub title: Option<String>,
    pub session_id: Option<String>,
    pub chat_id: Option<String>,
    pub transport: Option<String>,
}

/// Desktop generative surfaces (doc:desktop-generative-surfaces S1): apply an
/// A2UI batch to a hotel-owned surface.
///
/// The caller must be a registered guest; its base agent id becomes (or must
/// match) the owner, so one philote cannot repaint another's surface. The hotel
/// mints the surface id and every action id, validates against
/// `philotic.desktop.v1`, and stores nothing unless the whole batch applies.
pub(in crate::service) fn handle_apply_surface_messages(
    identity: Option<&GuestIdentity>,
    graph: &GraphDomain,
    local_node_id: &str,
    surface_id: Option<String>,
    messages: Vec<serde_json::Value>,
    attribution: SurfaceAttribution,
) -> IpcResponse {
    use ansible_mesh_core::surface::new_action_id;
    use ansible_mesh_core::surface::record::{
        ApplyContext, apply_surface_messages, new_surface_id,
    };
    const OP: &str = "apply_surface_messages";

    let Some(identity) = identity else {
        return IpcResponse::error(
            OP,
            "SURFACE_UNREGISTERED",
            "guest must register before writing surfaces",
        );
    };
    let existing = match surface_id.as_deref() {
        None => None,
        Some(id) => match graph.get_surface(id) {
            Ok(Some(record)) => Some(record),
            Ok(None) => {
                return IpcResponse::error(OP, "SURFACE_NOT_FOUND", format!("no surface {id}"));
            }
            Err(e) => return IpcResponse::error(OP, "SURFACE_ERROR", e.to_string()),
        },
    };
    let ctx = ApplyContext {
        caller_guest_id: identity.guest_id.clone(),
        source_hotel: local_node_id.to_string(),
        title: attribution.title,
        session_id: attribution.session_id,
        chat_id: attribution.chat_id,
        transport: attribution.transport,
        now: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    let record =
        match apply_surface_messages(existing, new_surface_id, &ctx, &messages, new_action_id) {
            Ok(record) => record,
            Err(e) => return IpcResponse::error(OP, e.code(), e.to_string()),
        };
    if let Err(e) = graph.upsert_surface(&record) {
        return IpcResponse::error(OP, "SURFACE_ERROR", e.to_string());
    }
    IpcResponse::success(
        OP,
        Some(serde_json::to_value(&record).unwrap_or(serde_json::Value::Null)),
    )
}

impl IpcServer {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_apply_surface_messages(
        surface_id: Option<String>,
        messages: Vec<serde_json::Value>,
        title: Option<String>,
        session_id: Option<String>,
        chat_id: Option<String>,
        transport: Option<String>,
        local_node_id: &str,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
        handle_apply_surface_messages(
            current_identity.as_ref(),
            graph,
            &local_node_id,
            surface_id,
            messages,
            SurfaceAttribution {
                title,
                session_id,
                chat_id,
                transport,
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_get_surface(surface_id: String, graph: &GraphDomain) -> IpcResponse {
        match graph.get_surface(&surface_id) {
            Ok(Some(s)) => IpcResponse::success(
                "get_surface",
                Some(serde_json::to_value(&s).unwrap_or(serde_json::Value::Null)),
            ),
            Ok(None) => IpcResponse::error(
                "get_surface",
                "SURFACE_NOT_FOUND",
                format!("no surface {surface_id}"),
            ),
            Err(e) => IpcResponse::error("get_surface", "SURFACE_ERROR", e.to_string()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_list_surfaces(
        owner_agent_id: Option<String>,
        session_id: Option<String>,
        include_deleted: bool,
        limit: Option<usize>,
        graph: &GraphDomain,
    ) -> IpcResponse {
        match graph.list_surfaces(
            owner_agent_id.as_deref(),
            session_id.as_deref(),
            include_deleted,
            limit.unwrap_or(50).min(500),
        ) {
            Ok(list) => IpcResponse::success(
                "list_surfaces",
                Some(serde_json::json!({ "surfaces": list })),
            ),
            Err(e) => IpcResponse::error("list_surfaces", "SURFACE_ERROR", e.to_string()),
        }
    }
}
