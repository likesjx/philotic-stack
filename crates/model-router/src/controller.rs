use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use philotic_client::{IpcRequest, IpcResponse, PhiloticClient};
use serde_json::{Map, Value, json};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    TextGenerate,
    VoiceSynthesize,
}

impl TaskKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TextGenerate => "text.generate",
            Self::VoiceSynthesize => "voice.synthesize",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerTask {
    pub kind: TaskKind,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub prompt: Option<String>,
    pub text: Option<String>,
    pub spoken_text: Option<String>,
    pub display_text: Option<String>,
    pub voice: Option<String>,
    pub voice_id: Option<String>,
    pub output_format: Option<String>,
    pub language_code: Option<String>,
    pub provider_options: Map<String, Value>,
}

impl ControllerTask {
    pub fn from_value(task: &Value) -> Result<Self> {
        let kind = match task.get("kind").and_then(Value::as_str) {
            Some("text.generate") | Some("text_generate") => TaskKind::TextGenerate,
            Some("voice.synthesize") | Some("voice_synthesize") => TaskKind::VoiceSynthesize,
            Some(other) => bail!("unsupported task kind [{}]", other),
            None if task.get("prompt").and_then(Value::as_str).is_some() => TaskKind::TextGenerate,
            None if task.get("text").and_then(Value::as_str).is_some() => TaskKind::VoiceSynthesize,
            None if task.get("spoken_text").and_then(Value::as_str).is_some() => {
                TaskKind::VoiceSynthesize
            }
            None => bail!("task is missing a recognized kind/prompt/text payload"),
        };

        let provider_options = task
            .get("provider_options")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        let controller_task = Self {
            kind,
            provider: task
                .get("provider")
                .and_then(Value::as_str)
                .map(str::to_string),
            model: task
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
            prompt: task
                .get("prompt")
                .and_then(Value::as_str)
                .map(str::to_string),
            text: task.get("text").and_then(Value::as_str).map(str::to_string),
            spoken_text: task
                .get("spoken_text")
                .and_then(Value::as_str)
                .map(str::to_string),
            display_text: task
                .get("display_text")
                .and_then(Value::as_str)
                .map(str::to_string),
            voice: task
                .get("voice")
                .and_then(Value::as_str)
                .map(str::to_string),
            voice_id: task
                .get("voice_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            output_format: task
                .get("output_format")
                .and_then(Value::as_str)
                .map(str::to_string),
            language_code: task
                .get("language_code")
                .and_then(Value::as_str)
                .map(str::to_string),
            provider_options,
        };

        controller_task.validate()?;
        Ok(controller_task)
    }

    pub fn provider_hint(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    pub fn prompt_text(&self) -> Option<&str> {
        self.prompt.as_deref()
    }

    pub fn voice_text(&self) -> Option<&str> {
        self.spoken_text
            .as_deref()
            .or(self.text.as_deref())
            .or(self.display_text.as_deref())
    }

    pub fn display_text(&self) -> Option<&str> {
        self.display_text.as_deref().or(self.text.as_deref())
    }

    pub fn requested_voice(&self) -> Option<&str> {
        self.voice.as_deref().or(self.voice_id.as_deref())
    }

    pub fn provider_option_str(&self, key: &str) -> Option<&str> {
        self.provider_options.get(key).and_then(Value::as_str)
    }

    fn validate(&self) -> Result<()> {
        match self.kind {
            TaskKind::TextGenerate => {
                let prompt = self
                    .prompt_text()
                    .context("text.generate task missing prompt")?;
                if prompt.trim().is_empty() {
                    bail!("text.generate task prompt cannot be empty");
                }
            }
            TaskKind::VoiceSynthesize => {
                let text = self
                    .voice_text()
                    .context("voice.synthesize task missing text/spoken_text/display_text")?;
                if text.trim().is_empty() {
                    bail!("voice.synthesize task text cannot be empty");
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioArtifact {
    pub provider: String,
    pub model: String,
    pub voice_id: String,
    pub mime_type: String,
    pub output_format: String,
    pub audio_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderOutput {
    Text { content: String },
    Audio(AudioArtifact),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderConfigs {
    pub gemini_api_key: Option<String>,
    pub gemini_oauth_access_token: Option<String>,
    pub gemini_oauth_project_id: Option<String>,
    pub elevenlabs_api_key: Option<String>,
    pub elevenlabs_default_voice_id: Option<String>,
}

impl ProviderConfigs {
    pub async fn load(ipc_client: &mut PhiloticClient) -> Result<Self> {
        Ok(Self {
            gemini_api_key: fetch_config_string(ipc_client, "gemini_api_key").await?,
            gemini_oauth_access_token: fetch_config_string(ipc_client, "gemini_oauth_access_token")
                .await?,
            gemini_oauth_project_id: fetch_config_string(ipc_client, "gemini_oauth_project_id")
                .await?,
            elevenlabs_api_key: fetch_config_string(ipc_client, "elevenlabs_api_key").await?,
            elevenlabs_default_voice_id: fetch_config_string(ipc_client, "elevenlabs_voice_id")
                .await?,
        })
    }
}

#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn supports(&self, task: &ControllerTask) -> bool;
    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput>;
}

#[derive(Clone)]
pub struct ProviderRegistry {
    providers: Vec<Arc<dyn ModelProvider>>,
}

impl ProviderRegistry {
    pub fn new(providers: Vec<Arc<dyn ModelProvider>>) -> Self {
        Self { providers }
    }

    pub fn resolve(&self, task: &ControllerTask) -> Result<Arc<dyn ModelProvider>> {
        if let Some(provider_id) = task.provider_hint() {
            return self
                .providers
                .iter()
                .find(|provider| provider.id() == provider_id && provider.supports(task))
                .cloned()
                .with_context(|| {
                    format!(
                        "requested provider [{}] does not support {}",
                        provider_id,
                        task.kind.as_str()
                    )
                });
        }

        self.providers
            .iter()
            .find(|provider| provider.supports(task))
            .cloned()
            .with_context(|| format!("no provider registered for {}", task.kind.as_str()))
    }
}

pub fn serialize_audio_artifact(artifact: &AudioArtifact) -> Result<String> {
    Ok(json!({
        "kind": "audio_artifact",
        "provider": artifact.provider,
        "model": artifact.model,
        "voice_id": artifact.voice_id,
        "mime_type": artifact.mime_type,
        "output_format": artifact.output_format,
        "audio_base64": BASE64_STANDARD.encode(&artifact.audio_bytes),
    })
    .to_string())
}

async fn fetch_config_string(ipc_client: &mut PhiloticClient, key: &str) -> Result<Option<String>> {
    let response = ipc_client
        .send_request(IpcRequest::GetConfig { key: key.into() })
        .await?;

    let value = match response {
        IpcResponse::ConfigData {
            key: _,
            value_json: Some(value_json),
        } => {
            if let Ok(val) = serde_json::from_str::<Value>(&value_json) {
                val.as_str().map(str::to_string).or(Some(value_json))
            } else {
                Some(value_json)
            }
        }
        _ => None,
    };

    Ok(value.filter(|value| !value.trim().is_empty()))
}

#[cfg(test)]
mod tests {
    use super::{
        AudioArtifact, ControllerTask, ProviderRegistry, TaskKind, serialize_audio_artifact,
    };
    use anyhow::Result;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Arc;

    struct FakeProvider {
        id: &'static str,
        kind: TaskKind,
    }

    #[async_trait]
    impl super::ModelProvider for FakeProvider {
        fn id(&self) -> &'static str {
            self.id
        }

        fn supports(&self, task: &ControllerTask) -> bool {
            task.kind == self.kind
        }

        async fn invoke(&self, _task: &ControllerTask) -> Result<super::ProviderOutput> {
            unreachable!("invoke is not used in registry tests")
        }
    }

    #[test]
    fn infers_legacy_text_task_from_prompt() {
        let task = ControllerTask::from_value(&json!({
            "prompt": "hello",
            "user_content": "hello"
        }))
        .unwrap();

        assert_eq!(task.kind, TaskKind::TextGenerate);
        assert_eq!(task.prompt_text(), Some("hello"));
    }

    #[test]
    fn infers_voice_task_from_spoken_text_payload() {
        let task = ControllerTask::from_value(&json!({
            "kind": "voice.synthesize",
            "provider": "elevenlabs",
            "spoken_text": "Speak now",
            "display_text": "Hi there",
            "voice": "voice-123"
        }))
        .unwrap();

        assert_eq!(task.kind, TaskKind::VoiceSynthesize);
        assert_eq!(task.voice_text(), Some("Speak now"));
        assert_eq!(task.display_text(), Some("Hi there"));
        assert_eq!(task.requested_voice(), Some("voice-123"));
    }

    #[test]
    fn registry_prefers_matching_provider_hint() {
        let registry = ProviderRegistry::new(vec![
            Arc::new(FakeProvider {
                id: "gemini",
                kind: TaskKind::TextGenerate,
            }),
            Arc::new(FakeProvider {
                id: "elevenlabs",
                kind: TaskKind::VoiceSynthesize,
            }),
        ]);

        let task = ControllerTask::from_value(&json!({
            "kind": "voice.synthesize",
            "provider": "elevenlabs",
            "text": "Speak now"
        }))
        .unwrap();

        let provider = registry.resolve(&task).unwrap();
        assert_eq!(provider.id(), "elevenlabs");
    }

    #[test]
    fn audio_artifact_serializes_to_inline_json() {
        let payload = serialize_audio_artifact(&AudioArtifact {
            provider: "elevenlabs".into(),
            model: "eleven_multilingual_v2".into(),
            voice_id: "voice-123".into(),
            mime_type: "audio/mpeg".into(),
            output_format: "mp3_44100_128".into(),
            audio_bytes: b"hello".to_vec(),
        })
        .unwrap();

        assert!(payload.contains("\"kind\":\"audio_artifact\""));
        assert!(payload.contains("\"audio_base64\":\"aGVsbG8=\""));
    }
}
