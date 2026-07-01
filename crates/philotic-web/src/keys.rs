use ansible_mesh_core::provider_keys::{provider_key_spec, provider_key_specs, ProviderKeySpec};
use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use inquire::{Password, Text};
use philotic_client::{IpcRequest, IpcResponse, PhiloticClient};

use crate::start::socket_path;

#[derive(Subcommand, Debug)]
pub enum KeysAction {
    /// Configure an API-key-backed model provider.
    Configure {
        /// Provider to configure. Currently supports gemini, openrouter, openai, and elevenlabs.
        provider: String,

        /// API key. Omit to enter it interactively without echoing it.
        #[arg(long)]
        api_key: Option<String>,

        /// Default model for text generation.
        #[arg(long)]
        model: Option<String>,

        /// Comma-separated fallback model list. OpenRouter only for now.
        #[arg(long, value_delimiter = ',')]
        fallback_model: Vec<String>,

        /// Provider routing mode. OpenRouter defaults to fallback when fallback models are set.
        #[arg(long)]
        route: Option<String>,

        /// Base URL override.
        #[arg(long)]
        base_url: Option<String>,
    },

    /// Show provider config/key status without exposing secret values.
    Status {
        /// Optional provider filter.
        provider: Option<String>,
    },
}

pub async fn run(action: KeysAction) -> Result<()> {
    match action {
        KeysAction::Configure {
            provider,
            api_key,
            model,
            fallback_model,
            route,
            base_url,
        } => configure(provider, api_key, model, fallback_model, route, base_url).await,
        KeysAction::Status { provider } => status(provider).await,
    }
}

async fn configure(
    provider: String,
    api_key: Option<String>,
    model: Option<String>,
    fallback_models: Vec<String>,
    route: Option<String>,
    base_url: Option<String>,
) -> Result<()> {
    let spec =
        provider_key_spec(&provider).ok_or_else(|| anyhow!("unsupported provider '{provider}'"))?;
    let api_key = match api_key {
        Some(value) if !value.trim().is_empty() => value,
        _ => Password::new(&format!("{} API key", spec.display_name))
            .without_confirmation()
            .prompt()
            .context("read API key")?,
    };
    if api_key.trim().is_empty() {
        anyhow::bail!("API key cannot be empty");
    }

    let model = optional_prompt("Default model", model, spec.default_model)?;
    let base_url = optional_prompt("Base URL", base_url, spec.default_base_url)?;
    let route = optional_prompt(
        "Route",
        route,
        default_route(spec, fallback_models.is_empty()),
    )?;

    let mut client = ipc_client().await?;
    let secret_ref = add_vault_entry(
        &mut client,
        spec.vault_name,
        api_key,
        spec.allowed_roles
            .iter()
            .map(|role| role.to_string())
            .collect(),
    )
    .await?;

    set_config(
        &mut client,
        spec.api_key_ref_key,
        serde_json::json!(secret_ref),
    )
    .await?;
    if let (Some(key), Some(model)) = (spec.default_model_key, model) {
        set_config(&mut client, key, serde_json::json!(model)).await?;
    }
    if let (Some(key), Some(base_url)) = (spec.base_url_key, base_url) {
        set_config(&mut client, key, serde_json::json!(base_url)).await?;
    }
    if let Some(route_key) = spec.route_key {
        if let Some(route) = route {
            set_config(&mut client, route_key, serde_json::json!(route)).await?;
        }
    }
    if let Some(fallback_key) = spec.fallback_models_key {
        let fallback_models: Vec<String> = fallback_models
            .into_iter()
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty())
            .collect();
        if !fallback_models.is_empty() {
            set_config(
                &mut client,
                fallback_key,
                serde_json::json!(fallback_models),
            )
            .await?;
        }
    }

    println!("{} key stored in the hotel vault.", spec.display_name);
    println!("  config ref: {}", spec.api_key_ref_key);
    println!("  secret ref: {}", secret_ref);
    println!("  secret value: hidden");
    Ok(())
}

