use crate::controller::{AttachmentInput, ControllerTask, ModelProvider, ProviderOutput, TaskKind};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::{Value, json};
use std::borrow::Cow;

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
    base_url: String,
}

impl GeminiProvider {
    pub fn new(
        http_client: reqwest::Client,
        auth: Option<GeminiAuth>,
        base_url: Option<String>,
    ) -> Self {
        Self {
            http_client,
            auth,
            default_model: "gemini-flash-latest".into(),
            base_url: base_url
                .unwrap_or_else(|| "https://generativelanguage.googleapis.com".into())
                .trim_end_matches('/')
                .to_string(),
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
                "{}/v1beta/models/{}:generateContent?key={}",
                self.base_url, model, api_key
            )),
            GeminiAuth::OAuthBearer { .. } => Ok(format!(
                "{}/v1beta/models/{}:generateContent",
                self.base_url, model
            )),
        }
    }

    fn request_payload(prompt: &str) -> Value {
        json!({
            "contents": [{"parts": [{"text": prompt}]}]
        })
    }

    fn structured_text_request_payload(prompt: &str) -> Value {
        json!({
            "system_instruction": {
                "parts": [{
                    "text": "When generating your response, produce a JSON object with two fields: \
                             \"display_text\" (your full response formatted for text display, \
                             markdown is fine) and \"spoken_text\" (a natural, expressive version \
                             for voice delivery — no markdown, conversational tone, written to be heard)."
                }]
            },
            "contents": [{"parts": [{"text": prompt}]}],
            "generationConfig": {
                "responseMimeType": "application/json",
                "responseSchema": {
                    "type": "OBJECT",
                    "properties": {
                        "display_text": { "type": "STRING" },
                        "spoken_text": { "type": "STRING" }
                    },
                    "required": ["display_text", "spoken_text"]
                }
            }
        })
    }

    fn parse_structured_response(
        status: reqwest::StatusCode,
        body: Value,
    ) -> (String, Option<String>) {
        let raw = Self::parse_response_text(status, body);
        // Try to parse as structured JSON output
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) {
            let display = parsed
                .get("display_text")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string);
            let spoken = parsed
                .get("spoken_text")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string);
            if let Some(display) = display {
                return (display, spoken);
            }
        }
        (raw, None)
    }

    async fn media_request_payload(&self, task: &ControllerTask) -> Result<Value> {
        let prompt = task
            .media_prompt()
            .context("Gemini media task missing prompt")?;
        let mut parts = vec![json!({ "text": prompt })];

        for attachment in task.media_attachments().iter().filter(|attachment| {
            attachment
                .url
                .as_deref()
                .map(|url| !url.trim().is_empty())
                .unwrap_or(false)
                && attachment
                    .transport_error
                    .as_deref()
                    .map(|error| error.trim().is_empty())
                    .unwrap_or(true)
        }) {
            let mime_type = attachment_mime_type(attachment)
                .with_context(|| format!("attachment {:?} missing mime type", attachment.kind))?;
            let url = attachment
                .url
                .as_deref()
                .context("media attachment missing download url")?;
            let response = self.http_client.get(url).send().await?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                bail!(
                    "failed to fetch media attachment from {}: HTTP {} {}",
                    url,
                    status,
                    body
                );
            }
            let bytes = response.bytes().await?;
            parts.push(json!({
                "inline_data": {
                    "mime_type": mime_type,
                    "data": BASE64_STANDARD.encode(bytes)
                }
            }));
        }

        Ok(json!({
            "contents": [{"parts": parts}]
        }))
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
        matches!(
            task.kind,
            TaskKind::TextGenerate | TaskKind::MediaAnalyze | TaskKind::AudioTranscribe
        )
    }

    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        let payload = match task.kind {
            TaskKind::TextGenerate => {
                let prompt = task
                    .composed_prompt_text()
                    .context("Gemini text task missing prompt")?;
                if task.wants_channel("spoken_text") {
                    Self::structured_text_request_payload(&prompt)
                } else {
                    Self::request_payload(&prompt)
                }
            }
            TaskKind::MediaAnalyze | TaskKind::AudioTranscribe => {
                self.media_request_payload(task).await?
            }
            TaskKind::VoiceSynthesize => bail!("Gemini does not support voice synthesis"),
        };
        let response = self
            .apply_auth_headers(
                self.http_client
                    .post(self.endpoint_url(task.model.as_deref())?),
            )?
            .json(&payload)
            .send()
            .await?;
        let status = response.status();
        let body = response.json::<Value>().await?;

        let (content, spoken_text) =
            if task.kind == TaskKind::TextGenerate && task.wants_channel("spoken_text") {
                Self::parse_structured_response(status, body)
            } else {
                (Self::parse_response_text(status, body), None)
            };

        if content.trim().is_empty() {
            bail!("Gemini returned an empty response");
        }

        Ok(ProviderOutput::Text {
            display_text: Some(content.clone()),
            content,
            spoken_text,
            working_memory_delta: None,
            follow_up_questions: Vec::new(),
            intent_summary: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{GeminiAuth, GeminiProvider};
    use crate::controller::{
        AttachmentInput, ContextEnvelope, ControllerTask, RequestClass, RoutingHints, TaskKind,
    };

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
            None,
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
            None,
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
            None,
        );

        let url = provider.endpoint_url(Some("gemini-2.5-flash")).unwrap();
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key=api-key"
        );
    }

    #[test]
    fn base_url_override_changes_endpoint_host() {
        let provider = GeminiProvider::new(
            reqwest::Client::new(),
            Some(GeminiAuth::OAuthBearer {
                access_token: "oauth-token".into(),
                project_id: Some("proj".into()),
            }),
            Some("http://127.0.0.1:40123".into()),
        );

        let url = provider.endpoint_url(Some("gemini-2.5-flash")).unwrap();
        assert_eq!(
            url,
            "http://127.0.0.1:40123/v1beta/models/gemini-2.5-flash:generateContent"
        );
    }

    #[test]
    fn image_attachment_defaults_to_jpeg() {
        let attachment = AttachmentInput {
            kind: Some("photo".into()),
            file_id: Some("photo-1".into()),
            mime_type: None,
            url: Some("http://127.0.0.1:9001/download/sha256-1".into()),
            blob_ref: Some("sha256-1".into()),
            transport_error: None,
        };

        assert_eq!(
            super::attachment_mime_type(&attachment).as_deref(),
            Some("image/jpeg")
        );
    }

    #[test]
    fn markdown_document_mime_normalizes_to_text_plain() {
        let attachment = AttachmentInput {
            kind: Some("document".into()),
            file_id: Some("doc-1".into()),
            mime_type: Some("text/x-web-markdown".into()),
            url: Some("http://127.0.0.1:9001/download/sha256-doc".into()),
            blob_ref: Some("sha256-doc".into()),
            transport_error: None,
        };

        assert_eq!(
            super::attachment_mime_type(&attachment).as_deref(),
            Some("text/plain")
        );
    }

    #[test]
    fn gemini_supports_media_analysis_tasks() {
        let provider = GeminiProvider::new(
            reqwest::Client::new(),
            Some(GeminiAuth::ApiKey("api-key".into())),
            None,
        );
        let task = ControllerTask {
            kind: TaskKind::MediaAnalyze,
            request_class: RequestClass::Transform,
            provider: None,
            model: None,
            prompt: Some("Describe this media".into()),
            text: None,
            spoken_text: None,
            display_text: None,
            voice: None,
            voice_id: None,
            output_format: None,
            language_code: None,
            response_contract: Default::default(),
            context: ContextEnvelope {
                attachments: vec![AttachmentInput {
                    kind: Some("voice".into()),
                    file_id: Some("voice-1".into()),
                    mime_type: Some("audio/ogg".into()),
                    url: Some("http://127.0.0.1:9001/download/sha256-2".into()),
                    blob_ref: Some("sha256-2".into()),
                    transport_error: None,
                }],
                ..Default::default()
            },
            context_projection: Default::default(),
            affordances: Default::default(),
            routing_hints: RoutingHints::default(),
            provider_options: Default::default(),
        };

        assert!(crate::controller::ModelProvider::supports(&provider, &task));
    }

    #[test]
    fn gemini_supports_audio_transcribe_tasks() {
        let provider = GeminiProvider::new(
            reqwest::Client::new(),
            Some(GeminiAuth::ApiKey("api-key".into())),
            None,
        );
        let task = ControllerTask {
            kind: TaskKind::AudioTranscribe,
            request_class: RequestClass::Transform,
            provider: None,
            model: None,
            prompt: Some("Transcribe this audio verbatim.".into()),
            text: None,
            spoken_text: None,
            display_text: None,
            voice: None,
            voice_id: None,
            output_format: None,
            language_code: None,
            response_contract: Default::default(),
            context: ContextEnvelope {
                attachments: vec![AttachmentInput {
                    kind: Some("voice".into()),
                    file_id: Some("voice-1".into()),
                    mime_type: Some("audio/ogg".into()),
                    url: Some("http://127.0.0.1:9001/download/sha256-voice-1".into()),
                    blob_ref: Some("sha256-voice-1".into()),
                    transport_error: None,
                }],
                ..Default::default()
            },
            context_projection: Default::default(),
            affordances: Default::default(),
            routing_hints: RoutingHints::default(),
            provider_options: Default::default(),
        };

        assert!(crate::controller::ModelProvider::supports(&provider, &task));
    }
}

fn attachment_mime_type(attachment: &AttachmentInput) -> Option<Cow<'_, str>> {
    attachment
        .mime_type
        .as_deref()
        .map(normalize_attachment_mime_type)
        .or_else(|| match attachment.kind.as_deref() {
            Some("photo") | Some("image") => Some(Cow::Borrowed("image/jpeg")),
            Some("voice") => Some(Cow::Borrowed("audio/ogg")),
            Some("sticker") => Some(Cow::Borrowed("image/webp")),
            _ => None,
        })
}

fn normalize_attachment_mime_type(mime_type: &str) -> Cow<'_, str> {
    let normalized = mime_type.trim();
    if normalized.eq_ignore_ascii_case("text/x-web-markdown")
        || normalized.eq_ignore_ascii_case("text/markdown")
        || normalized.eq_ignore_ascii_case("text/x-markdown")
    {
        Cow::Borrowed("text/plain")
    } else {
        Cow::Borrowed(normalized)
    }
}
