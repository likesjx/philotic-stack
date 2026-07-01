use crate::vault::{SecretAccess, SecretInput, resolve_secret, store_secret};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::provider_keys::{ProviderKeySpec, provider_key_spec};
use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use clap::{Args, Subcommand};
use reqwest::Url;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};
use uuid::Uuid;

const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com";
const DEFAULT_OPENAI_MODEL: &str = "gpt-4.1-mini";
const DEFAULT_OPENAI_EMBEDDING_MODEL: &str = "text-embedding-3-small";
const DEFAULT_SCOPES: [&str; 2] = [
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/generative-language.retriever",
];

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    Google(AuthGoogleArgs),
    OpenAI(AuthOpenAIArgs),
}

#[derive(Debug, Args)]
pub struct AuthGoogleArgs {
    #[command(subcommand)]
    command: GoogleAuthCommand,
}

#[derive(Debug, Args)]
pub struct AuthOpenAIArgs {
    #[command(subcommand)]
    command: OpenAIAuthCommand,
}

#[derive(Debug, Subcommand)]
pub enum OpenAIAuthCommand {
    Start(OpenAIStartArgs),
    Validate(OpenAIValidateArgs),
}

#[derive(Debug, Clone, Args)]
pub struct OpenAIStartArgs {
    #[arg(long, default_value = "openai")]
    pub provider: String,

    #[arg(long, default_value = DEFAULT_OPENAI_BASE_URL)]
    pub base_url: String,

    #[arg(long)]
    pub api_key: Option<String>,

    #[arg(long)]
    pub project_id: Option<String>,

    #[arg(long, default_value = DEFAULT_OPENAI_MODEL)]
    pub model: String,

    #[arg(long, default_value = DEFAULT_OPENAI_EMBEDDING_MODEL)]
    pub embedding_model: String,
}

#[derive(Debug, Clone, Args)]
pub struct OpenAIValidateArgs {
    #[arg(long, default_value = "openai")]
    pub provider: String,

    #[arg(long, default_value = DEFAULT_OPENAI_BASE_URL)]
    pub base_url: String,

    #[arg(long, default_value = DEFAULT_OPENAI_MODEL)]
    pub model: String,

    #[arg(long, default_value = "Reply with exactly: openai-ok")]
    pub prompt: String,
}

#[derive(Debug, Subcommand)]
pub enum GoogleAuthCommand {
    Start(GoogleStartArgs),
    Validate(GoogleValidateArgs),
}

#[derive(Debug, Clone, Args)]
pub struct GoogleStartArgs {
    #[arg(long, default_value = "gemini")]
    pub provider: String,

    #[arg(long)]
    pub client_id: String,

    #[arg(long)]
    pub client_secret: Option<String>,

    #[arg(long)]
    pub project_id: Option<String>,

    #[arg(long, default_value_t = 300)]
    pub timeout_secs: u64,

    #[arg(long, default_value_t = false)]
    pub no_browser: bool,

    #[arg(long, default_value_t = false)]
    pub insecure_store_refresh_token: bool,

    #[arg(long, value_delimiter = ',', num_args = 0.., default_values_t = default_scopes())]
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct GoogleValidateArgs {
    #[arg(long, default_value = "gemini")]
    pub provider: String,

    #[arg(long, default_value = "gemini-2.5-flash")]
    pub model: String,

    #[arg(long, default_value = "Reply with exactly: oauth-ok")]
    pub prompt: String,
}

#[derive(Debug, Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
    scope: Option<String>,
    token_type: Option<String>,
}

#[derive(Debug)]
struct OAuthCallback {
    code: String,
}

pub async fn run_auth_command(command: AuthCommand) -> Result<()> {
    match command {
        AuthCommand::Google(AuthGoogleArgs {
            command: GoogleAuthCommand::Start(args),
        }) => run_google_auth_start(args).await,
        AuthCommand::Google(AuthGoogleArgs {
            command: GoogleAuthCommand::Validate(args),
        }) => run_google_auth_validate(args).await,
        AuthCommand::OpenAI(AuthOpenAIArgs {
            command: OpenAIAuthCommand::Start(args),
        }) => run_openai_auth_start(args).await,
        AuthCommand::OpenAI(AuthOpenAIArgs {
            command: OpenAIAuthCommand::Validate(args),
        }) => run_openai_auth_validate(args).await,
    }
}

