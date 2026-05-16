use std::collections::BTreeSet;

pub const TOOL_RIGHT_PREFIX: &str = "tool.";
pub const SKILL_RIGHT_PREFIX: &str = "skill.";
pub const COMPONENT_RIGHT_PREFIX: &str = "component.";

pub fn tool_right(tool_name: &str) -> String {
    format!("{TOOL_RIGHT_PREFIX}{tool_name}")
}

pub fn skill_right(skill_name: &str) -> String {
    format!("{SKILL_RIGHT_PREFIX}{skill_name}")
}

pub fn component_right(capability: &str) -> String {
    format!("{COMPONENT_RIGHT_PREFIX}{capability}")
}

pub fn has_right(rights: &[String], right: &str) -> bool {
    rights.iter().any(|candidate| candidate == right)
}

pub fn normalize_rights(rights: impl IntoIterator<Item = String>) -> Vec<String> {
    rights
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
