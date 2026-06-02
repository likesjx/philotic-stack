use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Input ─────────────────────────────────────────────────────────────────────

/// A single modality-typed input to a capability invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CapabilityInput {
    Text {
        content: String,
    },
    Image {
        #[serde(skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        base64: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
    Audio {
        #[serde(skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        base64: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
    Document {
        #[serde(skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
}

// ── Context ───────────────────────────────────────────────────────────────────

/// Caller identity and conversation context injected by the philote pipeline.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilityContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub identity: Value,
}

// ── Constraints ───────────────────────────────────────────────────────────────

/// Hints to the provider about output format and resource limits.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilityConstraints {
    /// Desired output format: "json", "text", "markdown", etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

// ── Request ───────────────────────────────────────────────────────────────────

/// Normalized input envelope for any capability invocation.
///
/// The same shape covers `image.ocr`, `text.generate`, `audio.transcribe`, etc.
/// Providers extract the inputs they need and ignore the rest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityRequest {
    /// Task kind, e.g. `"image.ocr"`, `"text.generate"`, `"audio.transcribe"`.
    pub task: String,
    /// Ordered inputs for this invocation (image, text prompt, audio, …).
    #[serde(default)]
    pub inputs: Vec<CapabilityInput>,
    #[serde(default)]
    pub context: CapabilityContext,
    #[serde(default)]
    pub constraints: CapabilityConstraints,
}

impl CapabilityRequest {
    pub fn new(task: impl Into<String>) -> Self {
        Self {
            task: task.into(),
            inputs: Vec::new(),
            context: CapabilityContext::default(),
            constraints: CapabilityConstraints::default(),
        }
    }

    /// Convenience: add a text input.
    pub fn with_text(mut self, content: impl Into<String>) -> Self {
        self.inputs.push(CapabilityInput::Text {
            content: content.into(),
        });
        self
    }

    /// Convenience: add an image-URL input.
    pub fn with_image_url(mut self, url: impl Into<String>) -> Self {
        self.inputs.push(CapabilityInput::Image {
            url: Some(url.into()),
            base64: None,
            mime_type: None,
        });
        self
    }

    /// Convenience: add an inline base64 image.
    pub fn with_image_base64(mut self, b64: impl Into<String>, mime_type: Option<String>) -> Self {
        self.inputs.push(CapabilityInput::Image {
            url: None,
            base64: Some(b64.into()),
            mime_type,
        });
        self
    }

    /// Return the first text input's content, if any.
    pub fn first_text(&self) -> Option<&str> {
        self.inputs.iter().find_map(|i| match i {
            CapabilityInput::Text { content } => Some(content.as_str()),
            _ => None,
        })
    }

    /// Return the first image input, if any.
    pub fn first_image(&self) -> Option<&CapabilityInput> {
        self.inputs
            .iter()
            .find(|i| matches!(i, CapabilityInput::Image { .. }))
    }

    /// Return the first audio input, if any.
    pub fn first_audio(&self) -> Option<&CapabilityInput> {
        self.inputs
            .iter()
            .find(|i| matches!(i, CapabilityInput::Audio { .. }))
    }
}

// ── Events ────────────────────────────────────────────────────────────────────

/// Token usage reported with the `Done` event.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilityUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_gen: Option<String>,
}

/// Streaming event emitted by a `CapabilityProvider`.
///
/// Generative tasks emit `Chunk` × N then `Done`.
/// Perceptive tasks emit a single `Result` then `Done`.
/// `Done` is always the last event on success; `Error` terminates on failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CapabilityEvent {
    /// Incremental text token from a generative task.
    Chunk { delta: String },
    /// Complete structured result from a perceptive task.
    Result { data: Value },
    /// The provider wants to sub-call a tool (for agentic providers).
    Tool { call: Value },
    /// Stream complete.
    Done {
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<CapabilityUsage>,
    },
    /// Unrecoverable error; no `Done` follows.
    Error {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
}

impl CapabilityEvent {
    pub fn chunk(delta: impl Into<String>) -> Self {
        Self::Chunk {
            delta: delta.into(),
        }
    }

    pub fn result(data: Value) -> Self {
        Self::Result { data }
    }

    pub fn done() -> Self {
        Self::Done { usage: None }
    }

    pub fn done_with_usage(usage: CapabilityUsage) -> Self {
        Self::Done { usage: Some(usage) }
    }

    pub fn error(message: impl Into<String>, code: Option<String>) -> Self {
        Self::Error {
            message: message.into(),
            code,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done { .. } | Self::Error { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_request_builder() {
        let req = CapabilityRequest::new("image.ocr")
            .with_image_url("file:///tmp/screenshot.png")
            .with_text("extract the table only");

        assert_eq!(req.task, "image.ocr");
        assert!(req.first_image().is_some());
        assert_eq!(req.first_text(), Some("extract the table only"));
    }

    #[test]
    fn capability_event_roundtrip() {
        let event = CapabilityEvent::result(serde_json::json!({ "text": "hello" }));
        let json = serde_json::to_string(&event).unwrap();
        let back: CapabilityEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, CapabilityEvent::Result { .. }));
    }

    #[test]
    fn done_is_terminal() {
        assert!(CapabilityEvent::done().is_terminal());
        assert!(CapabilityEvent::error("oops", None).is_terminal());
        assert!(!CapabilityEvent::chunk("hi").is_terminal());
    }
}
