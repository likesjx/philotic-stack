use std::path::Path;
use std::sync::Arc;

use axum::{extract::State, http::Method, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tower_http::cors::{Any, CorsLayer};

use crate::scanner::{full_scan, ScanConfig};
use crate::schema::*;

use super::ws::ChangeEvent;
use super::AppState;

// ── JSON-RPC 2.0 types ──

#[derive(Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    params: Option<serde_json::Value>,
    // Optional so notifications (no id) deserialize without error
    #[serde(default)]
    id: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    // Omit null fields — MCP schema validator uses strict union, rejects extra keys
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

// ── MCP Tool Definitions ──

fn tool_definitions() -> serde_json::Value {
    serde_json::json!({
        "tools": [
            {
                "name": "graph_status",
                "description": "Get overall project graph status — node counts, last scan time, metrics",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "graph_query",
                "description": "Query nodes by kind and optional filters. Use compact=true for token-efficient summaries (id, name, kind, status only).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "description": "Node kind filter (proposal, seam, crate, module, type, function, skill, sver, domain, etc.)" },
                        "worktree": { "type": "string", "description": "Worktree filter" },
                        "status": { "type": "string", "description": "Status filter (for proposals)" },
                        "compact": { "type": "boolean", "description": "If true, return only id/name/kind/status/disposition (saves tokens). Default: false" }
                    }
                }
            },
            {
                "name": "graph_node",
                "description": "Get a single node by ID with all its edges and related nodes",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Node ID (e.g., 'proposal:desktop-membrane', 'crate:aiua', 'type:GraphDomain')" }
                    },
                    "required": ["id"]
                }
            },
            {
                "name": "graph_skeleton",
                "description": "Generate a PlantUML class diagram for a crate showing its types, traits, and relationships",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "crate_name": { "type": "string", "description": "Crate name (e.g., 'aiua', 'ansible-mesh-core')" }
                    },
                    "required": ["crate_name"]
                }
            },
            {
                "name": "graph_snippet",
                "description": "Get code snippets for a node. Returns signatures by default; pass full=true for complete source.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Node ID to get snippets for" },
                        "full": { "type": "boolean", "description": "Include full body (default: false, returns signatures only)" }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "graph_search",
                "description": "Full-text search across all nodes and code snippets",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search text" }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "graph_proposals",
                "description": "List all proposals with their current status, domain, and active seams",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "graph_manage_proposal",
                "description": "Manage a proposal as graph state and update the agent's work-focus record for it. Use this for proposal status/disposition changes and agent-visible working observations.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "proposal_id": { "type": "string", "description": "Proposal node ID or slug (e.g., 'doc:task-runner' or 'task-runner')" },
                        "agent": { "type": "string", "description": "Agent managing this proposal" },
                        "session": { "type": "string", "description": "Optional session ID" },
                        "status": { "type": "string", "description": "Optional proposal status to set" },
                        "disposition": { "type": "string", "description": "Optional proposal disposition to set" },
                        "current_goal": { "type": "string", "description": "What the agent is currently trying to accomplish for this proposal" },
                        "observation": { "type": "string", "description": "Agent working observation to append" },
                        "assumption": { "type": "string", "description": "Agent assumption to append" },
                        "open_question": { "type": "string", "description": "Open question to append" },
                        "pending_writeback_item": { "type": "string", "description": "Item the agent believes should eventually write back to shared docs or graph state" },
                        "reason": { "type": "string", "description": "Why this proposal management update is being made" }
                    },
                    "required": ["proposal_id", "agent", "reason"]
                }
            },
            {
                "name": "graph_decide",
                "description": "Record a traced decision about a node (e.g., change proposal status, record an architectural decision)",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target_node": { "type": "string", "description": "Node ID the decision applies to" },
                        "action": { "type": "string", "description": "What action was taken (e.g., 'status_change', 'decision', 'defer')" },
                        "from_value": { "type": "string", "description": "Previous value (if changing)" },
                        "to_value": { "type": "string", "description": "New value (if changing)" },
                        "reason": { "type": "string", "description": "Why this decision was made" },
                        "agent": { "type": "string", "description": "Agent making the decision" },
                        "session": { "type": "string", "description": "Session ID" }
                    },
                    "required": ["target_node", "action", "reason"]
                }
            },
            {
                "name": "graph_memory_true_up",
                "description": "Record a memory/graph true-up finding as an audited graph task node linked to affected graph nodes.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "finding_id": { "type": "string", "description": "Optional stable finding id. Defaults to a generated UUID." },
                        "finding_type": { "type": "string", "description": "Finding class, e.g. confirmed, contradicted, stale, missing_memory, underspecified, needs_operator." },
                        "scope": { "type": "string", "description": "Scope such as session, workspace, hotel, mesh, or global." },
                        "summary": { "type": "string", "description": "Concise finding summary." },
                        "muninn_ids": { "type": "array", "items": { "type": "string" }, "description": "Muninn engram ids involved." },
                        "graph_ids": { "type": "array", "items": { "type": "string" }, "description": "AgentGraph node ids involved." },
                        "evidence_refs": { "type": "array", "items": { "type": "string" }, "description": "Evidence references such as files, commands, tests, or smoke runs." },
                        "resolution": { "type": "string", "description": "Resolution or proposed resolution." },
                        "recommended_action": { "type": "string", "description": "Recommended next action." },
                        "requires_operator": { "type": "boolean", "description": "Whether operator review is required." },
                        "agent": { "type": "string", "description": "Agent recording the finding." },
                        "session": { "type": "string", "description": "Session id." }
                    },
                    "required": ["finding_type", "summary"]
                }
            },
            {
                "name": "graph_create_node",
                "description": "Create a new node in the graph (proposal, seam, task, decision). The graph is the source of truth for architecture.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Node ID (e.g., 'proposal:new-feature', 'seam:data-contract')" },
                        "kind": { "type": "string", "description": "Node kind: proposal, seam, task, decision, slice" },
                        "name": { "type": "string", "description": "Human-readable name" },
                        "properties": { "type": "object", "description": "Arbitrary properties (status, domain, tags, etc.)" },
                        "file_path": { "type": "string", "description": "Optional source file path" }
                    },
                    "required": ["id", "kind", "name"]
                }
            },
            {
                "name": "graph_update_node",
                "description": "Update an existing node's properties. Use for status changes, adding tags, etc.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Node ID to update" },
                        "properties": { "type": "object", "description": "Properties to merge/update" },
                        "reason": { "type": "string", "description": "Why this update is being made" }
                    },
                    "required": ["id", "properties", "reason"]
                }
            },
            {
                "name": "graph_create_edge",
                "description": "Create a relationship between two nodes (e.g., proposal applies_to seam, doc references task)",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "source_id": { "type": "string", "description": "Source node ID" },
                        "target_id": { "type": "string", "description": "Target node ID" },
                        "relation": { "type": "string", "description": "Relationship type: applies_to, references, implements, implemented_by, contains, tests, blocks" }
                    },
                    "required": ["source_id", "target_id", "relation"]
                }
            },
            {
                "name": "graph_writeback",
                "description": "Write graph node properties back to source markdown frontmatter. Updates the file on disk and optionally commits.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Node ID to write back (e.g., 'doc:proposal-name')" },
                        "commit": { "type": "boolean", "description": "Auto-commit the change (default: false)" },
                        "agent": { "type": "string", "description": "Agent name for commit" },
                        "reason": { "type": "string", "description": "Reason for writeback" }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "graph_record_test_run",
                "description": "Record a test run outcome for one or more seams/proposals. Creates a TestRun node linked to all targets. Use target_ids array to test a group of seams together — all get the same pass/fail status. Falls back to target_id for single-target runs.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target_id": { "type": "string", "description": "Single target proposal or seam ID (use target_ids for groups)" },
                        "target_ids": { "type": "array", "items": { "type": "string" }, "description": "Array of seam/proposal IDs tested as a group. All get linked to the same TestRun." },
                        "test_count": { "type": "integer", "description": "Total number of tests executed" },
                        "pass_count": { "type": "integer", "description": "Number of tests that passed" },
                        "fail_count": { "type": "integer", "description": "Number of tests that failed (default: 0)" },
                        "coverage_pct": { "type": "number", "description": "Code coverage percentage (default: 0)" },
                        "commit_sha": { "type": "string", "description": "Git commit SHA (optional)" },
                        "duration_ms": { "type": "integer", "description": "Test run duration in milliseconds (default: 0)" }
                    },
                    "required": ["test_count", "pass_count"]
                }
            },
            {
                "name": "graph_advance_verification",
                "description": "Advance a seam/proposal to next verification level (code-complete, test-green, smoke-green, uat-green)",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target_id": { "type": "string", "description": "Seam or proposal ID" },
                        "level": { "type": "string", "description": "Target level: code-complete, test-green, smoke-green, uat-green" },
                        "evidence": { "type": "string", "description": "Evidence ID (test_run, commit_sha, etc.)" },
                        "reason": { "type": "string", "description": "Reason for advancement" }
                    },
                    "required": ["target_id", "level"]
                }
            },
            {
                "name": "session_start",
                "description": "Start a new agent work session. Creates a session node and workstream linked to a seam and optionally a proposal. Call this at the beginning of every agent workstream.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "Unique session ID (e.g., 'session:2026-03-29-1745-cascade')" },
                        "agent": { "type": "string", "description": "Agent identifier (e.g., 'cascade-kimi-k2.5')" },
                        "agent_model": { "type": "string", "description": "Model being used (e.g., 'kimi-k2.5')" },
                        "seam_id": { "type": "string", "description": "Seam this session is working on (the place)" },
                        "proposal_id": { "type": "string", "description": "Proposal this work tracks against (the goal)" },
                        "task_id": { "type": "string", "description": "Optional specific task being implemented" },
                        "phase": { "type": "string", "description": "Initial phase: started, coding, testing" }
                    },
                    "required": ["session_id", "agent", "seam_id"]
                }
            },
            {
                "name": "session_activity",
                "description": "Record activity in an active session. Use this to track file edits, test runs, commits, and token usage. Report tokens_input and tokens_output from each API call to track cost per seam.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "Session ID" },
                        "activity_type": { "type": "string", "description": "Type: file_edit, test_run, commit, phase_change, token_report" },
                        "details": { "type": "object", "description": "Activity details (files, lines changed, test results, tokens)" },
                        "phase": { "type": "string", "description": "New phase if changed (started, coding, testing, green)" },
                        "tokens_input": { "type": "integer", "description": "Input tokens consumed in this activity (from API usage response)" },
                        "tokens_output": { "type": "integer", "description": "Output tokens consumed in this activity (from API usage response)" }
                    },
                    "required": ["session_id", "activity_type"]
                }
            },
            {
                "name": "session_close",
                "description": "Close an agent session. Call this at the end of the workstream with final status. Include tokens_total if you tracked usage, or it will be auto-calculated from session_activity reports.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "Session ID to close" },
                        "status": { "type": "string", "description": "Final status: completed, cancelled, blocked, quota_exhausted" },
                        "verified": { "type": "string", "description": "Verification level: test-green, smoke-green, watched-live-green" },
                        "summary": { "type": "string", "description": "Brief summary of work completed" },
                        "tokens_total": { "type": "integer", "description": "Override total tokens if self-tracked (otherwise uses accumulated session_activity totals)" },
                        "quota_remaining": { "type": "integer", "description": "Approximate remaining quota tokens if known" }
                    },
                    "required": ["session_id", "status"]
                }
            },
            {
                "name": "graph_scan",
                "description": "Trigger a full rescan of the codebase, docs, and git state",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "graph_embed",
                "description": "Generate and store embedding for a node using the ONNX sidecar",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Node ID to embed" },
                        "text": { "type": "string", "description": "Optional text to embed (defaults to node name + properties)" }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "graph_semantic_search",
                "description": "Search nodes by semantic similarity to a query text",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Query text to search for" },
                        "kind": { "type": "string", "description": "Optional node kind filter" },
                        "limit": { "type": "integer", "description": "Max results (default 10)" },
                        "threshold": { "type": "number", "description": "Minimum similarity score (default 0.7)" }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "graph_embed_batch",
                "description": "Batch embed all nodes of a given kind (e.g., all proposals)",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "description": "Node kind to embed (default: proposal)" },
                        "force": { "type": "boolean", "description": "Re-embed even if hash matches (default: false)" }
                    }
                }
            },
            {
                "name": "graph_verify_semantic",
                "description": "Check semantic alignment between proposal and implementing code (verification ladder integration)",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "proposal_id": { "type": "string", "description": "Proposal node ID" },
                        "code_node_ids": { "type": "array", "description": "Code nodes that implement the proposal" }
                    },
                    "required": ["proposal_id"]
                }
            },
            {
                "name": "graph_digest",
                "description": "Compressed architecture overview: domain→proposal→seam→verification chain for the entire project. One call to orient any agent. Use this first.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "graph_export_docs",
                "description": "Sync graph state back to documentation files on disk. Writes status, disposition, domain, tags, active_seams, verification_level, and last_updated into each doc node's frontmatter. Dry-run supported.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "dry_run": { "type": "boolean", "description": "If true, report what would be written without touching files (default: false)" }
                    }
                }
            },
            {
                "name": "graph_export_sver",
                "description": "Generate canonical SVER document from graph verification state and write to docs/architecture/SVER_STATE.md. Returns the markdown. Pass write=true to persist to disk.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "write": { "type": "boolean", "description": "If true, write the file to disk at docs/architecture/SVER_STATE.md (default: false)" }
                    }
                }
            },
            {
                "name": "graph_context_for",
                "description": "Assemble complete work context for a proposal or seam in ONE call. Returns: proposal body, related seams, code signatures, verification state, decisions, active sessions, and a PlantUML diagram. Use this when starting work on a task.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target_id": { "type": "string", "description": "Proposal or seam ID (e.g., 'doc:GRAPH_INTELLIGENCE_PROPOSAL', 'seam:telegram-poll-lease')" }
                    },
                    "required": ["target_id"]
                }
            },
            {
                "name": "graph_next_task",
                "description": "Find the highest-priority unclaimed work item. Considers disposition, verification level, blocking edges, and active sessions to avoid conflicts. Returns scored recommendation with rationale.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "graph_impact",
                "description": "Analyze blast radius of a change. Given a file path or node ID, walks edges to find all affected proposals, seams, and tests. Use after making code changes to understand what needs re-verification.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target": { "type": "string", "description": "File path or node ID to analyze (e.g., 'crates/aiua/src/hotel.rs' or 'type:GraphEngine')" }
                    },
                    "required": ["target"]
                }
            },
            {
                "name": "graph_agent_dashboard",
                "description": "Dashboard of all agent activity: active sessions, recent work, per-agent summaries, and verification progress across all proposals.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "graph_persist_diagrams",
                "description": "Auto-generate and persist canonical PlantUML diagrams to docs/architecture/generated/. Writes C4 container, per-proposal architecture, and active seam diagrams.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "graph_diagram",
                "description": "Generate PlantUML diagrams from the graph. Supports: c4_context, c4_container, c4_component, proposal_architecture, seam_detail, sequence, state, module_interaction, crate_classes. Returns PlantUML source that can be rendered locally or via plantuml.com.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "diagram_type": { "type": "string", "description": "Diagram type: c4_context, c4_container, c4_component, proposal_architecture, seam_detail, sequence, state, module_interaction, crate_classes" },
                        "target": { "type": "string", "description": "Target entity (crate name, proposal ID, seam ID, enum ID, function ID — depends on diagram_type)" },
                        "max_depth": { "type": "integer", "description": "Max traversal depth for sequence diagrams (default: 3)" }
                    },
                    "required": ["diagram_type", "target"]
                }
            }
        ]
    })
}