async fn run_openai_auth_start(args: OpenAIStartArgs) -> Result<()> {
    if args.provider.trim().is_empty() {
        bail!("provider cannot be empty");
    }

    let base_url = resolve_openai_base_url(&args.base_url);
    let model = resolve_openai_model(&args.model);
    let embedding_model = resolve_openai_embedding_model(&args.embedding_model);
    let project_id = resolve_openai_project_id(&args.project_id);
    let spec = provider_key_spec(&args.provider).with_context(|| {
        format!(
            "provider key spec is not configured for '{}'",
            args.provider
        )
    })?;
    let api_key = resolve_provider_api_key(&args, spec).with_context(|| {
        format!(
            "{} API key is required; set --api-key or {}",
            spec.display_name, spec.env_api_key
        )
    })?;
    let graph = openai_graph()?;
    let domain = GraphDomain::new(Arc::new(graph.adapter()));
    let secret_ref = store_secret(
        &domain,
        SecretInput {
            secret_kind: spec.vault_name.to_string(),
            scope: "hotel".into(),
            allowed_roles: spec
                .allowed_roles
                .iter()
                .map(|role| role.to_string())
                .collect(),
            allowed_guests: Vec::new(),
            plaintext: api_key,
        },
    )?;

    domain.set_config_value(spec.api_key_ref_key, &serde_json::to_string(&secret_ref)?)?;
    if let Some(key) = spec.base_url_key {
        domain.set_config_value(key, &serde_json::to_string(&base_url)?)?;
    }
    if let Some(key) = spec.default_model_key {
        domain.set_config_value(key, &serde_json::to_string(&model)?)?;
    }
    if let Some(key) = spec.embedding_model_key {
        domain.set_config_value(key, &serde_json::to_string(&embedding_model)?)?;
    }
    if let Some(project_id) = project_id.as_deref() {
        domain.set_config_value("openai_project_id", &serde_json::to_string(project_id)?)?;
    }

    println!("{} API key stored in the hotel vault.", spec.display_name);
    println!("Provider: {}", spec.provider);
    println!("Base URL: {}", base_url);
    println!("Secret ref: {}", secret_ref);
    Ok(())
}

async fn run_openai_auth_validate(args: OpenAIValidateArgs) -> Result<()> {
    if args.provider.trim().is_empty() {
        bail!("provider cannot be empty");
    }

    let base_url = resolve_openai_base_url(&args.base_url);
    let model = resolve_openai_model(&args.model);
    let graph = openai_graph()?;
    let domain = GraphDomain::new(Arc::new(graph.adapter()));
    let api_key = load_openai_api_key(&domain)?;
    let project_id = load_openai_project_id(&domain)?;
    let response = reqwest::Client::new()
        .post(format!(
            "{}/v1/chat/completions",
            base_url.trim_end_matches('/')
        ))
        .bearer_auth(&api_key)
        .header("Content-Type", "application/json")
        .header(
            "OpenAI-Project",
            project_id
                .as_deref()
                .context("openai_project_id is required for OpenAI validation")?,
        )
        .json(&serde_json::json!({
            "model": model,
            "messages": [{
                "role": "user",
                "content": args.prompt
            }]
        }))
        .send()
        .await
        .context("failed to call OpenAI validation request")?;

    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .context("failed to decode OpenAI validation response")?;

    if !status.is_success() {
        bail!("OpenAI validation failed ({}): {}", status.as_u16(), body);
    }

    let content = body
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();

    println!("OpenAI validation succeeded.");
    println!("Provider: {}", args.provider);
    println!("Base URL: {}", base_url);
    println!("Response: {}", content);
    Ok(())
}

async fn run_google_auth_start(args: GoogleStartArgs) -> Result<()> {
    if args.provider != "gemini" {
        bail!(
            "unsupported provider [{}]; current hotel OAuth slice only handles gemini",
            args.provider
        );
    }

    let state = format!("google-oauth-{}", Uuid::new_v4().simple());
    let code_verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let code_challenge = pkce_code_challenge(&code_verifier);

    let listener = TcpListener::bind("127.0.0.1:0").context("failed to bind localhost callback")?;
    let redirect_uri = format!(
        "http://127.0.0.1:{}/oauth/google/callback",
        listener
            .local_addr()
            .context("failed to read localhost callback address")?
            .port()
    );

    let auth_url = build_google_auth_url(&args, &redirect_uri, &state, &code_challenge)?;
    let callback_rx = spawn_callback_listener(listener, state);

    info!("Google OAuth flow ready for provider [{}].", args.provider);
    println!("Open this URL to continue Google OAuth:\n{}", auth_url);

    if !args.no_browser {
        if let Err(err) = open_browser(&auth_url) {
            warn!("Failed to open browser automatically: {}", err);
            println!("Browser launch failed; open the URL above manually.");
        }
    }

    let callback = callback_rx
        .recv_timeout(Duration::from_secs(args.timeout_secs))
        .context("timed out waiting for Google OAuth callback")??;

    let token = exchange_google_code(&args, &redirect_uri, &code_verifier, &callback.code).await?;
    persist_gemini_oauth_config(&args, &token)?;

    println!("Google OAuth succeeded for provider gemini.");
    if args.insecure_store_refresh_token {
        println!(
            "Refresh token was stored in the hotel vault because --insecure-store-refresh-token was set."
        );
    } else {
        println!(
            "Refresh token was not stored. Current auth is access-token-only until refresh lifecycle and policy are wired."
        );
    }

    Ok(())
}

