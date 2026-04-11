use anyhow::{Context, Result};
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// Path to the philotic operator directory: `~/.philotic/`.
pub fn philotic_dir() -> PathBuf {
    dirs::home_dir()
        .expect("cannot determine home directory")
        .join(".philotic")
}

/// Returns the active profile name from `PHILOTIC_PROFILE`, or `None` if not set.
pub fn active_profile() -> Option<String> {
    std::env::var("PHILOTIC_PROFILE")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Returns the active profile directory.
///
/// When `PHILOTIC_PROFILE` is set: `~/.philotic/<profile>/`
/// When unset: falls back to `~/.philotic/` for backward compatibility.
///
/// All profile-namespaced paths (pid, log, socket, config, DB) should derive
/// from this function so that two profiles cannot collide by construction.
pub fn profile_dir() -> PathBuf {
    match active_profile() {
        Some(profile) => philotic_dir().join(profile),
        None => philotic_dir(),
    }
}

pub fn identity_dir() -> PathBuf {
    philotic_dir().join("identity")
}

pub fn private_key_path() -> PathBuf {
    identity_dir().join("operator.key")
}

pub fn public_key_path() -> PathBuf {
    identity_dir().join("operator.pub")
}

/// Backward-compatible entry point — runs identity + config template + muninn.
#[allow(dead_code)]
pub async fn run(config: Option<PathBuf>, force: bool) -> Result<()> {
    run_inner(config, force, false).await
}

/// If `skip_config` is true, identity and muninn are set up but the
/// mesh-config template is not written (the interactive wizard handles it).
pub async fn run_inner(config: Option<PathBuf>, force: bool, skip_config: bool) -> Result<()> {
    let config_path = config.unwrap_or_else(|| PathBuf::from("mesh-config.json"));

    println!("philotic-web init");
    println!("─────────────────────────────────────────");

    // ── Operator identity ──────────────────────────────────────────────────
    let id_dir = identity_dir();
    fs::create_dir_all(&id_dir).context("failed to create ~/.philotic/identity/")?;

    let (_signing_key, verifying_key) = if private_key_path().exists() && !force {
        println!("  identity  already exists — skipping keygen (use --force to regenerate)");
        let raw = fs::read(private_key_path()).context("read operator.key")?;
        let bytes: [u8; 32] = raw
            .try_into()
            .map_err(|_| anyhow::anyhow!("operator.key is not 32 bytes"))?;
        let sk = SigningKey::from_bytes(&bytes);
        let vk = VerifyingKey::from(&sk);
        (sk, vk)
    } else {
        let sk = SigningKey::generate(&mut OsRng);
        let vk = VerifyingKey::from(&sk);

        // Write private key — 0600
        fs::write(private_key_path(), sk.to_bytes()).context("write operator.key")?;
        fs::set_permissions(private_key_path(), fs::Permissions::from_mode(0o600))
            .context("chmod operator.key")?;

        // Write public key — hex encoded
        fs::write(public_key_path(), hex::encode(vk.to_bytes())).context("write operator.pub")?;

        println!("  identity  generated");
        (sk, vk)
    };

    let fingerprint = key_fingerprint(&verifying_key);
    println!("  pubkey    {}", hex::encode(verifying_key.to_bytes()));
    println!("  fingerprint  {fingerprint}");

    // ── mesh-config.json template ──────────────────────────────────────────
    if skip_config {
        // Interactive wizard will handle config generation
    } else if config_path.exists() && !force {
        println!(
            "\n  config    {} already exists — skipping (use --force to overwrite)",
            config_path.display()
        );
    } else {
        fs::write(&config_path, CONFIG_TEMPLATE)
            .with_context(|| format!("write {}", config_path.display()))?;
        println!("\n  config    {} written", config_path.display());
        println!("\n  Edit mesh-config.json and fill in your API keys and Telegram bot tokens.");
        println!("  Then run:  phil start");
    }

    println!("\nNode fingerprint (share this when enrolling into a mesh):");
    println!("  ed25519:{fingerprint}");

    // ── muninn init ────────────────────────────────────────────────────────
    crate::muninn::run_init().await?;

    Ok(())
}

fn key_fingerprint(key: &VerifyingKey) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.to_bytes());
    let hash = hasher.finalize();
    // First 8 bytes as hex, colon-separated pairs — like SSH key fingerprints
    hash[..8]
        .chunks(2)
        .map(|b| format!("{:02x}{:02x}", b[0], b[1]))
        .collect::<Vec<_>>()
        .join(":")
}

/// Loads the operator public key from ~/.philotic/identity/operator.pub.
/// Returns None if not yet initialized.
#[allow(dead_code)]
pub fn load_public_key() -> Option<VerifyingKey> {
    let raw = fs::read(public_key_path()).ok()?;
    let hex_str = String::from_utf8(raw).ok()?;
    let bytes = hex::decode(hex_str.trim()).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    VerifyingKey::from_bytes(&arr).ok()
}

// ── Config template ────────────────────────────────────────────────────────

