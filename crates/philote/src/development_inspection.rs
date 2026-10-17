//! One opt-in deterministic task, dispatched by the ordinary runtime event loop.
use super::*;
use philotic_client::development_inspection::{ACTION, CHECKPOINT, KEY, Snapshot};
impl AgentRuntime {
    pub(super) async fn inspect_development_snapshot(&mut self, payload: &str) -> Result<()> {
        ensure_inspection_peer(&mut self.ipc_client)?;
        let binding: Value = serde_json::from_str(payload)?;
        let task_id = binding["task_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("task binding missing"))?;
        println!(
            "NORMAL_INSPECTION_DISPATCH pid={} task={task_id}",
            std::process::id()
        );
        let mut state = self.inspection_config(KEY).await?;
        for _ in 0..3 {
            if state["retry_required"] != true {
                break;
            }
            state = self.inspection_config(&format!("{KEY}:retry")).await?;
        }
        anyhow::ensure!(state["retry_required"] != true, "retry limit");
        if state["phase"] == "Prepared" {
            self.inspection_checkpoint("prepared").await?;
            let input = self.inspection_config(&format!("{KEY}:dispatch")).await?;
            let snapshot: Snapshot = serde_json::from_value(input["snapshot"].clone())?;
            anyhow::ensure!(
                binding["input_digest"] == snapshot.digest()?,
                "snapshot binding mismatch"
            );
            let receipt = json!({"schema_version":1,"task_id":task_id,"operation_id":format!("{task_id}:operation"),"revision":input["revision"],"environment":input["environment"],"result":snapshot.inspect()?});
            println!("NORMAL_INSPECTION_COMPUTED pid={}", std::process::id());
            let response = self
                .ipc_client
                .send_request(IpcRequest::EmitTask {
                    target_node: "fixture".into(),
                    target_role: ACTION.into(),
                    target_guest_id: Some(task_id.into()),
                    task_json: receipt.to_string(),
                })
                .await?;
            anyhow::ensure!(
                matches!(response, IpcResponse::Standard { ok: true, .. }),
                "inspection commit denied"
            );
            state = self.inspection_config(KEY).await?;
        }
        anyhow::ensure!(
            state["phase"] == "Succeeded"
                && state["receipt"]["task_id"] == task_id
                && state["receipt"]["result"]["input_digest"] == binding["input_digest"],
            "unknown inspection outcome"
        );
        self.inspection_checkpoint("complete").await?;
        println!("NORMAL_INSPECTION_COMPLETE pid={}", std::process::id());
        Ok(())
    }
    async fn inspection_config(&mut self, key: &str) -> Result<Value> {
        match self
            .ipc_client
            .send_request(IpcRequest::GetConfig { key: key.into() })
            .await?
        {
            IpcResponse::ConfigData {
                value_json: Some(v),
                ..
            } => Ok(serde_json::from_str(&v)?),
            _ => anyhow::bail!("inspection authority/budget/outcome refused"),
        }
    }
    async fn inspection_checkpoint(&mut self, phase: &str) -> Result<()> {
        let response = self
            .ipc_client
            .send_request(IpcRequest::SyncApartment {
                agent_id: self.agent_id.clone(),
                memory_type: CHECKPOINT.into(),
                content_json: json!({"phase":phase}),
            })
            .await?;
        anyhow::ensure!(
            matches!(response, IpcResponse::Standard { ok: true, .. }),
            "inspection checkpoint refused"
        );
        Ok(())
    }
}
pub(super) fn ensure_inspection_peer(client: &mut philotic_client::PhiloticClient) -> Result<()> {
    anyhow::ensure!(
        std::env::var("PHILOTIC_LOCAL_INSPECTION").as_deref() == Ok("1"),
        "inspection not opted in"
    );
    client.verify_hotel_peer(philotic_client::protected_authority::TrustedHotelPeer {
        pid: std::env::var("PHILOTIC_INSPECTION_PARENT_PID")?.parse()?,
        uid: std::env::var("PHILOTIC_INSPECTION_PARENT_UID")?.parse()?,
    })
}