pub fn router(state: Arc<AppState>) -> Router {
    let cors = CorsLayer::new()
        .allow_methods([Method::POST, Method::OPTIONS])
        .allow_origin(Any)
        .allow_headers(Any);

    Router::new()
        .route("/mcp", post(handle_mcp))
        .layer(cors)
        .with_state(state)
}

async fn handle_mcp(
    State(state): State<Arc<AppState>>,
    Json(req): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    if req.jsonrpc != "2.0" {
        return Json(JsonRpcResponse {
            jsonrpc: "2.0",
            result: None,
            error: Some(JsonRpcError {
                code: -32600,
                message: "Invalid JSON-RPC version".to_string(),
            }),
            id: req.id,
        });
    }

    let result = match req.method.as_str() {
        "tools/list" => Ok(tool_definitions()),
        "tools/call" => {
            let params = req.params.unwrap_or(serde_json::json!({}));
            let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            execute_tool(&state, tool_name, &arguments).await
        }
        "initialize" => Ok(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "graph-intelligence",
                "version": "0.1.0"
            }
        })),
        // Notifications have no id; acknowledge with empty result so we don't send
        // back a JSON-RPC error that confuses strict MCP validators.
        method if method.starts_with("notifications/") => Ok(serde_json::json!({})),
        _ => Err(JsonRpcError {
            code: -32601,
            message: format!("Method '{}' not found", req.method),
        }),
    };

    match result {
        Ok(value) => Json(JsonRpcResponse {
            jsonrpc: "2.0",
            result: Some(value),
            error: None,
            id: req.id,
        }),
        Err(err) => Json(JsonRpcResponse {
            jsonrpc: "2.0",
            result: None,
            error: Some(err),
            id: req.id,
        }),
    }
}

async fn execute_tool(
    state: &AppState,
    tool_name: &str,
    arguments: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    match tool_name {
        "graph_status" => tool_graph_status(state).await,
        "graph_query" => tool_graph_query(state, arguments).await,
        "graph_node" => tool_graph_node(state, arguments).await,
        "graph_skeleton" => tool_graph_skeleton(state, arguments).await,
        "graph_snippet" => tool_graph_snippet(state, arguments).await,
        "graph_search" => tool_graph_search(state, arguments).await,
        "graph_proposals" => tool_graph_proposals(state).await,
        "graph_manage_proposal" => tool_graph_manage_proposal(state, arguments).await,
        "graph_decide" => tool_graph_decide(state, arguments).await,
        "graph_memory_true_up" => tool_graph_memory_true_up(state, arguments).await,
        "graph_create_node" => tool_graph_create_node(state, arguments).await,
        "graph_update_node" => tool_graph_update_node(state, arguments).await,
        "graph_create_edge" => tool_graph_create_edge(state, arguments).await,
        "graph_writeback" => tool_graph_writeback(state, arguments).await,
        "graph_record_test_run" => tool_graph_record_test_run(state, arguments).await,
        "graph_advance_verification" => tool_graph_advance_verification(state, arguments).await,
        "graph_scan" => tool_graph_scan(state).await,
        "graph_embed" => tool_graph_embed(state, arguments).await,
        "graph_semantic_search" => tool_graph_semantic_search(state, arguments).await,
        "graph_embed_batch" => tool_graph_embed_batch(state, arguments).await,
        "graph_verify_semantic" => tool_graph_verify_semantic(state, arguments).await,
        "graph_digest" => tool_graph_digest(state).await,
        "graph_export_docs" => tool_graph_export_docs(state, arguments).await,
        "graph_export_sver" => tool_graph_export_sver(state, arguments).await,
        "graph_context_for" => tool_graph_context_for(state, arguments).await,
        "graph_next_task" => tool_graph_next_task(state).await,
        "graph_impact" => tool_graph_impact(state, arguments).await,
        "graph_agent_dashboard" => tool_graph_agent_dashboard(state).await,
        "graph_persist_diagrams" => tool_graph_persist_diagrams(state).await,
        "graph_diagram" => tool_graph_diagram(state, arguments).await,
        "session_start" => tool_session_start(state, arguments).await,
        "session_activity" => tool_session_activity(state, arguments).await,
        "session_close" => tool_session_close(state, arguments).await,
        _ => Err(JsonRpcError {
            code: -32602,
            message: format!("Unknown tool: {}", tool_name),
        }),
    }
}

fn mcp_err(msg: &str) -> JsonRpcError {
    JsonRpcError {
        code: -32603,
        message: msg.to_string(),
    }
}

async fn tool_graph_status(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let engine = state.engine.lock().await;

    let kinds = [
        NodeKind::Proposal,
        NodeKind::Seam,
        NodeKind::Crate,
        NodeKind::Module,
        NodeKind::Type,
        NodeKind::Function,
        NodeKind::Test,
        NodeKind::Commit,
        NodeKind::Branch,
        NodeKind::Domain,
        NodeKind::Skill,
        NodeKind::Sver,
        NodeKind::Document,
        NodeKind::Decision,
        NodeKind::Session,
        NodeKind::Workstream,
        NodeKind::Worktree,
        NodeKind::Component,
        NodeKind::ImplBlock,
        NodeKind::Slice,
        NodeKind::Task,
        NodeKind::Agent,
        NodeKind::AgentWorkFocus,
        NodeKind::TestRun,
        NodeKind::SmokeRun,
        NodeKind::UatRun,
    ];

    let mut counts = serde_json::Map::new();
    for kind in &kinds {
        let count = engine
            .count_nodes(Some(*kind))
            .map_err(|e| mcp_err(&e.to_string()))?;
        counts.insert(kind.as_str().to_string(), serde_json::json!(count));
    }
    let total = engine
        .count_nodes(None)
        .map_err(|e| mcp_err(&e.to_string()))?;
    counts.insert("total".to_string(), serde_json::json!(total));

    let edges = engine.count_edges().map_err(|e| mcp_err(&e.to_string()))?;
    let snippets = engine
        .count_snippets()
        .map_err(|e| mcp_err(&e.to_string()))?;

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&serde_json::json!({
                "node_counts": counts,
                "edge_count": edges,
                "snippet_count": snippets,
            })).unwrap_or_default()
        }]
    }))
}

