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
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::OnceLock;

static TOOL_CATALOG: OnceLock<HashMap<String, ToolDefinition>> = OnceLock::new();

fn graph_record_ref_schema() -> Value {
    json!({
        "type": "object",
        "required": ["id", "label"],
        "properties": {
            "id": {
                "type": "string",
                "description": "Life Graph node ID (e.g. 'life:open_loop:rowing')."
            },
            "label": {
                "type": "string",
                "description": "Life Graph node type. Examples: Person, Role, Goal, System, Habit, Project, Commitment, OpenLoop, NextAction, Routine, Decision, Preference, Value, Concern, Event, Signal, GrowthHypothesis, GrowthExperiment, DriftFinding, CapabilityPatch, SkillPatch, ToolPatch, SchemaPatch, AttentionPatch, SystemPatch, StewardshipInstruction."
            },
            "datasource": {
                "type": "string",
                "description": "Usually 'life-graph'.",
                "default": "life-graph"
            }
        }
    })
}

fn source_ref_schema() -> Value {
    json!({
        "type": "object",
        "required": ["source_id", "source_kind", "reliability"],
        "properties": {
            "source_id": {
                "type": "string",
                "description": "Source membrane, agent, or memory ID (e.g. 'membrane:telegram')."
            },
            "source_kind": {
                "type": "string",
                "enum": [
                    "operator_confirmation", "membrane_event",
                    "muninn_engram", "graph_passage",
                    "imported_record", "agent_inference",
                    "runtime_observation"
                ]
            },
            "reliability": {
                "type": "object",
                "required": ["score", "basis"],
                "properties": {
                    "score": {
                        "type": "number",
                        "minimum": 0,
                        "maximum": 1
                    },
                    "basis": {
                        "type": "string",
                        "enum": [
                            "operator_confirmed", "direct_observation",
                            "muninn_trust", "imported_authority",
                            "agent_inferred", "unknown"
                        ]
                    }
                }
            },
            "uri": { "type": "string" },
            "captured_at": {
                "type": "string",
                "description": "ISO 8601 timestamp when this source was captured."
            }
        }
    })
}

fn passage_ref_schema() -> Value {
    json!({
        "type": "object",
        "required": ["passage_id"],
        "properties": {
            "passage_id": { "type": "string" },
            "source_ref_id": { "type": "string" },
            "excerpt_hash": { "type": "string" },
            "muninn_engram_id": { "type": "string" },
            "graph_node_id": { "type": "string" }
        }
    })
}

fn evidence_packet_schema() -> Value {
    json!({
        "type": "object",
        "required": [
            "packet_id", "claim_ref", "claim_summary",
            "confidence", "validation_state", "source_reliability",
            "adjudication_status"
        ],
        "properties": {
            "packet_id": {
                "type": "string",
                "description": "Unique ID for this evidence packet."
            },
            "claim_ref": graph_record_ref_schema(),
            "claim_summary": {
                "type": "string",
                "description": "One or two sentence summary of what was observed."
            },
            "source_refs": {
                "type": "array",
                "items": source_ref_schema(),
                "default": []
            },
            "passage_refs": {
                "type": "array",
                "items": passage_ref_schema(),
                "default": []
            },
            "confidence": {
                "type": "number",
                "minimum": 0,
                "maximum": 1
            },
            "validation_state": {
                "type": "string",
                "enum": ["inferred", "proposed", "confirmed", "retired", "conflicted"],
                "default": "proposed"
            },
            "source_reliability": {
                "type": "number",
                "minimum": 0,
                "maximum": 1
            },
            "adjudication_status": {
                "type": "string",
                "enum": [
                    "not_needed", "pending", "muninn_first",
                    "graph_review", "operator_required", "resolved", "rejected"
                ],
                "default": "not_needed"
            },
            "observed_at": {
                "type": "string",
                "description": "ISO 8601 timestamp when this was observed."
            },
            "valid_time_range": {
                "type": "object",
                "properties": {
                    "starts_at": { "type": "string" },
                    "ends_at": { "type": "string" }
                }
            },
            "conflict_ids": {
                "type": "array",
                "items": {"type": "string"},
                "default": []
            },
            "metadata": {
                "type": "object",
                "default": {}
            }
        },
        "description": "EvidencePacket. Provide at least one source_ref or passage_ref; ungrounded evidence is rejected by the runner."
    })
}

fn conflict_handoff_schema() -> Value {
    json!({
        "type": "object",
        "required": [
            "handoff_id", "conflict_id", "finding_type", "summary",
            "recommended_owner", "requested_muninn_action", "risk",
            "requires_operator", "status"
        ],
        "properties": {
            "handoff_id": { "type": "string" },
            "conflict_id": { "type": "string" },
            "finding_type": {
                "type": "string",
                "enum": [
                    "direct_contradiction", "temporal_conflict",
                    "granularity_conflict", "identity_ambiguity",
                    "staleness", "policy_risk"
                ]
            },
            "summary": { "type": "string" },
            "graph_fact_refs": {
                "type": "array",
                "items": graph_record_ref_schema(),
                "default": []
            },
            "evidence_packets": {
                "type": "array",
                "items": evidence_packet_schema(),
                "default": []
            },
            "muninn_engram_ids": {
                "type": "array",
                "items": { "type": "string" },
                "default": []
            },
            "recommended_owner": {
                "type": "string",
                "enum": ["muninn", "data_memory_graph_rag", "shared_gate", "operator"]
            },
            "requested_muninn_action": {
                "type": "string",
                "enum": ["none", "true_up", "contradiction_review", "trust_update", "cultivate"]
            },
            "risk": {
                "type": "string",
                "enum": ["low", "medium", "high"]
            },
            "requires_operator": { "type": "boolean" },
            "status": {
                "type": "string",
                "enum": ["open", "sent_to_muninn", "awaiting_operator", "resolved", "closed_no_action"]
            },
            "metadata": {
                "type": "object",
                "default": {}
            }
        },
        "description": "ConflictHandoff. Include graph_fact_refs, evidence_packets, or muninn_engram_ids so the conflict is grounded."
    })
}

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
        "role.governance" => &[
            "session.status",
            "hotel.best_place_to_run",
            "agent.configure",
            "role.create_or_update",
            "role.set_home",
            "transport.set_home",
        ],
        "role.authoring" => &["session.status", "role.create_or_update", "handoff.to_role"],
        "memory" => &[
            "memory.recall",
            "memory.remember",
            "memory.cultivate",
            "memory.true_up",
            "memory.promote_candidate",
            "memory.status",
            "memory.fix",
        ],
        "routing.refinement" => &[
            "session.status",
            "agent.graph.read",
            "agent.graph.write",
            "routing.policy.propose",
            "routing.reflex.set",
            "routing.reflex.get",
            "routing.pipeline.set",
            "routing.pipeline.remove",
            "routing.pipeline.get",
        ],
        "graph.knowledge" => &[
            "graph.create",
            "graph.query",
            "graph.list",
            "graph.schema",
            "graph.drop",
            "graph.grant_access",
        ],
        "life.steward" => &[
            "life.observe",
            "life.recall",
            "life.recall.feedback",
            "life.commit",
            "life.resolve",
            "life.conflict",
            "life.patch.propose",
        ],
        "lifegraph.truth_summarizer" => &["life.recall", "graph.query"],
        "imessage-monitor" => &["bash.exec"],
        _ => &[],
    }
}

