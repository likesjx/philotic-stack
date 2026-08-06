//! Auxiliary-task model pinning (Model Failover Layers proposal, Slice 4 — final slice).
//!
//! Cognitive turns (`text.generate` / `response.generate` / `voice.dialogue`) ride
//! philote's session/turn-scoped fallback ladder (Slices 1-3). Auxiliary tasks —
//! media analysis, transcription, embeddings — are dispatched and must degrade
//! **independently** of that ladder: a pinned provider is tried first, then each
//! `fallback_chain` entry gets exactly one attempt, in order, before the task
//! degrades per its own rules (see [`AuxTaskKind`] doc comments).
//!
//! ## Deviations from the proposal's sketch (documented, not silent)
//!
//! - **No `TitleGen` / `aux_model.title`.** The controller's [`TaskKind`] has no
//!   task kind distinguishing "generate a title" from an ordinary
//!   `text.generate` prompt — title generation is just a text-generate prompt
//!   today. Inventing a task kind with no real dispatch signal behind it would
//!   violate "trust symbols, not the proposal's line numbers," so this slice
//!   supports only the three aux kinds that map onto a real, distinct
//!   `TaskKind`: [`AuxTaskKind::Summarization`], [`AuxTaskKind::Transcription`],
//!   [`AuxTaskKind::Embedding`].
//! - **No `AuxProvider::Main` variant.** The proposal sketched
//!   `Auto | Main | Named(String)`; there is no existing "main provider"
//!   concept in the controller to point `Main` at, so this slice collapses to
//!   `Auto` (no config / absent-equivalent) vs. a `Named` pin (any non-empty,
//!   non-`"auto"` provider string in config). This preserves 100% of the
//!   required behavior (`Auto` = today; pinned = new) without inventing a
//!   third state nothing in the codebase can distinguish.
//! - **`aux_model.summarization` covers ALL of `TaskKind::MediaAnalyze`,
//!   not just literal document-summarize prompts.** Philote's
//!   `action_to_capability` collapses `summarize` *and* `describe` actions
//!   onto the single `media.analyze` task kind — the controller has no signal
//!   left to split them once the task lands here. Pinning "summarization" to
//!   a text-only model will also capture `image.describe` (vision) tasks. This
//!   is a real limitation of the existing symbol set, not a design choice of
//!   this slice — flagged prominently so an operator configuring this key
//!   isn't surprised.

use crate::controller::{
    ControllerTask, ProviderOutput, ProviderRegistry, TaskKind, fetch_config_string,
};
use anyhow::Result;
use philotic_client::PhiloticClient;
use serde::Deserialize;
use std::time::Duration;

/// Auxiliary (non-cognitive) task kinds that can be pinned to a specific
/// provider/model independently of the cognitive fallback ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuxTaskKind {
    /// `TaskKind::MediaAnalyze` (`document.summarize` + `image.describe`
    /// capabilities on the philote side — see module docs).
    Summarization,
    /// `TaskKind::AudioTranscribe`.
    Transcription,
    /// `TaskKind::Embed`. On chain exhaustion this kind alone degrades to
    /// today's Auto path (the ONNX sidecar) rather than surfacing the error —
    /// embeddings must never leave a caller (e.g. memory indexing) hard-failed
    /// when a purely optional pin/chain is misconfigured.
    Embedding,
}

impl AuxTaskKind {
    /// The `node_config` key this aux kind is configured under.
    pub fn config_key(&self) -> &'static str {
        match self {
            Self::Summarization => "aux_model.summarization",
            Self::Transcription => "aux_model.transcription",
            Self::Embedding => "aux_model.embedding",
        }
    }

    /// Map a dispatched task's [`TaskKind`] onto the aux kind it configures,
    /// if any. Returns `None` for cognitive/synthesis kinds (`TextGenerate`,
    /// `ResponseGenerate`, `VoiceDialogue`, `VoiceSynthesize`) — those are out
    /// of scope for this slice (see module docs on `TitleGen`).
    pub fn from_task_kind(kind: TaskKind) -> Option<Self> {
        match kind {
            TaskKind::MediaAnalyze => Some(Self::Summarization),
            TaskKind::AudioTranscribe => Some(Self::Transcription),
            TaskKind::Embed => Some(Self::Embedding),
            TaskKind::TextGenerate
            | TaskKind::ResponseGenerate
            | TaskKind::VoiceDialogue
            | TaskKind::VoiceSynthesize => None,
        }
    }
}

