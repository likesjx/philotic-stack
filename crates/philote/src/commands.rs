use philotic_client::CommandManifestEntry;

/// A named model the operator can swap to via `/model <alias>`. Maps a friendly
/// alias to the provider tier role it routes through and the concrete model id
/// to bind for that tier (`None` for a provider whose default model is used,
/// e.g. Gemini). Backs the durable one-tap swap in the `/model` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelPreset {
    pub alias: &'static str,
    pub label: &'static str,
    /// Provider tier role this preset makes primary (e.g. `"model.openrouter"`).
    pub tier_role: &'static str,
    /// Concrete model id to bind for `tier_role`, or `None` to use the
    /// provider's own default model (Gemini).
    pub model_id: Option<&'static str>,
    pub description: &'static str,
}

/// Curated model presets, ordered for display in the `/model` list. Seeded from
/// the uncensored-companion model comparison (UGI leaderboard + OpenRouter).
pub fn model_presets() -> &'static [ModelPreset] {
    &[
        ModelPreset {
            alias: "cydonia",
            label: "Cydonia 24B",
            tier_role: "model.openrouter",
            model_id: Some("thedrummer/cydonia-24b-v4.1"),
            description: "uncensored RP companion",
        },
        ModelPreset {
            alias: "dolphin",
            label: "Dolphin Venice",
            tier_role: "model.openrouter",
            model_id: Some("cognitivecomputations/dolphin-mistral-24b-venice-edition:free"),
            description: "free, most willing",
        },
        ModelPreset {
            alias: "euryale",
            // L3.1, not L3.3: only the L3.1 finetune has tool-capable
            // endpoints on OpenRouter (L3.3 is chat-only).
            label: "Euryale 70B",
            tier_role: "model.openrouter",
            model_id: Some("sao10k/l3.1-euryale-70b"),
            description: "bigger, best coherence",
        },
        ModelPreset {
            alias: "deepseek",
            label: "DeepSeek V3.2",
            tier_role: "model.openrouter",
            model_id: Some("deepseek/deepseek-v3.2"),
            description: "smartest, still willing",
        },
        ModelPreset {
            alias: "glm",
            label: "GLM-5.2",
            tier_role: "model.openrouter",
            model_id: Some("z-ai/glm-5.2"),
            description: "GLM-5.2",
        },
        ModelPreset {
            alias: "gemini",
            label: "Gemini",
            tier_role: "model",
            model_id: None,
            description: "fast, filtered",
        },
    ]
}

/// Look up a preset by alias (case-insensitive).
pub fn find_preset(alias: &str) -> Option<&'static ModelPreset> {
    let alias = alias.trim().to_lowercase();
    model_presets().iter().find(|p| p.alias == alias)
}

/// An owned, dynamically-sourced model preset. The compiled-in
/// [`model_presets`] list is only the FALLBACK: the hotel config key
/// `model_presets` (a JSON array of `{alias, label, tier, model, description}`
/// objects) overrides or extends it live — editable via a config patch, no
/// redeploy — and any `vendor/model` slug resolves directly without a preset
/// entry at all (see [`resolve_model_preset`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModelPreset {
    pub alias: String,
    pub label: String,
    pub tier_role: String,
    pub model_id: Option<String>,
    pub description: String,
}

impl From<&ModelPreset> for ResolvedModelPreset {
    fn from(p: &ModelPreset) -> Self {
        Self {
            alias: p.alias.to_string(),
            label: p.label.to_string(),
            tier_role: p.tier_role.to_string(),
            model_id: p.model_id.map(str::to_string),
            description: p.description.to_string(),
        }
    }
}