/// Returns the tools that are exclusively granted by a given on-demand skill.
///
/// Used by `project_tools_for_turn` to know which tool schemas to suppress when
/// the skill is not relevant to the current turn. Tools that appear in multiple
/// skill grants are included in each grant — the caller should include the tool
/// if ANY of its owning skills is active.
pub fn tools_for_skill(skill_name: &str) -> &'static [&'static str] {
    match skill_name {
        "life.steward" => &[
            "life.observe",
            "life.recall",
            "life.recall.feedback",
            "life.commit",
            "life.resolve",
            "life.conflict",
            "life.patch.propose",
        ],
        "lifegraph.truth_summarizer" => &["life.recall", "graph.query"],
        "cron.manage" => &[
            "cron.register",
            "cron.list",
            "cron.enable",
            "cron.disable",
            "cron.remove",
        ],
        "observability.pipeline" => &[
            "table.configure",
            "table.query",
            "table.insert",
            "table.rolloff",
            "table.stats",
            "table.schema",
            "table.add_listener",
        ],
        "graph.knowledge" => &[
            "graph.create",
            "graph.query",
            "graph.list",
            "graph.schema",
            "graph.drop",
            "graph.grant_access",
        ],
        "routing.refinement" => &[
            "routing.policy.propose",
            "routing.reflex.set",
            "routing.reflex.get",
            "routing.pipeline.set",
            "routing.pipeline.remove",
            "routing.pipeline.get",
            "router.stats",
        ],
        "role.governance" => &[
            "agent.configure",
            "role.create_or_update",
            "role.set_home",
            "transport.set_home",
            "hotel.best_place_to_run",
        ],
        "role.authoring" => &["role.create_or_update"],
        "skill.authoring" => &["skill.register", "skill.assign", "skill.revoke"],
        "context.synthesize" => &["workspace.list", "workspace.read"],
        "agent.initiate" => &["agent.graph.write", "agent.graph.recall"],
        "profile.manage" => &["role.configure"],
        "mcp.manage" => &["mcp.provision", "mcp.revoke"],
        _ => &[],
    }
}

