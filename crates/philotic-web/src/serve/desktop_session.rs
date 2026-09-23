//! Confidential gateway attestation -> hotel-owned, non-root operator session.
//! The gateway verifies website invitation and current Mongo admin authority on
//! every request. This separate credential must never reach browser config.
use super::*;
use sha2::{Digest, Sha256};

const GATEWAY_HEADER: &str = "x-philotic-desktop-gateway";

pub(super) async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(session) = current_operator_session(&headers, &state) else {
        return unauthorized();
    };
    if session.auth_method != "desktop_gateway" {
        return unauthorized();
    }
    let mut response =
        Json(json!({"active":true,"user_id":session.user_id,"expires_at":session.expires_at}))
            .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(super) fn gateway_authorized(headers: &HeaderMap, state: &AppState) -> bool {
    let Some(key) = state.desktop_gateway_key.as_ref() else {
        return false;
    };
    headers
        .get(GATEWAY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(|candidate| constant_time_eq(candidate.as_bytes(), key.as_bytes()))
        .unwrap_or(false)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Admission {
    provider: String,
    provider_id: String,
    hotel: String,
    exchange_id: String,
    expires_at: i64,
}

pub(super) async fn issue(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Admission>,
) -> Response {
    if !gateway_authorized(&headers, &state) {
        return unauthorized();
    }
    match issue_session(&state.db_path, &state.hotel, &body, now_epoch_secs()) {
        Ok(session) => {
            let mut response = Json(json!({"token": session.session_token,
                "user_id": session.user_id, "expires_at": session.expires_at}))
            .into_response();
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(_) => (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"Desktop admission denied"})),
        )
            .into_response(),
    }
}

