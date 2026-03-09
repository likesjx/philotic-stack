use crate::controller::{AudioArtifact, ControllerTask, ModelProvider, ProviderOutput, TaskKind};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{Value, json};

pub struct ElevenLabsProvider {
    http_client: reqwest::Client,
    api_key: Option<String>,
    default_voice_id: Option<String>,
    default_model: String,
    default_output_format: String,
}

impl ElevenLabsProvider {
    pub fn new(
        http_client: reqwest::Client,
        api_key: Option<String>,
        default_voice_id: Option<String>,
    ) -> Self {
        Self {
            http_client,
            api_key,
            default_voice_id,
            default_model: "eleven_multilingual_v2".into(),
            default_output_format: "mp3_44100_128".into(),
        }
    }

    fn resolve_voice_id<'a>(&'a self, task: &'a ControllerTask) -> Result<&'a str> {
        task.requested_voice()
            .or(self.default_voice_id.as_deref())
            .or(task.provider_option_str("voice"))
            .or(task.provider_option_str("voice_id"))
            .context(
                "ElevenLabs voice.synthesize task is missing voice override and no default is configured",
            )
    }

    fn request_body(&self, task: &ControllerTask) -> Result<Value> {
        let text = task
            .voice_text()
            .context("ElevenLabs voice.synthesize task missing text")?;

        let mut body = json!({
            "text": text,
            "model_id": task.model.as_deref().unwrap_or(&self.default_model),
        });

        if let Some(language_code) = task.language_code.as_deref() {
            body["language_code"] = Value::String(language_code.to_string());
        }

        if let Some(seed) = task.provider_options.get("seed").cloned() {
            body["seed"] = seed;
        }

        if let Some(previous_text) = task.provider_option_str("previous_text") {
            body["previous_text"] = Value::String(previous_text.to_string());
        }

        if let Some(next_text) = task.provider_option_str("next_text") {
            body["next_text"] = Value::String(next_text.to_string());
        }

        if let Some(voice_settings) = task.provider_options.get("voice_settings").cloned() {
            body["voice_settings"] = voice_settings;
        }

        Ok(body)
    }

    fn output_format<'a>(&'a self, task: &'a ControllerTask) -> &'a str {
        task.output_format
            .as_deref()
            .or(task.provider_option_str("output_format"))
            .unwrap_or(&self.default_output_format)
    }
}

#[async_trait]
impl ModelProvider for ElevenLabsProvider {
    fn id(&self) -> &'static str {
        "elevenlabs"
    }

    fn supports(&self, task: &ControllerTask) -> bool {
        task.kind == TaskKind::VoiceSynthesize
    }

    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        let api_key = self
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .context("ElevenLabs API key missing from config")?;
        let voice_id = self.resolve_voice_id(task)?;
        let output_format = self.output_format(task).to_string();
        let model = task
            .model
            .as_deref()
            .unwrap_or(&self.default_model)
            .to_string();

        let response = self
            .http_client
            .post(format!(
                "https://api.elevenlabs.io/v1/text-to-speech/{}",
                voice_id
            ))
            .header("xi-api-key", api_key)
            .query(&[("output_format", output_format.as_str())])
            .json(&self.request_body(task)?)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "ElevenLabs API error ({}): {}",
                status.as_u16(),
                body.trim()
            );
        }

        let mime_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("audio/mpeg")
            .to_string();
        let audio_bytes = response.bytes().await?.to_vec();

        Ok(ProviderOutput::Audio(AudioArtifact {
            provider: self.id().into(),
            model,
            voice_id: voice_id.to_string(),
            mime_type,
            output_format,
            audio_bytes,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::ElevenLabsProvider;
    use crate::controller::ControllerTask;
    use serde_json::json;

    #[test]
    fn request_body_prefers_spoken_text_and_includes_provider_options() {
        let provider = ElevenLabsProvider::new(reqwest::Client::new(), None, None);
        let task = ControllerTask::from_value(&json!({
            "kind": "voice.synthesize",
            "text": "display",
            "spoken_text": "perform this expressively",
            "model": "eleven_v3",
            "provider_options": {
                "previous_text": "before",
                "next_text": "after",
                "voice_settings": {
                    "stability": 0.4
                }
            }
        }))
        .unwrap();

        let body = provider.request_body(&task).unwrap();
        assert_eq!(body["text"], "perform this expressively");
        assert_eq!(body["model_id"], "eleven_v3");
        assert_eq!(body["previous_text"], "before");
        assert_eq!(body["next_text"], "after");
        assert_eq!(body["voice_settings"]["stability"], 0.4);
    }

    #[test]
    fn resolves_request_voice_before_default_voice() {
        let provider =
            ElevenLabsProvider::new(reqwest::Client::new(), None, Some("default-voice".into()));
        let task = ControllerTask::from_value(&json!({
            "kind": "voice.synthesize",
            "text": "hello",
            "voice": "override-voice"
        }))
        .unwrap();

        assert_eq!(provider.resolve_voice_id(&task).unwrap(), "override-voice");
    }
}