/// Returns `true` if the given on-demand skill is relevant for the current turn's content.
///
/// Used by `project_tools_for_turn` to decide whether to suppress a skill's tools.
/// Errs on the side of inclusion — only suppresses when the turn clearly has no
/// bearing on the skill's domain.
pub fn skill_is_relevant_for_turn(skill_name: &str, turn_text: &str) -> bool {
    let t = turn_text;
    match skill_name {
        "life.steward" => {
            t.contains("life.")
                || t.contains("lifegraph")
                || t.contains("life graph")
                // "live graph" is a common mishearing/typo of "life graph" — voice
                // transcription and fast typing both produce it routinely.
                || t.contains("live graph")
                || t.contains("openloop")
                || t.contains("open loop")
                || t.contains("re-enter")
                || t.contains("reenter")
                || t.contains("re-entry")
                || t.contains("follow-through")
                || t.contains("follow through")
                || t.contains("commitment")
                || t.contains("commitments")
                || t.contains("goals")
                || t.contains("habits")
                || t.contains("signal node")
                || t.contains("life.observe")
                || t.contains("life.recall")
                || t.contains("life.recall.feedback")
                || t.contains("life.commit")
                || t.contains("record this")
                || t.contains("observe this")
                || t.contains("note this")
                || t.contains("remember this")
                || t.contains("log this")
                // Loop-lifecycle verbs: the words an operator actually types when
                // closing something out or ratifying a proposed item. Without these,
                // "Confirm for both. Finished my YPT." projects zero life.* tools —
                // the model has no way to call life.commit even though the
                // life.steward charter explicitly instructs it to close/confirm
                // loops when the operator reports them done.
                || t.contains("confirm")
                || t.contains("confirmed")
                || t.contains("done")
                || t.contains("finished")
                || t.contains("complete")
                || t.contains("completed")
                || t.contains("resolved")
                || t.contains("close the loop")
                || t.contains("close it out")
                || t.contains("mark done")
                || t.contains("mark complete")
                || t.contains("mark it done")
                || t.contains("cross the finish line")
        }
        "lifegraph.truth_summarizer" => {
            t.contains("lifegraph")
                || t.contains("life graph")
                || t.contains("live graph")
                || t.contains("what is in my graph")
                || t.contains("what's in my graph")
                || t.contains("summarize my graph")
                || t.contains("roles")
                || t.contains("habits")
                || t.contains("systems")
                || t.contains("goals")
        }
        "cron.manage" => {
            t.contains("cron")
                || t.contains("schedule")
                || t.contains("recurring")
                || t.contains("daily")
                || t.contains("weekly")
                || t.contains("hourly")
                || t.contains("every day")
                || t.contains("every hour")
                || t.contains("every week")
                || t.contains("remind me")
                || t.contains("run at ")
                || t.contains("timer")
                || t.contains("scheduled")
                || t.contains("at midnight")
                || t.contains("at noon")
        }
        "observability.pipeline" => {
            t.contains("table")
                || t.contains("pipeline")
                || t.contains("rolloff")
                || t.contains("datasource")
                || t.contains("table.")
                || t.contains("stream")
                || t.contains("insert row")
                || t.contains("table schema")
                || t.contains("listener")
        }
        "graph.knowledge" => {
            t.contains("graph.")
                || t.contains("knowledge graph")
                || t.contains("create graph")
                || t.contains("graph node")
                || t.contains("graph query")
                || t.contains("graph edge")
                || t.contains("grant_access")
                || t.contains("drop graph")
        }
        "routing.refinement" => {
            t.contains("routing")
                || t.contains("reflex")
                || t.contains("pipeline route")
                || t.contains("routing policy")
                || t.contains("router.stats")
                || t.contains("routing.")
                || t.contains("dispatch policy")
        }
        "role.governance" => {
            t.contains("create role")
                || t.contains("update role")
                || (t.contains("provision") && t.contains("role"))
                || (t.contains("equip") && t.contains("role"))
                || t.contains("configure agent")
                || t.contains("best place to run")
                || t.contains("place role")
                || t.contains("home node")
                || t.contains("role.create")
                || t.contains("agent.configure")
                || t.contains("set_home")
        }
        "role.authoring" => {
            t.contains("create role")
                || t.contains("update role")
                || t.contains("author role")
                || (t.contains("provision") && t.contains("role"))
                || (t.contains("equip") && t.contains("role"))
                || t.contains("role.create")
                || t.contains("role manifest")
                || t.contains("new role")
                || t.contains("write role")
        }
        "skill.authoring" => {
            t.contains("register skill")
                || t.contains("skill.register")
                || t.contains("skill.assign")
                || t.contains("create skill")
                || t.contains("add skill")
                || t.contains("assign skill")
                || (t.contains("equip") && t.contains("skill"))
                || t.contains("with skill")
                || t.contains("cron.manage")
                || t.contains("revoke skill")
        }
        "context.synthesize" => {
            t.contains("workspace")
                || t.contains("list files")
                || t.contains("read file")
                || t.contains("code file")
                || t.contains("synthesize context")
                || t.contains("workspace.")
                || t.contains("file list")
        }
        "agent.initiate" => {
            t.contains("agent.graph")
                || t.contains("write agent")
                || t.contains("initiate agent")
                || t.contains("agent graph write")
                || t.contains("recall agent")
        }
        "profile.manage" => {
            t.contains("role.configure")
                || t.contains("configure role")
                || t.contains("profile.manage")
                || t.contains("manage profile")
                || t.contains("role config")
        }
        "mcp.manage" => {
            t.contains("mcp")
                || t.contains("provision")
                || t.contains("revoke mcp")
                || t.contains("mcp.provision")
                || t.contains("mcp server")
                || t.contains("mcp route")
        }
        _ => false,
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
            description: "Diagnostic only. Returns a summary of this conversation session: active \
                          session ID, turn state, approval policy, and active tool runners. Use \
                          only when the operator asks about session/tool availability or when \
                          debugging routing. Do not use as a default first step for domain work."
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
            description: "Diagnostic only. Returns a safe view of the hotel's current state: \
                          hotel name, node ID, active/inactive guests, and registered agent \
                          identities. Use when the operator asks about hotel health, guest \
                          placement, or runtime debugging. Do not use for ordinary memory, \
                          LifeGraph, or planning questions."
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
        "router.stats".into(),
        ToolDefinition {
            tool_name: "router.stats".into(),
            description: "Returns aggregated routing performance statistics from the model-router \
                          trace store: per-provider success rate, average latency, p90 latency, \
                          and failure counts broken down by task_kind. Use this to inspect which \
                          providers are performing well or poorly, diagnose routing failures, or \
                          reason about model selection. Pass `window_hours` to limit to recent \
                          history (e.g. 24 for last day); omit for all-time stats."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "window_hours": {
                        "type": "number",
                        "description": "If set, only include traces from the last N hours. Omit for all-time stats."
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
            description: "Read agent-local operating preferences and declarations from the \
                          agent graph. This is not the operator LifeGraph and not the repo intel \
                          graph. Use only for agent policy such as tool_preferences, routing, \
                          resource grants, or reflex preferences."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "entity": {
                        "type": "string",
                        "enum": ["resource_grants", "tool_preferences", "routing_preferences", "reflex_preferences", "resource_declarations"],
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
                        "enum": ["tool_preference", "routing_preference", "reflex_preference"]
                    },
                    "tool_name": { "type": "string" },
                    "preference_key": { "type": "string" },
                    "stage_kind": { "type": "string" },
                    "capability": { "type": "string" },
                    "provider_hint": { "type": "string" },
                    "model_ref": { "type": "string" },
                    "preference_level": { "type": "integer" },
                    "precedence": { "type": "integer" },
                    "reflexes": { "type": "object" },
                    "weight": { "type": "integer" },
                    "config": {}
                },
                "required": ["entity"]
            }),
            class: Some("capability".into()),
        },
    );

    m.insert(
        "agent.graph.declare".into(),
        ToolDefinition {
            tool_name: "agent.graph.declare".into(),
            description: "Declare an agent-local resource shape in the agent graph. Use this for \
                          durable cognitive posture about resources the agent expects to use; it \
                          does not grant hotel authority by itself."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "resource_type": {
                        "type": "string",
                        "description": "Resource type to declare, matching the shared resource enum."
                    },
                    "config_hint": {
                        "type": ["string", "null"],
                        "description": "Optional configuration hint to store with the declaration."
                    }
                },
                "required": ["resource_type"]
            }),
            class: Some("capability".into()),
        },
    );

    m.insert(
        "agent.graph.recall".into(),
        ToolDefinition {
            tool_name: "agent.graph.recall".into(),
            description: "Recall recent agent-graph experience traces. Use this to inspect prior \
                          tool calls, resource requests, model invocations, or approval outcomes \
                          recorded for this agent."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 100,
                        "description": "Maximum number of traces to return."
                    },
                    "event_type": {
                        "type": "string",
                        "description": "Optional event type filter, such as tool_call."
                    }
                }
            }),
            class: Some("capability".into()),
        },
    );

    m.insert(
        "agent.graph.sync".into(),
        ToolDefinition {
            tool_name: "agent.graph.sync".into(),
            description: "Apply an inbound agent-graph snapshot using last-writer-wins merge \
                          semantics. This is normally used by mesh synchronization rather than \
                          casual model turns."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "agent_id": { "type": "string" },
                    "source_node_id": { "type": "string" },
                    "exported_at": { "type": "integer" },
                    "preferences": { "type": "array", "items": { "type": "object" } },
                    "routing_preferences": { "type": "array", "items": { "type": "object" } },
                    "reflex_preferences": { "type": "array", "items": { "type": "object" } },
                    "declarations": { "type": "array", "items": { "type": "object" } }
                },
                "required": ["agent_id", "source_node_id", "exported_at", "preferences", "declarations"]
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
                          Supported patterns: \
                          MATCH (n) RETURN n [all nodes], \
                          MATCH (n:Label) RETURN n [by label], \
                          MATCH (n:Label {id:'X'}) RETURN n [by label+id], \
                          CREATE (n:Label {id:'...', ...}) [create node], \
                          CREATE (a:L {id:'A'})-[:REL]->(b:L {id:'B'}) [create edge], \
                          MATCH (n {id:'X'}) DELETE n [delete node], \
                          MATCH (n:Label {id:'X'}) DETACH DELETE n [delete node with label], \
                          MATCH ()-[r {id:'X'}]-() DELETE r [delete edge]."
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
        "graph.schema".into(),
        ToolDefinition {
            tool_name: "graph.schema".into(),
            description: "Inspect the labels and relationship types visible in the active graph \
                          datasource provider."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "graph_id": {
                        "type": "string",
                        "description": "Optional graph partition identifier."
                    }
                }
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
            description:
                "Grant another agent read or write access to one of your graph partitions.".into(),
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
            description: "Return the CREATE TABLE DDL for a table in the local table store.".into(),
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
        "cron.register".into(),
        ToolDefinition {
            tool_name: "cron.register".into(),
            description: "Register a cron job on the hotel. Use cron.list first to avoid \
                          duplicates. The schedule is a 7-field cron expression and the payload \
                          is a JSON string delivered to the target role."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "schedule": {
                        "type": "string",
                        "description": "7-field cron expression: <sec> <min> <hour> <dom> <month> <dow> <year>. Example: \"0 0 7 * * * *\" for 7:00 AM daily."
                    },
                    "target_role": {
                        "type": "string",
                        "description": "Role name whose inbox receives the trigger payload, such as orchestrator."
                    },
                    "payload": {
                        "type": "string",
                        "description": "JSON payload string delivered to the role. Include enough context for the recipient to act without extra lookup."
                    },
                    "guaranteed": {
                        "type": "boolean",
                        "description": "Mesh-coordinated delivery flag. Optional; defaults to false."
                    },
                    "silent_ok": {
                        "type": "boolean",
                        "description": "When true, a fire's reply is suppressed (never delivered to the operator channel) if it matches the Hermes [SILENT]/NO_REPLY convention (whole response, or standing alone on the first/last line). Optional; defaults to false."
                    }
                },
                "required": ["schedule", "target_role", "payload"]
            }),
            class: Some("cron".into()),
        },
    );

    m.insert(
        "cron.list".into(),
        ToolDefinition {
            tool_name: "cron.list".into(),
            description: "List cron jobs registered on this hotel, including schedule, target \
                          role, enabled state, and next fire time."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
            class: Some("cron".into()),
        },
    );

    for (tool_name, verb) in [
        ("cron.enable", "Re-enable"),
        ("cron.disable", "Disable"),
        ("cron.remove", "Remove"),
    ] {
        m.insert(
            tool_name.into(),
            ToolDefinition {
                tool_name: tool_name.into(),
                description: format!("{verb} a cron job by id."),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "job_id": {
                            "type": "string",
                            "description": "The cron job UUID."
                        }
                    },
                    "required": ["job_id"]
                }),
                class: Some("cron".into()),
            },
        );
    }

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
            description: "Configuration inspection only. Lists registered skills in the hotel's \
                          skill catalog, including validation states and implied tools. Use when \
                          the operator asks to inspect or assign skills. Do not call just to decide \
                          how to answer a normal user request."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
            class: Some("capability".into()),
        },
    );

    m.insert(
        "role.list".into(),
        ToolDefinition {
            tool_name: "role.list".into(),
            description: "Configuration inspection only. Lists configured roles and active role \
                          incarnations for this agent/hotel. Use when the operator asks which \
                          roles exist, which role is active, or when debugging role routing. Do \
                          not use as a generic discovery step before answering domain questions."
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
        "transport.set_home".into(),
        ToolDefinition {
            tool_name: "transport.set_home".into(),
            description: "Pin an external membrane transport resource to the one hotel that may \
                          own its inbound poller or gateway. Use this for scarce ingress like \
                          Telegram long polling, Discord gateway sessions, or desktop chat \
                          surfaces. This is separate from role.set_home: roles may run elsewhere \
                          while a single stable hotel owns the transport. Requires operator \
                          approval."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "transport": {
                        "type": "string",
                        "description": "Transport implementation name, e.g. 'telegram', 'discord', or 'desktop'."
                    },
                    "resource_ref": {
                        "type": "string",
                        "description": "Stable transport resource reference, such as a bot token key."
                    },
                    "target_hotel": {
                        "type": "string",
                        "description": "Hotel node_id/name that should own the active poller or gateway."
                    },
                    "standby_hotels": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional standby hotels allowed for explicit future failover."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Why this transport belongs on this hotel. Required for operator visibility."
                    }
                },
                "required": ["transport", "resource_ref", "target_hotel", "reason"]
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
                    "fallback_tiers": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Ordered model-role fallback ladder for this role (e.g. ['model', 'model.openrouter', 'model.ollama']). OMIT this field to leave the role's existing ladder untouched — omitting it never clears a previously configured ladder. Pass an explicit non-empty list only when you intend to replace it."
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
            description: "Paracrine dispatch — silently consults a specialist role. By default \
                          the specialist's response arrives back asynchronously as a \
                          paracrine_response. Use routing='enriched_tool_result' or \
                          wait_for_response=true when the main loop should pause until the \
                          aside completes and then continue with the specialist result as this \
                          tool's result. Use other routing modes for quiet delegation, mid-turn \
                          enrichment, or specialist consultation where the user does not need to \
                          see the handoff. \
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
                        "description": "A succinct structured brief for the specialist — state the goal, the minimum essential context, any constraints, and the expected return format. Do NOT paste conversation transcripts, tool output, or your full working context; the specialist keeps its own session context per conversation. Budget: ~4000 characters — anything longer is truncated before dispatch."
                    },
                    "reply_to": {
                        "type": "string",
                        "description": "Where the specialist's response should go. 'self' = back to this philote as paracrine_response (default). 'membrane' = directly to the user with a role-switch button. '<node>/<role>' = explicit routing."
                    },
                    "routing": {
                        "type": "string",
                        "enum": ["cognitive_re_entry", "enriched_tool_result", "datasource_injection", "memory_enrichment", "progress_update", "heartbeat", "raw_forward", "priority_re_entry", "approval_resolution"],
                        "description": "How to handle the specialist's response when it arrives. Defaults to reflective_re_entry (the orchestrator reflects on the reply privately and decides whether to surface it)."
                    },
                    "wait_for_response": {
                        "type": "boolean",
                        "description": "When true, keep the current turn in waiting_tool until the specialist completes. If routing is omitted, this implies routing='enriched_tool_result'."
                    },
                    "authority": {
                        "type": "string",
                        "enum": ["advice_only", "context_patch", "execute_with_approval"],
                        "description": "Authority granted to the paracrine side loop. Defaults to advice_only."
                    },
                    "tool_policy": {
                        "type": "string",
                        "enum": ["role_default", "read_only", "no_tools"],
                        "description": "Tool policy for the specialist side loop. Defaults to role_default."
                    },
                    "approval_scope": {
                        "type": "string",
                        "enum": ["originating_session", "specialist_session"],
                        "description": "Where approval requests from the side loop should resolve. Defaults to originating_session."
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
                    "fallback_tiers": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Ordered model-role fallback ladder for this role (e.g. ['model', 'model.openrouter', 'model.ollama']). OMIT this field to leave the role's existing ladder untouched — omitting it never clears a previously configured ladder. Pass an explicit non-empty list only when you intend to replace it."
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
        "routing.reflex.set".into(),
        ToolDefinition {
            tool_name: "routing.reflex.set".into(),
            description: "Set a durable routing reflex that takes effect immediately on the \
                          next turn. Use to self-administer low-level dispatch preferences \
                          without operator approval. Currently supports \
                          `preferred_generation_capability` (values: `text.generate` for \
                          standard Gemini, `response.generate` for Gemini Live native audio). \
                          Setting a reflex overrides any prior value for the same key."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "preference_key": {
                        "type": "string",
                        "description": "Stable identifier for this reflex, e.g. 'generation_capability_preference'. Used to update or clear the reflex later."
                    },
                    "reflexes": {
                        "type": "object",
                        "description": "Reflex fields to set. Supported keys: 'preferred_generation_capability' ('text.generate' or 'response.generate').",
                        "properties": {
                            "preferred_generation_capability": {
                                "type": "string",
                                "enum": ["text.generate", "response.generate"],
                                "description": "Which generation capability to dispatch cognitive turns to."
                            }
                        }
                    },
                    "reason": {
                        "type": "string",
                        "description": "Brief reason for the change — stored in the config alongside the reflex for observability."
                    }
                },
                "required": ["preference_key", "reflexes"]
            }),
            class: Some("utility".into()),
        },
    );

    m.insert(
        "routing.reflex.get".into(),
        ToolDefinition {
            tool_name: "routing.reflex.get".into(),
            description: "Read the agent's current routing reflexes from the agent graph. \
                          Returns all stored reflex preference rows including their keys, \
                          precedences, and current reflex values. Use to inspect what \
                          generation capability or routing posture is active before deciding \
                          whether to change it."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "preference_key": {
                        "type": "string",
                        "description": "Optional — if provided, returns only the row with this key. If omitted, returns all rows."
                    }
                },
                "required": []
            }),
            class: Some("utility".into()),
        },
    );

    m.insert(
        "routing.pipeline.set".into(),
        ToolDefinition {
            tool_name: "routing.pipeline.set".into(),
            description: "Declare or replace a routing pipeline rule in the agent graph. \
                          Pipeline rules define how the hotel pre-processes inbound envelopes \
                          before delivering them to this agent — for example, transcribing \
                          a voice memo before the agent sees it. Setting a rule with the same \
                          rule_id replaces the previous definition. Takes effect on the next \
                          inbound turn."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "rule_id": {
                        "type": "string",
                        "description": "Stable identifier for this rule (e.g. 'voice-transcribe'). \
                                        Used as the primary key — re-set with the same rule_id to update."
                    },
                    "match": {
                        "type": "object",
                        "description": "Envelope match criteria. Supported fields: frame_kind (array of strings), \
                                        channel_kind (array of strings), has_text (bool)."
                    },
                    "stages": {
                        "type": "array",
                        "description": "Ordered list of transform stages. Each stage has: capability (string), \
                                        mode ('blob'|'stream'), collect ('full'|'incremental'), \
                                        output_as ('replace_content'|'append_content'), \
                                        on_failure ('passthrough'|'drop'|'error').",
                        "items": { "type": "object" }
                    },
                    "deliver_as": {
                        "type": "string",
                        "description": "How to deliver the transformed envelope to the agent. \
                                        One of: 'user_message' (default), 'tool_result'."
                    }
                },
                "required": ["rule_id", "match", "stages"]
            }),
            class: Some("utility".into()),
        },
    );

    m.insert(
        "routing.pipeline.remove".into(),
        ToolDefinition {
            tool_name: "routing.pipeline.remove".into(),
            description: "Remove a routing pipeline rule from the agent graph. \
                          After removal, inbound envelopes that previously matched this rule \
                          will be delivered to the agent without transformation."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "rule_id": {
                        "type": "string",
                        "description": "The rule_id of the rule to remove."
                    }
                },
                "required": ["rule_id"]
            }),
            class: Some("utility".into()),
        },
    );

    m.insert(
        "routing.pipeline.get".into(),
        ToolDefinition {
            tool_name: "routing.pipeline.get".into(),
            description: "Read the agent's declared routing pipeline rules from the agent graph. \
                          Returns all stored rules or a specific rule by rule_id. \
                          Use to inspect which pipeline transforms are currently active."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "rule_id": {
                        "type": "string",
                        "description": "Optional — if provided, returns only the rule with this id. \
                                        If omitted, returns all rules."
                    }
                },
                "required": []
            }),
            class: Some("utility".into()),
        },
    );

    m.insert(
        "memory.recall".into(),
        ToolDefinition {
            tool_name: "memory.recall".into(),
            description: "Retrieve memories relevant to a query from the agent's long-term \
                          autobiographical store (MuninnDB). Returns the most salient engrams \
                          based on semantic + graph activation. Use for prior conversation, \
                          operator preferences, and agent continuity. For roles, goals, habits, \
                          systems, open loops, or commitments in the operator's LifeGraph, prefer \
                          life.recall."
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
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["self", "shared_user", "session", "cross"],
                        "description": "Optional memory scope. 'cross' searches agent self, \
                                        shared user, and active session memories."
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
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["self", "shared_user", "session"],
                        "description": "Optional write scope. Use 'shared_user' for operator \
                                        preferences, 'session' for active workstream context, \
                                        and 'self' for the agent's own habits or observations."
                    },
                    "temporal_kind": {
                        "type": "string",
                        "enum": ["event", "state", "preference", "rule", "decision", "hypothesis", "gap", "checkpoint"],
                        "description": "Optional temporal behavior for the memory."
                    },
                    "spatial_scope": {
                        "type": "string",
                        "enum": ["self", "agent", "user", "session", "workspace", "hotel", "mesh", "global"],
                        "description": "Optional spatial scope for where this memory applies."
                    },
                    "authority": {
                        "type": "string",
                        "enum": ["observed_runtime", "observed_repo", "graph_structured", "user_stated", "verified_memory", "inferred_memory", "external", "untrusted"],
                        "description": "Optional evidence authority. Runtime/repo observations should be used only when actually observed."
                    },
                    "validation_level": {
                        "type": "string",
                        "enum": ["unverified", "check-green", "test-green", "smoke-green", "watched-live-green"],
                        "description": "Optional validation level backing the memory."
                    },
                    "observed_at": {
                        "type": "integer",
                        "description": "Optional Unix timestamp in milliseconds for when this was observed. Defaults to write time."
                    },
                    "valid_from": {
                        "type": "integer",
                        "description": "Optional Unix timestamp in milliseconds when this memory starts being valid."
                    },
                    "valid_until": {
                        "type": "integer",
                        "description": "Optional Unix timestamp in milliseconds when this memory stops being valid."
                    },
                    "last_verified_at": {
                        "type": "integer",
                        "description": "Optional Unix timestamp in milliseconds when this memory was last verified."
                    },
                    "hotel_id": {
                        "type": "string",
                        "description": "Optional hotel this memory applies to."
                    },
                    "node_id": {
                        "type": "string",
                        "description": "Optional node this memory applies to."
                    },
                    "workspace_path": {
                        "type": "string",
                        "description": "Optional workspace path this memory applies to."
                    },
                    "branch": {
                        "type": "string",
                        "description": "Optional git branch this memory applies to."
                    },
                    "proposal_id": {
                        "type": "string",
                        "description": "Optional AgentGraph proposal anchor."
                    },
                    "seam_id": {
                        "type": "string",
                        "description": "Optional AgentGraph seam anchor."
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Optional AgentGraph task anchor."
                    },
                    "affected_nodes": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional graph/code node ids this memory affects."
                    }
                },
                "required": ["concept", "content"]
            }),
            class: Some("memory".into()),
        },
    );

    m.insert(
        "memory.cultivate".into(),
        ToolDefinition {
            tool_name: "memory.cultivate".into(),
            description: "Inspect recalled memories for cultivation needs without mutating memory. \
                          Use during closeout, reflection, or explicit memory-maintenance turns to \
                          find missing spacetime frames, entity overlays, or stale candidates."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural-language scope for candidate memories to inspect."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum candidate count. Defaults to 8. Range 1-20."
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["self", "shared_user", "session", "cross"],
                        "description": "Optional memory scope to inspect."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["report", "closeout", "true_up"],
                        "description": "Cultivation intent. Current implementation reports candidates and performs no mutation."
                    }
                },
                "required": ["query"]
            }),
            class: Some("memory".into()),
        },
    );

    m.insert(
        "memory.true_up".into(),
        ToolDefinition {
            tool_name: "memory.true_up".into(),
            description: "Compare a memory claim with observed repo/runtime truth or AgentGraph truth \
                          and return a structured true-up finding. Use when memory and graph may have \
                          holes, contradictions, or stale assumptions."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "memory_claim": {
                        "type": "string",
                        "description": "Claim currently held in memory."
                    },
                    "observed_truth": {
                        "type": "string",
                        "description": "Observed repo/runtime truth, if directly checked."
                    },
                    "graph_truth": {
                        "type": "string",
                        "description": "AgentGraph structured truth, if available."
                    },
                    "resolution": {
                        "type": "string",
                        "description": "Optional proposed resolution or operator note."
                    },
                    "muninn_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Muninn engram ids involved in this finding."
                    },
                    "graph_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "AgentGraph node ids involved in this finding."
                    },
                    "evidence_refs": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Evidence references such as file paths, test names, or smoke runs."
                    }
                },
                "required": []
            }),
            class: Some("memory".into()),
        },
    );

    m.insert(
        "memory.promote_candidate".into(),
        ToolDefinition {
            tool_name: "memory.promote_candidate".into(),
            description: "Evaluate whether a memory candidate is allowed to become durable shared \
                          memory. This is a gate: it reports missing authority, validation, or evidence \
                          and does not promote unverified inferred claims."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "candidate": {
                        "type": "string",
                        "description": "Candidate memory or claim being considered for promotion."
                    },
                    "authority": {
                        "type": "string",
                        "enum": ["observed_runtime", "observed_repo", "graph_structured", "user_stated", "verified_memory", "inferred_memory", "external", "untrusted"],
                        "description": "Evidence authority behind the candidate."
                    },
                    "validation_level": {
                        "type": "string",
                        "enum": ["unverified", "check-green", "test-green", "smoke-green", "watched-live-green"],
                        "description": "Validation level backing the candidate."
                    },
                    "evidence_refs": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Evidence references such as files, test runs, graph ids, or Muninn ids."
                    },
                    "operator_approved": {
                        "type": "boolean",
                        "description": "Set only when the operator explicitly approves promotion despite missing gates."
                    }
                },
                "required": ["candidate"]
            }),
            class: Some("memory".into()),
        },
    );

    m.insert(
        "memory.status".into(),
        ToolDefinition {
            tool_name: "memory.status".into(),
            description: "Report the current MuninnDB connection status: whether the hotel has \
                          the endpoint configured, whether it was reachable on the last probe, \
                          and how many vaults are registered. Use before memory.recall/remember \
                          if you suspect a connection issue."
                .into(),
            input_schema: json!({ "type": "object", "properties": {} }),
            class: Some("memory".into()),
        },
    );

    m.insert(
        "memory.fix".into(),
        ToolDefinition {
            tool_name: "memory.fix".into(),
            description: "Diagnose and recover MuninnDB memory connectivity. Triggers an \
                          immediate hotel-side reachability probe and returns the result. \
                          If the probe succeeds, memory tools are re-enabled. If it fails, \
                          the outage is recorded in the heal queue for dispatcher action. \
                          Use when memory.recall or memory.remember report errors or when \
                          memory.status shows unreachable."
                .into(),
            input_schema: json!({ "type": "object", "properties": {} }),
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
                                    "description": "Always use kind='field_map'. Do NOT pass a string or template.",
                                    "properties": {
                                        "kind": { "type": "string", "enum": ["field_map"] },
                                        "action": { "type": "string", "description": "Envelope action string, e.g. 'echo' or 'datasource.query'." },
                                        "target": {
                                            "type": "object",
                                            "description": "philote: {kind:'philote',agent_id:'...'}  datasource: {kind:'datasource',datasource_id:'...'}  tool: {kind:'tool',tool_ref:'...'}",
                                            "properties": {
                                                "kind": { "type": "string", "enum": ["philote", "datasource", "tool"] },
                                                "agent_id": { "type": "string" },
                                                "datasource_id": { "type": "string" },
                                                "tool_ref": { "type": "string" }
                                            },
                                            "required": ["kind"]
                                        },
                                        "mappings": {
                                            "type": "array",
                                            "items": {
                                                "type": "object",
                                                "properties": {
                                                    "from": { "type": "string", "description": "Arg key from MCP call, e.g. 'query'." },
                                                    "to": { "type": "string", "description": "Destination key in envelope payload, e.g. 'query'." }
                                                },
                                                "required": ["from", "to"]
                                            }
                                        }
                                    },
                                    "required": ["kind", "action", "target"]
                                },
                                "outbound_transform": {
                                    "type": "object",
                                    "description": "pass_through: {kind:'pass_through'}  extract: {kind:'extract',path:'dot.path'}",
                                    "properties": {
                                        "kind": { "type": "string", "enum": ["pass_through", "extract"] },
                                        "path": { "type": "string" }
                                    },
                                    "required": ["kind"]
                                }
                            },
                            "required": ["name", "description", "input_schema",
                                         "inbound_transform", "outbound_transform"]
                        }
                    },
                    "exposure": {
                        "type": "string",
                        "enum": ["local", "lan", "mesh", "internet"],
                        "description": "Minimum tier at which this endpoint is intentionally \
                                        served. The hotel validates that exposure <= its current \
                                        perimeter ceiling and rejects the provision if it does not. \
                                        Defaults to 'local'. Set to 'mesh' for Tailscale-reachable \
                                        endpoints or 'internet' for public endpoints."
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

    m.insert(
        "mcp.status".into(),
        ToolDefinition {
            tool_name: "mcp.status".into(),
            description: "Return the current configuration and exposure status of an MCP \
                          endpoint. Shows the stored McpEndpointConfig (tools, preapproval rules, \
                          port), the hotel's current perimeter ceiling, and whether the endpoint \
                          is active. Use this to verify a provision succeeded, check whether an \
                          endpoint's exposure tier is within the hotel's perimeter ceiling, or \
                          inspect the live configuration."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "endpoint_id": {
                        "type": "string",
                        "description": "The endpoint ID to inspect."
                    }
                },
                "required": ["endpoint_id"]
            }),
            class: Some("session".into()),
        },
    );

    m.insert(
        "agent.migrate_to".into(),
        ToolDefinition {
            tool_name: "agent.migrate_to".into(),
            description: "Migrate this agent to a different hotel. All context, memory, \
                          role incarnations, vault secrets (Telegram tokens, etc.), and \
                          guest processes travel to the destination. The source hotel \
                          remains active as a fallback. Requires operator approval."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "dest_hotel": {
                        "type": "string",
                        "description": "Name of the destination hotel (e.g. 'vps-jane')."
                    }
                },
                "required": ["dest_hotel"]
            }),
            class: Some("admin".into()),
        },
    );

    // ── Life Graph OS tools ────────────────────────────────────────────────────

    m.insert(
        "life.observe".into(),
        ToolDefinition {
            tool_name: "life.observe".into(),
            description: "Propose a grounded observation as a new Life Graph evidence node. \
                          Use this to record commitments, open loops, goals, habits, events, \
                          and other lived-reality facts with provenance. \
                          The node is written to the Life Graph with validation_state=proposed \
                          and must be confirmed before becoming durable truth."
                .into(),
            input_schema: json!({
                "type": "object",
                "required": ["observation_id", "evidence"],
                "properties": {
                    "observation_id": {
                        "type": "string",
                        "description": "Unique ID for this observation (e.g. 'obs:rowing-2026-06-05')."
                    },
                    "evidence": {
                        "type": "object",
                        "required": ["packet_id", "claim_ref", "claim_summary", "source_refs",
                                     "confidence", "validation_state", "source_reliability",
                                     "adjudication_status"],
                        "properties": {
                            "packet_id": {
                                "type": "string",
                                "description": "Unique ID for this evidence packet."
                            },
                            "claim_ref": {
                                "type": "object",
                                "required": ["id", "label"],
                                "properties": {
                                    "id": {
                                        "type": "string",
                                        "description": "Life Graph node ID (e.g. 'life:open_loop:rowing')."
                                    },
                                    "label": {
                                        "type": "string",
                                        "description": "Life Graph node type. Must be one of: \
                                            Person, Role, Goal, System, Habit, Project, Commitment, \
                                            OpenLoop, NextAction, Routine, Decision, Preference, Value, \
                                            Concern, Event, Signal, GrowthHypothesis, GrowthExperiment, \
                                            DriftFinding, CapabilityPatch, SkillPatch, ToolPatch, \
                                            SchemaPatch, AttentionPatch, SystemPatch, StewardshipInstruction."
                                    },
                                    "datasource": {
                                        "type": "string",
                                        "description": "Always 'life-graph'.",
                                        "default": "life-graph"
                                    }
                                }
                            },
                            "claim_summary": {
                                "type": "string",
                                "description": "One or two sentence summary of what was observed."
                            },
                            "source_refs": {
                                "type": "array",
                                "minItems": 1,
                                "items": {
                                    "type": "object",
                                    "required": ["source_id", "source_kind", "reliability"],
                                    "properties": {
                                        "source_id": {
                                            "type": "string",
                                            "description": "Source membrane or agent ID (e.g. 'membrane:telegram')."
                                        },
                                        "source_kind": {
                                            "type": "string",
                                            "enum": [
                                                "operator_confirmation", "membrane_event",
                                                "muninn_engram", "graph_passage",
                                                "imported_record", "agent_inference",
                                                "runtime_observation"
                                            ]
                                        },
                                        "reliability": {
                                            "type": "object",
                                            "required": ["score", "basis"],
                                            "properties": {
                                                "score": {
                                                    "type": "number",
                                                    "minimum": 0, "maximum": 1
                                                },
                                                "basis": {
                                                    "type": "string",
                                                    "enum": [
                                                        "operator_confirmed", "direct_observation",
                                                        "muninn_trust", "imported_authority",
                                                        "agent_inferred", "unknown"
                                                    ]
                                                }
                                            }
                                        }
                                    }
                                }
                            },
                            "confidence": {
                                "type": "number",
                                "minimum": 0, "maximum": 1,
                                "description": "Confidence in this claim (0.0–1.0)."
                            },
                            "validation_state": {
                                "type": "string",
                                "enum": ["inferred", "proposed", "confirmed", "retired", "conflicted"],
                                "default": "proposed"
                            },
                            "source_reliability": {
                                "type": "number",
                                "minimum": 0, "maximum": 1
                            },
                            "adjudication_status": {
                                "type": "string",
                                "enum": [
                                    "not_needed", "pending", "muninn_first",
                                    "graph_review", "operator_required", "resolved", "rejected"
                                ],
                                "default": "not_needed"
                            },
                            "observed_at": {
                                "type": "string",
                                "description": "ISO 8601 timestamp when this was observed."
                            },
                            "conflict_ids": {
                                "type": "array",
                                "items": {"type": "string"},
                                "default": []
                            },
                            "passage_refs": {
                                "type": "array",
                                "items": {"type": "object"},
                                "default": []
                            },
                            "metadata": {
                                "type": "object",
                                "default": {}
                            }
                        }
                    },
                    "proposed_graph_refs": {
                        "type": "array",
                        "items": {"type": "object"},
                        "default": []
                    }
                }
            }),
            class: Some("life_graph".into()),
        },
    );

    m.insert(
        "life.recall".into(),
        ToolDefinition {
            tool_name: "life.recall".into(),
            description: "Retrieve context from the operator's LifeGraph: roles, goals, habits, \
                          systems, open loops, commitments, and next actions. Use this whenever \
                          the operator asks what is in the LifeGraph or asks about life structure. \
                          Provide query_text and, when useful, operator_intent; the runner can use \
                          text/fallback recall when an embedding is not available. Named intents: \
                          open_loops_by_context, goals_and_next_actions, commitments_approaching, \
                          re_entry_context."
                .into(),
            input_schema: json!({
                "type": "object",
                "required": ["query_id", "query_text"],
                "properties": {
                    "query_id": {"type": "string"},
                    "query_text": {"type": "string", "description": "Human-language query."},
                    "strategy": {
                        "type": "string",
                        "enum": ["semantic_pivot", "vector_then_expand", "memory_aware_graph_rank"],
                        "default": "memory_aware_graph_rank",
                        "description": "Defaults to memory_aware_graph_rank when omitted."
                    },
                    "operator_intent": {
                        "type": "string",
                        "description": "Named strategy hint: open_loops_by_context, goals_and_next_actions, commitments_approaching, re_entry_context."
                    },
                    "semantic_pivots": {
                        "type": "array",
                        "description": "Optional. The runner can auto-embed query_text when this is omitted.",
                        "items": {
                            "type": "object",
                            "required": ["space", "embedding_model", "embedding_dims", "query_text_hash"],
                            "properties": {
                                "space": {
                                    "type": "string",
                                    "enum": ["life_event_semantic", "goal_system_semantic",
                                             "skill_tool_semantic", "role_person_semantic",
                                             "memory_bridge_semantic"]
                                },
                                "embedding_model": {"type": "string", "default": "Xenova/all-mpnet-base-v2"},
                                "embedding_dims": {"type": "integer", "default": 768},
                                "query_text_hash": {"type": "string"}
                            }
                        }
                    },
                    "expansion_policy": {
                        "type": "object",
                        "properties": {
                            "max_hops": {"type": "integer", "default": 2},
                            "max_nodes": {"type": "integer", "default": 24},
                            "allowed_edge_types": {"type": "array", "items": {"type": "string"}, "default": []}
                        }
                    },
                    "policy_filters": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["exclude_retired", "exclude_conflicted_unless_requested",
                                     "require_evidence", "role_appropriate", "low_agency_only"]
                        },
                        "default": ["exclude_retired", "require_evidence"]
                    },
                    "ranking_weights": {
                        "type": "object",
                        "properties": {
                            "semantic_similarity": {"type": "number", "default": 0.45},
                            "graph_specificity": {"type": "number", "default": 0.2},
                            "recency": {"type": "number", "default": 0.1},
                            "confirmation": {"type": "number", "default": 0.15},
                            "active_commitment": {"type": "number", "default": 0.1},
                            "role_relevance": {"type": "number", "default": 0.15}
                        }
                    },
                    "active_role": {"type": "string"},
                    "max_context_packets": {"type": "integer", "default": 6}
                }
            }),
            class: Some("life_graph".into()),
        },
    );

    m.insert(
        "life.recall.feedback".into(),
        ToolDefinition {
            tool_name: "life.recall.feedback".into(),
            description: "Record whether a LifeGraph recall packet was useful, stale, missing \
                          context, noisy, overconfident, or disconnected. Use this after a \
                          LifeGraph recall result influences or fails to influence the turn so \
                          the graph can improve bridge/ranking/attention behavior without \
                          silently confirming new life truth."
                .into(),
            input_schema: json!({
                "type": "object",
                "required": ["feedback_id", "packet_id", "rating"],
                "properties": {
                    "feedback_id": {
                        "type": "string",
                        "description": "Unique ID for this retrieval feedback event."
                    },
                    "packet_id": {
                        "type": "string",
                        "description": "The RetrievalContextPacket or recall packet ID being evaluated."
                    },
                    "query_summary": {
                        "type": "string",
                        "description": "Short summary of the original recall query."
                    },
                    "rating": {
                        "type": "string",
                        "enum": ["useful", "stale", "missing", "noisy", "overconfident", "disconnected"]
                    },
                    "note": {
                        "type": "string",
                        "description": "Brief reason for the rating."
                    },
                    "candidate_count": {
                        "type": "integer",
                        "minimum": 0,
                        "default": 0
                    },
                    "connected_candidate_count": {
                        "type": "integer",
                        "minimum": 0,
                        "default": 0
                    },
                    "missing_context_refs": {
                        "type": "array",
                        "items": {"type": "string"},
                        "default": []
                    },
                    "noisy_node_refs": {
                        "type": "array",
                        "items": {"type": "object"},
                        "default": []
                    },
                    "stale_node_refs": {
                        "type": "array",
                        "items": {"type": "object"},
                        "default": []
                    },
                    "evidence_packets": {
                        "type": "array",
                        "items": {"type": "object"},
                        "default": []
                    }
                }
            }),
            class: Some("life_graph".into()),
        },
    );

    m.insert(
        "life.commit".into(),
        ToolDefinition {
            tool_name: "life.commit".into(),
            description: "Promote a validated Life Graph evidence node to confirmed truth. \
                          Requires either confirmed evidence or explicit operator approval. \
                          This is also the tool for closing a loop: when the operator says a \
                          previously-observed OpenLoop/Commitment/Goal is done, life.recall it \
                          first to get its exact node id, then call life.commit on that id with \
                          loop_status set to \"resolved\" and an up-to-date claim_summary — do \
                          NOT leave stale recalled content (e.g. yesterday's \"paused/halfway\" \
                          note) standing as the record once the operator has reported it done. \
                          life.resolve is a SEPARATE tool for governed conflict handoffs only — \
                          it cannot close an ordinary loop."
                .into(),
            input_schema: json!({
                "type": "object",
                "required": ["evidence", "operator_approved"],
                "properties": {
                    "evidence": evidence_packet_schema(),
                    "operator_approved": {
                        "type": "boolean",
                        "description": "True if operator has explicitly approved this commit."
                    },
                    "loop_status": {
                        "type": "string",
                        "description": "Optional. Set to \"resolved\" when the operator has \
                            reported this node's underlying loop/commitment/goal as done — \
                            marks it closed (distinct from validation_state, which tracks \
                            evidence trust, not lifecycle). Omit to leave lifecycle status \
                            unchanged."
                    },
                    "resolution_note": {
                        "type": "string",
                        "description": "Optional. Short note on how/why this closed, in the \
                            operator's own words where possible (e.g. \"operator reported \
                            complete 2026-07-09\"). Only meaningful alongside loop_status."
                    }
                }
            }),
            class: Some("life_graph".into()),
        },
    );

    m.insert(
        "life.conflict".into(),
        ToolDefinition {
            tool_name: "life.conflict".into(),
            description: "Open a governed Life Graph conflict handoff when graph evidence, \
                          Muninn memory, or operator context disagrees. Use this to record \
                          the conflict and route it toward Muninn, graph review, shared gate, \
                          or operator resolution; do not use it for ordinary observations."
                .into(),
            input_schema: json!({
                "type": "object",
                "required": ["handoff"],
                "properties": {
                    "handoff": conflict_handoff_schema()
                }
            }),
            class: Some("life_graph".into()),
        },
    );

    m.insert(
        "life.resolve".into(),
        ToolDefinition {
            tool_name: "life.resolve".into(),
            description: "Resolve a Life Graph CONFLICT HANDOFF after review — operates only \
                          on ConflictHandoff nodes opened via life.conflict. This is a \
                          governance tool: high-risk or operator-required handoffs need \
                          explicit operator approval before resolution. It does NOT close an \
                          ordinary OpenLoop/Commitment/Goal — for \"the operator says this is \
                          done\", use life.commit with loop_status=\"resolved\" instead."
                .into(),
            input_schema: json!({
                "type": "object",
                "required": ["handoff", "resolution_summary", "operator_approved"],
                "properties": {
                    "handoff": conflict_handoff_schema(),
                    "resolution_summary": {
                        "type": "string",
                        "description": "Concise explanation of how the conflict should be resolved."
                    },
                    "operator_approved": {
                        "type": "boolean",
                        "description": "True only when the operator explicitly approved this resolution."
                    }
                }
            }),
            class: Some("life_graph".into()),
        },
    );

    m.insert(
        "life.patch.propose".into(),
        ToolDefinition {
            tool_name: "life.patch.propose".into(),
            description: "Propose a governed Life Graph improvement: schema change, skill update, \
                          tool addition, attention policy change, or system-level change. \
                          High-risk patches require operator approval before applying."
                .into(),
            input_schema: json!({
                "type": "object",
                "required": ["patch_id", "patch_kind", "summary", "rationale", "evidence_packets", "risk"],
                "properties": {
                    "patch_id": {"type": "string"},
                    "patch_kind": {
                        "type": "string",
                        "enum": ["schema_patch", "skill_patch", "tool_patch",
                                 "attention_patch", "system_patch"]
                    },
                    "summary": {"type": "string"},
                    "rationale": {"type": "string"},
                    "evidence_packets": {
                        "type": "array",
                        "minItems": 1,
                        "items": evidence_packet_schema()
                    },
                    "risk": {
                        "type": "string",
                        "enum": ["low", "medium", "high"]
                    },
                    "operator_approved": {
                        "type": "boolean",
                        "default": false
                    }
                }
            }),
            class: Some("life_graph".into()),
        },
    );

    m
}

