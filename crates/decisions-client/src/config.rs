//! Loading the dedicated `decisions` key and its settings from the hotel.
//!
//! The key is its own vault entry (`decisions_api_key`), readable only by the
//! roles on its `ProviderKeySpec` (`model.decisions` and `heal-dispatcher`), so
//! the chat OpenRouter key's role list is never widened and the decisions key
//! can be rotated, budgeted and revoked on its own.
//!
//! Unlike `model-router`'s per-provider loader, an ACL denial here is an error,
//! not a soft `None`: the caller is one of the intended roles, so a denial means
//! the key was sealed without its role and needs `phil keys configure decisions`
//! re-run. Callers treat any error as "no decisions available" and fall back.

use ansible_mesh_core::provider_keys::provider_key_spec;
use anyhow::{Context, Result, bail};
use philotic_client::{IpcRequest, IpcResponse, PhiloticClient};
use serde_json::Value;
use std::fmt;

const PROVIDER: &str = "decisions";
const ENV_BASE_URL: &str = "PHILOTIC_DECISIONS_BASE_URL";
const ENV_MODEL: &str = "PHILOTIC_DECISIONS_MODEL";

/// The decisions key and settings. `Debug` redacts the key.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct DecisionsConfig {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
}

impl fmt::Debug for DecisionsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecisionsConfig")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish()
    }
}

/// Load the decisions config: environment overrides first (ephemeral or CI use),
/// then the hotel's vault entry and config keys.
pub async fn load_decisions_config(ipc: &mut PhiloticClient) -> Result<DecisionsConfig> {
    let spec = provider_key_spec(PROVIDER).context("decisions provider key spec missing")?;

    let api_key = if let Some(value) = env_nonempty(spec.env_api_key) {
        Some(value)
    } else if let Some(secret_ref) = env_nonempty(spec.env_api_key_ref) {
        fetch_secret(ipc, &secret_ref).await?
    } else if let Some(secret_ref) = fetch_config(ipc, spec.api_key_ref_key).await? {
        fetch_secret(ipc, &secret_ref).await?
    } else {
        None
    };

    let base_url = match env_nonempty(ENV_BASE_URL) {
        Some(value) => Some(value),
        None => match spec.base_url_key {
            Some(key) => fetch_config(ipc, key).await?,
            None => None,
        },
    };
    let model = match env_nonempty(ENV_MODEL) {
        Some(value) => Some(value),
        None => match spec.default_model_key {
            Some(key) => fetch_config(ipc, key).await?,
            None => None,
        },
    };

    Ok(DecisionsConfig {
        api_key,
        base_url,
        model,
    })
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

async fn fetch_config(ipc: &mut PhiloticClient, key: &str) -> Result<Option<String>> {
    let response = ipc
        .send_request(IpcRequest::GetConfig { key: key.into() })
        .await?;
    Ok(config_from_response(response))
}

async fn fetch_secret(ipc: &mut PhiloticClient, secret_ref: &str) -> Result<Option<String>> {
    let response = ipc
        .send_request(IpcRequest::GetSecret {
            secret_ref: secret_ref.into(),
        })
        .await?;
    secret_from_response(response)
}

/// A config value: a JSON string, or the raw JSON text for anything else.
/// Missing and blank values are `None`.
fn config_from_response(response: IpcResponse) -> Option<String> {
    let IpcResponse::ConfigData {
        value_json: Some(value_json),
        ..
    } = response
    else {
        return None;
    };
    unquote(value_json)
}

fn secret_from_response(response: IpcResponse) -> Result<Option<String>> {
    match response {
        IpcResponse::SecretData {
            value_json: Some(value_json),
            ..
        } => Ok(unquote(value_json)),
        IpcResponse::SecretData {
            value_json: None, ..
        } => Ok(None),
        IpcResponse::Standard {
            ok: false, message, ..
        } => bail!("decisions key fetch failed: {message}"),
        other => bail!("unexpected GetSecret response: {other:?}"),
    }
}

fn unquote(value_json: String) -> Option<String> {
    let value = match serde_json::from_str::<Value>(&value_json) {
        Ok(Value::String(s)) => s,
        _ => value_json,
    };
    Some(value).filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(value_json: Option<&str>) -> IpcResponse {
        IpcResponse::SecretData {
            secret_ref: "ref".into(),
            value_json: value_json.map(str::to_string),
        }
    }

    #[test]
    fn secrets_come_back_unquoted_and_blank_is_none() {
        assert_eq!(
            secret_from_response(secret(Some("\"sk-test\""))).unwrap(),
            Some("sk-test".to_string())
        );
        // A bare non-JSON value is taken as is.
        assert_eq!(
            secret_from_response(secret(Some("sk-raw"))).unwrap(),
            Some("sk-raw".to_string())
        );
        assert_eq!(secret_from_response(secret(Some("\"  \""))).unwrap(), None);
        assert_eq!(secret_from_response(secret(None)).unwrap(), None);
    }

    #[test]
    fn a_denied_or_failed_secret_fetch_is_an_error_not_a_soft_none() {
        let denied = IpcResponse::Standard {
            ok: false,
            code: "DENIED".into(),
            message: "secret is not accessible to role heal-dispatcher".into(),
            corr_id: String::new(),
            data: None,
        };
        let err = secret_from_response(denied).expect_err("denial must surface");
        assert!(err.to_string().contains("not accessible"), "{err}");
    }

    #[test]
    fn config_values_are_unquoted_and_missing_is_none() {
        let present = IpcResponse::ConfigData {
            key: "k".into(),
            value_json: Some("\"https://openrouter.ai/api\"".into()),
        };
        assert_eq!(
            config_from_response(present),
            Some("https://openrouter.ai/api".to_string())
        );
        let absent = IpcResponse::ConfigData {
            key: "k".into(),
            value_json: None,
        };
        assert_eq!(config_from_response(absent), None);
    }

    #[test]
    fn debug_never_prints_the_key() {
        let config = DecisionsConfig {
            api_key: Some("sk-or-v1-secret".into()),
            base_url: Some("https://openrouter.ai/api".into()),
            model: None,
        };
        let shown = format!("{config:?}");
        assert!(!shown.contains("sk-or-v1-secret"), "{shown}");
        assert!(shown.contains("<redacted>"));
    }

    #[test]
    fn the_dedicated_spec_is_scoped_to_exactly_its_two_roles() {
        let spec = provider_key_spec("decisions").expect("spec exists");
        assert_eq!(spec.vault_name, "decisions_api_key");
        assert_eq!(spec.allowed_roles, &["model.decisions", "heal-dispatcher"]);
        // It never widens the chat OpenRouter key.
        let openrouter = provider_key_spec("openrouter").unwrap();
        assert!(!openrouter.allowed_roles.contains(&"heal-dispatcher"));
        assert!(!openrouter.allowed_roles.contains(&"model.decisions"));
    }
}