/// Merge the hotel's `model_presets` config value (if any) over the built-in
/// preset list. Config entries win on alias collision and append otherwise;
/// malformed entries are skipped so one bad row can't take out the whole
/// list. `None` / unparsable config yields exactly the built-ins.
pub fn merge_config_model_presets(config_json: Option<&str>) -> Vec<ResolvedModelPreset> {
    let mut presets: Vec<ResolvedModelPreset> = model_presets().iter().map(Into::into).collect();
    let Some(raw) = config_json else {
        return presets;
    };
    let Ok(serde_json::Value::Array(entries)) = serde_json::from_str::<serde_json::Value>(raw)
    else {
        return presets;
    };
    for entry in entries {
        let Some(alias) = entry
            .get("alias")
            .and_then(serde_json::Value::as_str)
            .map(|a| a.trim().to_lowercase())
            .filter(|a| !a.is_empty())
        else {
            continue;
        };
        let model_id = entry
            .get("model")
            .or_else(|| entry.get("model_id"))
            .and_then(serde_json::Value::as_str)
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty());
        let tier_role = entry
            .get("tier")
            .or_else(|| entry.get("tier_role"))
            .and_then(serde_json::Value::as_str)
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "model.openrouter".to_string());
        let label = entry
            .get("label")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| alias.clone());
        let description = entry
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let resolved = ResolvedModelPreset {
            alias: alias.clone(),
            label,
            tier_role,
            model_id,
            description,
        };
        match presets.iter_mut().find(|p| p.alias == alias) {
            Some(existing) => *existing = resolved,
            None => presets.push(resolved),
        }
    }
    presets
}

/// Resolve a `/model` argument against the merged preset list. Falls through
/// to a direct binding for any `vendor/model` OpenRouter slug — the operator
/// is not limited to curated aliases.
pub fn resolve_model_preset(
    alias: &str,
    presets: &[ResolvedModelPreset],
) -> Option<ResolvedModelPreset> {
    let lowered = alias.trim().to_lowercase();
    if let Some(preset) = presets.iter().find(|p| p.alias == lowered) {
        return Some(preset.clone());
    }
    let raw = alias.trim();
    if raw.contains('/') && !raw.starts_with('/') && !raw.ends_with('/') {
        return Some(ResolvedModelPreset {
            alias: raw.to_string(),
            label: raw.to_string(),
            tier_role: "model.openrouter".to_string(),
            model_id: Some(raw.to_string()),
            description: "direct OpenRouter model".to_string(),
        });
    }
    None
}