#[cfg(test)]
mod tests {
    use super::{skill_implied_tools, skill_is_relevant_for_turn, tool_catalog};
    use serde_json::json;

    #[test]
    fn role_list_has_specific_model_facing_description() {
        let catalog = tool_catalog();
        let role_list = catalog.get("role.list").expect("role.list catalog entry");

        assert!(
            role_list
                .description
                .contains("Configuration inspection only")
        );
        assert!(!role_list.description.contains("Execute the role.list tool"));
        assert_eq!(role_list.input_schema["type"], "object");
    }

    #[test]
    fn life_recall_description_does_not_require_model_generated_embedding() {
        let catalog = tool_catalog();
        let life_recall = catalog
            .get("life.recall")
            .expect("life.recall catalog entry");

        assert!(life_recall.description.contains("operator's LifeGraph"));
        assert!(life_recall.description.contains("text/fallback recall"));
        assert_eq!(
            life_recall.input_schema["required"],
            json!(["query_id", "query_text"])
        );
        assert_eq!(
            life_recall.input_schema["properties"]["semantic_pivots"]["description"],
            "Optional. The runner can auto-embed query_text when this is omitted."
        );
        assert!(
            !life_recall
                .description
                .contains("Requires an embedding vector")
        );
    }

    #[test]
    fn life_steward_implied_tools_have_catalog_entries() {
        let catalog = tool_catalog();
        for tool in skill_implied_tools("life.steward") {
            assert!(
                catalog.contains_key(*tool),
                "life.steward implied tool {tool} should have a real catalog schema"
            );
        }
    }

