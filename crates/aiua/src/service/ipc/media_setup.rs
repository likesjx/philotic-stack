//! Media setup: training samples, ASR and vision setup/status.
//!
//! Moved verbatim from `ipc/mod.rs` (IPC_DISPATCH_SPLIT); only item
//! visibility was widened so the parent module can reach it.

use super::*;

impl IpcServer {
    pub(super) fn handle_list_training_samples(
        storage: Option<&dyn ansible_mesh_core::whisper_training::WhisperTrainingStorage>,
        agent_id: Option<&str>,
        limit: usize,
        filter: &ansible_mesh_core::whisper_training::TrainingFilter,
    ) -> IpcResponse {
        let Some(storage) = storage else {
            return IpcResponse::error(
                "training_list",
                "TRAINING_STORAGE_UNAVAILABLE",
                "Training DB not configured for this hotel",
            );
        };
        let capped = limit.min(200);
        match storage.list_filtered(filter, agent_id, capped) {
            Ok(samples) => {
                let samples_json =
                    serde_json::to_value(&samples).unwrap_or(serde_json::Value::Null);
                IpcResponse::success(
                    "training_list",
                    Some(serde_json::json!({ "samples": samples_json, "count": samples.len() })),
                )
            }
            Err(e) => IpcResponse::error("training_list", "QUERY_FAILED", &e.to_string()),
        }
    }

    pub(super) fn handle_correct_training_sample(
        storage: Option<&dyn ansible_mesh_core::whisper_training::WhisperTrainingStorage>,
        turn_id: &str,
        corrected_transcript: &str,
    ) -> IpcResponse {
        let Some(storage) = storage else {
            return IpcResponse::error(
                "training_correct",
                "TRAINING_STORAGE_UNAVAILABLE",
                "Training DB not configured for this hotel",
            );
        };
        match storage.update_correction(turn_id, corrected_transcript, "operator") {
            Ok(true) => IpcResponse::success(
                "training_correct",
                Some(serde_json::json!({ "turn_id": turn_id, "updated": true })),
            ),
            Ok(false) => IpcResponse::error(
                "training_correct",
                "TURN_NOT_FOUND",
                &format!("No sample found for turn_id '{turn_id}'"),
            ),
            Err(e) => IpcResponse::error("training_correct", "UPDATE_FAILED", &e.to_string()),
        }
    }