/// Returns the agent's command manifest.  Pass `active_skills` to include skill-specific commands.
/// This is the single source of truth for what slash commands the agent handles.
pub fn command_manifest(_active_skills: &[String]) -> Vec<CommandManifestEntry> {
    vec![
        CommandManifestEntry {
            command: "status".into(),
            description: "Show current session status.".into(),
            usage_hint: None,
        },
        CommandManifestEntry {
            command: "context".into(),
            description:
                "Show a breakdown of the current context envelope (sections, sizes, turn history)."
                    .into(),
            usage_hint: None,
        },
        CommandManifestEntry {
            command: "pause".into(),
            description: "Pause the current session.".into(),
            usage_hint: None,
        },
        CommandManifestEntry {
            command: "resume".into(),
            description: "Resume a paused session.".into(),
            usage_hint: None,
        },
        CommandManifestEntry {
            command: "role".into(),
            description: "Switch to a named role.".into(),
            usage_hint: Some("/role <name>".into()),
        },
        CommandManifestEntry {
            command: "roles".into(),
            description: "List configured roles and highlight the active one.".into(),
            usage_hint: None,
        },
        CommandManifestEntry {
            command: "hotel".into(),
            description: "Show which hotel and role this philote is currently running as.".into(),
            usage_hint: None,
        },
        CommandManifestEntry {
            command: "back".into(),
            description: "Return to the orchestrator role.".into(),
            usage_hint: None,
        },
        CommandManifestEntry {
            command: "approve".into(),
            description: "Approve the pending action.".into(),
            usage_hint: Some("/approve [note]".into()),
        },
        CommandManifestEntry {
            command: "deny".into(),
            description: "Deny the pending action.".into(),
            usage_hint: Some("/deny [note]".into()),
        },
        CommandManifestEntry {
            command: "abandon".into(),
            description: "Abandon the current turn.".into(),
            usage_hint: None,
        },
        CommandManifestEntry {
            command: "tts".into(),
            description: "Set text-to-speech mode.".into(),
            usage_hint: Some("/tts [on|off|auto]".into()),
        },
        CommandManifestEntry {
            command: "voice".into(),
            description: "Switch voice provider (and optionally voice ID) for this session.".into(),
            usage_hint: Some("/voice [kokoro|elevenlabs|openai] [voice_id]".into()),
        },
        CommandManifestEntry {
            command: "model".into(),
            description: "Swap this agent's model by preset (durable), or list the options.".into(),
            usage_hint: Some("/model [<preset>|<vendor/model>]".into()),
        },
        CommandManifestEntry {
            command: "models".into(),
            description: "Browse the model catalog — vendors, then models, as tappable buttons."
                .into(),
            usage_hint: Some("/models [<vendor>|<search>]".into()),
        },
        CommandManifestEntry {
            command: "preapprove".into(),
            description: "Pre-approve a tool or class for this session.".into(),
            usage_hint: Some("/preapprove <tool|class> | this-session".into()),
        },
        CommandManifestEntry {
            command: "approval".into(),
            description: "Show or reset the session approval policy.".into(),
            usage_hint: Some("/approval status | reset".into()),
        },
        CommandManifestEntry {
            command: "correct".into(),
            description:
                "Submit a correction for the most recent transcription (Whisper flywheel).".into(),
            usage_hint: Some("/correct <turn_id> <corrected text>".into()),
        },
        CommandManifestEntry {
            command: "plan".into(),
            description: "Show plan carryover status, or drop the carried-over plan.".into(),
            usage_hint: Some("/plan [drop]".into()),
        },
        CommandManifestEntry {
            command: "dirty".into(),
            description: "Switch to the intimate 'vixen' register (private, explicit).".into(),
            usage_hint: None,
        },
        CommandManifestEntry {
            command: "sfw".into(),
            description: "Return from the vixen register to normal.".into(),
            usage_hint: None,
        },
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    Ping,
    Status,
    Context,
    Pause,
    Resume,
    Role {
        role_name: String,
    },
    Roles,
    Back,
    /// Report which hotel (node) and role this philote is currently
    /// materialized as — a quick "where am I running" readout.
    Hotel,
    ToolsAdd {
        tool: String,
    },
    ToolsClear,
    SkillsAdd {
        skill: String,
    },
    SkillsClear,
    WorkspaceSet {
        workspace: String,
    },
    WorkspaceClear,
    Approve {
        note: Option<String>,
    },
    Deny {
        note: Option<String>,
    },
    Abandon {
        reason: Option<String>,
    },
    PreapproveThisSession,
    Preapprove {
        name: String,
    },
    ApprovalStatus,
    ApprovalReset,
    /// Explicitly cancel a parked approval turn, unblocking the session and notifying the original chat.
    ApprovalClear {
        reason: Option<String>,
    },
    Tts {
        mode: Option<String>,
    },
    /// Switch voice provider (and optionally voice ID) for this session.
    Voice {
        provider: Option<String>,
        voice_id: Option<String>,
    },
    /// Pin this session to a model tier role (e.g. `model.ollama`), or with no
    /// argument show the current model and the preset list.
    Model {
        tier: Option<String>,
    },
    /// Durably swap this agent's model to a named preset (e.g. `cydonia`).
    /// Applies live (updates the active role's `model_bindings` + primary tier)
    /// and persists via `role.configure`.
    ModelPreset {
        alias: String,
    },
    /// Browse the hotel's model catalog: bare `/models` lists vendors as
    /// tappable buttons; `/models <vendor-or-search>` drills into matching
    /// models, each button firing `/model <id>` to bind it.
    Models {
        query: Option<String>,
    },
    /// Submit a corrected transcript for a Whisper turn — feeds the training flywheel.
    Correct {
        turn_id: String,
        text: String,
    },
    /// Show plan carryover status (`/plan`) or drop the carried-over plan (`/plan drop`).
    Plan {
        drop: bool,
    },
    /// Switch this agent into the intimate `vixen` register (creates the role if
    /// needed, then hands off to it). `/sfw` returns to the orchestrator.
    Dirty,
    /// Return from the `vixen` register to the normal orchestrator role.
    Sfw,
}

impl SlashCommand {
    pub fn reply_text(&self) -> Option<&'static str> {
        match self {
            Self::Ping => Some("pong"),
            Self::Status => None,
            Self::Context => None,
            Self::Pause => None,
            Self::Resume => None,
            Self::Role { .. } => None,
            Self::Roles => None,
            Self::Back => None,
            Self::Hotel => None,
            Self::ToolsAdd { .. } => None,
            Self::ToolsClear => None,
            Self::SkillsAdd { .. } => None,
            Self::SkillsClear => None,
            Self::WorkspaceSet { .. } => None,
            Self::WorkspaceClear => None,
            Self::Approve { .. } => Some("approved"),
            Self::Deny { .. } => Some("denied"),
            Self::Abandon { .. } => None,
            Self::PreapproveThisSession
            | Self::Preapprove { .. }
            | Self::ApprovalStatus
            | Self::ApprovalReset
            | Self::ApprovalClear { .. } => None,
            Self::Tts { .. } => None,
            Self::Voice { .. } => None,
            Self::Model { .. } => None,
            Self::ModelPreset { .. } => None,
            Self::Models { .. } => None,
            Self::Dirty => None,
            Self::Sfw => None,
            Self::Correct { .. } => None,
            Self::Plan { .. } => None,
        }
    }

    pub fn steering_note(&self) -> Option<&str> {
        match self {
            Self::Approve { note } | Self::Deny { note } => note.as_deref(),
            Self::Correct { text, .. } => Some(text.as_str()),
            _ => None,
        }
    }
}

