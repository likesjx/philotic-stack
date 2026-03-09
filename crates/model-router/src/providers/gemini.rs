use crate::controller::{ControllerTask, MediaAttachment, ModelProvider, ProviderOutput, TaskKind};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::{Value, json};

pub struct GeminiProvider {
    http_client: reqwest::Client,
    api_key: Option<String>,
    default_model: String,
}

impl GeminiProvider {
    pub fn new(http_client: reqwest::Client, api_key: Option<String>) -> Self {
        Self {
            http_client,
            api_key,
            default_model: "gemini-flash-latest".into(),
        }
    }

    fn endpoint_url(&self, model: Option<&str>) -> Result<String> {
        let api_key = self
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .context("Gemini API key missing from config")?;
        let model = model.unwrap_or(&self.default_model);

        Ok(format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            model, api_key
        ))
    }

    fn text_request_payload(prompt: &str) -> Value {
        json!({
            "contents": [{"parts": [{"text": prompt}]}]
        })
    }

    async fn media_request_payload(&self, task: &ControllerTask) -> Result<Value> {
        let prompt = task
            .media_prompt()
            .context("Gemini media task missing prompt")?;
        let mut parts = vec![json!({ "text": prompt })];

        for attachment in task.attachments.iter().filter(|attachment| {
            attachment
                .blob_download_url
                .as_deref()
                .map(|url| !url.is_empty())
                .unwrap_or(false)
        }) {
            let mime_type = attachment_mime_type(attachment)
                .with_context(|| format!("attachment [{}] missing mime type", attachment.kind))?;
            let blob_url = attachment
                .blob_download_url
                .as_deref()
                .context("attachment missing blob download url")?;
            let response = self.http_client.get(blob_url).send().await?;
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
}

#[async_trait]
impl ModelProvider for GeminiProvider {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn supports(&self, task: &ControllerTask) -> bool {
        matches!(task.kind, TaskKind::TextGenerate | TaskKind::MediaAnalyze)
    }

    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        let payload = match task.kind {
            TaskKind::TextGenerate => Self::text_request_payload(
                task.prompt_text()
                    .context("Gemini text task missing prompt")?,
            ),
            TaskKind::MediaAnalyze => self.media_request_payload(task).await?,
            TaskKind::VoiceSynthesize => bail!("Gemini does not support voice synthesis"),
        };
        let response = self
            .http_client
            .post(self.endpoint_url(task.model.as_deref())?)
            .json(&payload)
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
    use super::GeminiProvider;
    use crate::controller::{ControllerTask, MediaAttachment, TaskKind};
    use serde_json::json;

    #[test]
    fn request_payload_wraps_prompt_in_contents() {
        let payload = GeminiProvider::text_request_payload("hello");
        assert_eq!(payload["contents"][0]["parts"][0]["text"], "hello");
    }

    #[test]
    fn parser_extracts_candidate_text() {
        let text = GeminiProvider::parse_response_text(
            reqwest::StatusCode::OK,
            json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "text": "hi from gemini"
                        }]
                    }
                }]
            }),
        );

        assert_eq!(text, "hi from gemini");
    }

    #[test]
    fn parser_surfaces_api_errors() {
        let text = GeminiProvider::parse_response_text(
            reqwest::StatusCode::BAD_REQUEST,
            json!({
                "error": {
                    "message": "bad prompt"
                }
            }),
        );

        assert_eq!(text, "Gemini API Error (400): bad prompt");
    }

    #[test]
    fn image_attachments_default_to_jpeg() {
        let attachment = MediaAttachment {
            kind: "photo".into(),
            file_id: "file-1".into(),
            mime_type: None,
            file_name: None,
            file_size: None,
            telegram_file_path: None,
            blob_id: None,
            blob_download_url: Some("http://127.0.0.1:9001/download/sha256-1".into()),
            transport_error: None,
        };

        assert_eq!(super::attachment_mime_type(&attachment), Some("image/jpeg"));
    }

    #[test]
    fn gemini_supports_media_analysis_tasks() {
        let provider = GeminiProvider::new(reqwest::Client::new(), Some("key".into()));
        let task = ControllerTask {
            kind: TaskKind::MediaAnalyze,
            provider: None,
            model: None,
            prompt: Some("Describe this image".into()),
            text: None,
            attachments: vec![MediaAttachment {
                kind: "photo".into(),
                file_id: "file-1".into(),
                mime_type: Some("image/jpeg".into()),
                file_name: None,
                file_size: None,
                telegram_file_path: None,
                blob_id: Some("sha256-1".into()),
                blob_download_url: Some("http://127.0.0.1:9001/download/sha256-1".into()),
                transport_error: None,
            }],
            voice_id: None,
            output_format: None,
            language_code: None,
        };

        assert!(crate::controller::ModelProvider::supports(&provider, &task));
    }
}

fn attachment_mime_type(attachment: &MediaAttachment) -> Option<&str> {
    attachment
        .mime_type
        .as_deref()
        .or(match attachment.kind.as_str() {
            "photo" => Some("image/jpeg"),
            "sticker" => Some("image/webp"),
            _ => None,
        })
}