    #[test]
    fn life_write_tools_expose_governance_payloads() {
        let catalog = tool_catalog();
        let commit = catalog
            .get("life.commit")
            .expect("life.commit catalog entry");
        assert_eq!(
            commit.input_schema["required"],
            json!(["evidence", "operator_approved"])
        );
        assert_eq!(
            commit.input_schema["properties"]["evidence"]["required"],
            json!([
                "packet_id",
                "claim_ref",
                "claim_summary",
                "confidence",
                "validation_state",
                "source_reliability",
                "adjudication_status"
            ])
        );

        let conflict = catalog
            .get("life.conflict")
            .expect("life.conflict catalog entry");
        assert_eq!(conflict.input_schema["required"], json!(["handoff"]));
        assert_eq!(
            conflict.input_schema["properties"]["handoff"]["required"],
            json!([
                "handoff_id",
                "conflict_id",
                "finding_type",
                "summary",
                "recommended_owner",
                "requested_muninn_action",
                "risk",
                "requires_operator",
                "status"
            ])
        );

        let resolve = catalog
            .get("life.resolve")
            .expect("life.resolve catalog entry");
        assert_eq!(
            resolve.input_schema["required"],
            json!(["handoff", "resolution_summary", "operator_approved"])
        );
    }

    #[test]
    fn life_graph_feedback_tool_is_model_visible_and_skill_granted() {
        let catalog = tool_catalog();
        let feedback = catalog
            .get("life.recall.feedback")
            .expect("life.recall.feedback catalog entry");

        assert_eq!(feedback.class.as_deref(), Some("life_graph"));
        assert!(feedback.description.contains("recall packet"));
        assert!(
            skill_implied_tools("life.steward")
                .iter()
                .any(|tool| *tool == "life.recall.feedback")
        );
    }

