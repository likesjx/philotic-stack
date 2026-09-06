//! Runtime half of deterministic-first MCP handling for [`AgentRuntime`]:
//! runs the per-tool handler ladder declared at `mcp.provision` time, executes
//! the built-in reflexes (`echo`, `memory.recall`, `memory.capture`), and
//! emits the reply or error back to the `membrane-mcp` guest that is holding
//! the caller's HTTP connection open.
//!
//! Declared as a `#[path]` child module of `runtime` (like
//! `memory_integration`) so private `AgentRuntime` fields stay accessible.
//! The pure parsing / validation / templating logic lives in
//! `crate::mcp_ingress` where it is unit-tested without a runtime.

use super::*;
use crate::mcp_ingress;
use ansible_mesh_core::mcp_endpoint::{McpHandlerFallback, McpHandlerStep};

/// Upper bound on `memory.recall` results a reflex may return, regardless of
/// what the caller asked for.
const MCP_RECALL_MAX_LIMIT: u64 = 25;
const MCP_RECALL_DEFAULT_LIMIT: u64 = 5;

impl AgentRuntime {
    /// Entry point for an inbound `tools/call` routed at this philote.
    ///
    /// Order of operations, by design:
    /// 1. **Validate** the caller's arguments against the advertised schema
    ///    (unless the policy opts out). A violation is answered
    ///    deterministically with an `isError` result — no model, no approval.
    /// 2. **Ladder**: `static` steps answer immediately (their content was
    ///    declared in an approved provisioning turn, so they are safe to serve
    ///    even on approval-gated calls); `reflex` steps run only when the call
    ///    is pre-approved, because reflexes touch memory.
    /// 3. **Fallback**: either a deterministic error, or the cognitive loop
    ///    (which still honours the MCP approval gate before invoking a model).
    pub(super) async fn handle_mcp_call(
        &mut self,
        task: InboundTaskPayload,
        task_id: Uuid,
    ) -> Result<()> {
        let Some(call) = mcp_ingress::parse_call(&task) else {
            // Not actually an MCP call (defensive) — legacy behaviour.
            return self.handle_user_message(task, task_id).await;
        };
        let policy = call.policy.clone().unwrap_or_default();
        let has_policy = call.policy.is_some();

        if policy.validate_input
            && let Err(msg) = mcp_ingress::validate_args(&call.input_schema, &call.args)
        {
            info!(
                tool = %call.tool,
                turn_id = ?task.turn_id,
                reason = %msg,
                "MCP call rejected deterministically (schema)"
            );
            return self
                .emit_mcp_error(
                    &task,
                    task_id,
                    format!("invalid arguments for '{}': {msg}", call.tool),
                )
                .await;
        }

        for (idx, step) in policy.steps.iter().enumerate() {
            match step {
                McpHandlerStep::Static { result } => {
                    info!(tool = %call.tool, step = idx, "MCP call answered by static step");
                    return self
                        .emit_mcp_reply(&task, task_id, result.to_string())
                        .await;
                }
                McpHandlerStep::Reflex {
                    reflex,
                    args,
                    escalate_on_empty,
                } => {
                    if task.requires_approval {
                        // Reflexes have side effects (memory reads/writes).
                        // Without a pre-approval rule for this action the
                        // operator has to see the call first; the cognitive
                        // path below parks it for approval.
                        info!(
                            tool = %call.tool,
                            reflex = %reflex,
                            "MCP call requires approval — skipping reflex ladder"
                        );
                        break;
                    }
                    let rendered = mcp_ingress::render_template(args, &call.payload);
                    match self.run_mcp_reflex(reflex, &rendered).await {
                        Ok((value, is_empty)) => {
                            if is_empty && *escalate_on_empty {
                                info!(
                                    tool = %call.tool,
                                    reflex = %reflex,
                                    step = idx,
                                    "MCP reflex returned nothing — escalating"
                                );
                                continue;
                            }
                            info!(tool = %call.tool, reflex = %reflex, step = idx, "MCP call answered by reflex");
                            return self.emit_mcp_reply(&task, task_id, value.to_string()).await;
                        }
                        Err(err) => {
                            warn!(tool = %call.tool, reflex = %reflex, error = %err, "MCP reflex failed");
                            return self
                                .emit_mcp_error(
                                    &task,
                                    task_id,
                                    format!("reflex '{reflex}' failed: {err}"),
                                )
                                .await;
                        }
                    }
                }
            }
        }

        match policy.fallback {
            McpHandlerFallback::Error { message } => {
                info!(tool = %call.tool, "MCP call refused by deterministic fallback");
                self.emit_mcp_error(&task, task_id, message).await
            }
            McpHandlerFallback::Model { instructions } => {
                info!(
                    tool = %call.tool,
                    declared_policy = has_policy,
                    "MCP call falling through to cognitive loop"
                );
                let mut task = task;
                task.content = Some(mcp_ingress::model_prompt(&call, instructions.as_deref()));
                self.handle_user_message(task, task_id).await
            }
        }
    }

