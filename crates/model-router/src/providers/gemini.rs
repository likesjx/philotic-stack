use crate::controller::{ControllerTask, ModelProvider, ProviderOutput, TaskKind};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiAuth {
    ApiKey(String),
    OAuthBearer {
        access_token: String,
        project_id: Option<String>,
    },
}

pub struct GeminiProvider {
    http_client: reqwest::Client,
    auth: Option<GeminiAuth>,
    default_model: String,
}

impl GeminiProvider {
    pub fn new(http_client: reqwest::Client, auth: Option<GeminiAuth>) -> Self {
        Self {
            http_client,
            auth,
            default_model: "gemini-flash-latest".into(),
        }
    }

    pub fn auth_from_config(
        oauth_access_token: Option<String>,
        oauth_project_id: Option<String>,
        api_key: Option<String>,
    ) -> Option<GeminiAuth> {
        if let Some(access_token) = oauth_access_token
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty())
        {
            let project_id = oauth_project_id
                .map(|project| project.trim().to_string())
                .filter(|project| !project.is_empty());
            return Some(GeminiAuth::OAuthBearer {
                access_token,
                project_id,
            });
        }

        api_key
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
            .map(GeminiAuth::ApiKey)
    }

    fn endpoint_url(&self, model: Option<&str>) -> Result<String> {
        let model = model.unwrap_or(&self.default_model);

        match self
            .auth
            .as_ref()
            .context("Gemini auth missing from config; expected OAuth bearer or API key")?
        {
            GeminiAuth::ApiKey(api_key) => Ok(format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
                model, api_key
            )),
            GeminiAuth::OAuthBearer { .. } => Ok(format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
                model
            )),
        }
    }

    fn request_payload(prompt: &str) -> Value {
        json!({
            "contents": [{"parts": [{"text": prompt}]}]
        })
    }

    fn parse_response_text(status: reqwest::StatusCode, body: Value) -> String {
        if !status.is_success() {
            if let Some(message) = body
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
            {
                return format!("Gemini API Error ({}): {}", status.as_u16(), message);
            }

            return format!("Gemini API Error ({}): {}", status.as_u16(), body);
        }

        body.get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first())
            .and_then(|candidate| candidate.get("content"))
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
            .and_then(|parts| parts.first())
            .and_then(|part| part.get("text"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "Gemini returned an empty response.".into())
    }

    fn apply_auth_headers(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder> {
        match self
            .auth
            .as_ref()
            .context("Gemini auth missing from config; expected OAuth bearer or API key")?
        {
            GeminiAuth::ApiKey(_) => Ok(builder),
            GeminiAuth::OAuthBearer {
                access_token,
                project_id,
            } => {
                let builder = builder.bearer_auth(access_token);
                if let Some(project_id) = project_id {
                    Ok(builder.header("x-goog-user-project", project_id))
                } else {
                    Ok(builder)
                }
            }
        }
    }
}

#[async_trait]
impl ModelProvider for GeminiProvider {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn supports(&self, task: &ControllerTask) -> bool {
        task.kind == TaskKind::TextGenerate
    }

    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        let prompt = task
            .prompt_text()
            .context("Gemini text task missing prompt")?;
        let response = self
            .apply_auth_headers(
                self.http_client
                    .post(self.endpoint_url(task.model.as_deref())?),
            )?
            .json(&Self::request_payload(prompt))
            .send()
            .await?;
        let status = response.status();
        let body = response.json::<Value>().await?;
        let content = Self::parse_response_text(status, body);

        if content.trim().is_empty() {
            bail!("Gemini returned an empty response");
        }

        Ok(ProviderOutput::Text { content })
    }
}

#[cfg(test)]
mod tests {
    use super::{GeminiAuth, GeminiProvider};

    #[test]
    fn prefers_oauth_bearer_over_api_key() {
        let auth = GeminiProvider::auth_from_config(
            Some(" oauth-token ".into()),
            Some(" test-project ".into()),
            Some("api-key".into()),
        )
        .unwrap();

        assert_eq!(
            auth,
            GeminiAuth::OAuthBearer {
                access_token: "oauth-token".into(),
                project_id: Some("test-project".into())
            }
        );
    }

    #[test]
    fn falls_back_to_api_key_when_oauth_missing() {
        let auth = GeminiProvider::auth_from_config(
            None,
            Some("ignored-project".into()),
            Some(" api ".into()),
        )
        .unwrap();

        assert_eq!(auth, GeminiAuth::ApiKey("api".into()));
    }

    #[test]
    fn oauth_endpoint_omits_query_api_key() {
        let provider = GeminiProvider::new(
            reqwest::Client::new(),
            Some(GeminiAuth::OAuthBearer {
                access_token: "oauth-token".into(),
                project_id: Some("proj".into()),
            }),
        );

        let url = provider.endpoint_url(Some("gemini-2.5-pro")).unwrap();
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent"
        );
    }

    #[test]
    fn oauth_auth_adds_bearer_and_project_headers() {
        let provider = GeminiProvider::new(
            reqwest::Client::new(),
            Some(GeminiAuth::OAuthBearer {
                access_token: "oauth-token".into(),
                project_id: Some("proj-123".into()),
            }),
        );

        let request = provider
            .apply_auth_headers(reqwest::Client::new().post("https://example.com"))
            .unwrap()
            .build()
            .unwrap();

        assert_eq!(
            request.headers().get("authorization").unwrap(),
            "Bearer oauth-token"
        );
        assert_eq!(
            request.headers().get("x-goog-user-project").unwrap(),
            "proj-123"
        );
    }

    #[test]
    fn api_key_endpoint_keeps_query_key() {
        let provider = GeminiProvider::new(
            reqwest::Client::new(),
            Some(GeminiAuth::ApiKey("api-key".into())),
        );

        let url = provider.endpoint_url(Some("gemini-2.5-flash")).unwrap();
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key=api-key"
        );
    }
}