async fn run_google_auth_validate(args: GoogleValidateArgs) -> Result<()> {
    if args.provider != "gemini" {
        bail!(
            "unsupported provider [{}]; current hotel OAuth validation only handles gemini",
            args.provider
        );
    }

    let graph = ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open("aiua_context.db")?;
    let graph_domain = GraphDomain::new(Arc::new(graph.adapter()));
    let token = load_gemini_access_token(&graph_domain)?;
    let project_id = graph_domain
        .get_config_value("gemini_oauth_project_id")?
        .and_then(|value_json| serde_json::from_str::<String>(&value_json).ok());

    let response = reqwest::Client::new()
        .post(format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
            args.model
        ))
        .bearer_auth(&token)
        .header(
            "x-goog-user-project",
            project_id
                .as_deref()
                .context("gemini_oauth_project_id is required for Gemini OAuth validation")?,
        )
        .json(&serde_json::json!({
            "contents": [{
                "parts": [{"text": args.prompt}]
            }]
        }))
        .send()
        .await
        .context("failed to call Gemini validation request")?;

    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .context("failed to decode Gemini validation response")?;

    if !status.is_success() {
        bail!(
            "Gemini OAuth validation failed ({}): {}",
            status.as_u16(),
            body
        );
    }

    let content = body
        .get("candidates")
        .and_then(serde_json::Value::as_array)
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(serde_json::Value::as_array)
        .and_then(|parts| parts.first())
        .and_then(|part| part.get("text"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();

    println!("Gemini OAuth validation succeeded.");
    println!("Model: {}", args.model);
    println!("Response: {}", content);
    Ok(())
}

fn default_scopes() -> Vec<String> {
    DEFAULT_SCOPES
        .iter()
        .map(|scope| scope.to_string())
        .collect()
}

fn build_google_auth_url(
    args: &GoogleStartArgs,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
) -> Result<String> {
    let mut url = Url::parse(GOOGLE_AUTH_URL).context("failed to build Google auth URL")?;
    let scope = args.scopes.join(" ");
    url.query_pairs_mut()
        .append_pair("client_id", &args.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", &scope)
        .append_pair("state", state)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url.into())
}

fn pkce_code_challenge(code_verifier: &str) -> String {
    let digest = Sha256::digest(code_verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

fn spawn_callback_listener(
    listener: TcpListener,
    expected_state: String,
) -> mpsc::Receiver<Result<OAuthCallback>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = handle_callback(listener, &expected_state);
        let _ = tx.send(result);
    });
    rx
}

fn handle_callback(listener: TcpListener, expected_state: &str) -> Result<OAuthCallback> {
    let (mut stream, _) = listener.accept().context("failed to accept callback")?;
    let mut buf = [0u8; 8192];
    let read = stream
        .read(&mut buf)
        .context("failed to read callback request")?;
    let request = String::from_utf8_lossy(&buf[..read]);
    let request_line = request
        .lines()
        .next()
        .context("callback request missing request line")?;
    let path = request_line
        .split_whitespace()
        .nth(1)
        .context("callback request missing path")?;
    let url = Url::parse(&format!("http://localhost{}", path))
        .context("failed to parse callback query parameters")?;

    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }

    if let Some(error) = error {
        write_http_response(
            &mut stream,
            400,
            "Google OAuth failed. You can close this tab and return to the hotel CLI.",
        )?;
        bail!("Google OAuth callback returned error [{}]", error);
    }

    if state.as_deref() != Some(expected_state) {
        write_http_response(
            &mut stream,
            400,
            "Google OAuth state mismatch. You can close this tab and retry from the hotel CLI.",
        )?;
        bail!("Google OAuth callback state mismatch");
    }

    let code = code.context("Google OAuth callback missing authorization code")?;
    write_http_response(
        &mut stream,
        200,
        "Philotic captured the Google OAuth callback successfully. You can close this tab.",
    )?;
    Ok(OAuthCallback { code })
}

