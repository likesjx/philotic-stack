//! Static tool catalog for philote.
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

/// Returns the static list of tool names implied by a built-in skill.
///
/// When `effective_skillset` contains a skill name, all tools in this list
/// are merged into the visible toolset during assembly. Returns an empty slice
/// for unknown or zero-implied-tool skills.
pub fn skill_implied_tools(skill_name: &str) -> &'static [&'static str] {
    match skill_name {
        "handoff.to_role" => &["session.status", "handoff.to_role", "handoff.back"],
        "handoff.back" => &["session.status", "handoff.back"],
        "role.governance" => &["session.status", "agent.configure", "role.configure"],
        "memory" => &["memory.recall", "memory.remember"],
        _ => &[],
    }
}

/// Returns the approval/projection class for a tool name, or `None` if the tool
/// is not in the built-in catalog.
pub fn tool_class(tool_name: &str) -> Option<&'static str> {
    tool_catalog()
        .get(tool_name)
        .and_then(|d| d.class.as_deref())
}

/// Returns true if the tool requires operator approval before execution, regardless
/// of what the model requests. Tools in class "config", "handoff", or "shell" require
/// approval by default; others do not unless explicitly flagged.
pub fn tool_requires_approval(tool_name: &str) -> bool {
    matches!(
        tool_class(tool_name),
        Some("config") | Some("handoff") | Some("shell")
    )
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
        "bash.exec".into(),
        ToolDefinition {
            tool_name: "bash.exec".into(),
            description: "Runs a shell command and returns stdout, stderr, and exit code. \
                          Use for scripting, file system queries, or invoking CLI tools. \
                          Requires operator approval. Commands run under the agent's effective \
                          working directory unless overridden by working_dir. A timeout (default \
                          30 s) is enforced; the process is killed and an error returned if it \
                          exceeds the limit."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute (passed to `sh -c`)."
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Optional absolute path to use as the working directory. \
                                        Defaults to the agent session workspace path if set."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Maximum seconds to wait before killing the process. \
                                        Defaults to 30."
                    }
                },
                "required": ["command"]
            }),
            class: Some("shell".into()),
        },
    );

    m.insert(
        "skill.register".into(),
        ToolDefinition {
            tool_name: "skill.register".into(),
            description: "Registers a new delegation skill in the hotel's shared skill catalog. \
                          A delegation skill defines a reusable subagent role with a goal template, \
                          allowed tools, and lifecycle configuration. The hotel validates the skill \
                          structurally and returns the validation outcome. Once registered, the skill \
                          can be referenced by name when spawning subagents."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": "Stable identifier for the skill. Lowercase alphanumeric, \
                                        hyphens, and underscores only. Max 64 characters."
                    },
                    "description": {
                        "type": "string",
                        "description": "Human-readable description of what the skill does. Max 2048 characters."
                    },
                    "subagent_kind": {
                        "type": "string",
                        "description": "The role name of the subagent worker this skill delegates to \
                                        (e.g., 'philote-worker')."
                    },
                    "goal": {
                        "type": "string",
                        "description": "Goal template injected into the subagent context when this \
                                        skill is invoked. May include placeholders."
                    },
                    "allowed_tools": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of tool IDs the subagent is permitted to use."
                    },
                    "allowed_classes": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of tool class names the subagent may use \
                                        (e.g., 'utility', 'workspace')."
                    }
                },
                "required": ["skill_name", "description", "subagent_kind", "goal"]
            }),
            class: Some("capability".into()),
        },
    );

    m.insert(
        "skill.list".into(),
        ToolDefinition {
            tool_name: "skill.list".into(),
            description: "Lists all registered skills in the hotel's skill catalog, including their \
                          validation states and implied tools. Use to browse available skills before \
                          assigning them to a role."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
            class: Some("capability".into()),
        },
    );

    m.insert(
        "skill.assign".into(),
        ToolDefinition {
            tool_name: "skill.assign".into(),
            description: "Assigns a registered skill to a role's toolset profile. Once assigned, \
                          the skill's implied tools become available to the role on its next session. \
                          The skill must exist in the catalog. Idempotent."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "role_name": {
                        "type": "string",
                        "description": "The role to assign the skill to (e.g. 'developer')."
                    },
                    "skill_name": {
                        "type": "string",
                        "description": "The name of the skill to assign."
                    }
                },
                "required": ["role_name", "skill_name"]
            }),
            class: Some("config".into()),
        },
    );

    m.insert(
        "skill.revoke".into(),
        ToolDefinition {
            tool_name: "skill.revoke".into(),
            description: "Removes a skill from a role's toolset profile. The skill's implied tools \
                          will no longer be available to the role after the next session reset. \
                          Idempotent."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "role_name": {
                        "type": "string",
                        "description": "The role to revoke the skill from."
                    },
                    "skill_name": {
                        "type": "string",
                        "description": "The name of the skill to revoke."
                    }
                },
                "required": ["role_name", "skill_name"]
            }),
            class: Some("config".into()),
        },
    );

    m.insert(
        "subagent.spawn".into(),
        ToolDefinition {
            tool_name: "subagent.spawn".into(),
            description: "Spawns a new subagent worker in the hotel. The subagent runs \
                          independently with its own lease and model turn budget. Use this to \
                          delegate a discrete, self-contained task to a worker process. The hotel \
                          responds with the subagent's guest ID and confirmed lease details."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "goal": {
                        "type": "string",
                        "description": "The mission goal text delivered to the subagent."
                    },
                    "subagent_kind": {
                        "type": "string",
                        "description": "The worker role to spawn. Defaults to 'philote-worker'."
                    },
                    "context_summary": {
                        "type": "string",
                        "description": "Optional context summary paragraph handed to the subagent \
                                        as background knowledge."
                    },
                    "allowed_tools": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of tool IDs the subagent may use."
                    },
                    "iteration_budget": {
                        "type": "integer",
                        "description": "Maximum model-turn iterations for the subagent. Defaults to 5."
                    }
                },
                "required": ["goal"]
            }),
            class: Some("capability".into()),
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

    m.insert(
        "handoff.to_role".into(),
        ToolDefinition {
            tool_name: "handoff.to_role".into(),
            description: "Transfer active context and work custody to a configured role. \
                          Packages the current working state into a handoff bundle and signals \
                          the hotel to route the session to the named role. The current turn \
                          ends and the target role receives the bundle as its activation context. \
                          Requires operator approval."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "role_name": {
                        "type": "string",
                        "description": "The target role name to hand off to (e.g. 'developer', 'researcher')."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Why this handoff is warranted. Required for operator visibility."
                    },
                    "active_goal": {
                        "type": "string",
                        "description": "The current goal being transferred. The target role inherits this."
                    },
                    "context_summary": {
                        "type": "string",
                        "description": "Compact summary of relevant working context for the target role."
                    },
                    "expected_return_mode": {
                        "type": "string",
                        "enum": ["required", "optional", "none"],
                        "description": "Whether the role is expected to hand back on completion."
                    },
                    "cleanup_actions": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Actions completed before yielding (e.g. 'committed changes', 'closed open files')."
                    }
                },
                "required": ["role_name", "reason"]
            }),
            class: Some("handoff".into()),
        },
    );

    m.insert(
        "handoff.back".into(),
        ToolDefinition {
            tool_name: "handoff.back".into(),
            description: "Return context and work custody back to the orchestrator or a \
                          specified prior role. Packages a completion summary and signals \
                          the hotel to restore the previous session route. Use when the \
                          current role's mission is complete or blocked. \
                          Requires operator approval."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "Summary of what was accomplished or why control is being returned."
                    },
                    "return_to": {
                        "type": "string",
                        "description": "Optional role name to return to. Defaults to the orchestrator."
                    }
                },
                "required": ["summary"]
            }),
            class: Some("handoff".into()),
        },
    );

    m.insert(
        "role.configure".into(),
        ToolDefinition {
            tool_name: "role.configure".into(),
            description: "Create or update a role incarnation for the current agent identity. \
                          Requires reasoning about: purpose, toolset, skillset, handoff posture, \
                          and limits (TTL, iteration caps). Only the orchestrator can use this tool."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "role_name": {
                        "type": "string",
                        "description": "The name of the role (e.g. 'developer', 'researcher')."
                    },
                    "toolset_profile": {
                        "type": "string",
                        "description": "The profile name determining default tools/skills (e.g. 'codex', 'research', 'utility')."
                    },
                    "role_identity_addendum": {
                        "type": "string",
                        "description": "Additive persona/identity instructions for this specific role."
                    },
                    "role_manifest": {
                        "type": "string",
                        "description": "Governance document for this role — focus, rules, delegation posture, and approval constraints. Written in natural language. The agent sees this as its [Governance] context block when the role is active. Should describe: what this role does, what tools are available and when to use them, what requires approval, and when to hand off."
                    },
                    "is_admin": {
                        "type": "boolean",
                        "description": "If true, this role has admin authority — it may update operator-owned records such as the orchestrator manifest. Only existing admin roles may create other admin roles. Setting this to true always triggers a live operator approval interrupt that cannot be preapproved or bypassed."
                    },
                    "inactive_ttl_seconds": {
                        "type": "integer",
                        "description": "Seconds of inactivity before the role is suspended/terminated."
                    },
                    "iteration_cap": {
                        "type": "integer",
                        "description": "Maximum model-turn iterations allowed for this role before it must return or stop."
                    },
                    "approval_policy": {
                        "type": "string",
                        "description": "Stringified JSON describing the approval policy structure."
                    },
                    "model_profile": {
                        "type": "string",
                        "description": "Stringified JSON describing model preferences (provider, temperature)."
                    },
                    "context_window_policy": {
                        "type": "string",
                        "description": "Stringified JSON describing context packaging rules."
                    },
                    "reasoning": {
                        "type": "object",
                        "description": "Required reasoning for this role's existence, purpose, and capability posture.",
                        "properties": {
                            "purpose": { "type": "string" },
                            "toolset_rationale": { "type": "string" },
                            "handoff_posture_and_limits": { "type": "string" }
                        },
                        "required": ["purpose", "toolset_rationale", "handoff_posture_and_limits"]
                    }
                },
                "required": ["role_name", "toolset_profile", "reasoning"]
            }),
            class: Some("config".into()),
        },
    );

    m.insert(
        "rule.propose".into(),
        ToolDefinition {
            tool_name: "rule.propose".into(),
            description: "Propose a durable behavioral rule to be stored permanently in the \
                          agent's context graph. Rules survive dialogue window compaction and \
                          are injected into every cognitive call. A rule captures a constraint, \
                          pattern, or standing preference that should govern all future behavior. \
                          Always requires live operator approval — cannot be preapproved or \
                          bypassed by policy."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "The behavioral rule in the imperative. \
                                        E.g. 'Always ask for clarification before deleting files.' \
                                        Max 512 characters."
                    },
                    "rationale": {
                        "type": "string",
                        "description": "The observation or reasoning that motivates this rule. \
                                        Reference the specific turn, pattern, or user correction \
                                        that led here. Max 1024 characters."
                    }
                },
                "required": ["description", "rationale"]
            }),
            class: Some("config".into()),
        },
    );

    m.insert(
        "memory.recall".into(),
        ToolDefinition {
            tool_name: "memory.recall".into(),
            description: "Retrieve memories relevant to a query from the agent's long-term \
                          autobiographical store (MuninnDB). Returns the most salient engrams \
                          based on semantic + graph activation. Use when the current context \
                          is insufficient and prior knowledge may apply."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural-language query describing what to recall. \
                                        Be specific — the activation pipeline uses this as \
                                        the primary context signal."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of engrams to return. Defaults to the \
                                        session recall_limit setting (default 5). Range 1–20."
                    }
                },
                "required": ["query"]
            }),
            class: Some("memory".into()),
        },
    );

    m.insert(
        "memory.remember".into(),
        ToolDefinition {
            tool_name: "memory.remember".into(),
            description: "Store a new memory in the agent's long-term autobiographical store \
                          (MuninnDB). Use for facts, decisions, user preferences, or observations \
                          that should persist across sessions. Keep concept slugs short and \
                          content atomic (1–3 sentences)."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "concept": {
                        "type": "string",
                        "description": "Short slug identifying the memory (e.g. 'user preference: dark mode', \
                                        'decision: use postgres'). Max 128 characters."
                    },
                    "content": {
                        "type": "string",
                        "description": "The memory content. Keep atomic — one fact or decision. \
                                        1–3 sentences max."
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional tags for recall filtering (e.g. ['preference', 'user', 'decision'])."
                    }
                },
                "required": ["concept", "content"]
            }),
            class: Some("memory".into()),
        },
    );

    m
}