    /// Execute one built-in reflex. Returns the JSON result and whether it is
    /// "empty" for `escalate_on_empty` purposes.
    async fn run_mcp_reflex(&mut self, reflex: &str, args: &Value) -> Result<(Value, bool)> {
        use memory_core::MemoryEngine as _;

        match reflex {
            "echo" => Ok((args.clone(), false)),

            "memory.recall" => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|q| !q.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("memory.recall needs a non-empty 'query'"))?
                    .to_string();
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(MCP_RECALL_DEFAULT_LIMIT)
                    .clamp(1, MCP_RECALL_MAX_LIMIT) as usize;
                let scope = match args.get("scope").and_then(Value::as_str) {
                    Some("user") => MemoryScope::SharedUser,
                    _ => MemoryScope::SelfOnly,
                };
                let Some(engine) = self.memory_engine_for(&self.agent_id, &self.agent_id) else {
                    anyhow::bail!("memory backend not configured on this node");
                };
                let activation = engine.activate(&query, scope, Some(limit)).await?;
                let is_empty = activation.engrams.is_empty();
                let memories: Vec<Value> = activation
                    .engrams
                    .iter()
                    .map(|e: &Engram| {
                        json!({
                            "id": e.id.to_string(),
                            "concept": e.concept,
                            "content": e.content,
                            "tags": e.tags,
                            "confidence": e.confidence,
                            "created_at": e.created_at,
                        })
                    })
                    .collect();
                Ok((
                    json!({
                        "query": query,
                        "total": activation.total,
                        "memories": memories,
                    }),
                    is_empty,
                ))
            }

            "memory.capture" => {
                let content = args
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("memory.capture needs a non-empty 'content'"))?
                    .to_string();
                let category = args
                    .get("category")
                    .and_then(Value::as_str)
                    .unwrap_or("note")
                    .to_string();
                let mut tags: Vec<String> = args
                    .get("tags")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|t| t.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                if !tags.iter().any(|t| t == "mcp") {
                    tags.push("mcp".into());
                }
                if !tags.contains(&category) {
                    tags.push(category.clone());
                }
                let first_line: String = content
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(50)
                    .collect();
                let concept = format!("mcp.{category}: {first_line}");
                let Some(engine) = self.memory_engine_for(&self.agent_id, &self.agent_id) else {
                    anyhow::bail!("memory backend not configured on this node");
                };
                let engram = engine
                    .remember(MemoryScope::SelfOnly, &concept, &content, tags)
                    .await?;
                Ok((
                    json!({ "captured": true, "id": engram.id.to_string(), "concept": concept }),
                    false,
                ))
            }

            other => anyhow::bail!("unknown reflex '{other}'"),
        }
    }

    fn mcp_reply_route(&self, task: &InboundTaskPayload, task_id: Uuid) -> McpReplyRoute {
        McpReplyRoute {
            final_reply_to: task.final_reply_to.clone().unwrap_or_else(local_node_id),
            final_reply_role: task
                .final_reply_role
                .clone()
                .unwrap_or_else(|| DEFAULT_REPLY_ROLE.to_string()),
            final_reply_guest_id: task.final_reply_guest_id.clone(),
            session_id: task.session_id_or_default(&self.agent_id),
            turn_id: task.turn_id.clone().unwrap_or_else(|| task_id.to_string()),
            chat_id: task.chat_id.clone().unwrap_or_default(),
        }
    }

    /// Successful deterministic answer → `send_reply` (the membrane returns
    /// it as the tool result).
    pub(super) async fn emit_mcp_reply(
        &mut self,
        task: &InboundTaskPayload,
        task_id: Uuid,
        content: String,
    ) -> Result<()> {
        let route = self.mcp_reply_route(task, task_id);
        let payload = FinalReplyPayload {
            action: "send_reply",
            session_id: route.session_id,
            turn_id: route.turn_id,
            chat_id: route.chat_id,
            content,
            audio_artifact: None,
            send_text_caption: false,
            reply_markup: None,
        };
        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: route.final_reply_to,
                target_role: route.final_reply_role,
                target_guest_id: route.final_reply_guest_id,
                task_json: serde_json::to_string(&payload)?,
            })
            .await?;
        Ok(())
    }

    /// Deterministic refusal → `send_error` (membrane-mcp surfaces it as an
    /// `isError: true` tool result, never as a JSON-RPC transport error).
    pub(super) async fn emit_mcp_error(
        &mut self,
        task: &InboundTaskPayload,
        task_id: Uuid,
        message: String,
    ) -> Result<()> {
        let route = self.mcp_reply_route(task, task_id);
        let payload = json!({
            "action": "send_error",
            "session_id": route.session_id,
            "turn_id": route.turn_id,
            "chat_id": route.chat_id,
            "message": message,
        });
        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: route.final_reply_to,
                target_role: route.final_reply_role,
                target_guest_id: route.final_reply_guest_id,
                task_json: payload.to_string(),
            })
            .await?;
        Ok(())
    }
}

struct McpReplyRoute {
    final_reply_to: String,
    final_reply_role: String,
    final_reply_guest_id: Option<String>,
    session_id: String,
    turn_id: String,
    chat_id: String,
}

/// Reject a handler policy that could never run, so `mcp.provision` fails in
/// the provisioning turn instead of on the first external call.
pub(super) fn validate_handler_policies(
    tools: &[ansible_mesh_core::mcp_endpoint::McpToolSpec],
) -> Result<(), String> {
    for tool in tools {
        if let Some(policy) = &tool.handler {
            policy
                .validate()
                .map_err(|e| format!("tool '{}': {e}", tool.name))?;
        }
    }
    Ok(())
}