async fn tool_graph_query(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let engine = state.engine.lock().await;

    let kind = args
        .get("kind")
        .and_then(|v| v.as_str())
        .and_then(NodeKind::from_str);
    let worktree = args.get("worktree").and_then(|v| v.as_str());
    let status_filter = args.get("status").and_then(|v| v.as_str());

    let mut nodes = engine
        .query_nodes(kind, worktree)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Apply status filter if provided (checks properties.status)
    if let Some(status) = status_filter {
        nodes.retain(|n| {
            n.properties
                .get("status")
                .and_then(|v| v.as_str())
                .map(|s| s == status)
                .unwrap_or(false)
        });
    }

    let compact = args
        .get("compact")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let text = if compact {
        let summaries: Vec<serde_json::Value> = nodes
            .iter()
            .map(|n| {
                serde_json::json!({
                    "id": n.id,
                    "name": n.name,
                    "kind": n.kind.as_str(),
                    "status": n.properties.get("status"),
                    "disposition": n.properties.get("disposition"),
                })
            })
            .collect();
        serde_json::to_string_pretty(&summaries).unwrap_or_default()
    } else {
        serde_json::to_string_pretty(&nodes).unwrap_or_default()
    };

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    }))
}

async fn tool_graph_node(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: id"))?;

    let engine = state.engine.lock().await;
    let node = engine
        .get_node(id)
        .map_err(|e| mcp_err(&e.to_string()))?
        .ok_or_else(|| mcp_err(&format!("Node '{}' not found", id)))?;

    let outgoing = engine
        .get_edges_from(id)
        .map_err(|e| mcp_err(&e.to_string()))?;
    let incoming = engine
        .get_edges_to(id)
        .map_err(|e| mcp_err(&e.to_string()))?;

    let text = serde_json::to_string_pretty(&serde_json::json!({
        "node": node,
        "edges": {
            "outgoing": outgoing,
            "incoming": incoming,
        }
    }))
    .unwrap_or_default();

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    }))
}

async fn tool_graph_skeleton(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let crate_name = args
        .get("crate_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: crate_name"))?;

    let engine = state.engine.lock().await;
    let diagram = crate::plantuml::generate_crate_diagram(&engine, crate_name)
        .map_err(|e| mcp_err(&e.to_string()))?;

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": diagram }]
    }))
}

async fn tool_graph_snippet(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let node_id = args
        .get("node_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: node_id"))?;
    let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);

    let engine = state.engine.lock().await;
    let snippets = engine
        .get_snippets_for_node(node_id)
        .map_err(|e| mcp_err(&e.to_string()))?;

    let result: Vec<serde_json::Value> = if full {
        snippets
            .iter()
            .map(|s| serde_json::to_value(s).unwrap_or_default())
            .collect()
    } else {
        snippets
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "kind": s.kind,
                    "signature": s.signature,
                    "doc_comment": s.doc_comment,
                    "file_path": s.file_path,
                    "line_start": s.line_start,
                    "line_end": s.line_end,
                })
            })
            .collect()
    };

    let text = serde_json::to_string_pretty(&result).unwrap_or_default();
    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    }))
}

async fn tool_graph_search(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: query"))?;

    let engine = state.engine.lock().await;

    // Search nodes by FTS first, fall back to substring match
    let mut matching_nodes = engine.search_nodes(query).unwrap_or_default();

    // If FTS returned nothing (e.g., partial word), fall back to substring
    if matching_nodes.is_empty() {
        let all_nodes = engine
            .query_nodes(None, None)
            .map_err(|e| mcp_err(&e.to_string()))?;
        matching_nodes = all_nodes
            .into_iter()
            .filter(|n| n.name.to_lowercase().contains(&query.to_lowercase()))
            .collect();
    }

    // Search snippets via FTS
    let snippets = engine
        .search_snippets(query)
        .map_err(|e| mcp_err(&e.to_string()))?;

    let text = serde_json::to_string_pretty(&serde_json::json!({
        "nodes": matching_nodes,
        "snippets": snippets.iter().map(|s| serde_json::json!({
            "id": s.id,
            "node_id": s.node_id,
            "kind": s.kind,
            "signature": s.signature,
            "file_path": s.file_path,
        })).collect::<Vec<_>>(),
    }))
    .unwrap_or_default();

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    }))
}

async fn tool_graph_proposals(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let engine = state.engine.lock().await;
    let proposals = engine
        .query_nodes(Some(NodeKind::Proposal), None)
        .map_err(|e| mcp_err(&e.to_string()))?;

    let mut results = Vec::new();
    for p in &proposals {
        let edges = engine
            .get_edges_from(&p.id)
            .map_err(|e| mcp_err(&e.to_string()))?;
        let active_seams: Vec<&str> = edges
            .iter()
            .filter(|e| e.relation == EdgeRelation::Implements)
            .map(|e| e.target_id.as_str())
            .collect();

        results.push(serde_json::json!({
            "id": p.id,
            "name": p.name,
            "status": p.properties.get("status"),
            "disposition": p.properties.get("disposition"),
            "domain": p.properties.get("domain"),
            "verification_level": p.properties.get("verification_level"),
            "active_seams": active_seams,
        }));
    }

    let text = serde_json::to_string_pretty(&results).unwrap_or_default();
    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    }))
}

async fn tool_graph_manage_proposal(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let proposal_id = args
        .get("proposal_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: proposal_id"))?;
    let agent = args
        .get("agent")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: agent"))?;
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: reason"))?;

    let engine = state.engine.lock().await;
    let result = engine
        .manage_proposal(crate::engine::ManageProposalRequest {
            proposal_id: proposal_id.to_string(),
            agent: agent.to_string(),
            session: args
                .get("session")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            status: args
                .get("status")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            disposition: args
                .get("disposition")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            current_goal: args
                .get("current_goal")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            observation: args
                .get("observation")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            assumption: args
                .get("assumption")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            open_question: args
                .get("open_question")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            pending_writeback_item: args
                .get("pending_writeback_item")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            reason: reason.to_string(),
        })
        .map_err(|e| mcp_err(&e.to_string()))?;

    let _ = state.change_tx.send(ChangeEvent {
        event_type: "proposal_managed".to_string(),
        payload: serde_json::json!({
            "proposal_id": result.proposal.id,
            "agent": agent,
            "work_focus_id": result.work_focus.id,
            "mutation_id": result.mutation.id,
        }),
    });

    let text = serde_json::to_string_pretty(&serde_json::json!({
        "managed": true,
        "proposal": {
            "id": result.proposal.id,
            "name": result.proposal.name,
            "status": result.proposal.properties.get("status"),
            "disposition": result.proposal.properties.get("disposition"),
        },
        "agent": {
            "id": result.agent_node.id,
            "name": result.agent_node.name,
        },
        "work_focus": {
            "id": result.work_focus.id,
            "properties": result.work_focus.properties,
        },
        "mutation_id": result.mutation.id,
    }))
    .unwrap_or_default();

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    }))
}

async fn tool_graph_decide(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let target_node = args
        .get("target_node")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: target_node"))?;
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: action"))?;
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: reason"))?;

    let from_value = args.get("from_value").and_then(|v| v.as_str());
    let to_value = args.get("to_value").and_then(|v| v.as_str());
    let agent = args.get("agent").and_then(|v| v.as_str());
    let session = args.get("session").and_then(|v| v.as_str());

    let engine = state.engine.lock().await;

    let mutation = Mutation {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        agent: agent.map(String::from),
        session: session.map(String::from),
        action: action.to_string(),
        target_node: Some(target_node.to_string()),
        from_value: from_value.map(String::from),
        to_value: to_value.map(String::from),
        reason: Some(reason.to_string()),
        details: serde_json::json!({}),
    };

    engine
        .record_mutation(&mutation)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // If action is status_change and to_value is set, update the node properties
    if action == "status_change" {
        if let Some(new_status) = to_value {
            if let Some(mut node) = engine
                .get_node(target_node)
                .map_err(|e| mcp_err(&e.to_string()))?
            {
                if let serde_json::Value::Object(ref mut map) = node.properties {
                    map.insert(
                        "status".to_string(),
                        serde_json::Value::String(new_status.to_string()),
                    );
                }
                node.updated_at = chrono::Utc::now();
                engine
                    .upsert_node(&node)
                    .map_err(|e| mcp_err(&e.to_string()))?;
            }
        }
    }

    // Broadcast mutation event
    let _ = state.change_tx.send(ChangeEvent {
        event_type: "mutation_recorded".to_string(),
        payload: serde_json::json!({
            "mutation_id": mutation.id,
            "target_node": target_node,
            "action": action,
        }),
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!("Decision recorded: {} on {} — {}", action, target_node, reason)
        }]
    }))
}

fn string_array_arg(args: &serde_json::Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::trim))
                .filter(|value| !value.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

async fn tool_graph_memory_true_up(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let finding_type = args
        .get("finding_type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: finding_type"))?;
    let summary = args
        .get("summary")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: summary"))?;

    let request = crate::engine::MemoryTrueUpFindingRequest {
        finding_id: args
            .get("finding_id")
            .and_then(|v| v.as_str())
            .map(String::from),
        finding_type: finding_type.to_string(),
        scope: args.get("scope").and_then(|v| v.as_str()).map(String::from),
        summary: summary.to_string(),
        muninn_ids: string_array_arg(args, "muninn_ids"),
        graph_ids: string_array_arg(args, "graph_ids"),
        evidence_refs: string_array_arg(args, "evidence_refs"),
        resolution: args
            .get("resolution")
            .and_then(|v| v.as_str())
            .map(String::from),
        recommended_action: args
            .get("recommended_action")
            .and_then(|v| v.as_str())
            .map(String::from),
        requires_operator: args
            .get("requires_operator")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        agent: args.get("agent").and_then(|v| v.as_str()).map(String::from),
        session: args
            .get("session")
            .and_then(|v| v.as_str())
            .map(String::from),
    };

    let engine = state.engine.lock().await;
    let result = engine
        .record_memory_true_up_finding(request)
        .map_err(|e| mcp_err(&e.to_string()))?;

    let _ = state.change_tx.send(ChangeEvent {
        event_type: "memory_true_up_recorded".to_string(),
        payload: serde_json::json!({
            "node_id": result.finding.id,
            "mutation_id": result.mutation.id,
        }),
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!(
                "Memory true-up recorded: {} (mutation: {})",
                result.finding.id,
                result.mutation.id
            )
        }],
        "finding": result.finding,
        "mutation": result.mutation,
    }))
}

async fn tool_graph_scan(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let mut engine = state.engine.lock().await;

    let config = ScanConfig {
        rust_roots: state.scan_config.rust_roots.clone(),
        doc_roots: state.scan_config.doc_roots.clone(),
        git_repo: state.scan_config.git_repo.clone(),
        worktree: state.scan_config.worktree.clone(),
    };

    let root = Path::new(&state.repo_root);
    let result = full_scan(root, &config, &mut engine).map_err(|e| mcp_err(&e.to_string()))?;

    let _ = state.change_tx.send(ChangeEvent {
        event_type: "scan_complete".to_string(),
        payload: serde_json::json!({
            "crates": result.crates,
            "modules": result.modules,
            "types": result.types,
            "duration_ms": result.duration_ms,
        }),
    });

    // Auto-persist PlantUML diagrams after scan
    let diagram_count = match crate::egress::auto_persist_diagrams(&engine, root) {
        Ok(written) => {
            let _ = state.change_tx.send(ChangeEvent {
                event_type: "diagrams_persisted".to_string(),
                payload: serde_json::json!({ "count": written.len() }),
            });
            written.len()
        }
        Err(_) => 0,
    };

    let text = serde_json::to_string_pretty(&serde_json::json!({
        "crates": result.crates,
        "modules": result.modules,
        "types": result.types,
        "functions": result.functions,
        "tests": result.tests,
        "snippets": result.snippets,
        "docs": result.docs,
        "commits": result.commits,
        "branches": result.branches,
        "diagrams_persisted": diagram_count,
    }))
    .unwrap_or_default();

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    }))
}