static CONFIG_TEMPLATE: &str = r#"{
  "context_graph": {
    "gemini_api_key": "REPLACE_WITH_GEMINI_API_KEY",
    "elevenlabs_api_key": "",
    "default_model": "gemini-2.0-flash-exp"
  },
  "hotels": {
    "default": {
      "agents": {
        "jane": {
          "agent_id": "agent-jane",
          "persona_name": "Jane",
          "system_prompt": "You are Jane, a warm and capable conversational assistant. You are Jared's right-hand agent — attentive, thoughtful, and direct. You handle a wide range of requests and are the primary point of contact. You have access to bash for running commands when needed.",
          "telegram": {
            "bot_token": "REPLACE_WITH_JANE_BOT_TOKEN",
            "allowed_users": ["REPLACE_WITH_YOUR_TELEGRAM_USERNAME"]
          },
          "model": {
            "default_model": "gemini-2.0-flash-exp"
          },
          "response_route_policy": {
            "default_route": "auto"
          },
          "approval_policy": {
            "require_approval": true,
            "preapproved_classes": ["utility", "session"],
            "preapproved_tools": []
          },
          "media_routing_policy": {
            "forward_media_to_model": true,
            "voice_action": "transcribe",
            "image_action": "analyze_media",
            "document_action": "analyze_media",
            "strip_tools_on_media": false
          }
        },
        "aria": {
          "agent_id": "agent-aria",
          "persona_name": "Aria",
          "system_prompt": "You are Aria the Architect, a development specialist and technical lead. You track all active work, monitor documentation quality, and guide architectural decisions. You have an admin role — you can inspect and manage the Philotic Stack itself. You are precise, systematic, and think in systems. You use bash for development tasks.",
          "telegram": {
            "bot_token": "REPLACE_WITH_ARIA_BOT_TOKEN",
            "allowed_users": ["REPLACE_WITH_YOUR_TELEGRAM_USERNAME"]
          },
          "model": {
            "default_model": "gemini-2.0-flash-exp"
          },
          "response_route_policy": {
            "default_route": "auto"
          },
          "toolset_tags": ["admin-required"],
          "approval_policy": {
            "require_approval": true,
            "preapproved_classes": ["utility", "session", "workspace"],
            "preapproved_tools": []
          },
          "media_routing_policy": {
            "forward_media_to_model": true,
            "voice_action": "transcribe",
            "image_action": "analyze_media",
            "document_action": "analyze_media",
            "strip_tools_on_media": false
          }
        },
        "beacon": {
          "agent_id": "agent-beacon",
          "persona_name": "Beacon",
          "system_prompt": "You are Beacon, Chief of Staff. You are the keeper of goals, roles, events, projects, and ongoing efforts. You help maintain clarity on what matters, what's in flight, and what's next. You track commitments, surface blockers, and ensure nothing falls through the cracks. You are organized, proactive, and operate at a strategic level.",
          "telegram": {
            "bot_token": "REPLACE_WITH_BEACON_BOT_TOKEN",
            "allowed_users": ["REPLACE_WITH_YOUR_TELEGRAM_USERNAME"]
          },
          "model": {
            "default_model": "gemini-2.0-flash-exp"
          },
          "response_route_policy": {
            "default_route": "auto"
          },
          "approval_policy": {
            "require_approval": true,
            "preapproved_classes": ["utility", "session"],
            "preapproved_tools": []
          },
          "media_routing_policy": {
            "forward_media_to_model": true,
            "voice_action": "transcribe",
            "image_action": "analyze_media",
            "document_action": "analyze_media",
            "strip_tools_on_media": false
          }
        },
        "hermes": {
          "agent_id": "agent-hermes",
          "persona_name": "Hermes",
          "system_prompt": "You are Hermes, Communications Specialist. You manage all outbound and inbound communications — email drafts, message routing, notification summaries, and correspondence. You are concise, clear, and know when something needs immediate attention versus when it can wait. You can use bash to interact with mail and messaging tools.",
          "telegram": {
            "bot_token": "REPLACE_WITH_HERMES_BOT_TOKEN",
            "allowed_users": ["REPLACE_WITH_YOUR_TELEGRAM_USERNAME"]
          },
          "model": {
            "default_model": "gemini-2.0-flash-exp"
          },
          "response_route_policy": {
            "default_route": "auto"
          },
          "approval_policy": {
            "require_approval": true,
            "preapproved_classes": ["utility", "session"],
            "preapproved_tools": []
          },
          "media_routing_policy": {
            "forward_media_to_model": true,
            "voice_action": "transcribe",
            "image_action": "analyze_media",
            "document_action": "analyze_media",
            "strip_tools_on_media": false
          }
        },
        "astrid": {
          "agent_id": "agent-astrid",
          "persona_name": "Astrid",
          "system_prompt": "You are Astrid the Librarian, keeper of the Obsidian vault and documentation systems. You organize knowledge, maintain the tag library, structure notes, and ensure information is findable and well-linked. You are methodical, thorough, and take naming and organization seriously. You use bash to interact with the vault and documentation tools.",
          "telegram": {
            "bot_token": "REPLACE_WITH_ASTRID_BOT_TOKEN",
            "allowed_users": ["REPLACE_WITH_YOUR_TELEGRAM_USERNAME"]
          },
          "model": {
            "default_model": "gemini-2.0-flash-exp"
          },
          "response_route_policy": {
            "default_route": "auto"
          },
          "approval_policy": {
            "require_approval": true,
            "preapproved_classes": ["utility", "session", "workspace"],
            "preapproved_tools": []
          },
          "media_routing_policy": {
            "forward_media_to_model": true,
            "voice_action": "transcribe",
            "image_action": "analyze_media",
            "document_action": "analyze_media",
            "strip_tools_on_media": false
          }
        }
      }
    }
  }
}
"#;
