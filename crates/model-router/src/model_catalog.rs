use ansible_mesh_core::model_catalog::{
    ModelCapabilityNode, ModelCatalog, ModelEndpointRecord, ModelProviderRecord, ModelRecord,
    ModelScoreWeights, ModelVariantRecord,
};

pub fn shared_model_catalog() -> ModelCatalog {
    let catalog = ModelCatalog {
        version: "2026-03-26".into(),
        capability_tree: capability_tree(),
        providers: provider_records(),
        models: model_records(),
    };

    debug_assert!(
        catalog.validate().is_ok(),
        "shared model catalog should remain internally consistent"
    );

    catalog
}

fn capability_tree() -> Vec<ModelCapabilityNode> {
    vec![
        ModelCapabilityNode {
            capability_id: "response.generate".into(),
            display_name: "Response Generate".into(),
            description: Some(
                "Native multimodal response paths that can emit more than plain text.".into(),
            ),
            children: vec![ModelCapabilityNode {
                capability_id: "voice.dialogue".into(),
                display_name: "Voice Dialogue".into(),
                description: Some("Low-latency spoken interaction loops.".into()),
                children: vec![],
            }],
        },
        ModelCapabilityNode {
            capability_id: "text.generate".into(),
            display_name: "Text Generate".into(),
            description: Some("General text generation and cognitive turns.".into()),
            children: vec![ModelCapabilityNode {
                capability_id: "media.analyze".into(),
                display_name: "Media Analyze".into(),
                description: Some("Attachment-aware reasoning over image, audio, or video.".into()),
                children: vec![],
            }],
        },
        ModelCapabilityNode {
            capability_id: "voice.synthesize".into(),
            display_name: "Voice Synthesize".into(),
            description: Some("Text-to-speech artifact generation.".into()),
            children: vec![],
        },
        ModelCapabilityNode {
            capability_id: "speech.transcribe".into(),
            display_name: "Speech Transcribe".into(),
            description: Some("Speech-to-text or broader audio transcription.".into()),
            children: vec![],
        },
        ModelCapabilityNode {
            capability_id: "text.embed".into(),
            display_name: "Text Embed".into(),
            description: Some("Embedding/vectorization workloads.".into()),
            children: vec![],
        },
    ]
}

fn provider_records() -> Vec<ModelProviderRecord> {
    vec![
        ModelProviderRecord {
            provider_id: "gemini".into(),
            display_name: "Gemini".into(),
            auth_families: vec!["api_key".into(), "oauth_bearer".into()],
            endpoints: vec![
                ModelEndpointRecord {
                    endpoint_id: "gemini-rest-generate-content".into(),
                    api_family: "gemini-rest".into(),
                    transport: "https-json".into(),
                    base_url: Some("https://generativelanguage.googleapis.com".into()),
                    path_stem: Some("/v1beta/models/{model}:generateContent".into()),
                    notes: Some(
                        "Current controller path for text, media, and audio-input tasks.".into(),
                    ),
                },
                ModelEndpointRecord {
                    endpoint_id: "gemini-live-api".into(),
                    api_family: "gemini-live".into(),
                    transport: "streaming-session".into(),
                    base_url: None,
                    path_stem: None,
                    notes: Some(
                        "Reserved future endpoint family for native live/audio dialogue models."
                            .into(),
                    ),
                },
            ],
        },
        ModelProviderRecord {
            provider_id: "elevenlabs".into(),
            display_name: "ElevenLabs".into(),
            auth_families: vec!["api_key".into()],
            endpoints: vec![
                ModelEndpointRecord {
                    endpoint_id: "elevenlabs-tts".into(),
                    api_family: "elevenlabs-tts".into(),
                    transport: "https-json".into(),
                    base_url: Some("https://api.elevenlabs.io".into()),
                    path_stem: Some("/v1/text-to-speech/{voice_id}".into()),
                    notes: None,
                },
                ModelEndpointRecord {
                    endpoint_id: "elevenlabs-stt".into(),
                    api_family: "elevenlabs-stt".into(),
                    transport: "https-multipart".into(),
                    base_url: Some("https://api.elevenlabs.io".into()),
                    path_stem: Some("/v1/speech-to-text".into()),
                    notes: None,
                },
            ],
        },
        ModelProviderRecord {
            provider_id: "onnx".into(),
            display_name: "ONNX".into(),
            auth_families: vec!["local_runtime".into()],
            endpoints: vec![ModelEndpointRecord {
                endpoint_id: "onnx-local-runtime".into(),
                api_family: "onnx-local".into(),
                transport: "in-process".into(),
                base_url: None,
                path_stem: None,
                notes: Some("Local embeddings and whisper backends loaded in-process.".into()),
            }],
        },
        ModelProviderRecord {
            provider_id: "mlx".into(),
            display_name: "MLX".into(),
            auth_families: vec!["local_runtime".into()],
            endpoints: vec![
                ModelEndpointRecord {
                    endpoint_id: "mlx-openai-chat".into(),
                    api_family: "openai-compatible".into(),
                    transport: "https-json".into(),
                    base_url: None,
                    path_stem: Some("/v1/chat/completions".into()),
                    notes: Some("Per-instance MLX chat endpoint via mlx_lm.server.".into()),
                },
                ModelEndpointRecord {
                    endpoint_id: "mlx-whisper-local".into(),
                    api_family: "mlx-whisper".into(),
                    transport: "subprocess".into(),
                    base_url: None,
                    path_stem: None,
                    notes: Some("Local whisper subprocess path for transcription.".into()),
                },
            ],
        },
    ]
}

