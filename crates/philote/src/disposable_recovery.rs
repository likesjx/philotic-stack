//! Compile-time and runtime opt-in disposable driver. Default startup sweep is
//! unchanged. No model calls, tools, memory, or arbitrary script decisions run.
use super::AgentRuntime;
use crate::scripted_loop::{ScriptedLoopDecision, ScriptedLoopExecutor};
use anyhow::{Result, ensure};
use philotic_client::{IpcRequest, IpcResponse, protected_authority::TrustedHotelPeer};
use serde_json::{Value, json};
impl AgentRuntime {
    pub async fn run_disposable_recovery(&mut self) -> Result<()> {
        println!("DISPOSABLE_RECOVERY_START pid={}", std::process::id());
        // Supervisor-injected expected hotel identity, checked on this connection.
        // Environment selects this test path; it grants no server-side authority.
        self.ipc_client.verify_hotel_peer(TrustedHotelPeer {
            pid: std::env::var("PHILOTIC_DISPOSABLE_PARENT_PID")?.parse()?,
            uid: std::env::var("PHILOTIC_DISPOSABLE_PARENT_UID")?.parse()?,
        })?;
        let response = self
            .ipc_client
            .send_request(IpcRequest::GetConfig {
                key: "__disposable_recovery__".into(),
            })
            .await?;
        let IpcResponse::ConfigData {
            value_json: Some(config),
            ..
        } = response
        else {
            anyhow::bail!("recovery bootstrap refused");
        };
        let config: Value = serde_json::from_str(&config)?;
        let phase = config["phase"].as_str().unwrap_or("Unknown");
        ensure!(
            phase == "Prepared" || phase == "Succeeded",
            "unknown recovery outcome"
        );
        let mut script: ScriptedLoopExecutor =
            serde_json::from_value(config["checkpoint"]["script"].clone())?;
        let ScriptedLoopDecision::ExecuteNextTool {
            tool_name,
            arguments,
        } = script.advance()
        else {
            anyhow::bail!("unexpected disposable script");
        };
        ensure!(
            tool_name == "synthetic.receipt",
            "non-synthetic tool denied"
        );
        let task_json = serde_json::to_string(&arguments)?;
        let request = || IpcRequest::EmitTask {
            target_node: "fixture".into(),
            target_role: "synthetic".into(),
            target_guest_id: Some("sink".into()),
            task_json: task_json.clone(),
        };
        if phase == "Prepared" {
            let checkpoint = json!({"script":script, "phase":"prepared"});
            require_ok(
                self.ipc_client
                    .send_request(IpcRequest::SyncApartment {
                        agent_id: self.agent_id.clone(),
                        memory_type: "fixture-checkpoint".into(),
                        content_json: checkpoint,
                    })
                    .await?,
            )?;
            // Harness withholds this response after committing the effect. It then
            // terminates this owned process before any result checkpoint can occur.
            require_ok(self.ipc_client.send_request(request()).await?)?;
        } else {
            // Deliberately replay the event to prove a reconciled success cannot
            // dispatch again. A denial is required, not swallowed as success.
            ensure!(
                matches!(
                    self.ipc_client.send_request(request()).await?,
                    IpcResponse::Standard { ok: false, .. }
                ),
                "duplicate dispatch accepted"
            );
        }
        script.record_tool_result(json!({"reconciled":phase == "Succeeded"}));
        script.advance_tool_cursor();
        ensure!(
            matches!(script.advance(), ScriptedLoopDecision::ToolSequenceComplete),
            "unexpected next tool"
        );
        script.advance_past_tool_sequence();
        ensure!(
            matches!(script.advance(), ScriptedLoopDecision::Complete { .. }),
            "unexpected next step"
        );
        require_ok(
            self.ipc_client
                .send_request(IpcRequest::SyncApartment {
                    agent_id: self.agent_id.clone(),
                    memory_type: "fixture-checkpoint".into(),
                    content_json: json!({"script":script,"phase":"complete"}),
                })
                .await?,
        )?;
        println!(
            "DISPOSABLE_RECOVERY_COMPLETE reconciled={}",
            phase == "Succeeded"
        );
        Ok(())
    }
}
fn require_ok(response: IpcResponse) -> Result<()> {
    ensure!(
        matches!(response, IpcResponse::Standard { ok: true, .. }),
        "disposable request refused"
    );
    Ok(())
}