/// A resolved, non-Auto pin for one aux task kind.
///
/// Absent from [`AuxModelConfig`] (or explicitly `"provider": "auto"`) means
/// "Auto" — today's default resolution path, entirely unaffected by this
/// module. Only a `Named` pin (this struct) changes dispatch behavior.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuxiliaryTaskModel {
    pub provider: String,
    pub model: Option<String>,
    /// `(provider, model)` pairs walked in order after the pin fails, each
    /// getting exactly one attempt.
    pub fallback_chain: Vec<(String, String)>,
}

impl AuxiliaryTaskModel {
    /// `(provider, model_override)` pairs to try in order: the pin first,
    /// then each `fallback_chain` entry. Never empty.
    pub fn attempt_targets(&self) -> Vec<(String, Option<String>)> {
        let mut targets = vec![(self.provider.clone(), self.model.clone())];
        targets.extend(
            self.fallback_chain
                .iter()
                .map(|(provider, model)| (provider.clone(), Some(model.clone()))),
        );
        targets
    }
}

/// Raw shape of the `aux_model.<kind>` config value:
/// `{"provider": "...", "model": "...", "fallback_chain": [["provider","model"], ...]}`.
#[derive(Debug, Deserialize, Default)]
struct RawAuxModel {
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    fallback_chain: Vec<(String, String)>,
}

/// Resolved aux-model config for all three real aux kinds, loaded once per
/// dispatch cycle alongside [`crate::controller::ProviderConfigs::load`].
///
/// `Default` (all `None`) is exactly "Auto for everything" — the config-absent
/// case this slice's backward-compat guarantee is anchored on.
#[derive(Debug, Clone, Default)]
pub struct AuxModelConfig {
    summarization: Option<AuxiliaryTaskModel>,
    transcription: Option<AuxiliaryTaskModel>,
    embedding: Option<AuxiliaryTaskModel>,
}

impl AuxModelConfig {
    pub async fn load(ipc_client: &mut PhiloticClient) -> Result<Self> {
        Ok(Self {
            summarization: Self::load_one(ipc_client, AuxTaskKind::Summarization).await,
            transcription: Self::load_one(ipc_client, AuxTaskKind::Transcription).await,
            embedding: Self::load_one(ipc_client, AuxTaskKind::Embedding).await,
        })
    }

    async fn load_one(
        ipc_client: &mut PhiloticClient,
        kind: AuxTaskKind,
    ) -> Option<AuxiliaryTaskModel> {
        let raw = match fetch_config_string(ipc_client, kind.config_key()).await {
            Ok(value) => value?,
            Err(err) => {
                // Never fail the config load — a bad/unreachable config
                // fetch degrades this one aux kind to Auto, exactly like a
                // missing key or malformed JSON does below.
                tracing::warn!(
                    key = kind.config_key(),
                    error = %err,
                    "aux_model config fetch failed; treating as Auto"
                );
                return None;
            }
        };
        Self::parse_one(&raw, kind)
    }

    /// Parse one `aux_model.<kind>` value. Missing key (caller never reaches
    /// here), malformed JSON, an unexpected shape, or an explicit
    /// `"provider": "auto"` all resolve to `None` (Auto) — this function
    /// never returns an `Err`; it only warns and degrades.
    fn parse_one(raw: &str, kind: AuxTaskKind) -> Option<AuxiliaryTaskModel> {
        let parsed: RawAuxModel = match serde_json::from_str(raw) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(
                    key = kind.config_key(),
                    error = %err,
                    "aux_model config is not valid JSON; treating as Auto"
                );
                return None;
            }
        };

        let provider = parsed.provider?;
        let provider = provider.trim();
        if provider.is_empty() || provider.eq_ignore_ascii_case("auto") {
            return None;
        }

        Some(AuxiliaryTaskModel {
            provider: provider.to_string(),
            model: parsed
                .model
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty()),
            fallback_chain: parsed
                .fallback_chain
                .into_iter()
                .filter(|(p, m)| !p.trim().is_empty() && !m.trim().is_empty())
                .collect(),
        })
    }

    /// The resolved pin for `kind`, or `None` if it's Auto (absent config,
    /// malformed config, or an explicit `"auto"`).
    pub fn for_kind(&self, kind: AuxTaskKind) -> Option<&AuxiliaryTaskModel> {
        match kind {
            AuxTaskKind::Summarization => self.summarization.as_ref(),
            AuxTaskKind::Transcription => self.transcription.as_ref(),
            AuxTaskKind::Embedding => self.embedding.as_ref(),
        }
    }
}

/// Outcome of walking an [`AuxiliaryTaskModel`]'s attempt targets.
pub type AuxDispatchResult =
    std::result::Result<(String, ProviderOutput), (Option<String>, anyhow::Error)>;