    #[test]
    fn life_steward_tolerates_live_graph_typo() {
        // "live graph" is a one-letter typo of "life graph"/"lifegraph" that real
        // users hit (Telegram, voice transcription). skill_is_relevant_for_turn
        // intentionally errs on the side of inclusion, so this near-miss must still
        // count as relevant rather than silently falling through to zero tools.
        assert!(skill_is_relevant_for_turn(
            "life.steward",
            "can you take a look at the live graph and see what we have on for today"
        ));
        assert!(skill_is_relevant_for_turn(
            "lifegraph.truth_summarizer",
            "what's on the live graph"
        ));
    }

    #[test]
    fn life_steward_relevant_for_loop_lifecycle_verbs() {
        // Regression for the "done means done" bug: an operator turn like
        // "Confirm for both. Finished my YPT." contains none of the older
        // domain-noun keywords (no "life.", "openloop", "commitment", etc.),
        // so life.steward's whole tool group — including life.commit — was
        // silently suppressed and the model had no way to close the loop.
        // Callers normalize turn text to lowercase (see normalized_turn_text
        // in session/mod.rs) before reaching this function.
        for turn in [
            "confirm for both. finished my ypt.",
            "done with the taxes",
            "i finished that",
            "mark it done",
            "that's complete now",
            "yep, resolved",
        ] {
            assert!(
                skill_is_relevant_for_turn("life.steward", turn),
                "expected life.steward to be relevant for {turn:?}"
            );
        }
    }

    #[test]
    fn diagnostic_tool_descriptions_are_explicitly_diagnostic() {
        let catalog = tool_catalog();
        for name in ["session.status", "hotel.status", "skill.list"] {
            let tool = catalog.get(name).expect("cataloged diagnostic tool");
            assert!(
                tool.description.contains("Diagnostic only")
                    || tool.description.contains("Configuration inspection only"),
                "{name} should tell the model it is an inspection tool"
            );
        }
    }
}