// ── Graph Mutation Tools (Graph as Source of Truth) ──

async fn tool_graph_create_node(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: id"))?;
    let kind_str = args
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: kind"))?;
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: name"))?;

    let kind = NodeKind::from_str(kind_str)
        .ok_or_else(|| mcp_err(&format!("Unknown node kind: {}", kind_str)))?;

    let properties = args
        .get("properties")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .map(String::from);

    let engine = state.engine.lock().await;

    let node = Node {
        id: id.to_string(),
        kind,
        name: name.to_string(),
        properties,
        file_path,
        worktree: String::new(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };

    engine
        .upsert_node(&node)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Broadcast change event
    let _ = state.change_tx.send(ChangeEvent {
        event_type: "node_created".to_string(),
        payload: serde_json::json!({
            "node_id": id,
            "kind": kind_str,
            "name": name,
        }),
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!("Created {} node: {} — {}", kind_str, id, name)
        }]
    }))
}

async fn tool_graph_update_node(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: id"))?;
    let properties_update = args
        .get("properties")
        .and_then(|v| v.as_object())
        .ok_or_else(|| mcp_err("Missing required parameter: properties (must be object)"))?;
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: reason"))?;

    let engine = state.engine.lock().await;

    let mut node = engine
        .get_node(id)
        .map_err(|e| mcp_err(&e.to_string()))?
        .ok_or_else(|| mcp_err(&format!("Node not found: {}", id)))?;

    // Merge properties
    if let serde_json::Value::Object(ref mut existing) = node.properties {
        for (key, value) in properties_update {
            existing.insert(key.clone(), value.clone());
        }
    }
    node.updated_at = chrono::Utc::now();

    engine
        .upsert_node(&node)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Record mutation
    let mutation = Mutation {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        agent: None,
        session: None,
        action: "update_properties".to_string(),
        target_node: Some(id.to_string()),
        from_value: None,
        to_value: Some(serde_json::to_string(&properties_update).unwrap_or_default()),
        reason: Some(reason.to_string()),
        details: serde_json::json!({}),
    };
    engine.record_mutation(&mutation).ok();

    // Broadcast
    let _ = state.change_tx.send(ChangeEvent {
        event_type: "node_updated".to_string(),
        payload: serde_json::json!({
            "node_id": id,
            "updated_properties": properties_update.keys().collect::<Vec<_>>(),
        }),
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!("Updated {}: {} — {}", node.kind.as_str(), id, reason)
        }]
    }))
}

async fn tool_graph_create_edge(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let source_id = args
        .get("source_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: source_id"))?;
    let target_id = args
        .get("target_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: target_id"))?;
    let relation_str = args
        .get("relation")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: relation"))?;

    let relation = EdgeRelation::from_str(relation_str)
        .ok_or_else(|| mcp_err(&format!("Unknown relation: {}", relation_str)))?;

    let engine = state.engine.lock().await;

    let edge = Edge {
        source_id: source_id.to_string(),
        target_id: target_id.to_string(),
        relation,
        properties: serde_json::json!({}),
        worktree: String::new(),
    };

    engine
        .upsert_edge(&edge)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Broadcast
    let _ = state.change_tx.send(ChangeEvent {
        event_type: "edge_created".to_string(),
        payload: serde_json::json!({
            "source_id": source_id,
            "target_id": target_id,
            "relation": relation_str,
        }),
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!("Created edge: {} —({})→ {}", source_id, relation_str, target_id)
        }]
    }))
}

async fn tool_graph_writeback(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let node_id = args
        .get("node_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: node_id"))?;
    let should_commit = args
        .get("commit")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let agent = args.get("agent").and_then(|v| v.as_str());
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("Graph writeback");

    let engine = state.engine.lock().await;

    let node = engine
        .get_node(node_id)
        .map_err(|e| mcp_err(&e.to_string()))?
        .ok_or_else(|| mcp_err(&format!("Node not found: {}", node_id)))?;

    let file_path = node
        .file_path
        .as_ref()
        .ok_or_else(|| mcp_err(&format!("Node {} has no file_path", node_id)))?;

    let full_path = std::path::Path::new(&state.repo_root).join(file_path);

    // Build updates from node properties
    let mut updates: std::collections::HashMap<String, serde_yaml::Value> =
        std::collections::HashMap::new();

    if let serde_json::Value::Object(ref map) = node.properties {
        for (key, value) in map {
            if let Ok(yaml_val) = serde_yaml::to_value(value) {
                updates.insert(key.clone(), yaml_val);
            }
        }
    }

    // Update last_updated
    updates.insert(
        "last_updated".to_string(),
        serde_yaml::Value::String(chrono::Utc::now().format("%Y-%m-%d").to_string()),
    );

    // Drop lock before writeback (since writeback may take time)
    drop(engine);

    use crate::writeback;

    if should_commit {
        writeback::update_and_commit(
            &full_path,
            std::path::Path::new(&state.repo_root),
            &updates,
            agent,
            None,
            reason,
        )
        .map_err(|e| mcp_err(&e.to_string()))?;
    } else {
        writeback::update_frontmatter(&full_path, &updates).map_err(|e| mcp_err(&e.to_string()))?;
    }

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!("Writeback complete: {} → {} (commit: {})", node_id, file_path, should_commit)
        }]
    }))
}

async fn tool_graph_record_test_run(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    // Support both single target_id and target_ids array (test groups)
    let mut targets: Vec<String> = Vec::new();
    if let Some(ids) = args.get("target_ids").and_then(|v| v.as_array()) {
        for id in ids {
            if let Some(s) = id.as_str() {
                targets.push(s.to_string());
            }
        }
    }
    if let Some(single) = args.get("target_id").and_then(|v| v.as_str()) {
        if !targets.contains(&single.to_string()) {
            targets.push(single.to_string());
        }
    }
    if targets.is_empty() {
        return Err(mcp_err(
            "Missing required parameter: target_id or target_ids",
        ));
    }

    let test_count =
        args.get("test_count")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| mcp_err("Missing required parameter: test_count"))? as i64;
    let pass_count =
        args.get("pass_count")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| mcp_err("Missing required parameter: pass_count"))? as i64;
    let fail_count = args.get("fail_count").and_then(|v| v.as_u64()).unwrap_or(0) as i64;
    let coverage_pct = args
        .get("coverage_pct")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let commit_sha = args
        .get("commit_sha")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let duration_ms = args
        .get("duration_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as i64;

    let is_green = fail_count == 0 && test_count > 0;
    let target_label = if targets.len() == 1 {
        targets[0].clone()
    } else {
        format!("{} seams (group)", targets.len())
    };

    let engine = state.engine.lock().await;

    // Create test_run node
    let test_run_id = format!("test_run:{}", uuid::Uuid::new_v4());
    let test_run = crate::schema::Node {
        id: test_run_id.clone(),
        kind: crate::schema::NodeKind::TestRun,
        name: format!("Test run for {}", target_label),
        properties: serde_json::json!({
            "test_count": test_count,
            "pass_count": pass_count,
            "fail_count": fail_count,
            "coverage_pct": coverage_pct,
            "commit_sha": commit_sha,
            "duration_ms": duration_ms,
            "target_id": targets[0],
            "target_ids": targets,
            "is_group": targets.len() > 1,
            "status": if is_green { "green" } else { "red" },
        }),
        file_path: None,
        worktree: String::new(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };

    engine
        .upsert_node(&test_run)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Create edges for ALL targets in the group
    for tid in &targets {
        // target -> test_run (tested_by)
        let edge = crate::schema::Edge {
            source_id: tid.clone(),
            target_id: test_run_id.clone(),
            relation: crate::schema::EdgeRelation::TestedBy,
            properties: serde_json::json!({}),
            worktree: String::new(),
        };
        engine
            .upsert_edge(&edge)
            .map_err(|e| mcp_err(&e.to_string()))?;

        // test_run -> target (validates)
        let edge2 = crate::schema::Edge {
            source_id: test_run_id.clone(),
            target_id: tid.clone(),
            relation: crate::schema::EdgeRelation::Validates,
            properties: serde_json::json!({}),
            worktree: String::new(),
        };
        engine
            .upsert_edge(&edge2)
            .map_err(|e| mcp_err(&e.to_string()))?;

        // Update each target seam's test status
        if let Ok(Some(mut seam)) = engine.get_node(tid) {
            let mut seam_props = seam.properties.as_object().cloned().unwrap_or_default();
            let status_key = if is_green { "test-green" } else { "test-red" };
            seam_props.insert(
                "last_test_status".to_string(),
                serde_json::json!(status_key),
            );
            seam_props.insert("last_test_run".to_string(), serde_json::json!(test_run_id));
            seam_props.insert(
                "last_test_at".to_string(),
                serde_json::json!(chrono::Utc::now().to_rfc3339()),
            );
            if is_green {
                seam_props.insert(
                    "last_green_at".to_string(),
                    serde_json::json!(chrono::Utc::now().to_rfc3339()),
                );
            }
            seam.properties = serde_json::Value::Object(seam_props);
            seam.updated_at = chrono::Utc::now();
            let _ = engine.upsert_node(&seam);
        }
    }

    // Broadcast
    let _ = state.change_tx.send(ChangeEvent {
        event_type: "test_run_recorded".to_string(),
        payload: serde_json::json!({
            "test_run_id": test_run_id,
            "target_ids": targets,
            "target_id": targets[0],
            "test_count": test_count,
            "pass_count": pass_count,
            "is_green": is_green,
        }),
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!(
                "Recorded test run for {}: {}/{} passed ({}% coverage) — {}",
                target_label, pass_count, test_count, coverage_pct,
                if is_green { "GREEN" } else { "RED" }
            )
        }]
    }))
}

