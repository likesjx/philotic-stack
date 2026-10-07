//! Surface web renderer routes (doc:desktop-generative-surfaces, seam
//! surface-web-renderer).
//!
//! - `GET /s/:target/:surface_id`: a static HTML shell. It carries no surface
//!   data, so the edge fence may serve it unauthenticated; the page fetches
//!   the data from the authenticated API below.
//! - `GET /surface-ui/{surface.js,surface.css,catalog.json}`: the renderer and
//!   the `philotic.desktop.v1` catalog the hotel validator also enforces.
//! - `GET /api/mesh/targets/:target_node_id/surfaces/:surface_id`: one surface
//!   record from any mesh hotel (`QueryOperatorTargetSurface`), behind
//!   `check_auth`. `:target` may be a node id or a hotel name.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use philotic_client::{IpcRequest, IpcResponse};
use serde_json::{json, Value};

use super::AppState;

const SURFACE_HTML: &str = include_str!("../../surface-ui/surface.html");
const SURFACE_JS: &str = include_str!("../../surface-ui/surface.js");
const SURFACE_CSS: &str = include_str!("../../surface-ui/surface.css");

/// Scripts, styles and data only from this origin; no inline script, no
/// frames, no forms, no remote images. The renderer never needs more.
const SURFACE_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
     connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; \
     frame-ancestors 'none'";

fn hardened(mut response: Response, content_type: &'static str) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(SURFACE_CSP),
    );
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

pub(super) async fn handle_surface_page(
    Path((_target, _surface_id)): Path<(String, String)>,
) -> Response {
    hardened(SURFACE_HTML.into_response(), "text/html; charset=utf-8")
}

pub(super) async fn handle_surface_js() -> Response {
    hardened(SURFACE_JS.into_response(), "text/javascript; charset=utf-8")
}

pub(super) async fn handle_surface_css() -> Response {
    hardened(SURFACE_CSS.into_response(), "text/css; charset=utf-8")
}

pub(super) async fn handle_surface_catalog() -> Response {
    hardened(
        ansible_mesh_core::surface::CATALOG_JSON.into_response(),
        "application/json",
    )
}

pub(super) async fn handle_mesh_target_surface(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path((target, surface_id)): Path<(String, String)>,
) -> Response {
    if !super::check_auth(&headers, &state) {
        return super::unauthorized();
    }
    let target_node_id = super::edge::resolve_target_node_id(&state, &target).await;
    match query_target_surface(&state.socket, &target_node_id, &surface_id).await {
        Ok(Some(record)) => Json(super::surface_view(record)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("no surface {surface_id} on {target_node_id}")})),
        )
            .into_response(),
        Err(TargetSurfaceError::UnknownTarget(message)) => {
            (StatusCode::NOT_FOUND, Json(json!({"error": message}))).into_response()
        }
        Err(TargetSurfaceError::Failed(message)) => {
            (StatusCode::BAD_GATEWAY, Json(json!({"error": message}))).into_response()
        }
    }
}

enum TargetSurfaceError {
    UnknownTarget(String),
    Failed(String),
}

async fn query_target_surface(
    socket: &str,
    target_node_id: &str,
    surface_id: &str,
) -> Result<Option<Value>, TargetSurfaceError> {
    let mut client = super::connect_management_client(socket, "philotic-web-target-surface")
        .await
        .map_err(|e| TargetSurfaceError::Failed(e.to_string()))?;
    let response = client
        .send_request(IpcRequest::QueryOperatorTargetSurface {
            target_node_id: target_node_id.to_string(),
            surface_id: surface_id.to_string(),
        })
        .await
        .map_err(|e| TargetSurfaceError::Failed(e.to_string()))?;
    surface_from_response(response)
}

fn surface_from_response(response: IpcResponse) -> Result<Option<Value>, TargetSurfaceError> {
    match response {
        IpcResponse::Standard {
            ok: true,
            data: Some(data),
            ..
        } => Ok(data
            .get("found")
            .and_then(Value::as_bool)
            .filter(|found| *found)
            .and_then(|_| data.get("surface").cloned())),
        IpcResponse::Standard { message, .. } if message.contains("not currently active") => {
            Err(TargetSurfaceError::UnknownTarget(message))
        }
        IpcResponse::Standard { message, .. } => Err(TargetSurfaceError::Failed(message)),
        other => Err(TargetSurfaceError::Failed(format!(
            "unexpected surface response: {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_carries_no_inline_script_and_loads_the_renderer() {
        assert!(SURFACE_HTML.contains(r#"<script src="/surface-ui/surface.js" defer></script>"#));
        // Exactly one script tag, and it has a src (CSP forbids inline script).
        assert_eq!(SURFACE_HTML.matches("<script").count(), 1);
    }

    #[test]
    fn renderer_never_writes_html_or_evaluates_code() {
        for forbidden in [
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
            "eval(",
            "new Function",
        ] {
            assert!(!SURFACE_JS.contains(forbidden), "renderer uses {forbidden}");
        }
    }

    #[test]
    fn renderer_covers_every_catalog_component() {
        let catalog: Value =
            serde_json::from_str(ansible_mesh_core::surface::CATALOG_JSON).unwrap();
        for name in catalog["components"].as_object().unwrap().keys() {
            assert!(
                SURFACE_JS.contains(&format!("case \"{name}\"")),
                "renderer has no case for catalog component {name}"
            );
        }
    }

    #[tokio::test]
    async fn static_routes_send_strict_csp() {
        let response = handle_surface_js().await;
        let csp = response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(csp.contains("script-src 'self'"));
        assert!(csp.contains("frame-ancestors 'none'"));
        let page = handle_surface_page(Path(("mac-jane".into(), "s1".into()))).await;
        assert_eq!(
            page.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
    }

    #[test]
    fn surface_reply_maps_found_missing_and_unknown_targets() {
        let found = IpcResponse::success(
            "operator_target_surface",
            Some(json!({"target_node_id": "n", "found": true, "surface": {"surface_id": "s1"}})),
        );
        assert_eq!(
            surface_from_response(found).ok().flatten().unwrap()["surface_id"],
            "s1"
        );
        let missing = IpcResponse::success(
            "operator_target_surface",
            Some(json!({"target_node_id": "n", "found": false, "surface": null})),
        );
        assert!(surface_from_response(missing).ok().unwrap().is_none());
        let unknown = IpcResponse::error(
            "operator_target_surface",
            "OPERATOR_TARGET_SURFACE_ERROR",
            "mesh target [x] is not currently active in the registry",
        );
        assert!(matches!(
            surface_from_response(unknown),
            Err(TargetSurfaceError::UnknownTarget(_))
        ));
    }
}
