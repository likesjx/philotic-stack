#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    Ping,
    Status,
    Pause,
    Resume,
    ToolsAdd { tool: String },
    ToolsClear,
    SkillsAdd { skill: String },
    SkillsClear,
    WorkspaceSet { workspace: String },
    WorkspaceClear,
    Approve { note: Option<String> },
    Deny { note: Option<String> },
    PreapproveThisSession,
    ApprovalStatus,
    ApprovalReset,
}

impl SlashCommand {
    pub fn reply_text(&self) -> Option<&'static str> {
        match self {
            Self::Ping => Some("pong"),
            Self::Status => None,
            Self::Pause => None,
            Self::Resume => None,
            Self::ToolsAdd { .. } => None,
            Self::ToolsClear => None,
            Self::SkillsAdd { .. } => None,
            Self::SkillsClear => None,
            Self::WorkspaceSet { .. } => None,
            Self::WorkspaceClear => None,
            Self::Approve { .. } => Some("approved"),
            Self::Deny { .. } => Some("denied"),
            Self::PreapproveThisSession | Self::ApprovalStatus | Self::ApprovalReset => None,
        }
    }

    pub fn steering_note(&self) -> Option<&str> {
        match self {
            Self::Approve { note } | Self::Deny { note } => note.as_deref(),
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
        ["/pause", ..] => Some(SlashCommand::Pause),
        ["/resume", ..] => Some(SlashCommand::Resume),
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
        ["/preapprove", "this-session", ..] => Some(SlashCommand::PreapproveThisSession),
        ["/approval", "status", ..] => Some(SlashCommand::ApprovalStatus),
        ["/approval", "reset", ..] => Some(SlashCommand::ApprovalReset),
        _ => None,
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
        assert_eq!(parse_slash_command("/pause"), Some(SlashCommand::Pause));
        assert_eq!(parse_slash_command("/resume"), Some(SlashCommand::Resume));
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
    }

    #[test]
    fn ignores_non_commands_and_unknown_commands() {
        assert_eq!(parse_slash_command("hello"), None);
        assert_eq!(parse_slash_command("/unknown"), None);
    }
}