async fn tool_graph_advance_verification(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let target_id = args
        .get("target_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: target_id"))?;
    let level = args
        .get("level")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: level"))?;
    let evidence = args.get("evidence").and_then(|v| v.as_str()).unwrap_or("");
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("Verification advancement");

    // Validate level
    let valid_levels = [
        "code-complete",
        "test-green",
        "smoke-green",
        "uat-green",
        "implemented",
    ];
    if !valid_levels.contains(&level) {
        return Err(mcp_err(&format!(
            "Invalid level: {}. Must be one of: {:?}",
            level, valid_levels
        )));
    }

    let engine = state.engine.lock().await;

    // Get current node
    let mut node = engine
        .get_node(target_id)
        .map_err(|e| mcp_err(&e.to_string()))?
        .ok_or_else(|| mcp_err(&format!("Node not found: {}", target_id)))?;

    // Get current verification state
    let mut props = if let serde_json::Value::Object(ref map) = node.properties {
        map.clone()
    } else {
        serde_json::Map::new()
    };

    // Build verification ladder history
    let mut history = props
        .get("verification_history")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    history.push(serde_json::json!({
        "level": level,
        "date": chrono::Utc::now().to_rfc3339(),
        "evidence": evidence,
        "reason": reason,
    }));

    // Update properties
    props.insert("verification_level".to_string(), serde_json::json!(level));
    props.insert(
        "verification_history".to_string(),
        serde_json::json!(history),
    );
    props.insert(
        "last_updated".to_string(),
        serde_json::json!(chrono::Utc::now().format("%Y-%m-%d").to_string()),
    );

    node.properties = serde_json::Value::Object(props);
    node.updated_at = chrono::Utc::now();

    engine
        .upsert_node(&node)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Record mutation
    let mutation = crate::schema::Mutation {
        id: format!("mut:{}", uuid::Uuid::new_v4()),
        timestamp: chrono::Utc::now(),
        agent: Some("agent".to_string()),
        session: None,
        action: format!("verification_advance: {}", level),
        target_node: Some(target_id.to_string()),
        from_value: None,
        to_value: Some(level.to_string()),
        reason: Some(reason.to_string()),
        details: serde_json::json!({"evidence": evidence}),
    };

    engine
        .record_mutation(&mutation)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Broadcast
    let _ = state.change_tx.send(ChangeEvent {
        event_type: "verification_advanced".to_string(),
        payload: serde_json::json!({
            "target_id": target_id,
            "level": level,
            "evidence": evidence,
        }),
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!("Advanced {} to verification level: {}", target_id, level)
        }]
    }))
}

/// Generate and store embedding for a node.
async fn tool_graph_embed(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let node_id = args
        .get("node_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing node_id"))?;
    let custom_text = args.get("text").and_then(|v| v.as_str());

    // Get the node from the graph
    let engine = state.engine.lock().await;
    let mut node = engine
        .get_node(node_id)
        .map_err(|e| mcp_err(&e.to_string()))?
        .ok_or_else(|| mcp_err(&format!("Node not found: {}", node_id)))?;

    // Determine text to embed
    let text_to_embed = if let Some(text) = custom_text {
        text.to_string()
    } else {
        // Default: combine node name and key properties
        let mut parts = vec![node.name.clone()];
        if let Some(desc) = node.properties.get("description").and_then(|v| v.as_str()) {
            parts.push(desc.to_string());
        }
        if let Some(doc) = node.properties.get("doc").and_then(|v| v.as_str()) {
            parts.push(doc.to_string());
        }
        parts.join(" ")
    };

    // Generate hash of source text for change detection
    let text_hash = format!("{:x}", sha2::Sha256::digest(text_to_embed.as_bytes()));

    // Check if embedding already exists and hash matches
    if let Some(existing_hash) = &node.embedding_hash {
        if existing_hash == &text_hash && node.embedding.is_some() {
            return Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Embedding already up-to-date for {} (hash match)", node_id)
                }],
                "node_id": node_id,
                "dims": node.embedding_dims,
                "model": node.embedding_model,
            }));
        }
    }

    drop(engine); // Release lock before HTTP call

    // Call ONNX sidecar to generate embedding
    let client = crate::embeddings::EmbeddingsClient::new();
    let embed_resp = client
        .embed(&text_to_embed)
        .await
        .map_err(|e| mcp_err(&format!("Embedding generation failed: {}", e)))?;

    // Update node with embedding
    let engine = state.engine.lock().await;
    node.embedding = Some(embed_resp.embedding.clone());
    node.embedding_model = Some(embed_resp.model_gen.clone());
    node.embedding_dims = Some(embed_resp.embedding.len() as i32);
    node.embedding_updated = Some(chrono::Utc::now());
    node.embedding_hash = Some(text_hash.clone());
    node.updated_at = chrono::Utc::now();

    engine
        .upsert_node(&node)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Record mutation
    let mutation = crate::schema::Mutation {
        id: format!("mut:{}", uuid::Uuid::new_v4()),
        timestamp: chrono::Utc::now(),
        agent: Some("agent".to_string()),
        session: None,
        action: "embed".to_string(),
        target_node: Some(node_id.to_string()),
        from_value: node.embedding_hash.clone(),
        to_value: Some(text_hash),
        reason: Some(format!(
            "Generated embedding using {}",
            embed_resp.model_gen
        )),
        details: serde_json::json!({"dims": embed_resp.embedding.len(), "model_gen": embed_resp.model_gen}),
    };

    engine
        .record_mutation(&mutation)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Broadcast
    let _ = state.change_tx.send(ChangeEvent {
        event_type: "node_embedded".to_string(),
        payload: serde_json::json!({
            "node_id": node_id,
            "dims": embed_resp.embedding.len(),
            "model_gen": embed_resp.model_gen,
        }),
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!("Generated {}-dim embedding for {} using {}", embed_resp.embedding.len(), node_id, embed_resp.model_gen)
        }],
        "node_id": node_id,
        "dims": embed_resp.embedding.len(),
        "model_gen": embed_resp.model_gen,
    }))
}

/// Search nodes by semantic similarity.
async fn tool_graph_semantic_search(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing query"))?;
    let kind_filter = args.get("kind").and_then(|v| v.as_str());
    let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(10) as usize;
    let threshold = args
        .get("threshold")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.7) as f32;

    // Generate embedding for query
    let client = crate::embeddings::EmbeddingsClient::new();
    let query_embed = client
        .embed(query)
        .await
        .map_err(|e| mcp_err(&format!("Query embedding failed: {}", e)))?;

    let engine = state.engine.lock().await;

    // Get candidate nodes with embeddings
    let candidates: Vec<_> = if let Some(kind_str) = kind_filter {
        let kind = crate::schema::NodeKind::from_str(kind_str);
        if let Some(kind) = kind {
            engine
                .query_nodes(Some(kind), None)
                .map_err(|e| mcp_err(&e.to_string()))?
                .into_iter()
                .filter(|n| n.embedding.is_some())
                .collect()
        } else {
            vec![]
        }
    } else {
        engine
            .query_nodes(None, None)
            .map_err(|e| mcp_err(&e.to_string()))?
            .into_iter()
            .filter(|n| n.embedding.is_some())
            .collect()
    };

    // Calculate similarity scores
    let mut scored: Vec<_> = candidates
        .into_iter()
        .map(|node| {
            let node_vec = node.embedding.as_ref().unwrap();
            let similarity = crate::schema::cosine_similarity(&query_embed.embedding, node_vec);
            (similarity, node)
        })
        .filter(|(score, _)| *score >= threshold)
        .collect();

    // Sort by similarity (highest first)
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);

    // Build results
    let results: Vec<_> = scored
        .into_iter()
        .map(|(score, node)| {
            serde_json::json!({
                "id": node.id,
                "name": node.name,
                "kind": node.kind.as_str(),
                "similarity": score,
                "embedding_model": node.embedding_model,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!("Found {} results for '{}'", results.len(), query)
        }],
        "query": query,
        "results": results,
    }))
}

/// Batch embed all nodes of a given kind.
async fn tool_graph_embed_batch(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let kind_str = args
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("proposal");
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);

    let kind = crate::schema::NodeKind::from_str(kind_str)
        .ok_or_else(|| mcp_err(&format!("Unknown node kind: {}", kind_str)))?;

    // Query all nodes of this kind
    let engine = state.engine.lock().await;
    let nodes = engine
        .query_nodes(Some(kind), None)
        .map_err(|e| mcp_err(&e.to_string()))?;
    drop(engine); // Release lock before HTTP calls

    if nodes.is_empty() {
        return Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": format!("No nodes found with kind: {}", kind_str)
            }],
            "processed": 0,
            "embedded": 0,
            "skipped": 0,
        }));
    }

    // Create embeddings client
    let client = crate::embeddings::EmbeddingsClient::new();

    // Check sidecar health
    let healthy = client
        .health_check()
        .await
        .map_err(|e| mcp_err(&format!("Sidecar health check failed: {}", e)))?;

    if !healthy {
        return Err(mcp_err("ONNX sidecar not healthy. Start with: cargo run -p model-router --bin model-controller-onnx -- --sidecar-only"));
    }

    let mut processed = 0;
    let mut embedded = 0;
    let mut skipped = 0;
    let mut errors = Vec::new();

    for node in nodes {
        processed += 1;

        // Build text to embed
        let mut parts = vec![node.name.clone()];
        if let Some(desc) = node.properties.get("description").and_then(|v| v.as_str()) {
            parts.push(desc.to_string());
        }
        if let Some(doc) = node.properties.get("doc").and_then(|v| v.as_str()) {
            parts.push(doc.to_string());
        }
        let text_to_embed = parts.join(" ");

        // Generate hash
        let text_hash = format!("{:x}", Sha256::digest(text_to_embed.as_bytes()));

        // Check if we can skip
        if !force {
            if let Some(existing_hash) = &node.embedding_hash {
                if existing_hash == &text_hash && node.embedding.is_some() {
                    skipped += 1;
                    continue;
                }
            }
        }

        // Generate embedding
        match client.embed(&text_to_embed).await {
            Ok(embed_resp) => {
                let engine = state.engine.lock().await;

                // Update node
                let mut updated_node = node.clone();
                updated_node.embedding = Some(embed_resp.embedding.clone());
                updated_node.embedding_model = Some(embed_resp.model_gen.clone());
                updated_node.embedding_dims = Some(embed_resp.embedding.len() as i32);
                updated_node.embedding_updated = Some(chrono::Utc::now());
                updated_node.embedding_hash = Some(text_hash);
                updated_node.updated_at = chrono::Utc::now();

                if let Err(e) = engine.upsert_node(&updated_node) {
                    errors.push(format!("Failed to update {}: {}", node.id, e));
                    continue;
                }

                embedded += 1;

                // Record mutation (best effort)
                let mutation = crate::schema::Mutation {
                    id: format!("mut:{}", uuid::Uuid::new_v4()),
                    timestamp: chrono::Utc::now(),
                    agent: Some("agent".to_string()),
                    session: None,
                    action: "embed".to_string(),
                    target_node: Some(node.id.clone()),
                    from_value: node.embedding_hash.clone(),
                    to_value: updated_node.embedding_hash.clone(),
                    reason: Some(format!("Batch embed using {}", embed_resp.model_gen)),
                    details: serde_json::json!({"dims": embed_resp.embedding.len(), "model_gen": embed_resp.model_gen}),
                };
                let _ = engine.record_mutation(&mutation);

                drop(engine);
            }
            Err(e) => {
                errors.push(format!("Failed to embed {}: {}", node.id, e));
            }
        }
    }

    // Broadcast completion
    let _ = state.change_tx.send(ChangeEvent {
        event_type: "batch_embed_complete".to_string(),
        payload: serde_json::json!({
            "kind": kind_str,
            "processed": processed,
            "embedded": embedded,
            "skipped": skipped,
        }),
    });

    let status_text = if errors.is_empty() {
        format!(
            "Batch embed complete: {} processed, {} embedded, {} skipped",
            processed, embedded, skipped
        )
    } else {
        format!(
            "Batch embed complete with {} errors: {} processed, {} embedded, {} skipped",
            errors.len(),
            processed,
            embedded,
            skipped
        )
    };

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": status_text
        }],
        "kind": kind_str,
        "processed": processed,
        "embedded": embedded,
        "skipped": skipped,
        "errors": errors,
    }))
}

