//! Server-owned profile metadata for the existing ElevenLabs adapter.
//!
//! This module accepts no text/audio, credentials, endpoints or eligibility
//! flags, and has no transmission method. It is not installed in a route. Full
//! ControllerTask assembly and dispatch must use the privacy owner's verified
//! context and guarded_registry, including preview and every retry/fallback.

use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub(super) struct ServerVoiceProfile {
    pub selection_id: String,
    pub label: String,
    pub voice_id: String,
    pub model: Option<String>,
    pub stability: f64,
    pub style: Option<f64>,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ProfileError {
    InvalidConfiguration,
    UnknownSelection,
}

pub(super) struct VoiceProfileCatalog(HashMap<String, ServerVoiceProfile>);

impl VoiceProfileCatalog {
    /// Construct from server configuration, not the edge's submit/preview JSON.
    /// Numeric bounds are this catalog's conservative configuration policy;
    /// successful mapping does not prove a model honors an expressive option.
    pub fn new(profiles: Vec<ServerVoiceProfile>) -> Result<Self, ProfileError> {
        let mut entries = HashMap::new();
        for profile in profiles {
            let valid_id = |value: &str, max| !value.trim().is_empty() && value.len() <= max;
            let bounded = |value: f64| value.is_finite() && (0.0..=1.0).contains(&value);
            if !valid_id(&profile.selection_id, 64)
                || !valid_id(&profile.label, 128)
                || !valid_id(&profile.voice_id, 128)
                || profile
                    .model
                    .as_ref()
                    .is_some_and(|model| !valid_id(model, 128))
                || !bounded(profile.stability)
                || profile.style.is_some_and(|style| !bounded(style))
                || entries.contains_key(&profile.selection_id)
            {
                return Err(ProfileError::InvalidConfiguration);
            }
            entries.insert(profile.selection_id.clone(), profile);
        }
        Ok(Self(entries))
    }

    /// Configuration hints only, not a dispatch-ready task or privacy permit.
    /// Unknown selections deny instead of falling back to a default voice.
    pub fn synthesis_hints(&self, selection_id: &str) -> Result<Value, ProfileError> {
        let profile = self
            .0
            .get(selection_id)
            .ok_or(ProfileError::UnknownSelection)?;
        let mut settings = json!({"stability": profile.stability});
        if let Some(style) = profile.style {
            settings["style"] = json!(style);
        }
        let mut hints = json!({"provider":"elevenlabs", "voice":profile.voice_id,
            "provider_options":{"voice_settings":settings}});
        if let Some(model) = &profile.model {
            hints["model"] = json!(model);
        }
        Ok(hints)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> ServerVoiceProfile {
        ServerVoiceProfile {
            selection_id: "synthetic-expressive".into(),
            label: "Synthetic profile".into(),
            voice_id: "mock-voice".into(),
            model: Some("mock-model".into()),
            stability: 0.4,
            style: Some(0.2),
        }
    }
    #[test]
    fn mock_profile_maps_to_existing_adapter_metadata_without_content_or_authority() {
        let catalog = VoiceProfileCatalog::new(vec![fixture()]).unwrap();
        let hints = catalog.synthesis_hints("synthetic-expressive").unwrap();
        assert_eq!(
            hints,
            json!({"provider":"elevenlabs", "voice":"mock-voice", "model":"mock-model",
            "provider_options":{"voice_settings":{"stability":0.4,"style":0.2}}})
        );
        for key in [
            "text",
            "spoken_text",
            "audio",
            "api_key",
            "endpoint",
            "private",
            "egress",
            "actor",
        ] {
            assert!(hints.get(key).is_none());
        }
    }
    #[test]
    fn unknown_profile_and_duplicate_configuration_deny_without_fallback() {
        let catalog = VoiceProfileCatalog::new(vec![fixture()]).unwrap();
        assert_eq!(
            catalog.synthesis_hints("unknown"),
            Err(ProfileError::UnknownSelection)
        );
        assert!(matches!(
            VoiceProfileCatalog::new(vec![fixture(), fixture()]),
            Err(ProfileError::InvalidConfiguration)
        ));
    }
    #[test]
    fn malformed_expressive_configuration_denies() {
        for value in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
            let mut profile = fixture();
            profile.stability = value;
            assert!(matches!(
                VoiceProfileCatalog::new(vec![profile]),
                Err(ProfileError::InvalidConfiguration)
            ));
        }
        let mut profile = fixture();
        profile.voice_id.clear();
        assert!(matches!(
            VoiceProfileCatalog::new(vec![profile]),
            Err(ProfileError::InvalidConfiguration)
        ));
    }
}
