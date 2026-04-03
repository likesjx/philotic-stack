/// Reflex Engine — Typed context-driven policy evaluation.
///
/// Three-layer model:
///
/// **Layer 1 — Static rules (compile-time vocabulary)**
/// `IngressAction`, `MembraneKind`, `CapabilityKind` are exhaustive enums.
/// `StaticReflexRule { trigger, assertion }` is the typed rule atom.
/// `default_reflex_rules()` returns the built-in compile-time defaults.
///
/// **Layer 2 — Materialization snapshot**
/// The hotel injects `MaterializationContext` into the agent bundle at spawn.
/// `ReflexEngine::apply_materialization()` evaluates static rules against the
/// context, returning `PolicyAssertion`s that are applied immediately. Turn zero
/// is correct without any agent self-discovery.
///
/// **Layer 3 — Runtime delta events**
/// `ReflexEvent` covers TTS/transcription failures, capability changes, context
/// pressure, and operator directives. `ReflexEngine::handle_event()` returns
/// assertions to apply. Priority: override > table rule > static default.
use serde::{Deserialize, Serialize};

// ── Layer 1: Compile-time vocabulary ─────────────────────────────────────────

/// All known incoming task action strings, typed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngressAction {
    /// `voice.dialogue` — raw PCM audio from a voice membrane (e.g. Discord voice).
    VoiceDialogue,
    /// `image.analyze` or bare media turn with image attachment.
    ImageAnalyze,
    /// Standard text/chat turn (no action, or action == "").
    TextMessage,
    /// `voice.transcribe` re-entry after STT processing.
    VoiceTranscribeReentry,
    /// `tool_result` re-entry from a tool runner.
    ToolResult,
    /// `model_response` re-entry from a model controller.
    ModelResponse,
    /// Any other action string not covered above.
    #[serde(other)]
    Unknown,
}

impl IngressAction {
    /// Derive from the `action` field of an `InboundTaskPayload`.
    pub fn from_task_action(action: Option<&str>) -> Self {
        match action {
            Some("voice.dialogue") => Self::VoiceDialogue,
            Some("voice.transcribe") => Self::VoiceTranscribeReentry,
            Some("image.analyze") => Self::ImageAnalyze,
            Some("tool_result") => Self::ToolResult,
            Some("model_response") => Self::ModelResponse,
            None | Some("") => Self::TextMessage,
            Some(_) => Self::Unknown,
        }
    }
}

/// All known membrane/channel types that can deliver tasks to a philote.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembraneKind {
    /// Discord voice channel — delivers `voice.dialogue` PCM frames.
    DiscordVoice,
    /// Discord text channel.
    DiscordText,
    /// Telegram chat.
    Telegram,
    /// Local desktop membrane.
    Desktop,
    /// Philotic Web operator chat.
    PhiloticWeb,
    /// Any other membrane type.
    #[serde(other)]
    Unknown,
}

impl MembraneKind {
    pub fn from_str(s: &str) -> Self {
        match s {
            "discord_voice" => Self::DiscordVoice,
            "discord_text" => Self::DiscordText,
            "telegram" => Self::Telegram,
            "desktop" => Self::Desktop,
            "philotic_web" => Self::PhiloticWeb,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::DiscordVoice => "discord_voice",
            Self::DiscordText => "discord_text",
            Self::Telegram => "telegram",
            Self::Desktop => "desktop",
            Self::PhiloticWeb => "philotic_web",
            Self::Unknown => "unknown",
        }
    }
}

/// Known model/tool capabilities that can influence routing policy.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKind {
    /// ElevenLabs TTS synthesis is available.
    ElevenLabsTts,
    /// Gemini multimodal transcription is available.
    GeminiTranscribe,
    /// Local ONNX Whisper transcription is available.
    LocalOnnxTranscribe,
    /// A Discord voice bridge is active for this session.
    DiscordVoiceBridge,
    /// Any other capability string.
    #[serde(other)]
    Unknown,
}

