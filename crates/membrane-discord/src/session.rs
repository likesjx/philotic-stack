use std::collections::HashMap;

/// Tracks per-session state for an active Discord conversation turn.
#[derive(Debug, Clone)]
pub struct ActiveTurn {
    pub session_id: String,
    pub channel_id: String,
    pub guild_id: String,
    /// The message ID of the draft reply being progressively edited, if any.
    pub draft_message_id: Option<String>,
    /// The message ID of the user message that triggered this turn.
    pub trigger_message_id: Option<String>,
    /// If this was triggered from a text channel, this is the text channel to reply to.
    pub reply_channel_id: String,
}

/// Maps session_id → ActiveTurn.
pub type ActiveTurns = HashMap<String, ActiveTurn>;

/// Maps guild_id:channel_id → session_id override (for /new command).
pub type SessionOverrides = HashMap<String, String>;

/// Guild and channel state cache populated from GUILD_CREATE and VOICE_STATE_UPDATE.
#[derive(Debug, Default)]
pub struct GuildCache {
    /// guild_id → set of voice channel IDs
    pub voice_channels: HashMap<String, Vec<String>>,
    /// guild_id:channel_id → text channel associated with that voice channel (if any)
    pub voice_text_associations: HashMap<String, String>,
}

impl GuildCache {
    pub fn on_guild_create(&mut self, guild: &serde_json::Value) {
        let guild_id = match guild["id"].as_str() {
            Some(id) => id.to_string(),
            None => return,
        };

        let mut voice_channel_ids = vec![];

        if let Some(channels) = guild["channels"].as_array() {
            for ch in channels {
                let ch_type = ch["type"].as_u64().unwrap_or(99);
                let ch_id = ch["id"].as_str().unwrap_or("").to_string();

                if ch_type == 2 {
                    // GUILD_VOICE
                    voice_channel_ids.push(ch_id);
                }
            }
        }

        self.voice_channels.insert(guild_id, voice_channel_ids);
    }
}
