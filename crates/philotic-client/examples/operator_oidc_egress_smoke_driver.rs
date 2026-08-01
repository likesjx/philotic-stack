use ansible_mesh_core::integration::OidcExchangeResponse;
use anyhow::{bail, Context, Result};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Duration;

const CLIENT_SECRET: &str = "smoke-client-secret";
const ACCESS_TOKEN: &str = "smoke-access-token";
const AUTHORIZATION_CODE: &str = "smoke-authorization-code";
const CODE_VERIFIER: &str = "smoke-pkce-verifier-0123456789012345678901234567890123456789";

#[tokio::main]
async fn main() -> Result<()> {
    let token_listener = TcpListener::bind("127.0.0.1:0").await?;
    let token_address = token_listener.local_addr()?;
    let userinfo_listener = TcpListener::bind("127.0.0.1:0").await?;
    let userinfo_address = userinfo_listener.local_addr()?;

    let token_server = tokio::spawn(async move {
        let (mut stream, _) = token_listener.accept().await?;
        let request = read_http_request(&mut stream).await?;
        let lower = request.to_ascii_lowercase();
        if !lower.starts_with("post /token http/1.1\r\n") {
            bail!("unexpected token request: {request}");
        }
        for expected in [
            "client_id=smoke-client-id",
            "client_secret=smoke-client-secret",
            "code=smoke-authorization-code",
            "code_verifier=smoke-pkce-verifier",
            "grant_type=authorization_code",
        ] {
            if !request.contains(expected) {
                bail!("token request omitted {expected}: {request}");
            }
        }
        write_json_response(
            &mut stream,
            &json!({
                "access_token": ACCESS_TOKEN,
                "token_type": "Bearer",
                "refresh_token": "must-never-cross-runner-boundary"
            })
            .to_string(),
        )
        .await
    });

    let userinfo_server = tokio::spawn(async move {
        let (mut stream, _) = userinfo_listener.accept().await?;
        let request = read_http_request(&mut stream).await?;
        let lower = request.to_ascii_lowercase();
        if !lower.starts_with("get /userinfo http/1.1\r\n")
            || !lower.contains("\r\nauthorization: bearer smoke-access-token\r\n")
        {
            bail!("userinfo request did not carry the runner-local access token: {request}");
        }
        write_json_response(
            &mut stream,
            &json!({
                "sub": "smoke-operator-subject",
                "name": "Smoke Operator",
                "email": "smoke@example.invalid"
            })
            .to_string(),
        )
        .await
    });

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: "philotic-web-oidc".into(),
        role: "management".into(),
        supported_tools: Vec::new(),
    })
    .await
    .context("connect operator OIDC smoke to hotel")?;

    set_config(&mut client, "oidc_google_client_id", "smoke-client-id").await?;
    set_config(
        &mut client,
        "smoke_oidc_google_token_url",
        &format!("http://{token_address}/token"),
    )
    .await?;
    set_config(
        &mut client,
        "smoke_oidc_google_userinfo_url",
        &format!("http://{userinfo_address}/userinfo"),
    )
    .await?;
    let secret_ref = match client
        .send_request(IpcRequest::AddVaultEntry {
            vault_name: "operator-oidc-smoke".into(),
            plaintext: CLIENT_SECRET.into(),
            allowed_roles: vec!["egress-http-runner".into()],
            // No consumer filters this smoke vault by kind, so keep the
            // pre-DEF-065 default: store it under the vault name.
            secret_kind: None,
        })
        .await?
    {
        IpcResponse::Standard {
            ok: true,
            data: Some(data),
            ..
        } => data
            .get("secret_ref")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .context("vault response omitted secret_ref")?,
        other => bail!("failed to provision OIDC smoke secret: {other:?}"),
    };
    set_config(&mut client, "oidc_google_client_secret_ref", &secret_ref).await?;

    let response = client
        .send_request_with_timeout(
            IpcRequest::ExchangeOperatorOidc {
                provider: "google".into(),
                authorization_code: AUTHORIZATION_CODE.into(),
                code_verifier: CODE_VERIFIER.into(),
                redirect_uri: "http://127.0.0.1/auth/oidc/google/callback".into(),
            },
            Duration::from_secs(30),
        )
        .await?;
    let exchange: OidcExchangeResponse = match response {
        IpcResponse::Standard {
            ok: true,
            data: Some(data),
            ..
        } => serde_json::from_value(data).context("decode OIDC exchange response")?,
        other => bail!("operator OIDC exchange failed: {other:?}"),
    };
    if exchange.userinfo["sub"] != "smoke-operator-subject" || exchange.audits.len() != 2 {
        bail!("unexpected sanitized OIDC exchange response: {exchange:?}");
    }
    let serialized = serde_json::to_string(&exchange)?;
    if serialized.contains(ACCESS_TOKEN) || serialized.contains("must-never-cross") {
        bail!("OIDC token material escaped the runner boundary");
    }

    token_server.await.context("token server task panicked")??;
    userinfo_server
        .await
        .context("userinfo server task panicked")??;

    match client
        .send_request(IpcRequest::GetIntegrationAudit {
            binding_id: Some("operator-oidc-google".into()),
            limit: Some(10),
        })
        .await?
    {
        IpcResponse::IntegrationAuditState { integration_audits }
            if integration_audits.len() >= 2
                && integration_audits
                    .iter()
                    .any(|audit| audit.path == "/token" && audit.outcome == "oidc_token_http_200")
                && integration_audits
                    .iter()
                    .any(|audit| {
                        audit.path == "/userinfo" && audit.outcome == "oidc_userinfo_http_200"
                    }) => {}
        other => bail!("durable OIDC audits were not observable: {other:?}"),
    }

    println!(
        "operator OIDC egress smoke ok: web IPC -> local runner -> token -> userinfo -> claims-only response + durable audits"
    );
    Ok(())
}

async fn set_config(client: &mut PhiloticClient, key: &str, value: &str) -> Result<()> {
    match client
        .send_request(IpcRequest::SetConfig {
            key: key.into(),
            value_json: serde_json::to_string(value)?,
        })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        other => bail!("setting config '{key}' failed: {other:?}"),
    }
}

async fn read_http_request(stream: &mut TcpStream) -> Result<String> {
    let mut request = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..count]);
        let Some(headers_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let body_start = headers_end + 4;
        let headers = String::from_utf8_lossy(&request[..headers_end]).to_ascii_lowercase();
        let content_length = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or_default();
        if request.len() >= body_start + content_length {
            break;
        }
    }
    String::from_utf8(request).context("HTTP request was not UTF-8")
}

async fn write_json_response(stream: &mut TcpStream, body: &str) -> Result<()> {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}