pub fn parse_slash_command(input: &str) -> Option<SlashCommand> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }

    let parts = trimmed.split_whitespace().collect::<Vec<_>>();
    match parts.as_slice() {
        ["/ping", ..] => Some(SlashCommand::Ping),
        ["/status", ..] => Some(SlashCommand::Status),
        ["/context", ..] => Some(SlashCommand::Context),
        ["/pause", ..] => Some(SlashCommand::Pause),
        ["/resume", ..] => Some(SlashCommand::Resume),
        ["/role", role_name, ..] => Some(SlashCommand::Role {
            role_name: (*role_name).to_string(),
        }),
        ["/roles", ..] => Some(SlashCommand::Roles),
        ["/back", ..] => Some(SlashCommand::Back),
        ["/hotel", ..] => Some(SlashCommand::Hotel),
        ["/tools", "add", tool, ..] => Some(SlashCommand::ToolsAdd {
            tool: (*tool).to_string(),
        }),
        ["/tools", "clear", ..] => Some(SlashCommand::ToolsClear),
        ["/skills", "add", skill, ..] => Some(SlashCommand::SkillsAdd {
            skill: (*skill).to_string(),
        }),
        ["/skills", "clear", ..] => Some(SlashCommand::SkillsClear),
        ["/workspace", "set", workspace, ..] => Some(SlashCommand::WorkspaceSet {
            workspace: (*workspace).to_string(),
        }),
        ["/workspace", "clear", ..] => Some(SlashCommand::WorkspaceClear),
        ["/approve", rest @ ..] => Some(SlashCommand::Approve {
            note: join_command_note(rest),
        }),
        ["/deny", rest @ ..] => Some(SlashCommand::Deny {
            note: join_command_note(rest),
        }),
        ["/abandon", rest @ ..] => Some(SlashCommand::Abandon {
            reason: join_command_note(rest),
        }),
        ["/preapprove", "this-session", ..] => Some(SlashCommand::PreapproveThisSession),
        ["/preapprove", name, ..] => Some(SlashCommand::Preapprove {
            name: (*name).to_string(),
        }),
        ["/approval", "status", ..] => Some(SlashCommand::ApprovalStatus),
        ["/approval", "reset", ..] => Some(SlashCommand::ApprovalReset),
        ["/approval", "clear", rest @ ..] => Some(SlashCommand::ApprovalClear {
            reason: join_command_note(rest),
        }),
        ["/tts"] => Some(SlashCommand::Tts { mode: None }),
        ["/tts", mode, ..] => Some(SlashCommand::Tts {
            mode: Some((*mode).to_string()),
        }),
        ["/voice"] => Some(SlashCommand::Voice {
            provider: None,
            voice_id: None,
        }),
        ["/voice", provider] => Some(SlashCommand::Voice {
            provider: Some(normalize_voice_provider(provider)),
            voice_id: None,
        }),
        ["/voice", provider, voice_id, ..] => Some(SlashCommand::Voice {
            provider: Some(normalize_voice_provider(provider)),
            voice_id: Some((*voice_id).to_string()),
        }),
        ["/models"] => Some(SlashCommand::Models { query: None }),
        ["/models", rest @ ..] => Some(SlashCommand::Models {
            query: Some(rest.join(" ")),
        }),
        ["/model"] => Some(SlashCommand::Model { tier: None }),
        ["/model", arg, ..] => {
            // Explicit tier names (ollama/local/mlx/model.*) keep the legacy
            // session tier-pin. Everything else — built-in preset aliases,
            // config-defined aliases, or a direct `vendor/model` OpenRouter
            // slug — takes the durable ModelPreset swap path; the alias is
            // resolved at HANDLE time against the live merged preset list
            // (hotel config + built-ins), not the compiled-in list, so parse
            // must not gate on `find_preset`.
            let lowered = arg.to_lowercase();
            let is_tier_pin = arg.starts_with("model.")
                || matches!(
                    lowered.as_str(),
                    "ollama" | "local" | "onnx" | "kokoro" | "mlx" | "cloud" | "model"
                );
            if is_tier_pin {
                Some(SlashCommand::Model {
                    tier: Some(normalize_model_tier(arg)),
                })
            } else {
                // Slugs keep their case (OpenRouter ids are case-sensitive);
                // bare aliases are lowercased for case-insensitive matching.
                let alias = if arg.contains('/') {
                    (*arg).to_string()
                } else {
                    lowered
                };
                Some(SlashCommand::ModelPreset { alias })
            }
        }
        ["/correct", turn_id, rest @ ..] if !rest.is_empty() => Some(SlashCommand::Correct {
            turn_id: (*turn_id).to_string(),
            text: rest.join(" "),
        }),
        ["/plan"] => Some(SlashCommand::Plan { drop: false }),
        ["/plan", "drop", ..] => Some(SlashCommand::Plan { drop: true }),
        ["/plan", "status", ..] => Some(SlashCommand::Plan { drop: false }),
        ["/dirty", ..] => Some(SlashCommand::Dirty),
        ["/sfw", ..] => Some(SlashCommand::Sfw),
        _ => None,
    }
}