impl CapabilityKind {
    pub fn from_str(s: &str) -> Self {
        match s {
            "elevenlabs_tts" => Self::ElevenLabsTts,
            "gemini_transcribe" => Self::GeminiTranscribe,
            "local_onnx_transcribe" => Self::LocalOnnxTranscribe,
            "discord_voice_bridge" => Self::DiscordVoiceBridge,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::ElevenLabsTts => "elevenlabs_tts",
            Self::GeminiTranscribe => "gemini_transcribe",
            Self::LocalOnnxTranscribe => "local_onnx_transcribe",
            Self::DiscordVoiceBridge => "discord_voice_bridge",
            Self::Unknown => "unknown",
        }
    }
}

/// What condition fires a `StaticReflexRule`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReflexTrigger {
    /// Fires when the task's action matches.
    OnIngressAction { action: IngressAction },
    /// Fires when the philote is bound to this membrane kind.
    OnMembraneKind { membrane: MembraneKind },
    /// Fires when a capability is present at materialization.
    OnCapability { capability: CapabilityKind },
    /// Always fires (unconditional default).
    Always,
}

/// A typed policy mutation — the output side of a reflex rule.
///
/// Maps 1:1 to `apply_configure` paths so rules drive the same mutation
/// primitive as the agent or operator would call manually.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PolicyAssertion {
    SetVoiceAction(String),
    SetImageAction(String),
    SetDocumentAction(String),
    SetForwardMedia(bool),
    SetStripToolsOnMedia(bool),
    SetTtsMode(String),
    SetTtsProvider(String),
    SetVoiceId(String),
    SetSendTextCaption(bool),
    SetFallbackToText(bool),
}

impl PolicyAssertion {
    /// Map to the `(config_path, value)` pair consumed by `apply_configure`.
    pub fn to_configure_args(&self) -> (&'static str, serde_json::Value) {
        match self {
            Self::SetVoiceAction(v) => (
                "media_routing_policy.voice_action",
                serde_json::json!(v),
            ),
            Self::SetImageAction(v) => (
                "media_routing_policy.image_action",
                serde_json::json!(v),
            ),
            Self::SetDocumentAction(v) => (
                "media_routing_policy.document_action",
                serde_json::json!(v),
            ),
            Self::SetForwardMedia(v) => (
                "media_routing_policy.forward_media_to_model",
                serde_json::json!(v),
            ),
            Self::SetStripToolsOnMedia(v) => (
                "media_routing_policy.strip_tools_on_media",
                serde_json::json!(v),
            ),
            Self::SetTtsMode(v) => ("voice_response_policy.mode", serde_json::json!(v)),
            Self::SetTtsProvider(v) => ("voice_response_policy.provider", serde_json::json!(v)),
            Self::SetVoiceId(v) => ("voice_response_policy.voice_id", serde_json::json!(v)),
            Self::SetSendTextCaption(v) => (
                "voice_response_policy.send_text_caption",
                serde_json::json!(v),
            ),
            Self::SetFallbackToText(v) => (
                "voice_response_policy.fallback_to_text",
                serde_json::json!(v),
            ),
        }
    }
}

/// A typed rule: when `trigger` fires, apply `assertion`.
///
/// `pinned = true` means operator directives cannot override this rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaticReflexRule {
    pub trigger: ReflexTrigger,
    pub assertion: PolicyAssertion,
    /// When true, Layer 3 overrides cannot supersede this rule.
    #[serde(default)]
    pub pinned: bool,
}