fn write_http_response(stream: &mut std::net::TcpStream, status: u16, body: &str) -> Result<()> {
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n<html><body><p>{}</p></body></html>",
        status,
        body.len() + 33,
        body
    );
    stream
        .write_all(response.as_bytes())
        .context("failed to write OAuth callback response")?;
    Ok(())
}

async fn exchange_google_code(
    args: &GoogleStartArgs,
    redirect_uri: &str,
    code_verifier: &str,
    code: &str,
) -> Result<GoogleTokenResponse> {
    let client = reqwest::Client::new();
    let mut form = vec![
        ("client_id", args.client_id.clone()),
        ("code", code.to_string()),
        ("code_verifier", code_verifier.to_string()),
        ("grant_type", "authorization_code".into()),
        ("redirect_uri", redirect_uri.to_string()),
    ];

    if let Some(client_secret) = resolve_client_secret(args) {
        form.push(("client_secret", client_secret.to_string()));
    }

    let response = client
        .post(GOOGLE_TOKEN_URL)
        .form(&form)
        .send()
        .await
        .context("failed to exchange Google OAuth code for tokens")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!(
            "Google OAuth token exchange failed ({}): {}",
            status.as_u16(),
            body.trim()
        );
    }

    response
        .json::<GoogleTokenResponse>()
        .await
        .context("failed to decode Google OAuth token response")
}

fn resolve_client_secret(args: &GoogleStartArgs) -> Option<String> {
    args.client_secret
        .as_deref()
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("GOOGLE_CLIENT_SECRET")
                .ok()
                .map(|secret| secret.trim().to_string())
                .filter(|secret| !secret.is_empty())
        })
}

fn persist_gemini_oauth_config(args: &GoogleStartArgs, token: &GoogleTokenResponse) -> Result<()> {
    let graph = ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open("aiua_context.db")?;
    let domain = GraphDomain::new(Arc::new(graph.adapter()));

    let access_token_ref = store_secret(
        &domain,
        SecretInput {
            secret_kind: "gemini-access-token".into(),
            scope: "hotel".into(),
            allowed_roles: vec!["model".into(), "model".into()],
            allowed_guests: Vec::new(),
            plaintext: token.access_token.clone(),
        },
    )?;
    domain.set_config_value(
        "gemini_oauth_access_token_ref",
        &serde_json::to_string(&access_token_ref)?,
    )?;

    if let Some(project_id) = args
        .project_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        domain.set_config_value(
            "gemini_oauth_project_id",
            &serde_json::to_string(project_id)?,
        )?;
    }

    if let Some(expires_in) = token.expires_in {
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            + expires_in;
        domain.set_config_value(
            "gemini_oauth_access_token_expires_at",
            &expires_at.to_string(),
        )?;
    }

    if let Some(scope) = token.scope.as_deref() {
        domain.set_config_value("gemini_oauth_scope", &serde_json::to_string(scope)?)?;
    }

    if let Some(token_type) = token.token_type.as_deref() {
        domain.set_config_value(
            "gemini_oauth_token_type",
            &serde_json::to_string(token_type)?,
        )?;
    }

    if let Some(refresh_token) = token.refresh_token.as_deref() {
        if args.insecure_store_refresh_token {
            let refresh_token_ref = store_secret(
                &domain,
                SecretInput {
                    secret_kind: "gemini-refresh-token".into(),
                    scope: "hotel".into(),
                    allowed_roles: vec!["aiua".into()],
                    allowed_guests: Vec::new(),
                    plaintext: refresh_token.to_string(),
                },
            )?;
            domain.set_config_value(
                "gemini_oauth_refresh_token_ref",
                &serde_json::to_string(&refresh_token_ref)?,
            )?;
        } else {
            warn!(
                "Received a Google refresh token but did not persist it; refresh lifecycle is not wired yet."
            );
        }
    }

    info!("Persisted Gemini OAuth secret refs into the hotel vault/context graph.");
    Ok(())
}

fn openai_graph() -> Result<ansible_mesh_core::sqlite_storage::SqliteGraphStorage> {
    ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open("aiua_context.db")
}

fn resolve_provider_api_key(args: &OpenAIStartArgs, spec: &ProviderKeySpec) -> Option<String> {
    args.api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var(spec.env_api_key)
                .ok()
                .map(|key| key.trim().to_string())
                .filter(|key| !key.is_empty())
        })
}