fn normalize_voice_provider(raw: &str) -> String {
    match raw.to_lowercase().as_str() {
        "kokoro" | "onnx" | "local" => "onnx".into(),
        "elevenlabs" | "eleven" | "11labs" => "elevenlabs".into(),
        "openai" | "tts" => "openai".into(),
        other => other.to_string(),
    }
}

/// Normalize a bare model tier name (or provider alias) into its `model.*`
/// tier-role form. Anything already prefixed `model.` passes through
/// unchanged so the tier-role naming convention stays the single source of
/// truth (mirrors `normalize_voice_provider`).
fn normalize_model_tier(raw: &str) -> String {
    if raw.starts_with("model.") {
        return raw.to_string();
    }
    match raw.to_lowercase().as_str() {
        "gemini" | "cloud" | "model" => "model".into(),
        "ollama" => "model.ollama".into(),
        "local" | "onnx" | "kokoro" => "model.local".into(),
        "mlx" => "model.mlx".into(),
        other => format!("model.{other}"),
    }
}

fn join_command_note(parts: &[&str]) -> Option<String> {
    let note = parts.join(" ").trim().to_string();
    if note.is_empty() { None } else { Some(note) }
}

#[cfg(test)]
mod tests {
    use super::{SlashCommand, parse_slash_command};