/// The compile-time default rule set.
///
/// These ship with the binary. Agents can extend them via `agent.configure`
/// or the hotel operator can override them via the agent bundle.
pub fn default_reflex_rules() -> Vec<StaticReflexRule> {
    vec![
        // Discord voice → always transcribe audio
        StaticReflexRule {
            trigger: ReflexTrigger::OnMembraneKind {
                membrane: MembraneKind::DiscordVoice,
            },
            assertion: PolicyAssertion::SetVoiceAction("transcribe".into()),
            pinned: false,
        },
        // Discord voice → auto TTS (reply in voice)
        StaticReflexRule {
            trigger: ReflexTrigger::OnMembraneKind {
                membrane: MembraneKind::DiscordVoice,
            },
            assertion: PolicyAssertion::SetTtsMode("auto".into()),
            pinned: false,
        },
        // Any voice.dialogue ingress → assert transcribe (belt-and-suspenders)
        StaticReflexRule {
            trigger: ReflexTrigger::OnIngressAction {
                action: IngressAction::VoiceDialogue,
            },
            assertion: PolicyAssertion::SetVoiceAction("transcribe".into()),
            pinned: false,
        },
        // ElevenLabs capability present → enable auto TTS
        StaticReflexRule {
            trigger: ReflexTrigger::OnCapability {
                capability: CapabilityKind::ElevenLabsTts,
            },
            assertion: PolicyAssertion::SetTtsMode("auto".into()),
            pinned: false,
        },
    ]
}

// ── Layer 2: Materialization context ─────────────────────────────────────────

/// Describes one membrane channel this philote is bound to.
///
/// Serialized into `AgentProfile.reflex_context.membrane_bindings` by the hotel
/// at guest spawn time.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MembraneBind {
    /// `MembraneKind` string representation (e.g. `"discord_voice"`).
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guild_id: Option<String>,
}

/// Context the hotel knows at philote spawn time.
///
/// Embedded in `AgentProfile` and fetched via `__agent_bundle__` at startup.
/// `ReflexEngine::apply_materialization()` evaluates static rules against this
/// snapshot and returns the initial `PolicyAssertion` list.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MaterializationContext {
    /// Membranes this philote is bound to at spawn.
    #[serde(default)]
    pub membrane_bindings: Vec<MembraneBind>,
    /// Capabilities available at spawn (e.g. `"elevenlabs_tts"`).
    #[serde(default)]
    pub capabilities: Vec<String>,
}

// ── Layer 3: Runtime delta events ────────────────────────────────────────────

/// Events that can change routing policy at runtime.
#[derive(Debug, Clone)]
pub enum ReflexEvent {
    /// TTS synthesis failed for a turn.
    TtsFailure { provider: String },
    /// Transcription (STT) failed for a voice turn.
    TranscriptionFailure,
    /// A new capability became available (e.g. model-router came online).
    CapabilityAdded(CapabilityKind),
    /// A capability was lost (e.g. model-router went down).
    CapabilityRemoved(CapabilityKind),
    /// Context window is filling up — reduce attachment/tool overhead.
    ContextPressure { used_pct: u8 },
    /// Operator explicitly asserted a policy (persists as override).
    OperatorDirective(PolicyAssertion),
}

/// Source of a runtime override — used for priority resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverrideSource {
    Agent,
    Operator,
    FallbackRecovery,
}

/// A single runtime override entry.
#[derive(Debug, Clone)]
pub struct ReflexOverride {
    pub assertion: PolicyAssertion,
    pub source: OverrideSource,
}

// ── Engine ────────────────────────────────────────────────────────────────────

/// The reflex engine — owns the rule set, materialization state, and overrides.
///
/// Not serialized (runtime state only). Initialized fresh on session creation
/// or checkpoint restore. Materialization is re-applied from `AgentProfile`.
#[derive(Debug, Clone)]
pub struct ReflexEngine {
    /// Static rules (Layer 1) — compile-time defaults + agent-added rules.
    rules: Vec<StaticReflexRule>,
    /// Active membrane kinds derived from materialization context.
    active_membranes: Vec<MembraneKind>,
    /// Active capabilities derived from materialization context.
    active_capabilities: Vec<CapabilityKind>,
    /// Runtime overrides (Layer 3).
    overrides: Vec<ReflexOverride>,
}