fn resolve_openai_base_url(base_url: &str) -> String {
    std::env::var("PHILOTIC_OPENAI_BASE_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| base_url.trim().to_string())
}

fn resolve_openai_model(model: &str) -> String {
    std::env::var("PHILOTIC_OPENAI_MODEL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| model.trim().to_string())
}

fn resolve_openai_embedding_model(model: &str) -> String {
    std::env::var("PHILOTIC_OPENAI_EMBEDDING_MODEL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| model.trim().to_string())
}

fn resolve_openai_project_id(project_id: &Option<String>) -> Option<String> {
    project_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("PHILOTIC_OPENAI_PROJECT_ID")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
}

fn load_openai_api_key(graph: &GraphDomain) -> Result<String> {
    if let Some(value) = std::env::var("PHILOTIC_OPENAI_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Ok(value);
    }

    let spec = provider_key_spec("openai").context("OpenAI provider key spec missing")?;
    let secret_ref = std::env::var(spec.env_api_key_ref)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            graph
                .get_config_value(spec.api_key_ref_key)
                .ok()
                .flatten()
                .and_then(|value_json| serde_json::from_str::<String>(&value_json).ok())
        })
        .context("openai_api_key_ref is not configured")?;

    resolve_secret(
        graph,
        &secret_ref,
        &SecretAccess {
            role: "model".into(),
            guest_id: "ansible-auth-validator".into(),
        },
    )?
    .context("OpenAI API key secret could not be resolved")
}

fn load_openai_project_id(graph: &GraphDomain) -> Result<Option<String>> {
    Ok(graph
        .get_config_value("openai_project_id")?
        .and_then(|value_json| serde_json::from_str::<String>(&value_json).ok())
        .filter(|value| !value.trim().is_empty()))
}

fn load_gemini_access_token(graph: &GraphDomain) -> Result<String> {
    if let Some(value_json) = graph.get_config_value("gemini_oauth_access_token")? {
        return serde_json::from_str::<String>(&value_json)
            .context("failed to decode legacy gemini_oauth_access_token");
    }

    let secret_ref = graph
        .get_config_value("gemini_oauth_access_token_ref")?
        .and_then(|value_json| serde_json::from_str::<String>(&value_json).ok())
        .context("gemini_oauth_access_token_ref is not configured")?;

    resolve_secret(
        graph,
        &secret_ref,
        &SecretAccess {
            role: "model".into(),
            guest_id: "ansible-auth-validator".into(),
        },
    )?
    .context("Gemini OAuth access-token secret could not be resolved")
}

fn open_browser(url: &str) -> Result<()> {
    let status = if cfg!(target_os = "macos") {
        Command::new("open").arg(url).status()
    } else if cfg!(target_os = "windows") {
        Command::new("cmd").args(["/C", "start", url]).status()
    } else {
        Command::new("xdg-open").arg(url).status()
    }
    .context("failed to spawn browser opener")?;

    if !status.success() {
        bail!("browser opener exited with status {}", status);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        GoogleStartArgs, GoogleValidateArgs, build_google_auth_url, default_scopes,
        pkce_code_challenge,
    };

    #[test]
    fn pkce_code_challenge_is_url_safe() {
        let challenge = pkce_code_challenge("verifier-123");
        assert!(!challenge.contains('='));
        assert!(!challenge.contains('+'));
        assert!(!challenge.contains('/'));
    }

    #[test]
    fn auth_url_contains_expected_query_fields() {
        let args = GoogleStartArgs {
            provider: "gemini".into(),
            client_id: "client-123".into(),
            client_secret: None,
            project_id: Some("project-123".into()),
            timeout_secs: 60,
            no_browser: true,
            insecure_store_refresh_token: false,
            scopes: vec![
                "https://www.googleapis.com/auth/cloud-platform".into(),
                "https://www.googleapis.com/auth/generative-language.retriever".into(),
            ],
        };

        let url = build_google_auth_url(
            &args,
            "http://127.0.0.1:5555/oauth/google/callback",
            "state-123",
            "challenge-123",
        )
        .unwrap();

        assert!(url.contains("client_id=client-123"));
        assert!(
            url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5555%2Foauth%2Fgoogle%2Fcallback")
        );
        assert!(url.contains("code_challenge=challenge-123"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
    }

    #[test]
    fn default_validate_args_are_gemini_shaped() {
        let args = GoogleValidateArgs {
            provider: "gemini".into(),
            model: "gemini-2.5-flash".into(),
            prompt: "Reply with exactly: oauth-ok".into(),
        };

        assert_eq!(args.provider, "gemini");
        assert_eq!(args.model, "gemini-2.5-flash");
        assert_eq!(default_scopes().len(), 2);
    }
}
