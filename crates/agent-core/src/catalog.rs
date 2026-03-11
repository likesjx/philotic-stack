//! Static tool catalog for agent-core.
//!
//! Defines the canonical set of built-in tool definitions with real descriptions
//! and input schemas. Used by `default_tool_assembly_for_bindings` instead of
//! generating generic stubs, and mirrored into the context graph as `abstract_tool`
//! nodes at hotel startup.
//!
//! Tools not present in the catalog fall back to stubs — this keeps the catalog
//! forward-compatible with dynamically registered tools from tool-runner guests.

use crate::session::ToolDefinition;
use serde_json::json;
use std::collections::HashMap;
use std::sync::OnceLock;

static TOOL_CATALOG: OnceLock<HashMap<String, ToolDefinition>> = OnceLock::new();

/// Returns the static built-in tool catalog.
///
/// Call this to look up a real `ToolDefinition` by tool name before falling back
/// to a generated stub. The map is initialized once and reused for the lifetime
/// of the process.
pub fn tool_catalog() -> &'static HashMap<String, ToolDefinition> {
    TOOL_CATALOG.get_or_init(build_catalog)
}

/// Returns the approval/projection class for a tool name, or `None` if the tool
/// is not in the built-in catalog.
pub fn tool_class(tool_name: &str) -> Option<&'static str> {
    tool_catalog()
        .get(tool_name)
        .and_then(|d| d.class.as_deref())
}

/// Returns true if the tool requires operator approval before execution, regardless
/// of what the model requests. Tools in class "config" require approval by default;
/// others do not unless explicitly flagged.
pub fn tool_requires_approval(tool_name: &str) -> bool {
    match tool_class(tool_name) {
        Some("config") => true,
        _ => false,
    }
}

fn build_catalog() -> HashMap<String, ToolDefinition> {
    let mut m = HashMap::new();

    m.insert(
        "session.status".into(),
        ToolDefinition {
            tool_name: "session.status".into(),
            description: "Returns a summary of the current session state, including the active \
                          session ID, turn count, approval policy, and active tool runners."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
            class: Some("session".into()),
        },
    );

    m.insert(
        "echo".into(),
        ToolDefinition {
            tool_name: "echo".into(),
            description: "Echoes a string back unchanged. Use for testing tool routing and \
                          round-trip connectivity."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "The text to echo back."
                    }
                },
                "required": ["text"]
            }),
            class: Some("utility".into()),
        },
    );

    m.insert(
        "workspace.list".into(),
        ToolDefinition {
            tool_name: "workspace.list".into(),
            description: "Lists files and directories at the given path within the workspace. \
                          Defaults to the workspace root if no path is provided."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path within the workspace to list. Omit to list the root."
                    }
                }
            }),
            class: Some("workspace".into()),
        },
    );

    m.insert(
        "workspace.read".into(),
        ToolDefinition {
            tool_name: "workspace.read".into(),
            description: "Reads the contents of a file in the workspace. Supports optional \
                          byte-range limiting via offset and limit."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path to the file within the workspace."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Byte offset to start reading from."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of bytes to read."
                    }
                },
                "required": ["path"]
            }),
            class: Some("workspace".into()),
        },
    );

    m.insert(
        "agent.configure".into(),
        ToolDefinition {
            tool_name: "agent.configure".into(),
            description: "Update an agent configuration field. Supports approval_policy, \
                          profile, and bindings sections. Changes to sensitive fields \
                          (soul, identity, approval policy) require operator approval unless \
                          preapproved. Use operation 'set' to replace, 'append' to add to \
                          arrays, or 'remove' to delete from arrays."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "config_path": {
                        "type": "string",
                        "description": "Dot-separated path to the config field. Examples: \
                                        'approval_policy.preapproved_tools', \
                                        'approval_policy.preapproved_classes', \
                                        'approval_policy.auto_approve_all', \
                                        'profile.soul_text', \
                                        'profile.identity_text', \
                                        'profile.user_context_text', \
                                        'profile.memory_summary', \
                                        'bindings.effective_toolset', \
                                        'bindings.effective_skillset'"
                    },
                    "value": {
                        "description": "The new value. For array fields with 'append'/'remove', \
                                        provide a single string item."
                    },
                    "operation": {
                        "type": "string",
                        "enum": ["set", "append", "remove"],
                        "description": "How to apply the change. Defaults to 'set'."
                    }
                },
                "required": ["config_path", "value"]
            }),
            class: Some("config".into()),
        },
    );

    m
}
