//! Interactive onboarding TUI — guided setup flow for `phil init`.
//!
//! Walks the operator through a tiered series of questions and produces a valid
//! `mesh-config.json`. Uses the `inquire` crate for terminal prompts.

use anyhow::{Context, Result};
use inquire::{Confirm, Password, Select, Text};
use serde_json::{json, Map, Value};
use std::path::Path;

use crate::presets::{self, ApprovalPolicy};

/// Run the interactive onboarding wizard.
///
/// Produces a valid `mesh-config.json` at `config_path`. If `force` is true,
/// overwrites an existing config without asking.
pub async fn run_interactive(config_path: &Path, force: bool) -> Result<()> {
    if config_path.exists() && !force {
        let overwrite = Confirm::new(&format!(
            "{} already exists. Overwrite?",
            config_path.display()
        ))
        .with_default(false)
        .prompt()?;

        if !overwrite {
            println!("  Keeping existing config. Use --force to skip this check.");
            return Ok(());
        }
    }

    println!();
    println!("  Philotic Stack — Interactive Setup");
    println!("  ───────────────────────────────────");
    println!();

    // ── Tier 1: Get one agent running ────────────────────────────────────────

    let hotel_name = Text::new("Hotel name:")
        .with_default("default")
        .with_help_message("Name for this node — used in mesh identity")
        .prompt()?;

    let gemini_key = Password::new("Gemini API key:")
        .with_help_message("Required — get one at https://aistudio.google.com/apikey")
        .without_confirmation()
        .prompt()?;

    if gemini_key.trim().is_empty() {
        anyhow::bail!("Gemini API key is required. Cannot continue without an LLM provider.");
    }

    let default_model = Text::new("Default model:")
        .with_default("gemini-2.0-flash-exp")
        .with_help_message("Model ID for the primary LLM provider")
        .prompt()?;

    let response_route = Select::new(
        "Default response route:",
        vec![
            "auto".to_string(),
            "text_only".to_string(),
            "image_multimodal".to_string(),
            "audio_multimodal".to_string(),
            "realtime_websocket".to_string(),
        ],
    )
    .with_help_message("Preferred route for model turns; auto lets the runtime infer per turn")
    .prompt()?;

    println!();
    println!("  ── First Agent ──");
    println!();

    let agent_name = Text::new("Agent name:")
        .with_default("jane")
        .with_help_message("Lowercase, no spaces — becomes the agent identifier")
        .prompt()?;

    let agent_personality = Text::new("Agent personality (one-liner):")
        .with_default("A warm and capable conversational assistant")
        .with_help_message("Brief description — becomes part of the system prompt")
        .prompt()?;

    // Build custom system prompt for the first agent
    let first_agent_prompt = format!(
        "You are {name}, {personality}. You handle a wide range of requests and are the \
         primary point of contact. You have access to bash for running commands when needed.",
        name = capitalize(&agent_name),
        personality = agent_personality.trim_end_matches('.'),
    );

    let use_telegram = Confirm::new("Connect this agent to Telegram?")
        .with_default(false)
        .with_help_message("Requires a bot token from @BotFather")
        .prompt()?;

    let (first_tg_token, first_tg_username) = if use_telegram {
        let token = Password::new("  Telegram bot token:")
            .without_confirmation()
            .prompt()?;
        let username = Text::new("  Your Telegram username:")
            .with_help_message("Without the @ — used for allowed_users filter")
            .prompt()?;
        (Some(token), Some(username))
    } else {
        (None, None)
    };

    // ── Tier 2: Agent fleet ──────────────────────────────────────────────────

    println!();
    let configure_fleet = Confirm::new("Configure additional agents?")
        .with_default(false)
        .with_help_message("Add more agents from a preset fleet template")
        .prompt()?;

    let mut fleet_agents: Vec<(presets::AgentPreset, Option<String>)> = Vec::new();
    let mut approval_policy = ApprovalPolicy::ask_first();

    if configure_fleet {
        let preset_names = presets::list_preset_names();
        let options: Vec<String> = preset_names
            .iter()
            .map(|(name, desc)| format!("{name} — {desc}"))
            .collect();

        let choice = Select::new("Agent fleet preset:", options.clone()).prompt()?;
        let chosen_idx = options.iter().position(|o| o == &choice).unwrap_or(0);
        let preset_name = &preset_names[chosen_idx].0;

        if let Some(preset) = presets::load_preset(preset_name) {
            // Skip the first agent if it matches our custom one (by name)
            for agent in &preset.agents {
                if agent.persona_name.to_lowercase() == agent_name.to_lowercase() {
                    continue; // Already configured as the first agent
                }
                let tg_token = if use_telegram {
                    let token = Password::new(&format!(
                        "  Telegram bot token for {} (Enter to skip):",
                        agent.persona_name
                    ))
                    .without_confirmation()
                    .prompt()
                    .unwrap_or_default();
                    if token.is_empty() {
                        None
                    } else {
                        Some(token)
                    }
                } else {
                    None
                };
                fleet_agents.push((agent.clone(), tg_token));
            }
        }

        println!();
        let policy_options = vec![
            "ask-first — require approval for tool use (recommended)",
            "preapprove-safe — auto-approve utility, session, and workspace tools",
            "trust-all — no approval required (use with caution)",
        ];
        let policy_choice = Select::new("Tool approval policy:", policy_options).prompt()?;
        approval_policy = match policy_choice {
            s if s.starts_with("preapprove") => ApprovalPolicy::preapprove_safe(),
            s if s.starts_with("trust") => ApprovalPolicy::trust_all(),
            _ => ApprovalPolicy::ask_first(),
        };
    }

    // ── Tier 3: Advanced ─────────────────────────────────────────────────────

    println!();
    let configure_advanced = Confirm::new("Configure advanced options?")
        .with_default(false)
        .with_help_message("ElevenLabs voice, Muninn password, etc.")
        .prompt()?;

    let mut elevenlabs_key = String::new();
    let mut muninn_password = generate_password();

    if configure_advanced {
        let el_key = Password::new("  ElevenLabs API key (Enter to skip):")
            .without_confirmation()
            .prompt()
            .unwrap_or_default();
        if !el_key.is_empty() {
            elevenlabs_key = el_key;
        }

        let custom_muninn = Confirm::new("  Set custom Muninn password?")
            .with_default(false)
            .with_help_message("Auto-generated if skipped")
            .prompt()?;
        if custom_muninn {
            muninn_password = Password::new("  Muninn admin password:")
                .without_confirmation()
                .prompt()?;
        }
    }

    // ── Build config ─────────────────────────────────────────────────────────

    let mut agents = Map::new();

    // First agent (custom)
    let first_agent = build_agent_entry(
        &agent_name,
        &capitalize(&agent_name),
        &first_agent_prompt,
        &[],
        &approval_policy,
        first_tg_token.as_deref(),
        first_tg_username.as_deref(),
        &default_model,
        &response_route,
    );
    agents.insert(agent_name.clone(), first_agent);

    // Fleet agents
    for (agent, tg_token) in &fleet_agents {
        let key = agent.persona_name.to_lowercase();
        let entry = presets::preset_to_config_value(
            agent,
            tg_token.as_deref(),
            first_tg_username.as_deref(), // Reuse the same Telegram username
            &default_model,
        );
        agents.insert(key, entry);
    }

    let config = json!({
        "context_graph": {
            "muninn": {
                "endpoint": "http://127.0.0.1:8475",
                "admin_username": "root",
                "admin_password": muninn_password,
            },
            "gemini_api_key": gemini_key,
            "elevenlabs_api_key": elevenlabs_key,
            "default_model": default_model,
        },
        "hotels": {
            hotel_name: {
                "agents": Value::Object(agents),
            }
        }
    });

    // Write config
    let pretty = serde_json::to_string_pretty(&config)?;
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(config_path, &pretty)
        .with_context(|| format!("write {}", config_path.display()))?;

    // ── Summary ──────────────────────────────────────────────────────────────

    let agent_count = 1 + fleet_agents.len();
    let telegram_count = std::iter::once(first_tg_token.is_some())
        .chain(fleet_agents.iter().map(|(_, t)| t.is_some()))
        .filter(|&b| b)
        .count();

    println!();
    println!("  ✓ Config written to {}", config_path.display());
    println!(
        "  ✓ {} agent{} configured",
        agent_count,
        if agent_count > 1 { "s" } else { "" }
    );
    if telegram_count > 0 {
        println!(
            "  ✓ {} Telegram bot{} connected",
            telegram_count,
            if telegram_count > 1 { "s" } else { "" }
        );
    }
    if !elevenlabs_key.is_empty() {
        println!("  ✓ ElevenLabs voice enabled");
    }
    println!();
    println!("  Next steps:");
    println!("    phil start        start the hotel daemon");
    println!("    phil serve        open the management UI");
    println!();

    Ok(())
}