fn model_records() -> Vec<ModelRecord> {
    vec![
        ModelRecord {
            model_ref: "gemini-flash-latest".into(),
            provider_id: "gemini".into(),
            display_name: "Gemini Flash Latest".into(),
            family: Some("gemini-flash".into()),
            variant_group: Some("gemini-flash".into()),
            capabilities: vec![
                "text.generate".into(),
                "media.analyze".into(),
                "speech.transcribe".into(),
            ],
            endpoint_refs: vec!["gemini-rest-generate-content".into()],
            weights: ModelScoreWeights {
                capability: Some(4),
                speed: Some(5),
                thinking: Some(3),
                cost_efficiency: Some(4),
                tool_use: Some(4),
                audio_native: Some(2),
            },
            context_window_tokens: Some(128_000),
            max_output_tokens: Some(8_192),
            input_modalities: vec!["text".into(), "image".into(), "audio".into(), "video".into()],
            output_modalities: vec!["text".into()],
            variants: vec![],
            notes: Some(
                "Current Gemini provider default in Philotic. Future live/audio variants should land as sibling model records, not ad hoc provider flags.".into(),
            ),
        },
        ModelRecord {
            model_ref: "elevenlabs/eleven_multilingual_v2".into(),
            provider_id: "elevenlabs".into(),
            display_name: "Eleven Multilingual v2".into(),
            family: Some("elevenlabs-tts".into()),
            variant_group: Some("elevenlabs-voice".into()),
            capabilities: vec!["voice.synthesize".into()],
            endpoint_refs: vec!["elevenlabs-tts".into()],
            weights: ModelScoreWeights {
                capability: Some(3),
                speed: Some(4),
                thinking: Some(1),
                cost_efficiency: Some(3),
                tool_use: Some(0),
                audio_native: Some(5),
            },
            context_window_tokens: None,
            max_output_tokens: None,
            input_modalities: vec!["text".into()],
            output_modalities: vec!["audio".into()],
            variants: vec![],
            notes: Some("Current ElevenLabs default TTS model.".into()),
        },
        ModelRecord {
            model_ref: "elevenlabs/scribe_v1".into(),
            provider_id: "elevenlabs".into(),
            display_name: "Scribe v1".into(),
            family: Some("elevenlabs-stt".into()),
            variant_group: Some("elevenlabs-transcribe".into()),
            capabilities: vec!["speech.transcribe".into()],
            endpoint_refs: vec!["elevenlabs-stt".into()],
            weights: ModelScoreWeights {
                capability: Some(3),
                speed: Some(4),
                thinking: Some(1),
                cost_efficiency: Some(3),
                tool_use: Some(0),
                audio_native: Some(4),
            },
            context_window_tokens: None,
            max_output_tokens: None,
            input_modalities: vec!["audio".into()],
            output_modalities: vec!["text".into()],
            variants: vec![],
            notes: Some("Current ElevenLabs default speech-to-text model.".into()),
        },
        ModelRecord {
            model_ref: "onnx-community/embeddinggemma-300m-ONNX".into(),
            provider_id: "onnx".into(),
            display_name: "EmbeddingGemma 300M ONNX".into(),
            family: Some("embeddinggemma".into()),
            variant_group: Some("onnx-embeddings".into()),
            capabilities: vec!["text.embed".into()],
            endpoint_refs: vec!["onnx-local-runtime".into()],
            weights: ModelScoreWeights {
                capability: Some(3),
                speed: Some(5),
                thinking: Some(0),
                cost_efficiency: Some(5),
                tool_use: Some(0),
                audio_native: Some(0),
            },
            context_window_tokens: Some(2_048),
            max_output_tokens: None,
            input_modalities: vec!["text".into()],
            output_modalities: vec!["embedding".into()],
            variants: vec![],
            notes: Some("Default local embedding backend for the ONNX controller.".into()),
        },
        ModelRecord {
            model_ref: "onnx-community/whisper-small".into(),
            provider_id: "onnx".into(),
            display_name: "Whisper Small ONNX".into(),
            family: Some("whisper".into()),
            variant_group: Some("onnx-transcribe".into()),
            capabilities: vec!["speech.transcribe".into()],
            endpoint_refs: vec!["onnx-local-runtime".into()],
            weights: ModelScoreWeights {
                capability: Some(3),
                speed: Some(4),
                thinking: Some(0),
                cost_efficiency: Some(5),
                tool_use: Some(0),
                audio_native: Some(3),
            },
            context_window_tokens: None,
            max_output_tokens: None,
            input_modalities: vec!["audio".into()],
            output_modalities: vec!["text".into()],
            variants: vec![],
            notes: Some("Default local whisper backend for the ONNX controller.".into()),
        },
        ModelRecord {
            model_ref: "mlx/*".into(),
            provider_id: "mlx".into(),
            display_name: "MLX Fleet".into(),
            family: Some("mlx-fleet".into()),
            variant_group: Some("mlx-dynamic".into()),
            capabilities: vec!["text.generate".into(), "speech.transcribe".into()],
            endpoint_refs: vec!["mlx-openai-chat".into(), "mlx-whisper-local".into()],
            weights: ModelScoreWeights {
                capability: Some(4),
                speed: Some(4),
                thinking: Some(4),
                cost_efficiency: Some(5),
                tool_use: Some(3),
                audio_native: Some(2),
            },
            context_window_tokens: None,
            max_output_tokens: None,
            input_modalities: vec!["text".into(), "audio".into()],
            output_modalities: vec!["text".into()],
            variants: vec![ModelVariantRecord {
                variant_id: "attached-or-managed".into(),
                display_name: "Attached or Managed Instance".into(),
                weights: ModelScoreWeights::default(),
                notes: Some(
                    "Concrete MLX model IDs are fleet config data and should project into this record rather than replacing the shared schema.".into(),
                ),
            }],
            notes: Some(
                "Catalog placeholder for the MLX fleet boundary; concrete model instances are config-driven.".into(),
            ),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::shared_model_catalog;

    #[test]
    fn shared_catalog_is_valid() {
        shared_model_catalog().validate().unwrap();
    }

    #[test]
    fn shared_catalog_includes_gemini_live_endpoint_family_without_claiming_live_support() {
        let catalog = shared_model_catalog();
        let gemini = catalog
            .providers
            .iter()
            .find(|provider| provider.provider_id == "gemini")
            .unwrap();

        assert!(
            gemini
                .endpoints
                .iter()
                .any(|endpoint| endpoint.endpoint_id == "gemini-live-api")
        );
        assert!(
            catalog
                .models
                .iter()
                .all(|model| model.model_ref != "gemini-3-1-flash-live")
        );
    }
}