/// Check semantic alignment between proposal and implementing code.
/// Used in verification ladder to detect proposal/code drift.
async fn tool_graph_verify_semantic(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let proposal_id = args
        .get("proposal_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing proposal_id"))?;
    let code_node_ids: Vec<String> = args
        .get("code_node_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let engine = state.engine.lock().await;

    // Get proposal node
    let proposal = engine
        .get_node(proposal_id)
        .map_err(|e| mcp_err(&e.to_string()))?
        .ok_or_else(|| mcp_err(&format!("Proposal not found: {}", proposal_id)))?;

    // Check proposal has embedding
    let proposal_embedding = proposal.embedding.ok_or_else(|| {
        mcp_err(&format!(
            "Proposal {} has no embedding. Run graph_embed first.",
            proposal_id
        ))
    })?;

    // Query code nodes if not provided
    let code_nodes: Vec<_> = if code_node_ids.is_empty() {
        // Find nodes connected by "implements" edge from proposal
        // For now, we'll check all Function/ImplBlock types
        engine
            .query_nodes(Some(crate::schema::NodeKind::Function), None)
            .map_err(|e| mcp_err(&e.to_string()))?
            .into_iter()
            .filter(|n| n.embedding.is_some())
            .take(20)
            .collect()
    } else {
        code_node_ids
            .into_iter()
            .filter_map(|id| engine.get_node(&id).ok().flatten())
            .filter(|n| n.embedding.is_some())
            .collect()
    };

    drop(engine);

    if code_nodes.is_empty() {
        return Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": format!("No code nodes with embeddings found for proposal {}", proposal_id)
            }],
            "proposal_id": proposal_id,
            "alignment_score": None::<f32>,
        }));
    }

    // Calculate similarities
    let mut similarities = Vec::new();
    for code_node in &code_nodes {
        let code_emb = code_node.embedding.as_ref().unwrap();
        let similarity = crate::schema::cosine_similarity(&proposal_embedding, code_emb);
        similarities.push((similarity, code_node));
    }

    // Sort by similarity
    similarities.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let avg_similarity: f32 =
        similarities.iter().map(|(s, _)| s).sum::<f32>() / similarities.len() as f32;
    let top_similarity = similarities.first().map(|(s, _)| *s).unwrap_or(0.0);

    // Determine alignment status
    let alignment_status = if top_similarity > 0.8 {
        "strong"
    } else if top_similarity > 0.6 {
        "moderate"
    } else if top_similarity > 0.4 {
        "weak"
    } else {
        "misaligned"
    };

    // Build detailed results
    let top_results: Vec<_> = similarities.iter().take(5).map(|(sim, node)| {
        serde_json::json!({
            "node_id": node.id,
            "name": node.name,
            "similarity": sim,
            "status": if *sim > 0.7 { "aligned" } else if *sim > 0.4 { "partial" } else { "divergent" },
        })
    }).collect();

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!(
                "Semantic verification for {}: {} alignment (avg: {:.2}, top: {:.2})",
                proposal_id, alignment_status, avg_similarity, top_similarity
            )
        }],
        "proposal_id": proposal_id,
        "alignment_status": alignment_status,
        "avg_similarity": avg_similarity,
        "top_similarity": top_similarity,
        "top_results": top_results,
    }))
}

async fn tool_graph_digest(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let engine = state.engine.lock().await;
    let report = crate::egress::generate_digest(&engine).map_err(|e| mcp_err(&e.to_string()))?;

    // Build compact text representation
    let mut text = String::new();
    text.push_str(&format!(
        "# Architecture Digest\n\nnodes: {}  edges: {}  proposals: {}  seams: {}  skills: {}\n\n",
        report.total_nodes,
        report.total_edges,
        report.proposal_count,
        report.seam_count,
        report.skill_count
    ));

    for domain in &report.domains {
        text.push_str(&format!("## {}\n", domain.name));
        for p in &domain.proposals {
            let seams_str = if p.active_seams.is_empty() {
                "—".to_string()
            } else {
                p.active_seams.join(", ")
            };
            text.push_str(&format!(
                "  - [{}] {} | {} | verified:{} | seams:{}\n",
                p.disposition, p.name, p.status, p.verification, seams_str
            ));
        }
        text.push('\n');
    }

    if !report.active_seams.is_empty() {
        text.push_str("## Active Seams\n");
        for s in &report.active_seams {
            text.push_str(&format!("  - {} [{}]\n", s.id, s.status));
        }
        text.push('\n');
    }

    if !report.active_sessions.is_empty() {
        text.push_str("## Active Sessions\n");
        for s in &report.active_sessions {
            text.push_str(&format!(
                "  - {} ({}) on {} phase:{}\n",
                s.id, s.agent, s.seam_id, s.phase
            ));
        }
        text.push('\n');
    }

    if !report.recent_decisions.is_empty() {
        text.push_str("## Recent Decisions\n");
        for d in &report.recent_decisions {
            text.push_str(&format!("  - {} — {}\n", d.id, d.name));
        }
    }

    // Also return structured JSON for programmatic use
    let structured = serde_json::json!({
        "total_nodes": report.total_nodes,
        "total_edges": report.total_edges,
        "proposal_count": report.proposal_count,
        "seam_count": report.seam_count,
        "skill_count": report.skill_count,
        "domains": report.domains.iter().map(|d| {
            serde_json::json!({
                "name": d.name,
                "proposals": d.proposals.iter().map(|p| {
                    serde_json::json!({
                        "id": p.id,
                        "name": p.name,
                        "disposition": p.disposition,
                        "status": p.status,
                        "verification": p.verification,
                        "active_seams": p.active_seams,
                    })
                }).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
        "active_seams": report.active_seams.iter().map(|s| {
            serde_json::json!({ "id": s.id, "name": s.name, "status": s.status })
        }).collect::<Vec<_>>(),
        "active_sessions": report.active_sessions.iter().map(|s| {
            serde_json::json!({ "id": s.id, "agent": s.agent, "seam_id": s.seam_id, "phase": s.phase })
        }).collect::<Vec<_>>(),
    });

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }],
        "digest": structured,
    }))
}

async fn tool_graph_export_docs(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let dry_run = args
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let repo_root = std::path::Path::new(&state.repo_root);

    let engine = state.engine.lock().await;

    if dry_run {
        // Collect what would be written without touching files
        let doc_kinds = [
            NodeKind::Proposal,
            NodeKind::Seam,
            NodeKind::Task,
            NodeKind::Domain,
            NodeKind::Document,
            NodeKind::Skill,
            NodeKind::Sver,
        ];
        let mut would_update = Vec::new();
        let mut would_skip = Vec::new();
        for kind in &doc_kinds {
            let nodes = engine
                .query_nodes(Some(*kind), None)
                .map_err(|e| mcp_err(&e.to_string()))?;
            for node in nodes {
                match node.file_path.as_deref() {
                    Some(p) if !p.is_empty() => {
                        let path = if std::path::Path::new(p).is_absolute() {
                            std::path::PathBuf::from(p)
                        } else {
                            repo_root.join(p)
                        };
                        if path.exists() {
                            would_update.push(format!("{} → {}", node.id, path.display()));
                        } else {
                            would_skip.push(format!("{} (not on disk)", node.id));
                        }
                    }
                    _ => would_skip.push(format!("{} (no file_path)", node.id)),
                }
            }
        }
        let text = format!(
            "DRY RUN: {} files would be updated, {} skipped\n\nWould update:\n{}\n\nWould skip:\n{}",
            would_update.len(),
            would_skip.len(),
            would_update.join("\n"),
            would_skip.join("\n"),
        );
        return Ok(serde_json::json!({
            "content": [{ "type": "text", "text": text }],
            "dry_run": true,
            "would_update": would_update.len(),
            "would_skip": would_skip.len(),
        }));
    }

    let report =
        crate::egress::export_docs(&engine, repo_root).map_err(|e| mcp_err(&e.to_string()))?;

    let text = format!(
        "export_docs: {} updated, {} skipped, {} errors\n\nUpdated:\n{}\n\nErrors:\n{}",
        report.updated.len(),
        report.skipped.len(),
        report.errors.len(),
        report.updated.join("\n"),
        report.errors.join("\n"),
    );

    // Broadcast
    let _ = state.change_tx.send(crate::server::ws::ChangeEvent {
        event_type: "docs_exported".to_string(),
        payload: serde_json::json!({
            "updated": report.updated.len(),
            "errors": report.errors.len(),
        }),
    });

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }],
        "updated": report.updated.len(),
        "skipped": report.skipped.len(),
        "errors": report.errors,
    }))
}

async fn tool_graph_export_sver(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let write = args.get("write").and_then(|v| v.as_bool()).unwrap_or(false);
    let engine = state.engine.lock().await;

    let markdown = crate::egress::export_sver_doc(&engine).map_err(|e| mcp_err(&e.to_string()))?;

    let mut written_path: Option<String> = None;

    if write {
        let out_path =
            std::path::Path::new(&state.repo_root).join("docs/architecture/SVER_STATE.md");

        // Ensure directory exists
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| mcp_err(&format!("Failed to create directory: {}", e)))?;
        }

        std::fs::write(&out_path, &markdown)
            .map_err(|e| mcp_err(&format!("Failed to write SVER_STATE.md: {}", e)))?;

        written_path = Some(out_path.display().to_string());

        // Broadcast
        let _ = state.change_tx.send(crate::server::ws::ChangeEvent {
            event_type: "sver_exported".to_string(),
            payload: serde_json::json!({ "path": written_path }),
        });
    }

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": markdown }],
        "written": write,
        "path": written_path,
        "format": "markdown",
    }))
}

async fn tool_graph_context_for(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let target_id = args
        .get("target_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: target_id"))?;

    let engine = state.engine.lock().await;
    let ctx =
        crate::egress::context_for(&engine, target_id).map_err(|e| mcp_err(&e.to_string()))?;

    // Build compact text representation
    let mut text = String::new();
    text.push_str(&format!(
        "# Context: {} ({})\n\n",
        ctx.target_name, ctx.target_kind
    ));
    text.push_str(&format!(
        "domain: {}  status: {}  disposition: {}  verification: {}\n\n",
        ctx.domain, ctx.status, ctx.disposition, ctx.verification_level
    ));

    if let Some(ref body) = ctx.proposal_body {
        // Truncate body to first 2000 chars to save tokens
        let truncated = if body.len() > 2000 {
            &body[..2000]
        } else {
            body.as_str()
        };
        text.push_str("## Proposal Body\n\n");
        text.push_str(truncated);
        if body.len() > 2000 {
            text.push_str("\n\n...(truncated, use graph_snippet for full text)");
        }
        text.push_str("\n\n");
    }

    if !ctx.related_seams.is_empty() {
        text.push_str("## Related Seams\n");
        for s in &ctx.related_seams {
            text.push_str(&format!("  - {} [{}] {}\n", s.id, s.status, s.name));
        }
        text.push('\n');
    }

    if !ctx.related_proposals.is_empty() {
        text.push_str("## Related Proposals\n");
        for p in &ctx.related_proposals {
            text.push_str(&format!("  - {} [{}] {}\n", p.id, p.status, p.name));
        }
        text.push('\n');
    }

    if !ctx.implementing_code.is_empty() {
        text.push_str("## Implementing Code\n");
        for c in &ctx.implementing_code {
            text.push_str(&format!(
                "  - {}:{}-{} `{}`\n",
                c.file_path, c.line_start, c.line_end, c.signature
            ));
        }
        text.push('\n');
    }

    if !ctx.blocking.is_empty() {
        text.push_str(&format!("## Blocking: {}\n\n", ctx.blocking.join(", ")));
    }

    if !ctx.decisions.is_empty() {
        text.push_str("## Recent Decisions\n");
        for d in &ctx.decisions {
            text.push_str(&format!("  - {} — {}\n", d.id, d.name));
        }
        text.push('\n');
    }

    if !ctx.active_sessions.is_empty() {
        text.push_str("## ⚠ Active Sessions (conflict check)\n");
        for s in &ctx.active_sessions {
            text.push_str(&format!("  - {} [{}]\n", s.id, s.status));
        }
        text.push('\n');
    }

    if !ctx.recent_runs.is_empty() {
        text.push_str("## Recent Verification Runs\n");
        for r in &ctx.recent_runs {
            text.push_str(&format!("  - {} [{}]\n", r.id, r.kind));
        }
        text.push('\n');
    }

    if let Some(ref diagram) = ctx.diagram {
        text.push_str("## PlantUML Diagram\n\n```plantuml\n");
        text.push_str(diagram);
        text.push_str("\n```\n");
    }

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }],
    }))
}

