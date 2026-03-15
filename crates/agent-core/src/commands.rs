#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    Ping,
    Status,
    Pause,
    Resume,
    Role { role_name: String },
    Roles,
    Back,
    ToolsAdd { tool: String },
    ToolsClear,
    SkillsAdd { skill: String },
    SkillsClear,
    WorkspaceSet { workspace: String },
    WorkspaceClear,
    Approve { note: Option<String> },
    Deny { note: Option<String> },
    Abandon { reason: Option<String> },
    PreapproveThisSession,
    Preapprove { name: String },
    ApprovalStatus,
    ApprovalReset,
    Tts { mode: Option<String> },
}

impl SlashCommand {
    pub fn reply_text(&self) -> Option<&'static str> {
        match self {
            Self::Ping => Some("pong"),
            Self::Status => None,
            Self::Pause => None,
            Self::Resume => None,
            Self::Role { .. } => None,
            Self::Roles => None,
            Self::Back => None,
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
            | Self::ApprovalReset => None,
            Self::Tts { .. } => None,
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
        ["/role", role_name, ..] => Some(SlashCommand::Role {
            role_name: (*role_name).to_string(),
        }),
        ["/roles", ..] => Some(SlashCommand::Roles),
        ["/back", ..] => Some(SlashCommand::Back),
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
        ["/tts"] => Some(SlashCommand::Tts { mode: None }),
        ["/tts", mode, ..] => Some(SlashCommand::Tts {
            mode: Some((*mode).to_string()),
        }),
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
            parse_slash_command("/role developer"),
            Some(SlashCommand::Role {
                role_name: "developer".into()
            })
        );
        assert_eq!(parse_slash_command("/roles"), Some(SlashCommand::Roles));
        assert_eq!(parse_slash_command("/back"), Some(SlashCommand::Back));
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
    fn ignores_non_commands_and_unknown_commands() {
        assert_eq!(parse_slash_command("hello"), None);
        assert_eq!(parse_slash_command("/unknown"), None);
    }

    #[test]
    fn parses_preapprove_tool_name() {
        assert_eq!(
            parse_slash_command("/preapprove echo"),
            Some(SlashCommand::Preapprove { name: "echo".into() })
        );
    }

    #[test]
    fn parses_preapprove_class_name() {
        assert_eq!(
            parse_slash_command("/preapprove workspace"),
            Some(SlashCommand::Preapprove { name: "workspace".into() })
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
