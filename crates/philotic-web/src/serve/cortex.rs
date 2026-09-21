use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CortexQuery {
    vault: Option<String>,
    id: Option<String>,
    #[serde(default)]
    offset: u32,
}

pub(super) async fn read(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(query): Query<CortexQuery>,
) -> Response {
    let Some(session) = current_operator_session(&headers, &state) else {
        return unauthorized();
    };
    if session.posture != "admin" {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"Cortex browsing requires operator administrator authorization"})),
        )
            .into_response();
    }
    let result = async {
        let mut client = connect_client_with_identity(
            &state.socket,
            GuestIdentity {
                guest_id: "philotic-web-cortex".into(),
                role: "management".into(),
                supported_tools: vec![],
            },
        )
        .await?;
        client
            .send_request_with_timeout(
                IpcRequest::ReadCortex {
                    vault: query.vault,
                    id: query.id,
                    offset: query.offset,
                },
                Duration::from_secs(40),
            )
            .await
    }
    .await;
    // A long inventory read must not outlive the authority that initiated it.
    let still_authorized = current_operator_session(&headers, &state).is_some_and(|current| {
        current.session_id == session.session_id && current.posture == "admin"
    });
    let mut response = if !still_authorized {
        unauthorized()
    } else {
        match result {
        Ok(IpcResponse::Standard { ok: true, data: Some(data), .. }) => Json(data).into_response(),
        _ => (StatusCode::BAD_GATEWAY, Json(json!({"error":"Cortex unavailable. Connect to the Cortex hotel and check its memory configuration."}))).into_response(),
    }
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    eprintln!(
        "operator Cortex read session={} status={}",
        session.session_id,
        response.status().as_u16()
    );
    response
}
