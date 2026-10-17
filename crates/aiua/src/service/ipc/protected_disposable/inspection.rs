//! Explicit local inspection adapter. Never installed by normal hotel startup.
use super::*;
use graph_intelligence::local_work::{Fence, LocalWork, Phase, Scope};
use philotic_client::development_inspection::{ACTION, CHECKPOINT, KEY, Snapshot};
use serde_json::{Value, json};

pub struct Inspection {
    started: Instant,
    pub(super) state: Mutex<InspectionState>,
    pub(super) snapshot: Snapshot,
    expected: Value,
    task: String,
    digest: String,
    #[cfg(test)]
    pub(super) failures: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(super) hold: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    pub(super) hold_dispatch: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    pub(super) hold_bootstrap: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    pub(super) reached: tokio::sync::Notify,
    #[cfg(test)]
    pub(super) release: tokio::sync::Notify,
}
pub(super) struct InspectionState {
    pub ledger: LocalWork,
    pub fence: Fence,
    pub now: u64,
}
impl Inspection {
    /// Supervisor-supplied snapshot and lease; never wire-selected database/path.
    pub fn new(
        mut ledger: LocalWork,
        fence: Fence,
        now: u64,
        task: String,
        snapshot: Snapshot,
    ) -> Result<Self> {
        let digest = snapshot.digest()?;
        let work = ledger.inspect(&task)?;
        let expected = json!({"schema_version":1,"task_id":task,"operation_id":format!("{task}:operation"),"revision":work.revision,"environment":work.environment,"result":snapshot.inspect()?});
        Ok(Self {
            started: Instant::now(),
            state: Mutex::new(InspectionState { ledger, fence, now }),
            snapshot,
            expected,
            task,
            digest,
            #[cfg(test)]
            failures: 0.into(),
            #[cfg(test)]
            hold: false.into(),
            #[cfg(test)]
            hold_dispatch: false.into(),
            #[cfg(test)]
            hold_bootstrap: false.into(),
            #[cfg(test)]
            reached: tokio::sync::Notify::new(),
            #[cfg(test)]
            release: tokio::sync::Notify::new(),
        })
    }
    pub(super) fn envelope(&self) -> IpcResponse {
        IpcResponse::InboundTask {
            source_node: "fixture".into(),
            task_id: uuid::Uuid::nil(),
            task_json: json!({"action":ACTION,"task_id":self.task,"input_digest":self.digest})
                .to_string(),
        }
    }
    fn scope(&self) -> Scope {
        Scope {
            action: ACTION.into(),
            resource: self.digest.clone(),
            destination: format!("{}:receipt", self.task),
        }
    }
    pub(super) fn handle(&self, actor: &str, request: IpcRequest) -> Result<IpcResponse> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("inspection poisoned"))?;
        let f = s.fence.clone();
        let now = s
            .now
            .saturating_add(self.started.elapsed().as_millis() as u64);
        match request {
            IpcRequest::GetConfig { key }
                if key == KEY
                    || key == format!("{KEY}:retry")
                    || key == format!("{KEY}:dispatch") =>
            {
                ensure!(
                    s.ledger.reserve(&self.task, &f, now, 1, true)?,
                    "budget stopped"
                );
                let w = s.ledger.inspect(&self.task)?;
                if w.operation.is_none() {
                    ensure!(
                        s.ledger
                            .prepare(&self.task, &f, now, &self.task, self.scope())?,
                        "prepare stopped"
                    );
                }
                let w = s.ledger.inspect(&self.task)?;
                ensure!(
                    w.operation.as_ref().unwrap().scope == self.scope(),
                    "snapshot changed"
                );
                if key.ends_with(":retry") {
                    ensure!(
                        w.checkpoint
                            .as_ref()
                            .and_then(|c| c["retry_required"].as_bool())
                            == Some(true),
                        "no known safe failure"
                    );
                    ensure!(
                        s.ledger.retry_prepared(&self.task, &f, now)?,
                        "retry stopped"
                    );
                    s.ledger
                        .checkpoint(&self.task, &f, now, json!({"retry_required":false}))?;
                }
                let w = s.ledger.inspect(&self.task)?;
                if w.operation.as_ref().unwrap().phase != Phase::Prepared {
                    let receipt = s
                        .ledger
                        .reconcile_inspection(&self.task, &f, now)?
                        .ok_or_else(|| anyhow::anyhow!("unknown outcome blocked"))?;
                    ensure!(receipt == self.expected, "receipt binding mismatch");
                    return Ok(IpcResponse::ConfigData {
                        key,
                        value_json: Some(
                            json!({"phase":"Succeeded","receipt":receipt}).to_string(),
                        ),
                    });
                }
                if key.ends_with(":dispatch") {
                    ensure!(
                        w.checkpoint.as_ref().and_then(|c| c["phase"].as_str()) == Some("prepared"),
                        "intent checkpoint required"
                    );
                    s.ledger.dispatch(&self.task, &f, now)?;
                    return Ok(IpcResponse::ConfigData {key,value_json:Some(json!({"phase":"Dispatched","snapshot":self.snapshot,"revision":w.revision,"environment":w.environment}).to_string())});
                }
                #[cfg(test)]
                if self
                    .failures
                    .fetch_update(
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                        |n| n.checked_sub(1),
                    )
                    .is_ok()
                {
                    s.ledger
                        .checkpoint(&self.task, &f, now, json!({"retry_required":true}))?;
                }
                let checkpoint = s
                    .ledger
                    .inspect(&self.task)?
                    .checkpoint
                    .unwrap_or(json!({}));
                s.ledger
                    .checkpoint(&self.task, &f, now, checkpoint.clone())?;
                Ok(IpcResponse::ConfigData {
                    key,
                    value_json: Some(
                        json!({"phase":"Prepared","retry_required":checkpoint["retry_required"]})
                            .to_string(),
                    ),
                })
            }
            IpcRequest::SyncApartment {
                agent_id,
                memory_type,
                content_json,
            } if agent_id == actor && memory_type == CHECKPOINT => {
                ensure!(
                    content_json == json!({"phase":"prepared"})
                        || content_json == json!({"phase":"complete"}),
                    "checkpoint schema"
                );
                let w = s.ledger.inspect(&self.task)?;
                if content_json["phase"] == "complete" {
                    ensure!(
                        w.operation
                            .as_ref()
                            .is_some_and(|o| o.phase == Phase::Succeeded),
                        "result not reconciled"
                    );
                } else {
                    ensure!(
                        w.operation
                            .as_ref()
                            .is_some_and(|o| o.phase == Phase::Prepared)
                            && w.checkpoint
                                .as_ref()
                                .and_then(|c| c["retry_required"].as_bool())
                                != Some(true),
                        "retry required"
                    );
                }
                s.ledger.checkpoint(&self.task, &f, now, content_json)?;
                Ok(IpcResponse::success("inspection_checkpoint", None))
            }
            IpcRequest::EmitTask {
                target_node,
                target_role,
                target_guest_id,
                task_json,
            } => {
                ensure!(
                    target_node == "fixture"
                        && target_role == ACTION
                        && target_guest_id.as_deref() == Some(self.task.as_str()),
                    "inspection destination"
                );
                let receipt: Value = serde_json::from_str(&task_json)?;
                ensure!(receipt == self.expected, "inspection result mismatch");
                ensure!(
                    s.ledger.commit_inspection(&self.task, &f, now, &receipt)?,
                    "budget stopped"
                );
                Ok(IpcResponse::success("inspection_result", None))
            }
            IpcRequest::GetConfig { key }
                if [
                    format!("__agent_bundle__:{actor}"),
                    "default_fallback_tiers".into(),
                    format!("__mcp_routes__:{actor}"),
                    format!("__session_apartments__:{actor}"),
                ]
                .contains(&key) =>
            {
                let value_json = if key.starts_with("__session_apartments__") {
                    Some("[]".into())
                } else {
                    None
                };
                Ok(IpcResponse::ConfigData { key, value_json })
            }
            IpcRequest::ListRoleIncarnations { agent_id } if agent_id == actor => {
                Ok(IpcResponse::success("roles", Some(json!({"roles":[]}))))
            }
            IpcRequest::SyncApartment {
                agent_id,
                memory_type,
                ..
            } if agent_id == actor && memory_type == "command_manifest" => {
                Ok(IpcResponse::success("inert_manifest", None))
            }
            IpcRequest::RevokeMcpRoutes { agent_id } if agent_id == actor => {
                Ok(IpcResponse::success("inert_routes", None))
            }
            IpcRequest::FetchMemoryConfig => Ok(IpcResponse::MemoryConfig(
                philotic_client::MemoryConfigPayload { config_json: None },
            )),
            IpcRequest::GetToolCatalog {} => Ok(IpcResponse::ToolCatalogState {
                tool_catalog: vec![],
            }),
            IpcRequest::GetMcpUpstreams {} => Ok(IpcResponse::McpUpstreamsState {
                mcp_upstreams: vec![],
            }),
            IpcRequest::GetIntegrationBindings {} => Ok(IpcResponse::IntegrationBindingsState {
                integration_bindings: vec![],
            }),
            _ => anyhow::bail!("outside inspection contract"),
        }
    }
}