    #[test]
    fn parses_ping_command() {
        assert_eq!(parse_slash_command("/ping"), Some(SlashCommand::Ping));
        assert_eq!(parse_slash_command("/ping now"), Some(SlashCommand::Ping));
        assert_eq!(parse_slash_command("/status"), Some(SlashCommand::Status));
        assert_eq!(parse_slash_command("/context"), Some(SlashCommand::Context));
        assert_eq!(parse_slash_command("/pause"), Some(SlashCommand::Pause));
        assert_eq!(parse_slash_command("/resume"), Some(SlashCommand::Resume));
        assert_eq!(
            parse_slash_command("/role developer"),
            Some(SlashCommand::Role {
                role_name: "developer".into()
            })
        );
        assert_eq!(parse_slash_command("/roles"), Some(SlashCommand::Roles));
        assert_eq!(parse_slash_command("/back"), Some(SlashCommand::Back));
        assert_eq!(parse_slash_command("/hotel"), Some(SlashCommand::Hotel));
        assert_eq!(parse_slash_command("/hotel now"), Some(SlashCommand::Hotel));
        assert_eq!(
            parse_slash_command("/tools add echo"),
            Some(SlashCommand::ToolsAdd {
                tool: "echo".into()
            })
        );
        assert_eq!(
            parse_slash_command("/tools clear"),
            Some(SlashCommand::ToolsClear)
        );
        assert_eq!(
            parse_slash_command("/skills add planning"),
            Some(SlashCommand::SkillsAdd {
                skill: "planning".into()
            })
        );
        assert_eq!(
            parse_slash_command("/skills clear"),
            Some(SlashCommand::SkillsClear)
        );
        assert_eq!(
            parse_slash_command("/workspace set workspace://main"),
            Some(SlashCommand::WorkspaceSet {
                workspace: "workspace://main".into()
            })
        );
        assert_eq!(
            parse_slash_command("/workspace clear"),
            Some(SlashCommand::WorkspaceClear)
        );
        assert_eq!(
            parse_slash_command("/approve"),
            Some(SlashCommand::Approve { note: None })
        );
        assert_eq!(
            parse_slash_command("/deny"),
            Some(SlashCommand::Deny { note: None })
        );
        assert_eq!(
            parse_slash_command("/approve use staging"),
            Some(SlashCommand::Approve {
                note: Some("use staging".into())
            })
        );
        assert_eq!(
            parse_slash_command("/deny summarize the plan instead"),
            Some(SlashCommand::Deny {
                note: Some("summarize the plan instead".into())
            })
        );
        assert_eq!(
            parse_slash_command("/preapprove this-session"),
            Some(SlashCommand::PreapproveThisSession)
        );
        assert_eq!(
            parse_slash_command("/approval status"),
            Some(SlashCommand::ApprovalStatus)
        );
        assert_eq!(
            parse_slash_command("/approval reset"),
            Some(SlashCommand::ApprovalReset)
        );
        assert_eq!(
            parse_slash_command("/tts"),
            Some(SlashCommand::Tts { mode: None })
        );
        assert_eq!(
            parse_slash_command("/tts on"),
            Some(SlashCommand::Tts {
                mode: Some("on".into())
            })
        );
        assert_eq!(
            parse_slash_command("/tts off"),
            Some(SlashCommand::Tts {
                mode: Some("off".into())
            })
        );
        assert_eq!(
            parse_slash_command("/tts auto"),
            Some(SlashCommand::Tts {
                mode: Some("auto".into())
            })
        );
    }

    #[test]
    fn parses_abandon_command() {
        assert_eq!(
            parse_slash_command("/abandon"),
            Some(SlashCommand::Abandon { reason: None })
        );
        assert_eq!(
            parse_slash_command("/abandon could not complete the task"),
            Some(SlashCommand::Abandon {
                reason: Some("could not complete the task".into())
            })
        );
    }

    #[test]
    fn parses_voice_command() {
        assert_eq!(
            parse_slash_command("/voice local"),
            Some(SlashCommand::Voice {
                provider: Some("onnx".into()),
                voice_id: None,
            })
        );
        assert_eq!(
            parse_slash_command("/voice elevenlabs"),
            Some(SlashCommand::Voice {
                provider: Some("elevenlabs".into()),
                voice_id: None,
            })
        );
        assert_eq!(
            parse_slash_command("/voice"),
            Some(SlashCommand::Voice {
                provider: None,
                voice_id: None,
            })
        );
        assert_eq!(
            parse_slash_command("/voice kokoro af_heart"),
            Some(SlashCommand::Voice {
                provider: Some("onnx".into()),
                voice_id: Some("af_heart".into()),
            })
        );
    }

