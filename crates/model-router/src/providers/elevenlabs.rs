use crate::controller::{AudioArtifact, ControllerTask, ModelProvider, ProviderOutput, TaskKind};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

pub struct ElevenLabsProvider {
    http_client: reqwest::Client,
    api_key: Option<String>,
    default_voice_id: Option<String>,
    default_model: String,
    default_output_format: String,
    default_stt_model: String,
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
            default_stt_model: "scribe_v1".into(),
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

// ── ElevenLabs STT response ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct SttResponse {
    text: String,
}

#[async_trait]
impl ModelProvider for ElevenLabsProvider {
    fn id(&self) -> &'static str {
        "elevenlabs"
    }

    fn supports(&self, task: &ControllerTask) -> bool {
        matches!(
            task.kind,
            TaskKind::VoiceSynthesize | TaskKind::AudioTranscribe
        )
    }

    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        let api_key = self
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .context("ElevenLabs API key missing from config")?;

        match task.kind {
            // ── VoiceSynthesize ─────────────────────────────────────────────
            TaskKind::VoiceSynthesize => {
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
                        "https://api.elevenlabs.io/v1/text-to-speech/{}/stream",
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

            // ── AudioTranscribe ─────────────────────────────────────────────
            TaskKind::AudioTranscribe => {
                // Fetch audio bytes from the first blob-backed attachment.
                let attachment = task
                    .media_attachments()
                    .iter()
                    .find(|a| {
                        a.url
                            .as_deref()
                            .map(|u| !u.trim().is_empty())
                            .unwrap_or(false)
                    })
                    .context("audio.transcribe task has no media attachment with a download URL")?;
                let url = attachment
                    .url
                    .as_deref()
                    .context("attachment missing url")?;

                let audio_resp = self
                    .http_client
                    .get(url)
                    .send()
                    .await
                    .with_context(|| format!("failed to fetch audio from {url}"))?;

                let status = audio_resp.status();
                if !status.is_success() {
                    let body = audio_resp.text().await.unwrap_or_default();
                    anyhow::bail!(
                        "failed to fetch audio attachment from {url}: HTTP {status} {body}"
                    );
                }

                let audio_bytes = audio_resp
                    .bytes()
                    .await
                    .context("failed to read audio response body")?
                    .to_vec();

                // Determine STT model.
                let stt_model = task
                    .model
                    .as_deref()
                    .unwrap_or(&self.default_stt_model)
                    .to_string();

                // Build multipart form: ElevenLabs STT requires `file` + optional params.
                let part = reqwest::multipart::Part::bytes(audio_bytes)
                    .file_name("audio.wav")
                    .mime_str("audio/wav")
                    .context("invalid audio mime type")?;

                let mut form = reqwest::multipart::Form::new()
                    .part("file", part)
                    .text("model_id", stt_model);

                if let Some(lang) = task.language_code.as_deref() {
                    form = form.text("language_code", lang.to_string());
                }

                let stt_resp = self
                    .http_client
                    .post("https://api.elevenlabs.io/v1/speech-to-text")
                    .header("xi-api-key", api_key)
                    .multipart(form)
                    .send()
                    .await
                    .context("ElevenLabs STT request failed")?;

                let status = stt_resp.status();
                if !status.is_success() {
                    let body = stt_resp.text().await.unwrap_or_default();
                    anyhow::bail!(
                        "ElevenLabs STT API error ({}): {}",
                        status.as_u16(),
                        body.trim()
                    );
                }

                let stt: SttResponse = stt_resp
                    .json()
                    .await
                    .context("failed to parse ElevenLabs STT response")?;

                Ok(ProviderOutput::Text {
                    display_text: Some(stt.text.clone()),
                    content: stt.text,
                    spoken_text: None,
                    partial_replies: Vec::new(),
                    working_memory_delta: None,
                    follow_up_questions: Vec::new(),
                    intent_summary: None,
                    memory_concept: None,
                    memory_candidate: None,
                    active_plan: None,
                })
            }

            other => anyhow::bail!(
                "ElevenLabsProvider does not support task kind [{}]",
                other.as_str()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ElevenLabsProvider;
    use crate::controller::{ControllerTask, ModelProvider};
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
    fn request_body_preserves_voice_speed_in_voice_settings() {
        let provider = ElevenLabsProvider::new(reqwest::Client::new(), None, None);
        let task = ControllerTask::from_value(&json!({
            "kind": "voice.synthesize",
            "spoken_text": "slow down a bit",
            "provider_options": {
                "voice_settings": {
                    "speed": 0.85
                }
            }
        }))
        .unwrap();

        let body = provider.request_body(&task).unwrap();
        assert_eq!(body["voice_settings"]["speed"], 0.85);
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

    #[test]
    fn supports_audio_transcribe_and_voice_synthesize() {
        let provider = ElevenLabsProvider::new(reqwest::Client::new(), None, None);

        let tts_task = ControllerTask::from_value(&json!({
            "kind": "voice.synthesize",
            "text": "hello"
        }))
        .unwrap();
        assert!(provider.supports(&tts_task));

        let stt_task = ControllerTask::from_value(&json!({
            "kind": "voice.transcribe",
            "prompt": "transcribe this",
            "context": {
                "attachments": [{"url": "http://localhost:9001/blob/abc"}]
            }
        }))
        .unwrap();
        assert!(provider.supports(&stt_task));
    }

    #[test]
    fn default_stt_model_is_scribe_v1() {
        let provider = ElevenLabsProvider::new(reqwest::Client::new(), None, None);
        assert_eq!(provider.default_stt_model, "scribe_v1");
    }
}