async fn tool_graph_next_task(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let engine = state.engine.lock().await;
    let result = crate::egress::next_task(&engine).map_err(|e| mcp_err(&e.to_string()))?;

    match result {
        Some(task) => {
            let mut text = format!(
                "# Recommended Next Task\n\n**{}** — {}\n\nscore: {}  disposition: {}  reason: {}\n",
                task.id, task.name, task.score, task.disposition, task.reason
            );
            if let Some(ref runner) = task.runner_up {
                text.push_str(&format!("\nRunner-up: {}\n", runner));
            }
            text.push_str(&format!("\nTotal candidates: {}\n", task.total_candidates));
            text.push_str(&format!(
                "\nTo start working: call `graph_context_for` with target_id=\"{}\"\n",
                task.id
            ));

            Ok(serde_json::json!({
                "content": [{ "type": "text", "text": text }],
                "task": {
                    "id": task.id,
                    "name": task.name,
                    "score": task.score,
                    "disposition": task.disposition,
                    "reason": task.reason,
                    "runner_up": task.runner_up,
                    "total_candidates": task.total_candidates,
                },
            }))
        }
        None => Ok(serde_json::json!({
            "content": [{ "type": "text", "text": "No unclaimed work items found. All proposals are either completed, deferred, or actively being worked on." }],
            "task": null,
        })),
    }
}

async fn tool_graph_impact(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let target = args
        .get("target")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: target"))?;

    let engine = state.engine.lock().await;
    let report =
        crate::egress::impact_analysis(&engine, target).map_err(|e| mcp_err(&e.to_string()))?;

    let mut text = format!(
        "# Impact Analysis: {}\n\nSeed nodes: {}  Total reached: {}\n\n",
        report.target, report.seed_nodes, report.total_reached
    );

    if report.affected_proposals.is_empty()
        && report.affected_seams.is_empty()
        && report.affected_tests.is_empty()
    {
        text.push_str("No proposals, seams, or tests affected by this change.\n");
    } else {
        if !report.affected_proposals.is_empty() {
            text.push_str(&format!(
                "## Affected Proposals ({})\n",
                report.affected_proposals.len()
            ));
            for p in &report.affected_proposals {
                text.push_str(&format!("  - {}\n", p));
            }
            text.push('\n');
        }
        if !report.affected_seams.is_empty() {
            text.push_str(&format!(
                "## Affected Seams ({})\n",
                report.affected_seams.len()
            ));
            for s in &report.affected_seams {
                text.push_str(&format!("  - {}\n", s));
            }
            text.push('\n');
        }
        if !report.affected_tests.is_empty() {
            text.push_str(&format!(
                "## Affected Tests ({})\n",
                report.affected_tests.len()
            ));
            for t in &report.affected_tests {
                text.push_str(&format!("  - {}\n", t));
            }
            text.push('\n');
        }
    }

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }],
        "affected_proposals": report.affected_proposals,
        "affected_seams": report.affected_seams,
        "affected_tests": report.affected_tests,
        "total_reached": report.total_reached,
    }))
}

async fn tool_graph_agent_dashboard(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let engine = state.engine.lock().await;
    let report = crate::egress::agent_dashboard(&engine).map_err(|e| mcp_err(&e.to_string()))?;

    let mut text = String::new();
    text.push_str(&format!(
        "# Agent Dashboard\n\nproposals: {}  sessions: {}  decisions: {}\n\n",
        report.total_proposals, report.total_sessions, report.total_decisions
    ));

    // Verification progress
    text.push_str("## Verification Progress\n");
    let mut levels: Vec<_> = report.verification_progress.iter().collect();
    levels.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    for (level, count) in &levels {
        text.push_str(&format!("  - {}: {}\n", level, count));
    }
    text.push('\n');

    // Active sessions
    if report.active_sessions.is_empty() {
        text.push_str("## Active Sessions: none\n\n");
    } else {
        text.push_str(&format!(
            "## Active Sessions ({})\n",
            report.active_sessions.len()
        ));
        for s in &report.active_sessions {
            text.push_str(&format!(
                "  - {} ({}) on {} phase:{}\n",
                s.id, s.agent, s.seam_id, s.phase
            ));
        }
        text.push('\n');
    }

    // Per-agent summary
    if !report.agents.is_empty() {
        text.push_str("## Agents\n");
        for a in &report.agents {
            text.push_str(&format!(
                "  - **{}**: {} sessions ({} active), {} decisions, last active {}\n",
                a.agent,
                a.total_sessions,
                a.active_sessions,
                a.decisions_made,
                a.last_active.format("%Y-%m-%d %H:%M")
            ));
        }
        text.push('\n');
    }

    // Recent closed sessions
    if !report.recent_closed.is_empty() {
        text.push_str("## Recent Closed Sessions\n");
        for s in report.recent_closed.iter().take(10) {
            text.push_str(&format!(
                "  - {} ({}) on {} — {}\n",
                s.id, s.agent, s.seam_id, s.phase
            ));
        }
    }

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }],
    }))
}

async fn tool_graph_persist_diagrams(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let repo_root = std::path::Path::new(&state.repo_root);
    let engine = state.engine.lock().await;

    let written = crate::egress::auto_persist_diagrams(&engine, repo_root)
        .map_err(|e| mcp_err(&e.to_string()))?;

    let text = format!(
        "Persisted {} diagrams to docs/architecture/generated/\n\n{}",
        written.len(),
        written
            .iter()
            .map(|p| {
                let name = std::path::Path::new(p)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();
                format!("  - {}", name)
            })
            .collect::<Vec<_>>()
            .join("\n"),
    );

    // Broadcast
    let _ = state.change_tx.send(crate::server::ws::ChangeEvent {
        event_type: "diagrams_persisted".to_string(),
        payload: serde_json::json!({ "count": written.len() }),
    });

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }],
        "written": written.len(),
        "files": written,
    }))
}

async fn tool_graph_diagram(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let diagram_type = args
        .get("diagram_type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: diagram_type"))?;
    let target = args
        .get("target")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: target"))?;
    let max_depth = args.get("max_depth").and_then(|v| v.as_u64()).unwrap_or(3) as usize;

    let engine = state.engine.lock().await;

    let diagram = match diagram_type {
        "c4_context" => {
            crate::c4::generate_c4_context(&engine, target).map_err(|e| mcp_err(&e.to_string()))?
        }
        "c4_container" => crate::c4::generate_c4_container(&engine, target)
            .map_err(|e| mcp_err(&e.to_string()))?,
        "c4_component" => crate::c4::generate_c4_component(&engine, target)
            .map_err(|e| mcp_err(&e.to_string()))?,
        "proposal_architecture" => crate::c4::generate_proposal_architecture(&engine, target)
            .map_err(|e| mcp_err(&e.to_string()))?,
        "seam_detail" => {
            crate::c4::generate_seam_detail(&engine, target).map_err(|e| mcp_err(&e.to_string()))?
        }
        "sequence" => crate::diagrams::generate_sequence_diagram(&engine, target, max_depth)
            .map_err(|e| mcp_err(&e.to_string()))?,
        "state" => crate::diagrams::generate_state_diagram(&engine, target)
            .map_err(|e| mcp_err(&e.to_string()))?,
        "module_interaction" => crate::diagrams::generate_module_interaction(&engine, target)
            .map_err(|e| mcp_err(&e.to_string()))?,
        "crate_classes" => crate::plantuml::generate_crate_diagram(&engine, target)
            .map_err(|e| mcp_err(&e.to_string()))?,
        _ => {
            return Err(mcp_err(&format!(
                "Unknown diagram_type: '{}'. Valid types: c4_context, c4_container, c4_component, proposal_architecture, seam_detail, sequence, state, module_interaction, crate_classes",
                diagram_type
            )));
        }
    };

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": diagram
        }],
        "diagram_type": diagram_type,
        "target": target,
        "format": "plantuml",
    }))
}