    #[test]
    fn parses_model_command() {
        assert_eq!(
            parse_slash_command("/model"),
            Some(SlashCommand::Model { tier: None })
        );
        // `gemini` is a known preset now, so it takes the durable-swap path.
        assert_eq!(
            parse_slash_command("/model gemini"),
            Some(SlashCommand::ModelPreset {
                alias: "gemini".into(),
            })
        );
        assert_eq!(
            parse_slash_command("/model ollama"),
            Some(SlashCommand::Model {
                tier: Some("model.ollama".into()),
            })
        );
        assert_eq!(
            parse_slash_command("/model local"),
            Some(SlashCommand::Model {
                tier: Some("model.local".into()),
            })
        );
        assert_eq!(
            parse_slash_command("/model mlx"),
            Some(SlashCommand::Model {
                tier: Some("model.mlx".into()),
            })
        );
        assert_eq!(
            parse_slash_command("/model model.ollama"),
            Some(SlashCommand::Model {
                tier: Some("model.ollama".into()),
            })
        );
    }

    #[test]
    fn parses_model_presets() {
        use super::{find_preset, model_presets};
        for preset in model_presets() {
            assert_eq!(
                parse_slash_command(&format!("/model {}", preset.alias)),
                Some(SlashCommand::ModelPreset {
                    alias: preset.alias.to_string(),
                }),
                "alias {} should parse to a preset swap",
                preset.alias
            );
        }
        // Case-insensitive.
        assert_eq!(
            parse_slash_command("/model CYDONIA"),
            Some(SlashCommand::ModelPreset {
                alias: "cydonia".into(),
            })
        );
        // find_preset resolves aliases and rejects unknowns.
        assert_eq!(
            find_preset("glm").map(|p| p.tier_role),
            Some("model.openrouter")
        );
        assert_eq!(find_preset("gemini").and_then(|p| p.model_id), None);
        assert!(find_preset("nope-not-a-model").is_none());
    }

    #[test]
    fn parses_models_browse_command() {
        assert_eq!(
            parse_slash_command("/models"),
            Some(SlashCommand::Models { query: None })
        );
        assert_eq!(
            parse_slash_command("/models sao10k"),
            Some(SlashCommand::Models {
                query: Some("sao10k".into()),
            })
        );
        assert_eq!(
            parse_slash_command("/models euryale 70b"),
            Some(SlashCommand::Models {
                query: Some("euryale 70b".into()),
            })
        );
    }

    #[test]
    fn parses_slugs_and_unknown_aliases_as_preset_swaps() {
        // A direct OpenRouter `vendor/model` slug keeps its case and takes
        // the durable swap path.
        assert_eq!(
            parse_slash_command("/model sao10k/l3.1-euryale-70b"),
            Some(SlashCommand::ModelPreset {
                alias: "sao10k/l3.1-euryale-70b".into(),
            })
        );
        // An unknown bare alias also goes to the preset path (resolved at
        // handle time against hotel-config presets), lowercased.
        assert_eq!(
            parse_slash_command("/model MyAlias"),
            Some(SlashCommand::ModelPreset {
                alias: "myalias".into(),
            })
        );
        // Explicit tier pins are untouched.
        assert_eq!(
            parse_slash_command("/model model.anthropic"),
            Some(SlashCommand::Model {
                tier: Some("model.anthropic".into()),
            })
        );
    }

    #[test]
    fn merges_config_presets_over_builtins() {
        use super::{merge_config_model_presets, model_presets};

        // No config → exactly the built-ins.
        let defaults = merge_config_model_presets(None);
        assert_eq!(defaults.len(), model_presets().len());

        // Config overrides an existing alias and appends a new one; a
        // malformed row (no alias) is skipped without poisoning the list.
        let config = r#"[
            {"alias": "cydonia", "model": "thedrummer/cydonia-24b-v9", "label": "Cydonia v9"},
            {"alias": "mist", "model": "vendor/mist-large", "description": "config-added"},
            {"label": "no alias — skipped"}
        ]"#;
        let merged = merge_config_model_presets(Some(config));
        assert_eq!(merged.len(), model_presets().len() + 1);
        let cydonia = merged.iter().find(|p| p.alias == "cydonia").unwrap();
        assert_eq!(
            cydonia.model_id.as_deref(),
            Some("thedrummer/cydonia-24b-v9")
        );
        assert_eq!(cydonia.label, "Cydonia v9");
        let mist = merged.iter().find(|p| p.alias == "mist").unwrap();
        assert_eq!(mist.tier_role, "model.openrouter");
        assert_eq!(mist.model_id.as_deref(), Some("vendor/mist-large"));