    pub(super) fn handle_export_training_samples(
        storage: Option<&dyn ansible_mesh_core::whisper_training::WhisperTrainingStorage>,
        format: &ansible_mesh_core::whisper_training::TrainingExportFormat,
        output_path: &str,
        limit: Option<usize>,
    ) -> IpcResponse {
        use ansible_mesh_core::whisper_training::TrainingExportFormat;
        use std::io::Write;

        let Some(storage) = storage else {
            return IpcResponse::error(
                "training_export",
                "TRAINING_STORAGE_UNAVAILABLE",
                "Training DB not configured for this hotel",
            );
        };
        let capped = limit.unwrap_or(usize::MAX).min(10_000);
        let samples = match storage.list_eligible(capped) {
            Ok(s) => s,
            Err(e) => return IpcResponse::error("training_export", "QUERY_FAILED", &e.to_string()),
        };
        if samples.is_empty() {
            return IpcResponse::success(
                "training_export",
                Some(serde_json::json!({ "exported_count": 0, "output_path": output_path })),
            );
        }

        let write_result = match format {
            TrainingExportFormat::HuggingFace => {
                let records: Vec<serde_json::Value> = samples
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "audio": s.audio_path,
                            "sentence": s.corrected_transcript.as_deref().unwrap_or(&s.raw_transcript),
                            "model_gen": s.model_gen,
                            "sample_id": s.sample_id,
                        })
                    })
                    .collect();
                std::fs::write(
                    output_path,
                    serde_json::to_vec_pretty(&records).unwrap_or_default(),
                )
            }
            TrainingExportFormat::Nemo => {
                let mut file = match std::fs::File::create(output_path) {
                    Ok(f) => f,
                    Err(e) => {
                        return IpcResponse::error("training_export", "IO_ERROR", &e.to_string());
                    }
                };
                for s in &samples {
                    let record = serde_json::json!({
                        "audio_filepath": s.audio_path.as_deref().unwrap_or(""),
                        "text": s.corrected_transcript.as_deref().unwrap_or(&s.raw_transcript),
                        "duration": 0.0,
                    });
                    if let Err(e) = writeln!(file, "{}", record) {
                        return IpcResponse::error("training_export", "IO_ERROR", &e.to_string());
                    }
                }
                Ok(())
            }
        };

        if let Err(e) = write_result {
            return IpcResponse::error("training_export", "IO_ERROR", &e.to_string());
        }

        let exported_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let ids: Vec<String> = samples.iter().map(|s| s.sample_id.clone()).collect();
        let count = ids.len();
        if let Err(e) = storage.mark_exported_at(&ids, exported_at) {
            warn!(
                "training_export: failed to mark {} samples exported: {}",
                count, e
            );
        }

        IpcResponse::success(
            "training_export",
            Some(serde_json::json!({ "exported_count": count, "output_path": output_path })),
        )
    }

    pub(super) fn handle_get_training_status(
        storage: Option<&dyn ansible_mesh_core::whisper_training::WhisperTrainingStorage>,
        agent_id: Option<&str>,
    ) -> IpcResponse {
        let Some(storage) = storage else {
            return IpcResponse::error(
                "training_status",
                "TRAINING_STORAGE_UNAVAILABLE",
                "Training DB not configured for this hotel",
            );
        };
        match storage.count_status(agent_id) {
            Ok(counts) => {
                let status_json = serde_json::to_value(&counts).unwrap_or(serde_json::Value::Null);
                IpcResponse::success(
                    "training_status",
                    Some(serde_json::json!({ "status": status_json })),
                )
            }
            Err(e) => IpcResponse::error("training_status", "QUERY_FAILED", &e.to_string()),
        }
    }

    pub(super) fn handle_asr_setup(
        graph: &GraphDomain,
        local_node_id: &str,
        socket_path: &str,
        python_path: &str,
        model_name: Option<&str>,
        auto_install: bool,
    ) -> IpcResponse {
        use std::process::Command;

        let model = model_name.unwrap_or(parakeet_runner::DEFAULT_MODEL);

        // Step 1: check nemo import.
        let check_ok = Command::new(python_path)
            .args(["-c", "import nemo.collections.asr"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        let nemo_available = if !check_ok && auto_install {
            info!("asr.setup: nemo import failed; attempting pip install nemo-toolkit[asr]");
            let install = Command::new(python_path)
                .args(["-m", "pip", "install", "nemo-toolkit[asr]"])
                .output();
            match install {
                Ok(o) if o.status.success() => {
                    // Re-verify after install.
                    Command::new(python_path)
                        .args(["-c", "import nemo.collections.asr"])
                        .output()
                        .map(|o| o.status.success())
                        .unwrap_or(false)
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr).to_string();
                    warn!("asr.setup: pip install failed: {stderr}");
                    false
                }
                Err(e) => {
                    warn!("asr.setup: pip install error: {e}");
                    false
                }
            }
        } else {
            check_ok
        };

        if !nemo_available {
            let hint = format!(
                "nemo_asr import failed. Install manually: `{python_path} -m pip install nemo-toolkit[asr]`"
            );
            return IpcResponse::error("asr_setup", "NEMO_UNAVAILABLE", &hint);
        }

        // Step 2: write component config to hotel context graph.
        let Some(hotel_name) = IpcServer::local_hotel_name(graph, local_node_id) else {
            return IpcResponse::error(
                "asr_setup",
                "HOTEL_NOT_FOUND",
                "could not resolve hotel name",
            );
        };
        let guest_id = format!("{hotel_name}:{}", parakeet_runner::DEFAULT_GUEST_ID_SUFFIX);
        let config = parakeet_runner::ParakeetConfig {
            python_path: python_path.to_string(),
            model_name: model.to_string(),
            priority: 10,
        };
        let config_json = match serde_json::to_string(&config) {
            Ok(j) => j,
            Err(e) => return IpcResponse::error("asr_setup", "SERIALIZE_ERROR", &e.to_string()),
        };
        let key = format!("component:{guest_id}");
        if let Err(e) = graph.set_config_value(&key, &config_json) {
            return IpcResponse::error("asr_setup", "CONFIG_WRITE_ERROR", &e.to_string());
        }

        // Step 3: upsert the guest record so the reconcile loop spawns it.
        let guest_record = GuestRecord {
            hotel_name: hotel_name.clone(),
            guest_id: guest_id.clone(),
            role: "model.parakeet".to_string(),
            config_json: serde_json::json!({
                "command": "model-controller-parakeet",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path,
                    "PHILOTIC_PARAKEET_GUEST_ID": guest_id,
                }
            })
            .to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        };
        if let Err(e) = graph.upsert_guest(&guest_record) {
            return IpcResponse::error("asr_setup", "GUEST_REGISTER_ERROR", &e.to_string());
        }

        info!(
            hotel = %hotel_name,
            guest_id = %guest_id,
            model = %model,
            "asr.setup: parakeet guest registered; will be spawned within 5s"
        );

        IpcResponse::success(
            "asr_setup",
            Some(serde_json::json!({
                "message": format!(
                    "Parakeet ASR configured. Guest '{guest_id}' registered — will start within 5 seconds."
                ),
                "guest_id": guest_id,
                "model": model,
                "python_path": python_path,
                "nemo_available": true,
            })),
        )
    }

    pub(super) fn handle_asr_status(graph: &GraphDomain, local_node_id: &str) -> IpcResponse {
        use std::process::Command;

        let Some(hotel_name) = IpcServer::local_hotel_name(graph, local_node_id) else {
            return IpcResponse::error(
                "asr_status",
                "HOTEL_NOT_FOUND",
                "could not resolve hotel name",
            );
        };
        let guest_id = format!("{hotel_name}:{}", parakeet_runner::DEFAULT_GUEST_ID_SUFFIX);

        // Read python_path from stored component config, fall back to "python3".
        let component_key = format!("component:{guest_id}");
        let python_path = graph
            .get_config_value(&component_key)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<parakeet_runner::ParakeetConfig>(&json).ok())
            .map(|cfg| cfg.python_path)
            .unwrap_or_else(|| "python3".to_string());

        let guest = graph.get_guest(&hotel_name, &guest_id).ok().flatten();
        let guest_registered = guest.is_some();
        let guest_active = guest.as_ref().map(|g| g.is_active).unwrap_or(false);
        let guest_pid = guest.as_ref().and_then(|g| g.active_pid.clone());

        let nemo_available = Command::new(&python_path)
            .args(["-c", "import nemo.collections.asr"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        IpcResponse::success(
            "asr_status",
            Some(serde_json::json!({
                "guest_id": guest_id,
                "registered": guest_registered,
                "active": guest_active,
                "pid": guest_pid,
                "nemo_available": nemo_available,
                "python_path": python_path,
            })),
        )
    }

    pub(super) fn handle_vision_setup(
        graph: &GraphDomain,
        local_node_id: &str,
        socket_path: &str,
        repo_id: Option<&str>,
    ) -> IpcResponse {
        let repo_id = repo_id.unwrap_or("onnx-community/Florence-2-base-ft");

        // Step 1: resolve hotel name.
        let Some(hotel_name) = IpcServer::local_hotel_name(graph, local_node_id) else {
            return IpcResponse::error(
                "vision_setup",
                "HOTEL_NOT_FOUND",
                "could not resolve hotel name",
            );
        };
        let guest_id = format!("{hotel_name}:{}", Self::VISION_GUEST_ID_SUFFIX);

        // Step 2: write component config (repo_id for the ONNX backend).
        let config = serde_json::json!({ "repo_id": repo_id });
        let config_json = match serde_json::to_string(&config) {
            Ok(j) => j,
            Err(e) => return IpcResponse::error("vision_setup", "SERIALIZE_ERROR", &e.to_string()),
        };
        let key = format!("component:{guest_id}");
        if let Err(e) = graph.set_config_value(&key, &config_json) {
            return IpcResponse::error("vision_setup", "CONFIG_WRITE_ERROR", &e.to_string());
        }

        // Step 3: upsert ModelProfileRecord so health-aware routing picks this provider up.
        let profile = ModelProfileRecord {
            model_ref: "vision".to_string(),
            node_id: local_node_id.to_string(),
            provider: "onnx".to_string(),
            task_kinds: vec!["image.ocr".to_string(), "image.ground".to_string()],
            trust_tier: "local_experimental".to_string(),
            // ONNX/Florence has no tool calling or JSON-contract support.
            supports_tools: false,
            supports_structured: false,
            ..Default::default()
        };
        if let Err(e) = graph.upsert_model_profile(&profile) {
            return IpcResponse::error("vision_setup", "PROFILE_REGISTER_ERROR", &e.to_string());
        }

        // Step 4: upsert guest record so the reconcile loop spawns model-controller-vision.
        let guest_record = GuestRecord {
            hotel_name: hotel_name.clone(),
            guest_id: guest_id.clone(),
            role: "model-controller-vision".to_string(),
            config_json: serde_json::json!({
                "command": "model-controller-vision",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path,
                    "PHILOTIC_VISION_GUEST_ID": guest_id,
                    "PHILOTIC_VISION_REPO_ID": repo_id,
                }
            })
            .to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        };
        if let Err(e) = graph.upsert_guest(&guest_record) {
            return IpcResponse::error("vision_setup", "GUEST_REGISTER_ERROR", &e.to_string());
        }

        info!(
            hotel = %hotel_name,
            guest_id = %guest_id,
            repo_id,
            "vision.setup: ONNX guest registered; will be spawned within 5s"
        );

        IpcResponse::success(
            "vision_setup",
            Some(serde_json::json!({
                "message": format!(
                    "Vision provider configured (ONNX/Florence-2). Guest '{guest_id}' registered — will start within 5 seconds."
                ),
                "guest_id": guest_id,
                "repo_id": repo_id,
                "backend": "onnx",
            })),
        )
    }

    pub(super) fn handle_vision_status(graph: &GraphDomain, local_node_id: &str) -> IpcResponse {
        let Some(hotel_name) = IpcServer::local_hotel_name(graph, local_node_id) else {
            return IpcResponse::error(
                "vision_status",
                "HOTEL_NOT_FOUND",
                "could not resolve hotel name",
            );
        };
        let guest_id = format!("{hotel_name}:{}", Self::VISION_GUEST_ID_SUFFIX);

        let component_key = format!("component:{guest_id}");
        let repo_id = graph
            .get_config_value(&component_key)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
            .and_then(|v| v.get("repo_id").and_then(|r| r.as_str()).map(String::from))
            .unwrap_or_else(|| "onnx-community/Florence-2-base-ft".to_string());

        let guest = graph.get_guest(&hotel_name, &guest_id).ok().flatten();
        let guest_registered = guest.is_some();
        let guest_active = guest.as_ref().map(|g| g.is_active).unwrap_or(false);
        let guest_pid = guest.as_ref().and_then(|g| g.active_pid.clone());

        let model_profile = graph
            .get_model_profile("vision", local_node_id)
            .ok()
            .flatten();

        IpcResponse::success(
            "vision_status",
            Some(serde_json::json!({
                "guest_id": guest_id,
                "registered": guest_registered,
                "active": guest_active,
                "pid": guest_pid,
                "backend": "onnx",
                "repo_id": repo_id,
                "model_profile_status": model_profile.as_ref().map(|p| &p.status),
            })),
        )
    }
}