async fn tool_session_start(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: session_id"))?;
    let agent = args
        .get("agent")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: agent"))?;
    let agent_model = args
        .get("agent_model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let seam_id = args
        .get("seam_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: seam_id"))?;
    let proposal_id = args.get("proposal_id").and_then(|v| v.as_str());
    let task_id = args.get("task_id").and_then(|v| v.as_str());
    let phase = args
        .get("phase")
        .and_then(|v| v.as_str())
        .unwrap_or("started");

    let engine = state.engine.lock().await;

    // Create or update Workstream node (represents the active work effort)
    let workstream_id = format!("workstream:{}", seam_id.replace("seam:", ""));
    let workstream_name = if let Some(prop_id) = proposal_id {
        // Try to get proposal name for context
        if let Ok(Some(proposal)) = engine.get_node(prop_id) {
            format!("Work on {} - {}", seam_id, proposal.name)
        } else {
            format!("Work on {}", seam_id)
        }
    } else {
        format!("Work on {}", seam_id)
    };

    let workstream = Node {
        id: workstream_id.clone(),
        kind: NodeKind::Workstream,
        name: workstream_name,
        properties: serde_json::json!({
            "seam_id": seam_id,
            "proposal_id": proposal_id,
            "task_id": task_id,
            "agent": agent,
            "status": "active",
            "phase": phase,
            "started_at": chrono::Utc::now().to_rfc3339(),
            "last_activity": chrono::Utc::now().to_rfc3339(),
        }),
        file_path: None,
        worktree: String::new(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };
    engine
        .upsert_node(&workstream)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Link workstream to seam (PartOf - workstream is part of the seam context)
    let ws_seam_edge = crate::schema::Edge {
        source_id: workstream_id.clone(),
        target_id: seam_id.to_string(),
        relation: crate::schema::EdgeRelation::PartOf,
        properties: serde_json::json!({"role": "active_work"}),
        worktree: String::new(),
    };
    engine
        .upsert_edge(&ws_seam_edge)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Link workstream to proposal (Governs - proposal governs what the work achieves)
    if let Some(prop_id) = proposal_id {
        let ws_prop_edge = crate::schema::Edge {
            source_id: workstream_id.clone(),
            target_id: prop_id.to_string(),
            relation: crate::schema::EdgeRelation::Governs,
            properties: serde_json::json!({"direction": "implements"}),
            worktree: String::new(),
        };
        engine
            .upsert_edge(&ws_prop_edge)
            .map_err(|e| mcp_err(&e.to_string()))?;
    }

    // Create session node (represents this specific agent session)
    let session = Node {
        id: session_id.to_string(),
        kind: NodeKind::Session,
        name: format!(
            "Session {} - {}",
            agent,
            chrono::Utc::now().format("%Y-%m-%d %H:%M")
        ),
        properties: serde_json::json!({
            "agent": agent,
            "agent_model": agent_model,
            "seam_id": seam_id,
            "task_id": task_id,
            "status": "active",
            "phase": phase,
            "start_time": chrono::Utc::now().to_rfc3339(),
            "last_activity": chrono::Utc::now().to_rfc3339(),
            "files_touched": [],
            "lines_changed": 0,
            "test_runs": 0,
        }),
        file_path: None,
        worktree: String::new(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };

    engine
        .upsert_node(&session)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Create edge: session -> seam (working_on)
    let edge = crate::schema::Edge {
        source_id: session_id.to_string(),
        target_id: seam_id.to_string(),
        relation: crate::schema::EdgeRelation::WorkingOn,
        properties: serde_json::json!({"since": chrono::Utc::now().to_rfc3339()}),
        worktree: String::new(),
    };
    engine
        .upsert_edge(&edge)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Optionally link to task
    if let Some(task) = task_id {
        let edge2 = crate::schema::Edge {
            source_id: session_id.to_string(),
            target_id: task.to_string(),
            relation: crate::schema::EdgeRelation::Implements,
            properties: serde_json::json!({}),
            worktree: String::new(),
        };
        engine
            .upsert_edge(&edge2)
            .map_err(|e| mcp_err(&e.to_string()))?;
    }

    // Record mutation
    let mutation = crate::schema::Mutation {
        id: format!("mut:{}", uuid::Uuid::new_v4()),
        timestamp: chrono::Utc::now(),
        agent: Some(agent.to_string()),
        session: Some(session_id.to_string()),
        action: "session_start".to_string(),
        target_node: Some(session_id.to_string()),
        from_value: None,
        to_value: Some("active".to_string()),
        reason: Some(format!("Session started on {}", seam_id)),
        details: serde_json::json!({"seam_id": seam_id, "phase": phase}),
    };
    engine
        .record_mutation(&mutation)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Broadcast
    let _ = state.change_tx.send(ChangeEvent {
        event_type: "session_started".to_string(),
        payload: serde_json::json!({
            "session_id": session_id,
            "agent": agent,
            "seam_id": seam_id,
            "phase": phase,
        }),
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!("Session started: {} working on {} (workstream: {})", session_id, seam_id, workstream_id)
        }],
        "session_id": session_id,
        "workstream_id": workstream_id,
        "agent": agent,
        "seam_id": seam_id,
        "proposal_id": proposal_id,
        "phase": phase,
    }))
}

async fn tool_session_activity(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: session_id"))?;
    let activity_type = args
        .get("activity_type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: activity_type"))?;
    let details = args
        .get("details")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let new_phase = args.get("phase").and_then(|v| v.as_str());

    let engine = state.engine.lock().await;

    // Get existing session
    let mut session = engine
        .get_node(session_id)
        .map_err(|e| mcp_err(&e.to_string()))?
        .ok_or_else(|| mcp_err(&format!("Session {} not found", session_id)))?;

    // Update session properties
    let mut props = session.properties.as_object().cloned().unwrap_or_default();
    props.insert(
        "last_activity".to_string(),
        serde_json::json!(chrono::Utc::now().to_rfc3339()),
    );

    if let Some(phase) = new_phase {
        props.insert("phase".to_string(), serde_json::json!(phase));
    }

    // Track files touched
    if let Some(files) = details.get("files").and_then(|v| v.as_array()) {
        let existing_files = props
            .get("files_touched")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut all_files: Vec<String> = existing_files
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        for f in files {
            if let Some(fname) = f.as_str() {
                if !all_files.contains(&fname.to_string()) {
                    all_files.push(fname.to_string());
                }
            }
        }
        props.insert("files_touched".to_string(), serde_json::json!(all_files));
    }

    // Track lines changed
    if let Some(lines) = details.get("lines_changed").and_then(|v| v.as_i64()) {
        let existing_lines = props
            .get("lines_changed")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        props.insert(
            "lines_changed".to_string(),
            serde_json::json!(existing_lines + lines),
        );
    }

    // Track test runs
    if activity_type == "test_run" {
        let existing_tests = props.get("test_runs").and_then(|v| v.as_i64()).unwrap_or(0);
        props.insert(
            "test_runs".to_string(),
            serde_json::json!(existing_tests + 1),
        );
    }

    // Track token usage (accumulate from each activity report)
    let tokens_in = args
        .get("tokens_input")
        .and_then(|v| v.as_i64())
        .or_else(|| details.get("tokens_input").and_then(|v| v.as_i64()))
        .unwrap_or(0);
    let tokens_out = args
        .get("tokens_output")
        .and_then(|v| v.as_i64())
        .or_else(|| details.get("tokens_output").and_then(|v| v.as_i64()))
        .unwrap_or(0);

    if tokens_in > 0 || tokens_out > 0 {
        let existing_in = props
            .get("tokens_input")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let existing_out = props
            .get("tokens_output")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let new_in = existing_in + tokens_in;
        let new_out = existing_out + tokens_out;
        props.insert("tokens_input".to_string(), serde_json::json!(new_in));
        props.insert("tokens_output".to_string(), serde_json::json!(new_out));
        props.insert(
            "tokens_total".to_string(),
            serde_json::json!(new_in + new_out),
        );
    }

    session.properties = serde_json::Value::Object(props);
    session.updated_at = chrono::Utc::now();

    engine
        .upsert_node(&session)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Record mutation with session_id
    let agent = session
        .properties
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let mutation = crate::schema::Mutation {
        id: format!("mut:{}", uuid::Uuid::new_v4()),
        timestamp: chrono::Utc::now(),
        agent: Some(agent.clone()),
        session: Some(session_id.to_string()),
        action: activity_type.to_string(),
        target_node: Some(session_id.to_string()),
        from_value: None,
        to_value: new_phase.map(|p| p.to_string()),
        reason: Some(format!("Session activity: {}", activity_type)),
        details: details.clone(),
    };
    engine
        .record_mutation(&mutation)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Broadcast
    let _ = state.change_tx.send(ChangeEvent {
        event_type: "session_activity".to_string(),
        payload: serde_json::json!({
            "session_id": session_id,
            "activity_type": activity_type,
            "phase": new_phase,
            "details": details,
        }),
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!("Recorded {} activity for {}", activity_type, session_id)
        }],
        "session_id": session_id,
        "activity_type": activity_type,
    }))
}

async fn tool_session_close(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: session_id"))?;
    let status = args
        .get("status")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: status"))?;
    let verified = args.get("verified").and_then(|v| v.as_str());
    let summary = args.get("summary").and_then(|v| v.as_str()).unwrap_or("");

    let engine = state.engine.lock().await;

    // Get existing session
    let mut session = engine
        .get_node(session_id)
        .map_err(|e| mcp_err(&e.to_string()))?
        .ok_or_else(|| mcp_err(&format!("Session {} not found", session_id)))?;

    let agent = session
        .properties
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    // Update session properties
    let mut props = session.properties.as_object().cloned().unwrap_or_default();
    props.insert("status".to_string(), serde_json::json!(status));
    props.insert(
        "end_time".to_string(),
        serde_json::json!(chrono::Utc::now().to_rfc3339()),
    );
    if let Some(v) = verified {
        props.insert("verified".to_string(), serde_json::json!(v));
    }
    if !summary.is_empty() {
        props.insert("summary".to_string(), serde_json::json!(summary));
    }

    // Token totals: use override if provided, otherwise keep accumulated values
    if let Some(total_override) = args.get("tokens_total").and_then(|v| v.as_i64()) {
        props.insert(
            "tokens_total".to_string(),
            serde_json::json!(total_override),
        );
    }
    if let Some(quota_rem) = args.get("quota_remaining").and_then(|v| v.as_i64()) {
        props.insert("quota_remaining".to_string(), serde_json::json!(quota_rem));
    }

    let session_tokens_total = props
        .get("tokens_total")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let session_tokens_in = props
        .get("tokens_input")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let session_tokens_out = props
        .get("tokens_output")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    session.properties = serde_json::Value::Object(props);
    session.updated_at = chrono::Utc::now();

    engine
        .upsert_node(&session)
        .map_err(|e| mcp_err(&e.to_string()))?;
    let closed_workstreams = engine
        .close_linked_workstreams(&session, status, Some(summary))
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Propagate token usage to linked seam(s)
    if session_tokens_total > 0 {
        if let Ok(edges) = engine.get_edges_from(session_id) {
            for edge in &edges {
                if edge.relation == crate::schema::EdgeRelation::WorkingOn
                    || edge.relation == crate::schema::EdgeRelation::Implements
                {
                    if let Ok(Some(mut seam)) = engine.get_node(&edge.target_id) {
                        let mut seam_props =
                            seam.properties.as_object().cloned().unwrap_or_default();
                        let existing = seam_props
                            .get("tokens_total")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        let existing_in = seam_props
                            .get("tokens_input")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        let existing_out = seam_props
                            .get("tokens_output")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        seam_props.insert(
                            "tokens_total".to_string(),
                            serde_json::json!(existing + session_tokens_total),
                        );
                        seam_props.insert(
                            "tokens_input".to_string(),
                            serde_json::json!(existing_in + session_tokens_in),
                        );
                        seam_props.insert(
                            "tokens_output".to_string(),
                            serde_json::json!(existing_out + session_tokens_out),
                        );
                        seam.properties = serde_json::Value::Object(seam_props);
                        seam.updated_at = chrono::Utc::now();
                        let _ = engine.upsert_node(&seam);
                    }
                }
            }
        }
    }

    // Record mutation
    let mutation = crate::schema::Mutation {
        id: format!("mut:{}", uuid::Uuid::new_v4()),
        timestamp: chrono::Utc::now(),
        agent: Some(agent.clone()),
        session: Some(session_id.to_string()),
        action: "session_close".to_string(),
        target_node: Some(session_id.to_string()),
        from_value: Some("active".to_string()),
        to_value: Some(status.to_string()),
        reason: Some(summary.to_string()),
        details: serde_json::json!({"verified": verified}),
    };
    engine
        .record_mutation(&mutation)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Broadcast
    let _ = state.change_tx.send(ChangeEvent {
        event_type: "session_closed".to_string(),
        payload: serde_json::json!({
            "session_id": session_id,
            "agent": agent,
            "status": status,
            "verified": verified,
            "workstream_ids": closed_workstreams,
        }),
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!("Session {} closed with status: {}", session_id, status)
        }],
        "session_id": session_id,
        "status": status,
        "verified": verified,
        "workstream_ids": closed_workstreams,
    }))
}
