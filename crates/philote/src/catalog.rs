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
        "role.governance" => &["session.status", "agent.configure", "role.create_or_update", "role.set_home"],
        "role.authoring" => &["session.status", "role.create_or_update", "handoff.to_role"],
        "memory" => &["memory.recall", "memory.remember"],
        "routing.refinement" => &[
            "session.status",
            "agent.graph.read",
            "agent.graph.write",
            "routing.policy.propose",
        ],
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
/// of what the model requests. Tools in class "config" or "shell" require
/// approval by default; same-self handoff tools are governed by projection/reflex
/// policy instead of per-action approval.
pub fn tool_requires_approval(tool_name: &str) -> bool {
    if matches!(tool_name, "handoff.to_role" | "handoff.back") {
        return false;
    }
    matches!(tool_class(tool_name), Some("config") | Some("shell"))
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
        "hotel.status".into(),
        ToolDefinition {
            tool_name: "hotel.status".into(),
            description: "Returns a safe view of the hotel's current state: hotel name, node ID, \
                          active and inactive guests (with roles), and registered agent identities. \
                          No credentials, API keys, or secret values are included. Use this to \
                          understand what guests are running, which agents are registered, and \
                          whether the hotel is healthy. Always prefer this over bash.exec for \
                          hotel introspection."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
            class: Some("session".into()),
        },
    );

    m.insert(
        "hotel.logs".into(),
        ToolDefinition {
            tool_name: "hotel.logs".into(),
            description: "Returns the last N lines from the hotel's log file (aiua.log). Use this \
                          to tail recent log output, diagnose guest failures, or inspect hotel \
                          activity. Defaults to 50 lines. Never use bash.exec to tail logs when \
                          this tool is available."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "lines": {
                        "type": "integer",
                        "description": "Number of log lines to return (default 50, max 500)."
                    }
                }
            }),
            class: Some("session".into()),
        },
    );

    m.insert(
        "echo".into(),
        ToolDefinition {
            tool_name: "echo".into(),
            description: "Echoes a string back unchanged. Only use when explicitly asked to \
                          test tool connectivity. Do not call during normal conversation or \
                          reasoning — this tool exists for diagnostics only."
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
        "agent.graph.read".into(),
        ToolDefinition {
            tool_name: "agent.graph.read".into(),
            description: "Read structured state from the agent's own graph substrate. Use this \
                          to inspect agent-local preferences, declarations, and other cognitive \
                          policy records without reaching into hotel-owned authority."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "entity": {
                        "type": "string",
                        "enum": ["resource_grants", "tool_preferences", "routing_preferences", "resource_declarations"],
                        "description": "The agent-graph entity collection to read."
                    }
                },
                "required": ["entity"]
            }),
            class: Some("capability".into()),
        },
    );

    m.insert(
        "agent.graph.write".into(),
        ToolDefinition {
            tool_name: "agent.graph.write".into(),
            description: "Write an agent-local graph preference or configuration record. Use for \
                          governed self-configuration inside the agent graph, such as tool or \
                          routing preferences. This does not mutate hotel authority or the shared \
                          model graph directly."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "entity": {
                        "type": "string",
                        "enum": ["tool_preference", "routing_preference"]
                    },
                    "tool_name": { "type": "string" },
                    "preference_key": { "type": "string" },
                    "stage_kind": { "type": "string" },
                    "capability": { "type": "string" },
                    "provider_hint": { "type": "string" },
                    "model_ref": { "type": "string" },
                    "preference_level": { "type": "integer" },
                    "weight": { "type": "integer" },
                    "config": {}
                },
                "required": ["entity"]
            }),
            class: Some("capability".into()),
        },
    );

    m.insert(
        "graph.create".into(),
        ToolDefinition {
            tool_name: "graph.create".into(),
            description: "Create a new named graph partition in the agent graph store. \
                          Omit graph_id to create your own personal partition (keyed to your agent_id). \
                          Each partition is an isolated SQLite-backed Cypher graph you own and can query."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "graph_id": {
                        "type": "string",
                        "description": "Partition name (defaults to your agent_id if omitted)."
                    }
                }
            }),
            class: Some("graph".into()),
        },
    );

    m.insert(
        "graph.query".into(),
        ToolDefinition {
            tool_name: "graph.query".into(),
            description: "Run a Cypher query against a graph partition. \
                          Use this to read or write structured knowledge — entities, relationships, \
                          and typed records — in the agent graph store. \
                          Omit graph_id to query your own partition. \
                          Supported patterns: MATCH (n:Label) RETURN n, \
                          CREATE (n:Label {id:'...', ...}), \
                          CREATE (a:L {id:'A'})-[:REL]->(b:L {id:'B'})."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "graph_id": {
                        "type": "string",
                        "description": "Partition to query (defaults to your agent_id)."
                    },
                    "query": {
                        "type": "string",
                        "description": "Cypher query string."
                    },
                    "parameters": {
                        "type": "object",
                        "description": "Optional query parameters.",
                        "default": {}
                    }
                },
                "required": ["query"]
            }),
            class: Some("graph".into()),
        },
    );

    m.insert(
        "graph.list".into(),
        ToolDefinition {
            tool_name: "graph.list".into(),
            description: "List all graph partitions accessible to you in the agent graph store."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
            class: Some("graph".into()),
        },
    );

    m.insert(
        "graph.drop".into(),
        ToolDefinition {
            tool_name: "graph.drop".into(),
            description: "Permanently drop a graph partition and all its data. \
                          Only drop partitions you own."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "graph_id": {
                        "type": "string",
                        "description": "Partition to drop."
                    }
                },
                "required": ["graph_id"]
            }),
            class: Some("graph".into()),
        },
    );

    m.insert(
        "graph.grant_access".into(),
        ToolDefinition {
            tool_name: "graph.grant_access".into(),
            description: "Grant another agent read or write access to one of your graph partitions."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "graph_id": {
                        "type": "string",
                        "description": "Partition to share."
                    },
                    "agent_id": {
                        "type": "string",
                        "description": "Agent to grant access to."
                    },
                    "access": {
                        "type": "string",
                        "enum": ["read", "write"],
                        "description": "Access level to grant."
                    }
                },
                "required": ["graph_id", "agent_id", "access"]
            }),
            class: Some("graph".into()),
        },
    );

    // ── Table tools (table-datasource) ─────────────────────────────────────────

    m.insert(
        "table.configure".into(),
        ToolDefinition {
            tool_name: "table.configure".into(),
            description: "Create a new table in the local table store using a CREATE TABLE SQL \
                          statement. Use this to set up observability tables, event logs, or any \
                          flat structured data that your agent-graph neighborhood should track."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "CREATE TABLE IF NOT EXISTS ... SQL DDL statement."
                    }
                },
                "required": ["query"]
            }),
            class: Some("table".into()),
        },
    );

    m.insert(
        "table.query".into(),
        ToolDefinition {
            tool_name: "table.query".into(),
            description: "Run a SELECT query against a table in the local table store. \
                          Returns rows as JSON objects. Use graph_id to specify the table name \
                          and query for the SQL. Pass limit in parameters to cap results."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "graph_id": {
                        "type": "string",
                        "description": "Table name (also set as the target table for the query context)."
                    },
                    "query": {
                        "type": "string",
                        "description": "SELECT SQL query."
                    },
                    "parameters": {
                        "type": "object",
                        "description": "Optional query parameters. Include 'limit' to cap result count.",
                        "default": {}
                    }
                },
                "required": ["query"]
            }),
            class: Some("table".into()),
        },
    );

    m.insert(
        "table.insert".into(),
        ToolDefinition {
            tool_name: "table.insert".into(),
            description: "Insert a row into a table in the local table store. \
                          Pass the table name as graph_id and the row as parameters."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "graph_id": {
                        "type": "string",
                        "description": "Table name."
                    },
                    "parameters": {
                        "type": "object",
                        "description": "Row data as column→value pairs."
                    }
                },
                "required": ["graph_id", "parameters"]
            }),
            class: Some("table".into()),
        },
    );

    m.insert(
        "table.rolloff".into(),
        ToolDefinition {
            tool_name: "table.rolloff".into(),
            description: "Apply data retention rules to a table — delete rows older than \
                          max_age_secs or keep only the newest max_rows rows. \
                          Pass the table name as graph_id."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "graph_id": {
                        "type": "string",
                        "description": "Table name."
                    },
                    "parameters": {
                        "type": "object",
                        "description": "Rolloff options: max_rows (integer), max_age_secs (integer), ts_column (string, default 'timestamp').",
                        "properties": {
                            "max_rows": { "type": "integer" },
                            "max_age_secs": { "type": "integer" },
                            "ts_column": { "type": "string" }
                        }
                    }
                },
                "required": ["graph_id"]
            }),
            class: Some("table".into()),
        },
    );

    m.insert(
        "table.stats".into(),
        ToolDefinition {
            tool_name: "table.stats".into(),
            description: "Return row count and latest timestamp for a table in the local table store."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "graph_id": {
                        "type": "string",
                        "description": "Table name."
                    },
                    "parameters": {
                        "type": "object",
                        "description": "Options: ts_column (string, default 'timestamp').",
                        "default": {}
                    }
                },
                "required": ["graph_id"]
            }),
            class: Some("table".into()),
        },
    );

    m.insert(
        "table.schema".into(),
        ToolDefinition {
            tool_name: "table.schema".into(),
            description: "Return the CREATE TABLE DDL for a table in the local table store."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "graph_id": {
                        "type": "string",
                        "description": "Table name."
                    }
                },
                "required": ["graph_id"]
            }),
            class: Some("table".into()),
        },
    );

    m.insert(
        "table.add_listener".into(),
        ToolDefinition {
            tool_name: "table.add_listener".into(),
            description: "Register a router-listener handler that writes matching inbound events \
                          into a local table. Merges into the hotel's router_listener.config so the \
                          router-listener starts routing the specified event_kind to the named table \
                          on its next reconnect. After calling this, run graph.query to store a \
                          TableConfig node in your partition so the table appears in your cognitive \
                          envelope on future sessions."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "event_kind": {
                        "type": "string",
                        "description": "The 'kind' field value on inbound envelopes to match (e.g. 'transcription_capture', 'routing_signal')."
                    },
                    "table_name": {
                        "type": "string",
                        "description": "Target table name in the local table store."
                    },
                    "schema_map": {
                        "type": "object",
                        "description": "Maps event envelope field names to table column names. Omit to pass through the full envelope."
                    },
                    "filter_keys": {
                        "type": "object",
                        "description": "Only process events where these envelope fields match these values."
                    },
                    "adapter_script": {
                        "type": "string",
                        "description": "Path to a Python adapter script. Receives raw event JSON on stdin, emits transformed row JSON on stdout."
                    },
                    "target_role": {
                        "type": "string",
                        "description": "Role to route table.insert tasks to (default: 'table-datasource')."
                    }
                },
                "required": ["event_kind", "table_name"]
            }),
            class: Some("table".into()),
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
            description: "Last-resort shell execution. Runs a shell command and returns stdout, \
                          stderr, and exit code. Use ONLY when no Philotic-native tool \
                          (workspace.read, agent.graph.read, session.status, etc.) can accomplish \
                          the task. Do not call speculatively or for diagnostic purposes. \
                          Requires explicit operator approval before execution. Commands run under \
                          the agent's effective working directory unless overridden by working_dir. \
                          A timeout (default 30 s) is enforced."
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
        "desktop.observe".into(),
        ToolDefinition {
            tool_name: "desktop.observe".into(),
            description: "Returns low-agency metadata about the bound desktop automation runner \
                          and desktop session. This observe-only CUA scaffold does not take a \
                          screenshot and cannot click, type, press keys, or scroll."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "detail": {
                        "type": "string",
                        "enum": ["summary"],
                        "description": "Observation detail level. Only summary metadata is supported in the first scaffold."
                    }
                }
            }),
            class: Some("desktop".into()),
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
                          can be referenced by name when spawning subagents. Always include \
                          skill_name, description, subagent_kind, and goal."
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
            description:
                "Removes a skill from a role's toolset profile. The skill's implied tools \
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
                          profile, bindings, settings, media_routing_policy, and \
                          voice_response_policy sections. Changes to sensitive fields \
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
                                        'bindings.effective_skillset', \
                                        'media_routing_policy.voice_action', \
                                        'media_routing_policy.image_action', \
                                        'media_routing_policy.document_action', \
                                        'media_routing_policy.forward_media_to_model', \
                                        'media_routing_policy.strip_tools_on_media', \
                                        'voice_response_policy.mode', \
                                        'voice_response_policy.provider', \
                                        'voice_response_policy.voice_id', \
                                        'voice_response_policy.send_text_caption', \
                                        'voice_response_policy.fallback_to_text'"
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
                    "target_focus_framing": {
                        "type": "string",
                        "description": "Specific natural language instructions framing what the target role should focus on upon waking."
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
                "required": ["role_name", "reason", "target_focus_framing"]
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
        "role.set_home".into(),
        ToolDefinition {
            tool_name: "role.set_home".into(),
            description: "Pin a role to a specific hotel so its philote process runs there, \
                          or clear an existing pin to return it to the authority hotel. \
                          Use this to place specialised roles on the machines where they \
                          have the right tool access — for example, pin an obsidian-keeper \
                          role to the machine where the Obsidian vault lives, or move yourself \
                          to a more capable host. Requires operator approval. \
                          After pinning, the next handoff.to_role call for that role will \
                          automatically route across the mesh."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "role_name": {
                        "type": "string",
                        "description": "The role to move. Use your current active role name to move yourself."
                    },
                    "target_hotel": {
                        "type": "string",
                        "description": "The hotel node_id to run the role on (e.g. 'mac-jane'). \
                                        Omit or pass null to clear the pin and run on the authority hotel."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Why this placement is needed. Required for operator visibility."
                    }
                },
                "required": ["role_name", "reason"]
            }),
            class: Some("config".into()),
        },
    );

    m.insert(
        "delegate.to_peer".into(),
        ToolDefinition {
            tool_name: "delegate.to_peer".into(),
            description: "Delegates a bounded task to a peer Philotic agent on the mesh. \
                          Crosses an identity boundary. Requires explicitly packaging a bounded \
                          context, goal, and clear return contract expectations. Used when \
                          a different agent's expertise or trust domain is needed, rather than \
                          your own active roles. Requires operator approval."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "target_agent_id": {
                        "type": "string",
                        "description": "The unique ID of the peer agent to delegate to (e.g., 'jane')."
                    },
                    "task_description": {
                        "type": "string",
                        "description": "Natural language description of the delegated task."
                    },
                    "context_package": {
                        "type": "string",
                        "description": "Explicit context snapshot necessary for the peer to execute the task."
                    },
                    "expected_artifacts": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of artifacts or outcomes expected back"
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Implicit contract SLA before the delegation is considered stalled."
                    }
                },
                "required": ["target_agent_id", "task_description", "context_package"]
            }),
            class: Some("delegate".into()),
        },
    );

    m.insert(
        "delegate.to_external_cognitive_peer".into(),
        ToolDefinition {
            tool_name: "delegate.to_external_cognitive_peer".into(),
            description: "Delegates a bounded task to an external cognitive peer (e.g., Claude Code, \
                          Codex) that operates outside the Philotic mesh. Crosses both identity and \
                          runtime trust boundaries. Requires explicit packaging, bounds, and a clear \
                          return expectation. Requires operator approval."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "target_peer_type": {
                        "type": "string",
                        "description": "The type or identifier of the external peer (e.g., 'claude_code', 'codex_worktree')."
                    },
                    "task_description": {
                        "type": "string",
                        "description": "Natural language description of the delegated task."
                    },
                    "context_package": {
                        "type": "string",
                        "description": "Explicit context snapshot necessary for the peer to execute the task."
                    },
                    "expected_artifacts": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of artifacts or outcomes expected back"
                    }
                },
                "required": ["target_peer_type", "task_description", "context_package"]
            }),
            class: Some("delegate".into()),
        },
    );

    m.insert(
        "role.create_or_update".into(),
        ToolDefinition {
            tool_name: "role.create_or_update".into(),
            description: "Creates or modifies a role DEFINITION in the hotel database — it writes \
                          a role's manifest, toolset profile, and identity addendum. This is a \
                          configuration-only operation: it does NOT activate, switch to, or hand \
                          off to the role. Do NOT call this before handoff.to_role or \
                          delegate.whisper. If a role already exists and you want to switch to it, \
                          use handoff.to_role directly without calling this first. Only call this \
                          when the operator explicitly asks you to create a new role or change an \
                          existing role's definition. Always include role_name, toolset_profile, \
                          and the full reasoning object with purpose, toolset_rationale, and \
                          handoff_posture_and_limits."
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
        "delegate.whisper".into(),
        ToolDefinition {
            tool_name: "delegate.whisper".into(),
            description: "Fire-and-forget paracrine dispatch — silently consults a specialist \
                          role without interrupting the current turn. The specialist's response \
                          arrives back asynchronously as a paracrine_response. Use for quiet \
                          delegation, mid-turn enrichment, or specialist consultation where the \
                          user does not need to see the handoff. \
                          Set reply_to='membrane' to route the specialist's response directly \
                          to the user with an inline role-switch button."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "role": {
                        "type": "string",
                        "description": "The target specialist role name to dispatch the exosome to."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The prompt or question for the specialist."
                    },
                    "reply_to": {
                        "type": "string",
                        "description": "Where the specialist's response should go. 'self' = back to this philote as paracrine_response (default). 'membrane' = directly to the user with a role-switch button. '<node>/<role>' = explicit routing."
                    },
                    "routing": {
                        "type": "string",
                        "enum": ["cognitive_re_entry", "enriched_tool_result", "datasource_injection", "memory_enrichment", "progress_update", "heartbeat", "raw_forward"],
                        "description": "How to handle the specialist's response when it arrives. Defaults to cognitive_re_entry."
                    }
                },
                "required": ["role", "prompt"]
            }),
            class: Some("delegate".into()),
        },
    );

    m.insert(
        "role.configure".into(),
        ToolDefinition {
            tool_name: "role.configure".into(),
            description: "Low-level compatibility surface for mutating a role incarnation for the \
                          current agent identity. Prefer the governed role.create_or_update workflow \
                          surface for prompt-facing role authoring. This tool still executes the \
                          underlying hotel mutation path and requires the same role_name, \
                          toolset_profile, and reasoning object."
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
        "routing.policy.propose".into(),
        ToolDefinition {
            tool_name: "routing.policy.propose".into(),
            description: "Propose a durable routing or cognition policy refinement for the \
                          agent. Use when repeated evidence suggests a turn stage, provider \
                          preference, context envelope, or affordance posture should be \
                          adjusted. Always requires live operator approval and is stored as a \
                          first-class routing policy artifact with disposition and evaluation \
                          history."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "problem": {
                        "type": "string",
                        "description": "Short description of the recurring routing or cognition problem being observed. Max 512 characters."
                    },
                    "proposed_change": {
                        "type": "string",
                        "description": "The durable routing or cognition policy update being proposed in the imperative. Max 512 characters."
                    },
                    "evidence": {
                        "type": "string",
                        "description": "Concrete evidence motivating the change: failed turn shapes, repeated user corrections, latency/cost mismatch, or provider misfit. Max 1024 characters."
                    },
                    "affected_stage": {
                        "type": "string",
                        "description": "Optional stage hint such as ingress, cognition, or egress."
                    },
                    "affected_capability": {
                        "type": "string",
                        "description": "Optional capability hint such as voice.transcribe, text.generate, or voice.synthesize."
                    },
                    "learned_reflex": {
                        "type": "object",
                        "description": "Optional approved reflex write-back to store in the agent graph if this proposal should immediately update durable adaptive posture.",
                        "properties": {
                            "preference_key": {
                                "type": "string",
                                "description": "Stable key for the learned reflex preference in the agent graph."
                            },
                            "precedence": {
                                "type": "integer",
                                "description": "Optional precedence for the learned reflex layer. Defaults to 70."
                            },
                            "reflexes": {
                                "type": "object",
                                "description": "Reflex fields to write, such as remote_tool_reflex, remote_component_reflex, or credential_scope_reflex."
                            }
                        },
                        "required": ["preference_key", "reflexes"]
                    }
                },
                "required": ["problem", "proposed_change", "evidence"]
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

    m.insert(
        "approval.request_standing".into(),
        ToolDefinition {
            tool_name: "approval.request_standing".into(),
            description: "Request standing (pre-approved) permission for a specific tool based \
                          on a demonstrated track record. Once the tool has succeeded the \
                          required number of times consecutively, operator approval will no \
                          longer be required for that tool. Use this when you have already \
                          proven reliability on a tool and the repeated approval gate is \
                          slowing down legitimate work. Does not require operator approval \
                          to register the threshold."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "tool_name": {
                        "type": "string",
                        "description": "The exact tool name to request standing approval for \
                                        (e.g. 'role.create_or_update')."
                    },
                    "required_successes": {
                        "type": "integer",
                        "description": "Number of consecutive successful executions required \
                                        before standing approval is auto-granted. \
                                        Defaults to 3 if not specified. Min 1, max 10."
                    }
                },
                "required": ["tool_name"]
            }),
            class: Some("approval".into()),
        },
    );

    m.insert(
        "mcp.provision".into(),
        ToolDefinition {
            tool_name: "mcp.provision".into(),
            description: "Declare or update an MCP endpoint this agent exposes to external \
                          callers. Specifies the port, tool listing with inbound/outbound \
                          transforms, and pre-approval rules. The hotel materializes a \
                          membrane-mcp guest for this endpoint. The provisioning turn is the \
                          authorization event — pre-approval rules carry that authority forward \
                          so future requests matching the declared envelope shapes are not \
                          re-blocked for approval."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "endpoint_id": {
                        "type": "string",
                        "description": "Stable ID for this endpoint (e.g. 'bjork-mcp-01'). \
                                        Must be unique within the hotel."
                    },
                    "port": {
                        "type": "integer",
                        "description": "Port the membrane-mcp guest should bind on (e.g. 8910)."
                    },
                    "tools": {
                        "type": "array",
                        "description": "Tools to advertise to MCP clients.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string" },
                                "description": { "type": "string" },
                                "input_schema": { "type": "object" },
                                "inbound_transform": {
                                    "type": "object",
                                    "description": "FieldMap: { kind: 'field_map', action, target, mappings }. \
                                                    Target: { kind: 'datasource'|'philote'|'tool', datasource_id|agent_id|tool_ref }. \
                                                    Mappings: [{ from: 'arg.key', to: 'payload.key' }]."
                                },
                                "outbound_transform": {
                                    "type": "object",
                                    "description": "PassThrough: { kind: 'pass_through' }. \
                                                    Extract: { kind: 'extract', path: 'dot.path' }."
                                }
                            },
                            "required": ["name", "description", "input_schema",
                                         "inbound_transform", "outbound_transform"]
                        }
                    },
                    "preapproval_rules": {
                        "type": "array",
                        "description": "Envelope actions pre-approved by this provisioning turn.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "action_pattern": {
                                    "type": "string",
                                    "description": "Exact action string or '*' to match all."
                                },
                                "expires_at": {
                                    "type": "integer",
                                    "description": "Optional unix epoch expiry. Absent = permanent."
                                }
                            },
                            "required": ["action_pattern"]
                        }
                    }
                },
                "required": ["endpoint_id", "port", "tools"]
            }),
            class: Some("config".into()),
        },
    );

    m.insert(
        "mcp.revoke".into(),
        ToolDefinition {
            tool_name: "mcp.revoke".into(),
            description: "Tear down an MCP endpoint and remove its configuration. \
                          The hotel signals the membrane-mcp guest to shut down and \
                          clears all stored config and pre-approval rules."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "endpoint_id": {
                        "type": "string",
                        "description": "The endpoint ID to revoke (must be owned by this agent)."
                    }
                },
                "required": ["endpoint_id"]
            }),
            class: Some("config".into()),
        },
    );

    m
}
