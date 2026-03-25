//! Agent Worker binary — executes [`SubagentDelegation`]s for the Philotic Hotel.
//!
//! # Lifecycle
//!
//! ```text
//! 1. Boot    → connect to hotel IPC via PhiloticClient
//! 2. Ready   → send AcceptSubagentLease to signal availability
//! 3. Idle    → wait for InboundTask (contains a SubagentDelegation)
//! 4. Execute → run the delegation goal (model call + tool loop)
//! 5. Commit  → fire FireSubagentHook(TurnCompleted) with result
//! 6. Idle    → return to step 3 without exiting
//! ```
//!
//! A worker process is long-lived. The hotel materializes one per spawned subagent
//! and may re-use it across delegations up to its lease TTL. The worker exits only
//! on hotel IPC disconnect or unrecoverable fatal error.

use agent_core::driver::AgentDriver;
use anyhow::Result;
use philotic_client::{
    GuestIdentity, HookKind, IpcRequest, IpcResponse, PhiloticClient, SubagentDelegation,
    is_ipc_disconnect,
};
use std::time::Duration;
use tracing::{error, info, warn};
use uuid::Uuid;

/// The IPC role the worker registers under.
const WORKER_ROLE: &str = "philote-worker";

/// Default model role used when the delegation does not specify an implementation.
const DEFAULT_MODEL_ROLE: &str = "model";

fn local_node_id() -> String {
    std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string())
}

fn worker_guest_id() -> String {
    std::env::var("PHILOTIC_AGENT_ID")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "philote-worker-01".to_string())
}

// ── WorkerRuntime ─────────────────────────────────────────────────────────────

/// Stateful runtime for a subagent worker process.
///
/// Each `WorkerRuntime` is bound to a single hotel-assigned `worker_id`
/// (the `PHILOTIC_AGENT_ID` environment variable). It holds the IPC connection
/// and runs the idle→execute→idle loop.
struct WorkerRuntime {
    ipc_client: PhiloticClient,
    worker_id: String,
}

impl WorkerRuntime {
    fn new(ipc_client: PhiloticClient, worker_id: impl Into<String>) -> Self {
        Self {
            ipc_client,
            worker_id: worker_id.into(),
        }
    }

    /// Send `AcceptSubagentLease` so the hotel knows this worker is alive.
    ///
    /// If the request fails (e.g., this binary is started without a hotel-assigned
    /// guest ID), the error is logged but does not abort boot — the worker
    /// continues and waits for inbound tasks anyway.
    async fn accept_lease(&mut self) {
        let result = self
            .ipc_client
            .send_request(IpcRequest::AcceptSubagentLease {
                subagent_guest_id: self.worker_id.clone(),
            })
            .await;

        match result {
            Ok(resp) => info!(
                worker_id = %self.worker_id,
                "Subagent lease accepted: {:?}", resp
            ),
            Err(e) => warn!(
                worker_id = %self.worker_id,
                "AcceptSubagentLease failed (may not be hotel-spawned): {}", e
            ),
        }
    }

    /// Execute a [`SubagentDelegation`] and return a brief result summary.
    ///
    /// Current implementation: single model call with the delegation goal and
    /// context. Multi-turn model loops and tool execution will be wired in
    /// subsequent blocks.
    async fn execute_delegation(&mut self, delegation: &SubagentDelegation) -> Result<String> {
        info!(
            worker_id = %self.worker_id,
            goal = %delegation.goal,
            iter_budget = ?delegation.iteration_budget,
            "Executing subagent delegation"
        );

        let _max_iters = delegation.iteration_budget.unwrap_or(5);

        // Derive the model role from the delegation's allowed capabilities, falling
        // back to the default Gemini role.
        let model_role = DEFAULT_MODEL_ROLE;

        // Build a context-enriched prompt from the delegation fields.
        let mut prompt = format!("Goal: {}", delegation.goal);
        if !delegation.context_packet.summary.is_empty() {
            prompt.push_str("\n\nContext:\n");
            prompt.push_str(&delegation.context_packet.summary);
        }
        if !delegation.context_packet.constraints.is_empty() {
            prompt.push_str("\n\nConstraints:\n");
            prompt.push_str(&delegation.context_packet.constraints.join("\n"));
        }
        if !delegation.context_packet.session_facts.is_empty() {
            prompt.push_str("\n\nSession facts:\n");
            prompt.push_str(&delegation.context_packet.session_facts.join("\n"));
        }

        // Dispatch to the model role.
        let task_id = Uuid::new_v4();
        let model_request = serde_json::json!({
            "task":       "subagent_turn",
            "session_id": format!("worker-{}-{}", self.worker_id, task_id),
            "content":    prompt,
            "model":      "gemini-1.5-flash-latest",
        });

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: local_node_id(),
                target_role: model_role.to_string(),
                target_guest_id: None,
                task_json: serde_json::to_string(&model_request)?,
            })
            .await?;

        // Wait for the model's reply (arrives as another InboundTask).
        // Best-effort: if the model doesn't respond within 120 s, fail the task.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(anyhow::anyhow!("Model response timeout"));
            }

            let window = remaining.min(Duration::from_secs(5));
            match tokio::time::timeout(window, self.ipc_client.recv_task()).await {
                Ok(Ok(IpcResponse::InboundTask { task_json, .. })) => {
                    // Accept the first inbound task as the model reply.
                    // Structured payloads carry `agent_action.content`; legacy text
                    // carries top-level `content`.
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&task_json) {
                        let content = v
                            .get("agent_action")
                            .and_then(|a| a.get("content"))
                            .and_then(|c| c.as_str())
                            .or_else(|| v.get("content").and_then(|c| c.as_str()))
                            .unwrap_or("Task completed.");
                        return Ok(content.to_string());
                    }
                    // Unparseable payload — treat as bare success.
                    return Ok("Task completed.".to_string());
                }
                Ok(Ok(_other)) => {
                    // Non-task IPC push while waiting for model; skip.
                }
                Ok(Err(e)) if is_ipc_disconnect(&e) => {
                    return Err(anyhow::anyhow!("IPC disconnect while waiting for model"));
                }
                Ok(Err(e)) => {
                    warn!(worker_id = %self.worker_id, "IPC recv error during model wait: {}", e);
                }
                Err(_timeout) => {
                    // Keep waiting until deadline.
                }
            }
        }
    }

    /// Notify the hotel that the current task completed successfully.
    async fn fire_completion(&mut self, result_summary: String) {
        let result = self
            .ipc_client
            .send_request(IpcRequest::FireSubagentHook {
                subagent_guest_id: self.worker_id.clone(),
                hook_kind: HookKind::TurnCompleted,
                payload: serde_json::json!({
                    "completed":       true,
                    "result_summary":  result_summary,
                }),
            })
            .await;

        match result {
            Ok(_) => info!(worker_id = %self.worker_id, "Completion hook fired."),
            Err(e) => warn!(worker_id = %self.worker_id, "Failed to fire completion hook: {}", e),
        }
    }

    /// Notify the hotel that the current task failed.
    async fn fire_failure(&mut self, error: String) {
        let result = self
            .ipc_client
            .send_request(IpcRequest::FireSubagentHook {
                subagent_guest_id: self.worker_id.clone(),
                hook_kind: HookKind::TurnCompleted,
                payload: serde_json::json!({
                    "completed": false,
                    "error":     error,
                }),
            })
            .await;

        match result {
            Ok(_) => info!(worker_id = %self.worker_id, "Failure hook fired."),
            Err(e) => warn!(worker_id = %self.worker_id, "Failed to fire failure hook: {}", e),
        }
    }
}