async fn status(provider: Option<String>) -> Result<()> {
    let mut client = ipc_client().await?;
    let specs = provider_key_specs();
    println!(
        "{:<12} {:<10} {:<28} {}",
        "PROVIDER", "KEY", "DEFAULT MODEL", "SECRET REF"
    );
    println!("{}", "-".repeat(92));
    let filter_provider = provider
        .as_deref()
        .map(|filter| {
            provider_key_spec(filter)
                .map(|spec| spec.provider)
                .ok_or_else(|| anyhow!("unsupported provider '{filter}'"))
        })
        .transpose()?;
    for spec in specs {
        if filter_provider.is_some() && Some(spec.provider) != filter_provider {
            continue;
        }
        let secret_ref = get_config_string(&mut client, spec.api_key_ref_key).await?;
        let model = if let Some(key) = spec.default_model_key {
            get_config_string(&mut client, key).await?
        } else {
            None
        };
        println!(
            "{:<12} {:<10} {:<28} {}",
            spec.provider,
            if secret_ref.is_some() {
                "set"
            } else {
                "missing"
            },
            model.unwrap_or_else(|| "-".into()),
            secret_ref.unwrap_or_else(|| "-".into())
        );
    }
    Ok(())
}

fn default_route(spec: &ProviderKeySpec, fallback_models_empty: bool) -> Option<&'static str> {
    if spec.provider == "openrouter" && !fallback_models_empty {
        Some("fallback")
    } else {
        None
    }
}

fn optional_prompt(
    label: &str,
    provided: Option<String>,
    default: Option<&str>,
) -> Result<Option<String>> {
    if let Some(value) = provided {
        let trimmed = value.trim().to_string();
        return Ok((!trimmed.is_empty()).then_some(trimmed));
    }
    let Some(default) = default else {
        return Ok(None);
    };
    let answer = Text::new(label)
        .with_default(default)
        .prompt()
        .with_context(|| format!("read {label}"))?;
    let trimmed = answer.trim().to_string();
    Ok((!trimmed.is_empty()).then_some(trimmed))
}

async fn ipc_client() -> Result<PhiloticClient> {
    let socket = socket_path("aiua");
    let identity = philotic_client::GuestIdentity {
        guest_id: "phil-keys".into(),
        role: "management".into(),
        supported_tools: vec![],
    };
    PhiloticClient::connect_at(&socket, identity)
        .await
        .with_context(|| format!("connect to aiua at {socket}"))
}

async fn add_vault_entry(
    client: &mut PhiloticClient,
    vault_name: &str,
    plaintext: String,
    allowed_roles: Vec<String>,
) -> Result<String> {
    match client
        .send_request(IpcRequest::AddVaultEntry {
            vault_name: vault_name.to_string(),
            plaintext,
            allowed_roles,
        })
        .await?
    {
        IpcResponse::Standard { ok: true, data, .. } => data
            .and_then(|data| {
                data.get("secret_ref")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .ok_or_else(|| anyhow!("vault response did not include secret_ref")),
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected add vault response: {other:?}")),
    }
}

async fn set_config(
    client: &mut PhiloticClient,
    key: &str,
    value: serde_json::Value,
) -> Result<()> {
    let value_json = serde_json::to_string(&value)?;
    match client
        .send_request(IpcRequest::SetConfig {
            key: key.to_string(),
            value_json,
        })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected set config response: {other:?}")),
    }
}

async fn get_config_string(client: &mut PhiloticClient, key: &str) -> Result<Option<String>> {
    match client
        .send_request(IpcRequest::GetConfig {
            key: key.to_string(),
        })
        .await?
    {
        IpcResponse::ConfigData {
            value_json: Some(raw),
            ..
        } => Ok(serde_json::from_str::<String>(&raw).ok()),
        IpcResponse::ConfigData {
            value_json: None, ..
        } => Ok(None),
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected get config response: {other:?}")),
    }
}