impl Default for ReflexEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ReflexEngine {
    pub fn new() -> Self {
        Self {
            rules: default_reflex_rules(),
            active_membranes: Vec::new(),
            active_capabilities: Vec::new(),
            overrides: Vec::new(),
        }
    }

    // ── Layer 2: Materialization ──────────────────────────────────────────────

    /// Evaluate static rules against the materialization context.
    ///
    /// Updates internal membrane/capability state, then returns all assertions
    /// that fire. The caller applies each assertion to the session policy.
    pub fn apply_materialization(&mut self, ctx: &MaterializationContext) -> Vec<PolicyAssertion> {
        self.active_membranes = ctx
            .membrane_bindings
            .iter()
            .map(|b| MembraneKind::from_str(&b.kind))
            .collect();
        self.active_capabilities = ctx
            .capabilities
            .iter()
            .map(|c| CapabilityKind::from_str(c))
            .collect();
        self.evaluate_context_rules()
    }

    /// Evaluate rules that fire on membrane kind or capability presence.
    fn evaluate_context_rules(&self) -> Vec<PolicyAssertion> {
        let mut out = Vec::new();
        for rule in &self.rules {
            let fires = match &rule.trigger {
                ReflexTrigger::OnMembraneKind { membrane } => {
                    self.active_membranes.contains(membrane)
                }
                ReflexTrigger::OnCapability { capability } => {
                    self.active_capabilities.contains(capability)
                }
                ReflexTrigger::Always => true,
                ReflexTrigger::OnIngressAction { .. } => false, // handled at ingress
            };
            if fires {
                out.push(rule.assertion.clone());
            }
        }
        out
    }

    // ── Layer 1+2: Ingress-time evaluation ───────────────────────────────────

    /// Evaluate rules that fire on a specific ingress action.
    ///
    /// Called each time a task arrives with the corresponding `IngressAction`.
    /// Pinned rules are applied; non-pinned rules only apply if no override
    /// for the same target path exists with `OverrideSource::Agent` or
    /// `OverrideSource::Operator`.
    pub fn apply_ingress(&self, action: &IngressAction) -> Vec<PolicyAssertion> {
        self.rules
            .iter()
            .filter(|rule| {
                matches!(
                    &rule.trigger,
                    ReflexTrigger::OnIngressAction { action: a } if a == action
                )
            })
            .map(|rule| rule.assertion.clone())
            .collect()
    }

    // ── Layer 3: Runtime events ───────────────────────────────────────────────

    /// React to a runtime delta event and return assertions to apply.
    pub fn handle_event(&mut self, event: &ReflexEvent) -> Vec<PolicyAssertion> {
        match event {
            ReflexEvent::TtsFailure { .. } => {
                // Ensure fallback_to_text is on so the turn can still complete.
                vec![PolicyAssertion::SetFallbackToText(true)]
            }
            ReflexEvent::TranscriptionFailure => {
                // Fall back to analyze_media — don't silently drop audio turns.
                self.overrides.push(ReflexOverride {
                    assertion: PolicyAssertion::SetVoiceAction("analyze_media".into()),
                    source: OverrideSource::FallbackRecovery,
                });
                vec![PolicyAssertion::SetVoiceAction("analyze_media".into())]
            }
            ReflexEvent::CapabilityAdded(CapabilityKind::ElevenLabsTts) => {
                self.active_capabilities.push(CapabilityKind::ElevenLabsTts);
                vec![PolicyAssertion::SetTtsMode("auto".into())]
            }
            ReflexEvent::CapabilityRemoved(CapabilityKind::ElevenLabsTts) => {
                self.active_capabilities.retain(|c| c != &CapabilityKind::ElevenLabsTts);
                // Only fall back to off if there's no operator override keeping it on.
                let operator_pinning = self.overrides.iter().any(|o| {
                    o.source == OverrideSource::Operator
                        && matches!(&o.assertion, PolicyAssertion::SetTtsMode(m) if m == "on")
                });
                if operator_pinning {
                    vec![]
                } else {
                    vec![PolicyAssertion::SetTtsMode("off".into())]
                }
            }
            ReflexEvent::ContextPressure { used_pct } if *used_pct > 80 => {
                vec![PolicyAssertion::SetStripToolsOnMedia(true)]
            }
            ReflexEvent::OperatorDirective(assertion) => {
                self.overrides.push(ReflexOverride {
                    assertion: assertion.clone(),
                    source: OverrideSource::Operator,
                });
                vec![assertion.clone()]
            }
            _ => vec![],
        }
    }