// ── AgentDriver impl ──────────────────────────────────────────────────────────

impl AgentDriver for WorkerRuntime {
    /// Boot the worker and run the idle/execute loop until hotel disconnect.
    async fn run(&mut self) -> Result<()> {
        info!(worker_id = %self.worker_id, "Subagent worker booting…");

        // Signal lease acceptance to the hotel.
        self.accept_lease().await;

        info!(
            worker_id = %self.worker_id,
            "Worker idle — waiting for SubagentDelegation…"
        );

        loop {
            match tokio::time::timeout(Duration::from_secs(5), self.ipc_client.recv_task()).await {
                // ── Inbound delegation ──────────────────────────────────
                Ok(Ok(IpcResponse::InboundTask {
                    task_id,
                    task_json,
                    source_node,
                })) => {
                    info!(
                        worker_id = %self.worker_id,
                        task_id = %task_id,
                        source  = %source_node,
                        "Received inbound task"
                    );

                    // The hotel serialises the `SubagentDelegation` directly as
                    // the `task_json` when routing an `AssignSubagentTask`.
                    match serde_json::from_str::<SubagentDelegation>(&task_json) {
                        Ok(delegation) => match self.execute_delegation(&delegation).await {
                            Ok(summary) => {
                                self.fire_completion(summary).await;
                            }
                            Err(err) => {
                                error!(
                                    worker_id = %self.worker_id,
                                    task_id = %task_id,
                                    "Delegation execution failed: {}", err
                                );
                                self.fire_failure(err.to_string()).await;
                            }
                        },
                        Err(err) => {
                            warn!(
                                worker_id = %self.worker_id,
                                task_id = %task_id,
                                "task_json is not a SubagentDelegation: {}", err
                            );
                        }
                    }

                    // Return to idle after every task regardless of outcome.
                    info!(
                        worker_id = %self.worker_id,
                        "Worker idle — waiting for next delegation…"
                    );
                }

                // ── Non-task hotel push (ApartmentUpdate, etc.) ─────────
                Ok(Ok(_other)) => {
                    // Workers do not handle arbitrary hotel pushes; ignore.
                }

                // ── IPC error / disconnect ──────────────────────────────
                Ok(Err(err)) => {
                    if is_ipc_disconnect(&err) {
                        info!(worker_id = %self.worker_id, "Hotel IPC disconnected; worker exiting.");
                        return Ok(());
                    }
                    warn!(worker_id = %self.worker_id, "IPC recv error: {}", err);
                }

                // ── Idle tick — no work yet ─────────────────────────────
                Err(_timeout) => {}
            }
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let wid = worker_guest_id();
    info!("Starting Agent Worker [{}]…", wid);

    let identity = GuestIdentity {
        guest_id: wid.clone(),
        role: WORKER_ROLE.into(),
        supported_tools: Vec::new(),
    };

    let ipc_client = PhiloticClient::connect(identity).await?;
    let mut runtime = WorkerRuntime::new(ipc_client, wid);

    // Run through the AgentDriver trait — same interface used by agent-persona.
    AgentDriver::run(&mut runtime).await
}