fn issue_session(
    db: &PathBuf,
    hotel: &str,
    body: &Admission,
    now: i64,
) -> Result<OperatorSessionRecord> {
    anyhow::ensure!(
        body.hotel == hotel && matches!(body.provider.as_str(), "google" | "github"),
        "invalid audience"
    );
    anyhow::ensure!(
        !body.provider_id.is_empty()
            && body.provider_id.len() <= 512
            && !body.provider_id.chars().any(char::is_control),
        "invalid subject"
    );
    anyhow::ensure!(
        body.exchange_id.len() == 43
            && body
                .exchange_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "invalid exchange"
    );
    anyhow::ensure!(
        body.expires_at > now && body.expires_at <= now + 900,
        "invalid expiry"
    );
    ensure_operator_auth_tables(db, hotel)?;
    let mut conn = Connection::open(db)?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS desktop_session_exchanges (exchange_id TEXT PRIMARY KEY, expires_at INTEGER NOT NULL)")?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute(
        "DELETE FROM desktop_session_exchanges WHERE expires_at <= ?1",
        [now],
    )?;
    tx.execute(
        "INSERT INTO desktop_session_exchanges(exchange_id, expires_at) VALUES (?1,?2)",
        rusqlite::params![body.exchange_id, body.expires_at],
    )?;
    let digest = hex::encode(Sha256::digest(serde_json::to_vec(&(
        hotel,
        &body.provider,
        &body.provider_id,
    ))?));
    let user_id = format!("desktop-user-{digest}");
    let display_name = format!("{} operator", body.provider);
    // Never silently remap a legacy root identity to a new principal.
    let linked: Option<String> = tx
        .query_row(
            "SELECT user_id FROM external_identity_links WHERE provider=?1 AND provider_subject=?2",
            rusqlite::params![body.provider, body.provider_id],
            |row| row.get(0),
        )
        .optional()?;
    anyhow::ensure!(
        linked.as_ref().is_none_or(|existing| existing == &user_id),
        "identity already mapped"
    );
    tx.execute("INSERT INTO operator_users(user_id,mesh_principal_id,display_name,home_hotel,status,onboarding_state,created_at,updated_at)
        VALUES (?1,?2,?3,?4,'active','invited',?5,?5) ON CONFLICT(user_id) DO NOTHING",
        rusqlite::params![user_id, format!("desktop-principal-{digest}"), display_name, hotel, now])?;
    let status: String = tx.query_row(
        "SELECT status FROM operator_users WHERE user_id=?1",
        [&user_id],
        |row| row.get(0),
    )?;
    anyhow::ensure!(status == "active", "hotel principal disabled");
    tx.execute("INSERT INTO external_identity_links(link_id,user_id,provider,provider_subject,display_name,verified_at,last_seen_at,created_at,updated_at)
        VALUES (?1,?2,?3,?4,?5,?6,?6,?6,?6) ON CONFLICT(provider,provider_subject) DO UPDATE SET last_seen_at=excluded.last_seen_at,updated_at=excluded.updated_at",
        rusqlite::params![format!("desktop-link-{digest}"), user_id, body.provider, body.provider_id, display_name, now])?;
    let session = OperatorSessionRecord {
        session_id: new_operator_chat_id("desktop-session"),
        session_token: new_secret_token("operator-token"),
        user_id,
        display_name,
        issuing_hotel: hotel.into(),
        surface_kind: "desktop_gateway".into(),
        posture: "admin".into(),
        issued_at: now,
        expires_at: body.expires_at,
        status: "active".into(),
        auth_method: "desktop_gateway".into(),
        bootstrap_id: Some(body.exchange_id.clone()),
    };
    tx.execute("INSERT INTO operator_sessions(session_id,session_token,user_id,display_name,issuing_hotel,surface_kind,posture,issued_at,expires_at,status,auth_method,bootstrap_id)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)", rusqlite::params![session.session_id,session.session_token,session.user_id,session.display_name,session.issuing_hotel,session.surface_kind,session.posture,session.issued_at,session.expires_at,session.status,session.auth_method,session.bootstrap_id])?;
    tx.commit()?;
    sync_projected_user_identity(db, hotel, &session.user_id)?;
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn admission(now: i64) -> Admission {
        Admission {
            provider: "google".into(),
            provider_id: "fixture".into(),
            hotel: "test-hotel".into(),
            exchange_id: "a".repeat(43),
            expires_at: now + 60,
        }
    }
    #[test]
    fn desktop_session_is_non_root_single_use_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("test.db");
        let now = now_epoch_secs();
        let mut body = admission(now);
        let first = issue_session(&db, "test-hotel", &body, now).unwrap();
        assert_ne!(first.user_id, default_operator_user_id("test-hotel"));
        assert_eq!(first.expires_at, now + 60);
        assert!(issue_session(&db, "test-hotel", &body, now).is_err());
        body.exchange_id = "b".repeat(43);
        let second = issue_session(&db, "test-hotel", &body, now).unwrap();
        assert_eq!(first.user_id, second.user_id);
        body.provider = "github".into();
        body.exchange_id = "c".repeat(43);
        assert_ne!(
            issue_session(&db, "test-hotel", &body, now)
                .unwrap()
                .user_id,
            first.user_id
        );
        body.expires_at = now + 901;
        assert!(issue_session(&db, "test-hotel", &body, now).is_err());
        body.expires_at = now;
        assert!(issue_session(&db, "test-hotel", &body, now).is_err());
    }

    fn state(db: PathBuf) -> AppState {
        let (tx, _) = broadcast::channel(4);
        AppState {
            bootstrap_token: Arc::new("test-only".into()),
            desktop_gateway_key: Some(Arc::new("k".repeat(32))),
            config_path: Arc::new(db.with_extension("config")),
            hotel: Arc::new("test-hotel".into()),
            socket: Arc::new("/nonexistent/test.sock".into()),
            tx,
            edge_token: None,
            exposure_tier: ExposureTier::Mesh,
            edge: edge::EdgeState::load(db.with_extension("edge"), None),
            db_path: db,
        }
    }

    #[test]
    fn desktop_token_requires_gateway_and_active_hotel_user() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("test.db");
        let state = state(db.clone());
        let now = now_epoch_secs();
        let session = issue_session(&db, "test-hotel", &admission(now), now).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", session.session_token).parse().unwrap(),
        );
        assert!(!check_auth(&headers, &state));
        headers.insert(GATEWAY_HEADER, "wrong".parse().unwrap());
        assert!(!check_auth(&headers, &state));
        headers.insert(GATEWAY_HEADER, "k".repeat(32).parse().unwrap());
        assert!(check_auth(&headers, &state));
        let mut disabled = state.clone();
        disabled.desktop_gateway_key = None;
        assert!(!check_auth(&headers, &disabled));
        Connection::open(&db)
            .unwrap()
            .execute(
                "UPDATE operator_users SET status='disabled' WHERE user_id=?1",
                [&session.user_id],
            )
            .unwrap();
        assert!(!check_auth(&headers, &state));
    }

    #[tokio::test]
    async fn desktop_issuer_http_requires_secret_and_rejects_replay() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path().join("http.db"));
        let app = Router::new()
            .route("/internal/desktop/session", post(issue))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/internal/desktop/session",
            listener.local_addr().unwrap()
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::default();
        let body = json!({"provider":"google","provider_id":"test","hotel":"test-hotel","exchange_id":"x".repeat(43),"expires_at":now_epoch_secs()+60});
        assert_eq!(
            client.post(&url).json(&body).send().await.unwrap().status(),
            401
        );
        assert_eq!(
            client
                .post(&url)
                .header(GATEWAY_HEADER, "wrong")
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        let response = client
            .post(&url)
            .header(GATEWAY_HEADER, "k".repeat(32))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert!(response.headers().get(header::SET_COOKIE).is_none());
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        assert_eq!(
            client
                .post(&url)
                .header(GATEWAY_HEADER, "k".repeat(32))
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        task.abort();
    }

    #[test]
    fn desktop_identity_conflict_preserves_existing_root_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("conflict.db");
        ensure_operator_auth_tables(&db, "test-hotel").unwrap();
        let conn = Connection::open(&db).unwrap();
        let root = default_operator_user_id("test-hotel");
        conn.execute("INSERT INTO external_identity_links(link_id,user_id,provider,provider_subject,verified_at,last_seen_at,created_at,updated_at)
            VALUES ('legacy',?1,'google','fixture',0,0,0,0)", [&root]).unwrap();
        let now = now_epoch_secs();
        assert!(issue_session(&db, "test-hotel", &admission(now), now).is_err());
        let owner: String = conn
            .query_row(
                "SELECT user_id FROM external_identity_links WHERE link_id='legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(owner, root);
        let sessions: i64 = conn
            .query_row("SELECT COUNT(*) FROM operator_sessions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(sessions, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires DESKTOP_GATEWAY_SMOKE_SCRIPT from the separate desktop checkout"]
    async fn desktop_real_gateway_integration() {
        let script = std::env::var("DESKTOP_GATEWAY_SMOKE_SCRIPT")
            .expect("desktop smoke script path required");
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path().join("integration.db"));
        async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
            match current_operator_session(&headers, &state) {
                Some(session) => Json(json!({"user_id":session.user_id})).into_response(),
                None => unauthorized(),
            }
        }
        let app = Router::new()
            .route("/internal/desktop/session", post(issue))
            .route("/internal/desktop/session/status", get(super::status))
            .route("/api/auth/status", get(status))
            .route("/api/auth/logout", post(handle_auth_logout))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let result = tokio::process::Command::new("node")
            .arg(script)
            .env("DESKTOP_TEST_HOTEL_URL", url)
            .env("DESKTOP_TEST_HOTEL_KEY", "k".repeat(32))
            .output()
            .await
            .unwrap();
        task.abort();
        assert!(
            result.status.success(),
            "gateway integration failed: {} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