fn build_agent_entry(
    name: &str,
    display_name: &str,
    system_prompt: &str,
    toolset_tags: &[&str],
    approval_policy: &ApprovalPolicy,
    telegram_token: Option<&str>,
    telegram_username: Option<&str>,
    model: &str,
    response_route: &str,
) -> Value {
    let mut entry = json!({
        "agent_id": format!("agent-{name}"),
        "persona_name": display_name,
        "system_prompt": system_prompt,
        "import_workspace": "",
        "model": {
            "default_model": model,
        },
        "response_route_policy": {
            "default_route": response_route,
        },
        "approval_policy": {
            "require_approval": approval_policy.require_approval,
            "preapproved_classes": approval_policy.preapproved_classes,
        },
        "media_routing_policy": {
            "forward_media_to_model": true,
            "voice_action": "transcribe",
            "image_action": "analyze_media",
            "document_action": "analyze_media",
            "strip_tools_on_media": false,
        },
    });

    if !toolset_tags.is_empty() {
        entry["toolset_tags"] = json!(toolset_tags);
    }

    if let (Some(token), Some(username)) = (telegram_token, telegram_username) {
        if !token.is_empty() {
            entry["telegram"] = json!({
                "bot_token": token,
                "allowed_users": [username],
            });
        }
    }

    entry
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

fn generate_password() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..24)
        .map(|_| {
            let idx = rng.gen_range(0..62);
            match idx {
                0..=9 => (b'0' + idx) as char,
                10..=35 => (b'a' + idx - 10) as char,
                _ => (b'A' + idx - 36) as char,
            }
        })
        .collect()
}