        // Unparsable config falls back to the built-ins.
        let broken = merge_config_model_presets(Some("not json"));
        assert_eq!(broken.len(), model_presets().len());
    }

    #[test]
    fn resolves_presets_and_direct_slugs() {
        use super::{merge_config_model_presets, resolve_model_preset};

        let presets = merge_config_model_presets(None);
        // Alias match is case-insensitive.
        let euryale = resolve_model_preset("EURYALE", &presets).unwrap();
        assert_eq!(euryale.model_id.as_deref(), Some("sao10k/l3.1-euryale-70b"));
        // Any vendor/model slug binds directly to the openrouter tier.
        let direct = resolve_model_preset("some-vendor/some-model:free", &presets).unwrap();
        assert_eq!(direct.tier_role, "model.openrouter");
        assert_eq!(
            direct.model_id.as_deref(),
            Some("some-vendor/some-model:free")
        );
        // Unknown bare aliases (and degenerate slugs) resolve to nothing.
        assert!(resolve_model_preset("nope", &presets).is_none());
        assert!(resolve_model_preset("/leading", &presets).is_none());
        assert!(resolve_model_preset("trailing/", &presets).is_none());
    }

    #[test]
    fn parses_dirty_sfw_commands() {
        assert_eq!(parse_slash_command("/dirty"), Some(SlashCommand::Dirty));
        assert_eq!(parse_slash_command("/dirty now"), Some(SlashCommand::Dirty));
        assert_eq!(parse_slash_command("/sfw"), Some(SlashCommand::Sfw));
    }

    #[test]
    fn parses_plan_command() {
        assert_eq!(
            parse_slash_command("/plan"),
            Some(SlashCommand::Plan { drop: false })
        );
        assert_eq!(
            parse_slash_command("/plan status"),
            Some(SlashCommand::Plan { drop: false })
        );
        assert_eq!(
            parse_slash_command("/plan drop"),
            Some(SlashCommand::Plan { drop: true })
        );
        // Unknown subcommand is not a plan command.
        assert_eq!(parse_slash_command("/plan bogus"), None);
    }

    #[test]
    fn ignores_non_commands_and_unknown_commands() {
        assert_eq!(parse_slash_command("hello"), None);
        assert_eq!(parse_slash_command("/unknown"), None);
    }

    #[test]
    fn parses_preapprove_tool_name() {
        assert_eq!(
            parse_slash_command("/preapprove echo"),
            Some(SlashCommand::Preapprove {
                name: "echo".into()
            })
        );
    }

    #[test]
    fn parses_preapprove_class_name() {
        assert_eq!(
            parse_slash_command("/preapprove workspace"),
            Some(SlashCommand::Preapprove {
                name: "workspace".into()
            })
        );
    }

    #[test]
    fn preapprove_this_session_still_parses() {
        assert_eq!(
            parse_slash_command("/preapprove this-session"),
            Some(SlashCommand::PreapproveThisSession)
        );
    }

    #[test]
    fn steering_note_is_accessible_from_approve_and_deny() {
        let approve_with_note = SlashCommand::Approve {
            note: Some("use the staging environment".into()),
        };
        assert_eq!(
            approve_with_note.steering_note(),
            Some("use the staging environment")
        );

        let approve_bare = SlashCommand::Approve { note: None };
        assert_eq!(approve_bare.steering_note(), None);

        let deny_with_note = SlashCommand::Deny {
            note: Some("summarize instead".into()),
        };
        assert_eq!(deny_with_note.steering_note(), Some("summarize instead"));

        let deny_bare = SlashCommand::Deny { note: None };
        assert_eq!(deny_bare.steering_note(), None);

        // Non-approval commands never carry a steering note.
        assert_eq!(SlashCommand::Ping.steering_note(), None);
    }

    #[test]
    fn approve_with_note_has_steering_deny_without_does_not() {
        let approve = parse_slash_command("/approve use staging").unwrap();
        assert!(approve.steering_note().is_some());

        let deny_bare = parse_slash_command("/deny").unwrap();
        assert!(deny_bare.steering_note().is_none());

        let deny_steered = parse_slash_command("/deny try a different approach").unwrap();
        assert_eq!(
            deny_steered.steering_note(),
            Some("try a different approach")
        );
    }
}