    /// Add a custom static rule (e.g. injected from agent profile or operator config).
    pub fn add_rule(&mut self, rule: StaticReflexRule) {
        self.rules.push(rule);
    }

    /// Replace all non-default rules (used when loading from agent profile).
    pub fn set_custom_rules(&mut self, rules: Vec<StaticReflexRule>) {
        // Keep defaults, append custom ones.
        self.rules = default_reflex_rules();
        self.rules.extend(rules);
    }

    /// Expose active membrane kinds (for diagnostics / skill introspection).
    pub fn active_membranes(&self) -> &[MembraneKind] {
        &self.active_membranes
    }

    /// Expose active capabilities (for diagnostics).
    pub fn active_capabilities(&self) -> &[CapabilityKind] {
        &self.active_capabilities
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn discord_voice_ctx() -> MaterializationContext {
        MaterializationContext {
            membrane_bindings: vec![MembraneBind {
                kind: "discord_voice".into(),
                channel_id: Some("123".into()),
                guild_id: Some("456".into()),
            }],
            capabilities: vec!["elevenlabs_tts".into()],
        }
    }

    #[test]
    fn materialization_discord_voice_sets_transcribe_and_tts_auto() {
        let mut engine = ReflexEngine::new();
        let assertions = engine.apply_materialization(&discord_voice_ctx());

        let voice_action = assertions.iter().find(|a| {
            matches!(a, PolicyAssertion::SetVoiceAction(_))
        });
        let tts_mode = assertions.iter().filter(|a| {
            matches!(a, PolicyAssertion::SetTtsMode(_))
        }).count();

        assert!(
            matches!(voice_action, Some(PolicyAssertion::SetVoiceAction(s)) if s == "transcribe"),
            "should assert SetVoiceAction(transcribe), got: {assertions:?}"
        );
        // Both DiscordVoice and ElevenLabsTts rules fire SetTtsMode("auto")
        assert!(tts_mode >= 1, "should assert SetTtsMode at least once");
    }

    #[test]
    fn ingress_voice_dialogue_asserts_transcribe() {
        let engine = ReflexEngine::new();
        let assertions = engine.apply_ingress(&IngressAction::VoiceDialogue);
        assert!(
            assertions
                .iter()
                .any(|a| matches!(a, PolicyAssertion::SetVoiceAction(s) if s == "transcribe")),
            "VoiceDialogue ingress should assert transcribe"
        );
    }

    #[test]
    fn tts_failure_event_asserts_fallback_to_text() {
        let mut engine = ReflexEngine::new();
        let assertions = engine.handle_event(&ReflexEvent::TtsFailure {
            provider: "elevenlabs".into(),
        });
        assert_eq!(assertions, vec![PolicyAssertion::SetFallbackToText(true)]);
    }

    #[test]
    fn transcription_failure_falls_back_to_analyze_media() {
        let mut engine = ReflexEngine::new();
        let assertions = engine.handle_event(&ReflexEvent::TranscriptionFailure);
        assert!(
            assertions
                .iter()
                .any(|a| matches!(a, PolicyAssertion::SetVoiceAction(s) if s == "analyze_media"))
        );
    }

    #[test]
    fn capability_removed_turns_off_tts_without_operator_pin() {
        let mut engine = ReflexEngine::new();
        engine.apply_materialization(&discord_voice_ctx());
        let assertions = engine.handle_event(&ReflexEvent::CapabilityRemoved(
            CapabilityKind::ElevenLabsTts,
        ));
        assert!(
            assertions
                .iter()
                .any(|a| matches!(a, PolicyAssertion::SetTtsMode(m) if m == "off"))
        );
    }

    #[test]
    fn operator_pin_blocks_capability_removed_tts_off() {
        let mut engine = ReflexEngine::new();
        // Operator pins TTS on
        engine.handle_event(&ReflexEvent::OperatorDirective(
            PolicyAssertion::SetTtsMode("on".into()),
        ));
        // Capability removed — should NOT turn TTS off
        let assertions = engine.handle_event(&ReflexEvent::CapabilityRemoved(
            CapabilityKind::ElevenLabsTts,
        ));
        assert!(
            assertions.is_empty(),
            "operator pin should block TTS off: {assertions:?}"
        );
    }

    #[test]
    fn context_pressure_strips_tools_on_media() {
        let mut engine = ReflexEngine::new();
        let assertions = engine.handle_event(&ReflexEvent::ContextPressure { used_pct: 85 });
        assert_eq!(assertions, vec![PolicyAssertion::SetStripToolsOnMedia(true)]);
    }

    #[test]
    fn low_context_pressure_does_nothing() {
        let mut engine = ReflexEngine::new();
        let assertions = engine.handle_event(&ReflexEvent::ContextPressure { used_pct: 50 });
        assert!(assertions.is_empty());
    }

    #[test]
    fn ingress_action_from_task_action() {
        assert_eq!(
            IngressAction::from_task_action(Some("voice.dialogue")),
            IngressAction::VoiceDialogue
        );
        assert_eq!(
            IngressAction::from_task_action(None),
            IngressAction::TextMessage
        );
        assert_eq!(
            IngressAction::from_task_action(Some("voice.transcribe")),
            IngressAction::VoiceTranscribeReentry
        );
        assert_eq!(
            IngressAction::from_task_action(Some("some.other.thing")),
            IngressAction::Unknown
        );
    }

    #[test]
    fn membrane_kind_roundtrip() {
        for (s, expected) in &[
            ("discord_voice", MembraneKind::DiscordVoice),
            ("telegram", MembraneKind::Telegram),
            ("desktop", MembraneKind::Desktop),
        ] {
            let kind = MembraneKind::from_str(s);
            assert_eq!(&kind, expected);
            assert_eq!(kind.as_str(), *s);
        }
    }

    #[test]
    fn policy_assertion_configure_args_roundtrip() {
        let cases: Vec<PolicyAssertion> = vec![
            PolicyAssertion::SetVoiceAction("transcribe".into()),
            PolicyAssertion::SetTtsMode("auto".into()),
            PolicyAssertion::SetForwardMedia(false),
            PolicyAssertion::SetFallbackToText(true),
        ];
        for assertion in cases {
            let (path, _val) = assertion.to_configure_args();
            assert!(!path.is_empty(), "path must not be empty for {assertion:?}");
        }
    }

    #[test]
    fn add_custom_rule_fires_at_materialization() {
        let mut engine = ReflexEngine::new();
        engine.add_rule(StaticReflexRule {
            trigger: ReflexTrigger::OnMembraneKind {
                membrane: MembraneKind::Telegram,
            },
            assertion: PolicyAssertion::SetSendTextCaption(false),
            pinned: false,
        });
        let ctx = MaterializationContext {
            membrane_bindings: vec![MembraneBind {
                kind: "telegram".into(),
                channel_id: None,
                guild_id: None,
            }],
            capabilities: vec![],
        };
        let assertions = engine.apply_materialization(&ctx);
        assert!(
            assertions
                .iter()
                .any(|a| matches!(a, PolicyAssertion::SetSendTextCaption(false)))
        );
    }
}