/// Walk `aux_model`'s attempt targets (pin, then `fallback_chain` in order),
/// giving each **one** attempt via `provider.invoke()`. Returns on the first
/// success; on total exhaustion returns the last provider tried (if any
/// target resolved to a registered provider at all) and the last error.
///
/// Deliberately reuses only the *shape* of the existing dispatch retry loop
/// (resolve → bounded invoke → classify-and-continue-on-failure), not its
/// full machinery (no per-provider retry policy, no streaming, no gemini
/// credential-pool rotation, no routing-reflex substitution): each chain
/// entry is a single, explicit attempt by design — see proposal deliverable
/// 3 ("each entry gets ONE attempt"). A pin's own transient hiccups are
/// exactly what the next chain entry exists to route around, so retrying the
/// same entry internally first would just delay the degrade.
pub async fn dispatch_aux_chain(
    base_task: &ControllerTask,
    aux_model: &AuxiliaryTaskModel,
    registry: &ProviderRegistry,
) -> AuxDispatchResult {
    let mut last_err = anyhow::anyhow!("aux dispatch: no attempt targets configured");
    let mut last_provider: Option<String> = None;

    for (provider_id, model_override) in aux_model.attempt_targets() {
        let mut task = base_task.clone();
        task.provider = Some(provider_id.clone());
        if let Some(model) = model_override {
            task.model = Some(model);
        }

        let provider = match registry.resolve(&task) {
            Ok(provider) => provider,
            Err(err) => {
                last_provider = Some(provider_id);
                last_err = err;
                continue;
            }
        };
        last_provider = Some(provider.id().to_string());

        let attempt_secs = provider.attempt_policy().total_secs;
        match tokio::time::timeout(Duration::from_secs(attempt_secs), provider.invoke(&task)).await
        {
            Ok(Ok(output)) => return Ok((provider.id().to_string(), output)),
            Ok(Err(err)) => {
                last_err = err;
            }
            Err(_) => {
                last_err = anyhow::anyhow!(
                    "aux dispatch: attempt on provider [{}] exceeded {}s budget",
                    provider.id(),
                    attempt_secs
                );
            }
        }
    }

    Err((last_provider, last_err))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::ModelProvider;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── AuxTaskKind mapping ───────────────────────────────────────────────

    #[test]
    fn maps_the_three_real_aux_kinds() {
        assert_eq!(
            AuxTaskKind::from_task_kind(TaskKind::MediaAnalyze),
            Some(AuxTaskKind::Summarization)
        );
        assert_eq!(
            AuxTaskKind::from_task_kind(TaskKind::AudioTranscribe),
            Some(AuxTaskKind::Transcription)
        );
        assert_eq!(
            AuxTaskKind::from_task_kind(TaskKind::Embed),
            Some(AuxTaskKind::Embedding)
        );
    }

    #[test]
    fn cognitive_and_synthesis_kinds_are_not_aux() {
        assert_eq!(AuxTaskKind::from_task_kind(TaskKind::TextGenerate), None);
        assert_eq!(
            AuxTaskKind::from_task_kind(TaskKind::ResponseGenerate),
            None
        );
        assert_eq!(AuxTaskKind::from_task_kind(TaskKind::VoiceDialogue), None);
        assert_eq!(AuxTaskKind::from_task_kind(TaskKind::VoiceSynthesize), None);
    }

    #[test]
    fn config_keys_match_proposal_names() {
        assert_eq!(
            AuxTaskKind::Summarization.config_key(),
            "aux_model.summarization"
        );
        assert_eq!(
            AuxTaskKind::Transcription.config_key(),
            "aux_model.transcription"
        );
        assert_eq!(AuxTaskKind::Embedding.config_key(), "aux_model.embedding");
    }

    // ── Config parsing ───────────────────────────────────────────────────

    #[test]
    fn parses_valid_pin_with_fallback_chain() {
        let raw = json!({
            "provider": "openai",
            "model": "gpt-4o-mini",
            "fallback_chain": [["gemini", "gemini-2.5-flash"], ["ollama", "gemma4:e4b"]],
        })
        .to_string();

        let parsed = AuxModelConfig::parse_one(&raw, AuxTaskKind::Summarization).unwrap();
        assert_eq!(parsed.provider, "openai");
        assert_eq!(parsed.model.as_deref(), Some("gpt-4o-mini"));
        assert_eq!(
            parsed.fallback_chain,
            vec![
                ("gemini".to_string(), "gemini-2.5-flash".to_string()),
                ("ollama".to_string(), "gemma4:e4b".to_string()),
            ]
        );
    }

    #[test]
    fn parses_pin_without_model_or_chain() {
        let raw = json!({"provider": "openai"}).to_string();
        let parsed = AuxModelConfig::parse_one(&raw, AuxTaskKind::Transcription).unwrap();
        assert_eq!(parsed.provider, "openai");
        assert_eq!(parsed.model, None);
        assert!(parsed.fallback_chain.is_empty());
    }

    #[test]
    fn explicit_auto_provider_is_none() {
        let raw = json!({"provider": "auto"}).to_string();
        assert!(AuxModelConfig::parse_one(&raw, AuxTaskKind::Embedding).is_none());
        let raw_case = json!({"provider": "Auto"}).to_string();
        assert!(AuxModelConfig::parse_one(&raw_case, AuxTaskKind::Embedding).is_none());
    }

    #[test]
    fn missing_provider_field_is_none() {
        let raw = json!({"model": "gpt-4o-mini"}).to_string();
        assert!(AuxModelConfig::parse_one(&raw, AuxTaskKind::Summarization).is_none());
    }

    #[test]
    fn malformed_json_warns_and_degrades_to_auto_never_errors() {
        // The function signature itself can't return Err — this asserts the
        // degrade-not-fail contract holds for garbage input.
        assert!(AuxModelConfig::parse_one("{not json", AuxTaskKind::Embedding).is_none());
        assert!(AuxModelConfig::parse_one("", AuxTaskKind::Embedding).is_none());
        assert!(AuxModelConfig::parse_one("null", AuxTaskKind::Embedding).is_none());
        assert!(AuxModelConfig::parse_one("\"just a string\"", AuxTaskKind::Embedding).is_none());
    }

    #[test]
    fn default_config_is_auto_for_all_kinds() {
        let config = AuxModelConfig::default();
        assert!(config.for_kind(AuxTaskKind::Summarization).is_none());
        assert!(config.for_kind(AuxTaskKind::Transcription).is_none());
        assert!(config.for_kind(AuxTaskKind::Embedding).is_none());
    }

    // ── attempt_targets ───────────────────────────────────────────────────

    #[test]
    fn attempt_targets_puts_pin_first_then_chain_in_order() {
        let model = AuxiliaryTaskModel {
            provider: "openai".into(),
            model: Some("gpt-4o-mini".into()),
            fallback_chain: vec![
                ("gemini".into(), "gemini-2.5-flash".into()),
                ("ollama".into(), "gemma4:e4b".into()),
            ],
        };
        assert_eq!(
            model.attempt_targets(),
            vec![
                ("openai".to_string(), Some("gpt-4o-mini".to_string())),
                ("gemini".to_string(), Some("gemini-2.5-flash".to_string())),
                ("ollama".to_string(), Some("gemma4:e4b".to_string())),
            ]
        );
    }

    #[test]
    fn attempt_targets_never_empty_even_without_chain() {
        let model = AuxiliaryTaskModel {
            provider: "openai".into(),
            model: None,
            fallback_chain: Vec::new(),
        };
        assert_eq!(model.attempt_targets(), vec![("openai".to_string(), None)]);
    }

    // ── dispatch_aux_chain ────────────────────────────────────────────────

    /// A provider whose `invoke` either always fails, or succeeds only after
    /// its id matches one of `succeed_on`. Tracks every task it was invoked
    /// with so tests can assert which (provider, model) pairs were actually
    /// tried, and in what order.
    /// (model_id, prompt) pairs recorded by the stub provider.
    type InvocationLog = Arc<std::sync::Mutex<Vec<(String, Option<String>)>>>;

    struct ScriptedProvider {
        id: &'static str,
        fail: bool,
        invocations: InvocationLog,
        call_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelProvider for ScriptedProvider {
        fn id(&self) -> &'static str {
            self.id
        }

        fn supports(&self, task: &ControllerTask) -> bool {
            task.kind == TaskKind::MediaAnalyze || task.kind == TaskKind::Embed
        }

        async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            self.invocations
                .lock()
                .unwrap()
                .push((self.id.to_string(), task.model.clone()));
            if self.fail {
                anyhow::bail!("scripted failure from provider [{}]", self.id);
            }
            Ok(ProviderOutput::Text {
                content: format!("ok from {}", self.id),
                display_text: None,
                spoken_text: None,
                partial_replies: Vec::new(),
                working_memory_delta: None,
                follow_up_questions: Vec::new(),
                intent_summary: None,
                memory_concept: None,
                memory_candidate: None,
                active_plan: None,
                model_gen: Some(format!("{}-model", self.id)),
            })
        }
    }

    fn media_analyze_task() -> ControllerTask {
        ControllerTask::from_value(&json!({
            "kind": "media.analyze",
            "prompt": "Summarize this document.",
            "attachments": [{
                "kind": "document",
                "file_id": "doc-1",
                "mime_type": "text/plain",
                "blob_download_url": "http://127.0.0.1:9001/download/sha256-doc-1"
            }],
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn pinned_provider_resolves_on_first_try() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let count = Arc::new(AtomicUsize::new(0));
        let registry = ProviderRegistry::new(vec![Arc::new(ScriptedProvider {
            id: "openai",
            fail: false,
            invocations: log.clone(),
            call_count: count.clone(),
        })]);
        let aux_model = AuxiliaryTaskModel {
            provider: "openai".into(),
            model: Some("gpt-4o-mini".into()),
            fallback_chain: Vec::new(),
        };

        let result = dispatch_aux_chain(&media_analyze_task(), &aux_model, &registry).await;

        let (provider_id, output) = result.expect("pinned provider should succeed");
        assert_eq!(provider_id, "openai");
        assert!(matches!(output, ProviderOutput::Text { .. }));
        assert_eq!(count.load(Ordering::SeqCst), 1, "exactly one attempt");
        assert_eq!(
            log.lock().unwrap().as_slice(),
            &[("openai".to_string(), Some("gpt-4o-mini".to_string()))]
        );
    }

    #[tokio::test]
    async fn failure_walks_fallback_chain_in_order_one_attempt_each() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let registry = ProviderRegistry::new(vec![
            Arc::new(ScriptedProvider {
                id: "openai",
                fail: true,
                invocations: log.clone(),
                call_count: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(ScriptedProvider {
                id: "gemini",
                fail: true,
                invocations: log.clone(),
                call_count: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(ScriptedProvider {
                id: "ollama",
                fail: false,
                invocations: log.clone(),
                call_count: Arc::new(AtomicUsize::new(0)),
            }),
        ]);
        let aux_model = AuxiliaryTaskModel {
            provider: "openai".into(),
            model: None,
            fallback_chain: vec![
                ("gemini".into(), "gemini-2.5-flash".into()),
                ("ollama".into(), "gemma4:e4b".into()),
            ],
        };

        let result = dispatch_aux_chain(&media_analyze_task(), &aux_model, &registry).await;

        let (provider_id, _) = result.expect("chain should eventually succeed on ollama");
        assert_eq!(provider_id, "ollama");
        assert_eq!(
            log.lock().unwrap().as_slice(),
            &[
                ("openai".to_string(), None),
                ("gemini".to_string(), Some("gemini-2.5-flash".to_string())),
                ("ollama".to_string(), Some("gemma4:e4b".to_string())),
            ],
            "each chain entry tried exactly once, in order"
        );
    }

    #[tokio::test]
    async fn chain_exhaustion_returns_last_provider_and_error() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let registry = ProviderRegistry::new(vec![
            Arc::new(ScriptedProvider {
                id: "openai",
                fail: true,
                invocations: log.clone(),
                call_count: Arc::new(AtomicUsize::new(0)),
            }),
            Arc::new(ScriptedProvider {
                id: "gemini",
                fail: true,
                invocations: log.clone(),
                call_count: Arc::new(AtomicUsize::new(0)),
            }),
        ]);
        let aux_model = AuxiliaryTaskModel {
            provider: "openai".into(),
            model: None,
            fallback_chain: vec![("gemini".into(), "gemini-2.5-flash".into())],
        };

        let result = dispatch_aux_chain(&media_analyze_task(), &aux_model, &registry).await;

        let (last_provider, err) = result.expect_err("both entries fail");
        assert_eq!(last_provider.as_deref(), Some("gemini"));
        assert!(err.to_string().contains("gemini"));
    }

    #[tokio::test]
    async fn unregistered_pinned_provider_falls_through_to_chain() {
        // The pin names a provider that isn't registered for this task kind
        // at all (e.g. operator typo, or a provider that doesn't support
        // media.analyze) — resolve() fails immediately, and the chain still
        // gets its turn.
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let registry = ProviderRegistry::new(vec![Arc::new(ScriptedProvider {
            id: "ollama",
            fail: false,
            invocations: log.clone(),
            call_count: Arc::new(AtomicUsize::new(0)),
        })]);
        let aux_model = AuxiliaryTaskModel {
            provider: "not-a-real-provider".into(),
            model: None,
            fallback_chain: vec![("ollama".into(), "gemma4:e4b".into())],
        };

        let result = dispatch_aux_chain(&media_analyze_task(), &aux_model, &registry).await;

        let (provider_id, _) = result.expect("chain entry should still be tried");
        assert_eq!(provider_id, "ollama");
    }
}
