use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::ErrorKind;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tracing::{debug, info};
use uuid::Uuid;

/// Represents the identity of a Guest materializing in the Hotel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestIdentity {
    pub guest_id: String,
    pub role: String,
    #[serde(default)]
    pub supported_tools: Vec<String>,
}

/// Shared cross-component task failure envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TaskErrorPayload {
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub component: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub capability: Option<String>,
    #[serde(default)]
    pub retryable: Option<bool>,
}

impl TaskErrorPayload {
    pub fn provider_failure(
        component: impl Into<String>,
        capability: Option<&str>,
        provider: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: "provider_failure".into(),
            message: message.into(),
            code: None,
            component: Some(component.into()),
            provider: provider.map(str::to_string),
            capability: capability.map(str::to_string),
            retryable: None,
        }
    }

    /// A tool execution failure originating inside agent-core local dispatch.
    pub fn tool_execution(
        tool_name: impl Into<String>,
        message: impl Into<String>,
        code: Option<&str>,
    ) -> Self {
        let tool_name = tool_name.into();
        Self {
            kind: "tool_execution_failure".into(),
            message: message.into(),
            code: code.map(str::to_string),
            component: Some("philote".into()),
            capability: Some(tool_name),
            provider: None,
            retryable: Some(false),
        }
    }

    /// A failure returned by the hotel IPC layer (error code + message from hotel).
    pub fn ipc_failure(
        component: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: "ipc_failure".into(),
            message: message.into(),
            code: Some(code.into()),
            component: Some(component.into()),
            provider: None,
            capability: None,
            retryable: Some(true),
        }
    }

    /// A transport-level failure (socket error, serialization failure, etc.).
    pub fn transport_error(
        component: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: "transport_error".into(),
            message: message.into(),
            code: None,
            component: Some(component.into()),
            provider: None,
            capability: None,
            retryable: Some(true),
        }
    }

    pub fn display_message(&self) -> String {
        let mut parts = vec![self.message.clone(), format!("kind={}", self.kind)];
        if let Some(code) = self.code.as_deref() {
            parts.push(format!("code={code}"));
        }
        if let Some(component) = self.component.as_deref() {
            parts.push(format!("component={component}"));
        }
        if let Some(provider) = self.provider.as_deref() {
            parts.push(format!("provider={provider}"));
        }
        if let Some(capability) = self.capability.as_deref() {
            parts.push(format!("capability={capability}"));
        }
        if let Some(retryable) = self.retryable {
            parts.push(format!("retryable={retryable}"));
        }
        parts.join(" | ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HandoffBundle {
    pub goal: String,
    pub context_excerpt: String,
    pub session_id: String,
    pub initiating_turn_id: String,
    #[serde(default)]
    pub return_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_reason: Option<String>,
    /// The role handing off (e.g. "orchestrator", "developer"). None = orchestrator base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_role: Option<String>,
    /// The role receiving the handoff. Always set for same-identity role handoffs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_goal: Option<String>,
    #[serde(default)]
    pub active_constraints: Vec<String>,
    /// Session-local facts still live at handoff time. Owned by the workflow, not the operator.
    #[serde(default)]
    pub relevant_session_facts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_summary: Option<String>,
    #[serde(default)]
    pub suggested_memory_refs: Vec<String>,
    /// One of: "required" (target must hand back), "optional", "none".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_return_mode: Option<String>,
    #[serde(default)]
    pub cleanup_actions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SubagentContextPacket {
    pub summary: String,
    #[serde(default)]
    pub session_facts: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub memory_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SubagentCompletionContract {
    #[serde(default)]
    pub summary_required: bool,
    #[serde(default)]
    pub artifact_refs_expected: bool,
    #[serde(default)]
    pub failure_summary_required: bool,
    #[serde(default)]
    pub requires_parent_ack: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookKind {
    Progress,
    TurnStarted,
    ToolCall,
    TurnCompleted,
    ApprovalNeeded,
}

/// Where a hook event is routed when it fires.
/// The delegation skill owns this decision — infrastructure does not hardcode it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum HookRoute {
    /// Deliver to the persona agent that spawned this subagent (default).
    PersonaAgent,
    /// Deliver to any currently active role with this name on the mesh.
    Role { role_name: String },
    /// Do not deliver; fire locally for side-effects only (requires `handler_skill`).
    Discard,
}

impl Default for HookRoute {
    fn default() -> Self {
        Self::PersonaAgent
    }
}

/// A single hook subscription declared by the delegation skill.
/// If a hook is listed here it fires. If it is not listed it does not fire.
/// Every subscription must resolve to a valid handler — either a route that
/// can respond, or an explicit local `handler_skill` for Discard routes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookSubscription {
    pub hook_kind: HookKind,
    #[serde(default)]
    pub route: HookRoute,
    /// Skill ID of the local handler to invoke, required when `route` is Discard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler_skill: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleBehavior {
    Terminate,
    NotifyPersona,
    AutoRenew,
}

impl Default for IdleBehavior {
    fn default() -> Self {
        Self::Terminate
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentLeaseTerms {
    pub ttl_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renewal_interval_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lifetime_seconds: Option<u64>,
    pub idle_behavior: IdleBehavior,
}

impl Default for SubagentLeaseTerms {
    fn default() -> Self {
        Self {
            ttl_seconds: 300,
            renewal_interval_seconds: None,
            max_lifetime_seconds: None,
            idle_behavior: IdleBehavior::Terminate,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SpawnSubagentDelta {
    pub requested_ttl: u64,
    pub confirmed_ttl: u64,
    pub requested_max_lifetime: Option<u64>,
    pub confirmed_max_lifetime: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SubagentDelegation {
    pub parent_agent_id: String,
    pub parent_role: String,
    pub subagent_kind: String,
    pub goal: String,
    pub context_packet: SubagentContextPacket,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub allowed_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_allowance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writeback_allowance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration_budget: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<u64>,
    #[serde(default)]
    pub completion_contract: SubagentCompletionContract,
    #[serde(default)]
    pub lease_terms: SubagentLeaseTerms,
    /// Declared hook subscriptions — only hooks listed here will fire.
    /// Each subscription owns its own routing decision.
    #[serde(default)]
    pub hook_subscriptions: Vec<HookSubscription>,
    /// Where to route the `subagent.complete` event. Defaults to PersonaAgent.
    #[serde(default)]
    pub completion_route: HookRoute,
    /// Where to route the `subagent.failed` event. Defaults to PersonaAgent.
    #[serde(default)]
    pub failure_route: HookRoute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseStatus {
    Active,
    Releasing,
    Expired,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseEnvelope {
    pub lease_type: String,
    pub lease_scope: String,
    pub authority_hotel: String,
    #[serde(default)]
    pub authority_component: Option<String>,
    pub owner_guest_id: String,
    #[serde(default)]
    pub owner_hotel: Option<String>,
    #[serde(default)]
    pub owner_component_type: Option<String>,
    pub lease_epoch: u64,
    pub lease_expires_at: u64,
    pub last_heartbeat_at: u64,
    pub status: LeaseStatus,
    #[serde(default)]
    pub delegated_from: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl LeaseEnvelope {
    pub fn is_active(&self) -> bool {
        matches!(self.status, LeaseStatus::Active)
    }
}

/// Represents the types of operations a Guest can perform locally over IPC to the Ansible Hotel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", content = "payload")]
#[serde(rename_all = "snake_case")]
pub enum IpcRequest {
    /// Connect and register as an active materialized guest
    Register(GuestIdentity),
    /// Ask the Hotel for configuration data from the local Context Graph
    GetConfig {
        key: String,
    },
    /// Ask the Hotel vault for a decrypted secret value by secret ref
    GetSecret {
        secret_ref: String,
    },
    /// Section 6 Blueprint Operations
    PublishMessage {
        target_role: String,
        payload: serde_json::Value,
    },
    CreateTask {
        target_role: String,
        payload: serde_json::Value,
    },
    AckEvent {
        event_id: Uuid,
    },
    UpdateTask {
        task_id: Uuid,
        state: String,
        payload: serde_json::Value,
    },
    CompleteTask {
        task_id: Uuid,
        result: serde_json::Value,
    },
    FailTask {
        task_id: Uuid,
        error_code: String,
        reason: String,
    },
    SubscribeInbox {
        role: String,
    },
    AcquireTelegramPollLease {
        lease_key: String,
        agent_id: String,
    },
    GetTelegramPollLeaseOwner {
        lease_key: String,
    },
    RenewTelegramPollLease {
        lease_key: String,
        agent_id: String,
        lease_epoch: u64,
    },
    ReleaseTelegramPollLease {
        lease_key: String,
    },
    HandoffToRole {
        session_id: String,
        role_name: String,
        handoff_bundle: HandoffBundle,
    },
    HandoffBack {
        session_id: String,
        summary: String,
        #[serde(default)]
        return_to: Option<String>,
    },
    SpawnSubagent {
        session_id: String,
        delegation: SubagentDelegation,
    },
    AssignSubagentTask {
        subagent_guest_id: String,
        lease_epoch: u64,
        delegation: SubagentDelegation,
    },
    RenewSubagentLease {
        subagent_guest_id: String,
        lease_epoch: u64,
    },
    ReleaseSubagent {
        subagent_guest_id: String,
    },
    FireSubagentHook {
        subagent_guest_id: String,
        hook_kind: HookKind,
        payload: serde_json::Value,
    },
    AcceptSubagentLease {
        subagent_guest_id: String,
    },
    AbortSubagentSpawn {
        subagent_guest_id: String,
    },
    /// Register a delegation skill with the hotel.
    ///
    /// The hotel validates the skill definition via Layer 1 validation and writes
    /// it to the context graph as an `abstract_skill` node. Returns
    /// [`IpcResponse::SkillRegistered`] on success (even if validation fails —
    /// the registration always writes; the state reflects the validation outcome).
    RegisterSkill {
        skill_name: String,
        description: String,
        /// The subagent worker kind (e.g. `"philote-worker"`).
        subagent_kind: String,
        /// High-level goal statement for this skill.
        goal: String,
        #[serde(default)]
        allowed_tools: Vec<String>,
        #[serde(default)]
        allowed_classes: Vec<String>,
        #[serde(default)]
        hook_subscriptions: Vec<HookSubscription>,
        #[serde(default)]
        completion_route: HookRoute,
        #[serde(default)]
        failure_route: HookRoute,
        #[serde(default)]
        idle_behavior: IdleBehavior,
        #[serde(default)]
        lease_terms: SubagentLeaseTerms,
    },
    ListRoleIncarnations {
        agent_id: String,
    },
    QueryStatus {
        task_id: Uuid,
    },
    QueryTimeline {
        task_id: Uuid,
    },
    /// Drop a task onto the Philotic Web (Legacy)
    EmitTask {
        target_node: String,
        target_role: String,
        #[serde(default)]
        target_guest_id: Option<String>,
        task_json: String,
    },
    /// Optimistically push a RAM-based memory apartment update to the Hotel's SQLite Graph
    SyncApartment {
        agent_id: String,
        memory_type: String,
        content_json: serde_json::Value,
    },
    /// Create or update a role incarnation definition (orchestrator only)
    ConfigureRole {
        agent_id: String,
        role_name: String,
        guest_id: String,
        toolset_profile: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role_identity_addendum: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inactive_ttl_seconds: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        iteration_cap: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approval_policy: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_profile: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_window_policy: Option<String>,
    },
    /// Request the hotel's loaded MuninnDB configuration (vault tokens included).
    FetchMemoryConfig,
}

/// Represents the canonical response from the local Ansible back to the Guest via IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IpcResponse {
    Ack {
        req_id: String,
    },
    ConfigData {
        key: String,
        value_json: Option<String>,
    },
    SecretData {
        secret_ref: String,
        value_json: Option<String>,
    },
    TelegramPollLease {
        granted: bool,
        lease: Option<LeaseEnvelope>,
    },
    TelegramPollLeaseStatus {
        active: bool,
        lease: Option<LeaseEnvelope>,
    },
    HandoffAck {
        handoff_guest_id: String,
        became_active: bool,
    },
    HandoffBackAck {
        handoff_guest_id: String,
        became_active: bool,
    },
    SpawnSubagentOk {
        subagent_guest_id: String,
        confirmed_lease: LeaseEnvelope,
    },
    SpawnSubagentProposal {
        subagent_guest_id: String,
        confirmed_lease: LeaseEnvelope,
        delta: SpawnSubagentDelta,
    },
    SubagentLeaseRenewed {
        subagent_guest_id: String,
        new_epoch: u64,
        expires_at: u64,
    },
    /// Response to [`IpcRequest::RegisterSkill`].
    SkillRegistered {
        skill_name: String,
        /// `"validated"` | `"invalid"` | `"draft"` depending on Layer 1 outcome.
        validation_state: String,
        /// Human-readable summary of any validation errors; empty on success.
        #[serde(default)]
        validation_errors: Vec<String>,
    },
    InboundTask {
        source_node: String,
        task_id: Uuid,
        task_json: String,
    },
    /// Response to [`IpcRequest::ConfigureRole`].
    ConfigureRoleOk {
        role_name: String,
    },
    Error(String),
    Standard {
        ok: bool,
        code: String,
        message: String,
        corr_id: String,
        data: Option<serde_json::Value>,
    },
    /// Hotel actively pushing a memory apartment conflict resolution or external sync to the Guest
    ApartmentUpdate {
        agent_id: String,
        memory_type: String,
        content_json: serde_json::Value,
    },
    /// Response to [`IpcRequest::FetchMemoryConfig`].
    /// `config_json` is `None` if MuninnDB is not configured on this hotel.
    MemoryConfig {
        config_json: Option<String>,
    },
}

impl IpcResponse {
    pub fn success(corr_id: impl Into<String>, data: Option<serde_json::Value>) -> Self {
        Self::Standard {
            ok: true,
            code: "OK".into(),
            message: "Success".into(),
            corr_id: corr_id.into(),
            data,
        }
    }

    pub fn error(
        corr_id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::Standard {
            ok: false,
            code: code.into(),
            message: message.into(),
            corr_id: corr_id.into(),
            data: None,
        }
    }
}

/// A pushed event delivered to an active inbox subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcPushEvent {
    pub event_id: Uuid,
    pub source_node: String,
    pub payload: serde_json::Value,
}

/// The concrete Universal Hotel Client SDK
pub struct PhiloticClient {
    stream: UnixStream,
    _identity: GuestIdentity,
    pending_push: VecDeque<IpcResponse>,
    read_buf: Vec<u8>,
}

pub fn is_ipc_disconnect(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .map(|io_err| {
                matches!(
                    io_err.kind(),
                    ErrorKind::UnexpectedEof
                        | ErrorKind::BrokenPipe
                        | ErrorKind::ConnectionReset
                        | ErrorKind::ConnectionAborted
                        | ErrorKind::NotConnected
                )
            })
            .unwrap_or(false)
    })
}

impl PhiloticClient {
    async fn write_frame(&mut self, payload: &[u8]) -> Result<()> {
        let len = u32::try_from(payload.len()).context("IPC payload too large")?;
        self.stream
            .write_all(&len.to_be_bytes())
            .await
            .context("Failed to send IPC frame header to Ansible")?;
        self.stream
            .write_all(payload)
            .await
            .context("Failed to send IPC frame payload to Ansible")?;
        Ok(())
    }

    async fn read_frame(&mut self) -> Result<Vec<u8>> {
        loop {
            if self.read_buf.len() >= 4 {
                let len = u32::from_be_bytes([
                    self.read_buf[0],
                    self.read_buf[1],
                    self.read_buf[2],
                    self.read_buf[3],
                ]) as usize;
                let frame_len = 4 + len;
                if self.read_buf.len() >= frame_len {
                    let payload = self.read_buf[4..frame_len].to_vec();
                    self.read_buf.drain(..frame_len);
                    return Ok(payload);
                }
            }

            self.stream
                .readable()
                .await
                .context("Failed to wait for IPC frame bytes")?;

            let mut chunk = [0u8; 8192];
            match self.stream.try_read(&mut chunk) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        ErrorKind::UnexpectedEof,
                        "IPC stream closed while receiving frame",
                    ))
                    .context("Failed to receive IPC frame payload");
                }
                Ok(n) => self.read_buf.extend_from_slice(&chunk[..n]),
                Err(err) if err.kind() == ErrorKind::WouldBlock => continue,
                Err(err) => return Err(err).context("Failed to receive IPC frame payload"),
            }
        }
    }

    fn socket_path() -> String {
        std::env::var("PHILOTIC_HOTEL_SOCKET")
            .unwrap_or_else(|_| "/tmp/philotic-aiua.sock".to_string())
    }

    /// Connect to the local Ansible daemon automatically, driven by environment variables.
    /// Default Hotel socket is `/tmp/philotic-aiua.sock` unless `PHILOTIC_HOTEL_SOCKET` is specified.
    pub async fn connect_at(socket_path: impl AsRef<str>, identity: GuestIdentity) -> Result<Self> {
        let socket_path = socket_path.as_ref().to_string();
        let stream = UnixStream::connect(&socket_path)
            .await
            .with_context(|| format!("Failed to connect to hotel IPC socket at {}", socket_path))?;

        debug!(
            "PhiloticClient connecting to local Ansible at {}...",
            socket_path
        );

        let mut client = Self {
            stream,
            _identity: identity.clone(),
            pending_push: VecDeque::new(),
            read_buf: Vec::new(),
        };

        info!("Registering as Materialized Guest: {:?}", identity);
        let resp = client.send_request(IpcRequest::Register(identity)).await?;
        info!("Ansible Hotel Registration Response: {:?}", resp);

        match resp {
            IpcResponse::Standard { ok, message, .. } if !ok => {
                anyhow::bail!("Hotel rejected registration: {}", message);
            }
            IpcResponse::Error(msg) => {
                anyhow::bail!("Hotel rejected registration: {}", msg);
            }
            _ => {}
        }

        Ok(client)
    }

    /// Connect to the local Ansible daemon automatically, driven by environment variables.
    /// Default Hotel socket is `/tmp/philotic-aiua.sock` unless `PHILOTIC_HOTEL_SOCKET` is specified.
    pub async fn connect(identity: GuestIdentity) -> Result<Self> {
        Self::connect_at(Self::socket_path(), identity).await
    }

    /// Send an IPC request to the local Ansible
    pub async fn send_request(&mut self, req: IpcRequest) -> Result<IpcResponse> {
        let payload = serde_json::to_vec(&req).context("Failed to serialize IpcRequest")?;
        self.write_frame(&payload).await?;

        loop {
            let resp = self.read_response().await?;
            if Self::is_push_message(&resp) {
                self.pending_push.push_back(resp);
                continue;
            }
            return Ok(resp);
        }
    }

    async fn read_response(&mut self) -> Result<IpcResponse> {
        let buf = self.read_frame().await?;
        let resp: IpcResponse =
            serde_json::from_slice(&buf).context("Failed to decode IpcResponse from Ansible")?;

        Ok(resp)
    }

    fn is_push_message(response: &IpcResponse) -> bool {
        matches!(
            response,
            IpcResponse::InboundTask { .. } | IpcResponse::ApartmentUpdate { .. }
        )
    }

    /// Poll for inbound tasks routed from the Philotic Web
    pub async fn recv_task(&mut self) -> Result<IpcResponse> {
        if let Some(pending) = self.pending_push.pop_front() {
            return Ok(pending);
        }

        loop {
            let resp = self.read_response().await?;
            if Self::is_push_message(&resp) {
                return Ok(resp);
            }
            anyhow::bail!(
                "Unexpected non-push IPC response while waiting for inbound task: {:?}",
                resp
            );
        }
    }

    /// Write a memory apartment update to the hotel and consume the response so the IPC stream stays framed.
    pub async fn sync_apartment(
        &mut self,
        agent_id: &str,
        memory_type: &str,
        content_json: serde_json::Value,
    ) -> Result<()> {
        let req = IpcRequest::SyncApartment {
            agent_id: agent_id.to_string(),
            memory_type: memory_type.to_string(),
            content_json,
        };
        let response = self.send_request(req).await?;
        match response {
            IpcResponse::Standard { ok: true, .. } => {}
            IpcResponse::Standard { message, .. } => {
                anyhow::bail!("SyncApartment failed: {}", message);
            }
            other => {
                anyhow::bail!("Unexpected SyncApartment response: {:?}", other);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use std::path::Path;
    use std::sync::{LazyLock, Mutex as StdMutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    static IPC_TEST_ENV_LOCK: LazyLock<StdMutex<()>> = LazyLock::new(|| StdMutex::new(()));

    fn ipc_env_guard() -> std::sync::MutexGuard<'static, ()> {
        IPC_TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn test_socket_path() -> String {
        format!("/tmp/pc-{}.sock", Uuid::new_v4().simple())
    }

    #[test]
    fn task_error_payload_formats_for_logs_and_fallbacks() {
        let payload = TaskErrorPayload {
            kind: "provider_failure".into(),
            message: "Voice synthesis failed".into(),
            code: Some("ELEVENLABS_BAD_RESPONSE".into()),
            component: Some("model-router".into()),
            provider: Some("elevenlabs".into()),
            capability: Some("voice.synthesize".into()),
            retryable: Some(false),
        };

        let rendered = payload.display_message();
        assert!(rendered.contains("Voice synthesis failed"));
        assert!(rendered.contains("kind=provider_failure"));
        assert!(rendered.contains("code=ELEVENLABS_BAD_RESPONSE"));
        assert!(rendered.contains("component=model-router"));
        assert!(rendered.contains("provider=elevenlabs"));
        assert!(rendered.contains("capability=voice.synthesize"));
        assert!(rendered.contains("retryable=false"));
    }

    #[test]
    fn telegram_poll_lease_response_roundtrips_with_envelope() {
        let response = IpcResponse::TelegramPollLease {
            granted: true,
            lease: Some(LeaseEnvelope {
                lease_type: "telegram_poll".into(),
                lease_scope: "telegram:bot-token:abcd".into(),
                authority_hotel: "hotel-alpha".into(),
                authority_component: Some("aiua".into()),
                owner_guest_id: "membrane-telegram-01".into(),
                owner_hotel: Some("hotel-alpha".into()),
                owner_component_type: Some("membrane".into()),
                lease_epoch: 7,
                lease_expires_at: 1234,
                last_heartbeat_at: 1222,
                status: LeaseStatus::Active,
                delegated_from: None,
                metadata: serde_json::json!({ "agent_id": "agent-jane-01" }),
            }),
        };

        let bytes = serde_json::to_vec(&response).expect("serialize lease response");
        let decoded: IpcResponse =
            serde_json::from_slice(&bytes).expect("deserialize lease response");

        match decoded {
            IpcResponse::TelegramPollLease {
                granted: true,
                lease: Some(lease),
            } => {
                assert_eq!(lease.lease_epoch, 7);
                assert_eq!(lease.owner_guest_id, "membrane-telegram-01");
                assert_eq!(lease.metadata["agent_id"], "agent-jane-01");
            }
            other => panic!("unexpected decoded response: {other:?}"),
        }
    }

    async fn read_frame(stream: &mut tokio::net::UnixStream) -> Vec<u8> {
        let mut len_buf = [0u8; 4];
        stream
            .read_exact(&mut len_buf)
            .await
            .expect("read frame header");
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        stream
            .read_exact(&mut buf)
            .await
            .expect("read frame payload");
        buf
    }

    async fn write_frame(stream: &mut tokio::net::UnixStream, payload: &[u8]) {
        let len = u32::try_from(payload.len()).expect("frame length");
        stream
            .write_all(&len.to_be_bytes())
            .await
            .expect("write frame header");
        stream
            .write_all(payload)
            .await
            .expect("write frame payload");
    }

    #[tokio::test]
    async fn connect_and_get_config_over_uds() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let listener = UnixListener::bind(&socket_path).expect("bind test socket");

        let server = tokio::spawn({
            let socket_path = socket_path.clone();
            async move {
                let (mut stream, _) = listener.accept().await.expect("accept client");

                let buf = read_frame(&mut stream).await;
                let req: IpcRequest = serde_json::from_slice(&buf).expect("decode register");
                match req {
                    IpcRequest::Register(identity) => assert_eq!(identity.guest_id, "guest-test-1"),
                    other => panic!("unexpected register request: {other:?}"),
                }
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::success("reg", None)).unwrap(),
                )
                .await;

                let buf = read_frame(&mut stream).await;
                let req: IpcRequest = serde_json::from_slice(&buf).expect("decode get_config");
                match req {
                    IpcRequest::GetConfig { key } => assert_eq!(key, "telegram_bot_token"),
                    other => panic!("unexpected config request: {other:?}"),
                }
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::ConfigData {
                        key: "telegram_bot_token".into(),
                        value_json: Some("\"secret-token\"".into()),
                    })
                    .unwrap(),
                )
                .await;

                let _ = std::fs::remove_file(&socket_path);
            }
        });

        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let identity = GuestIdentity {
            guest_id: "guest-test-1".into(),
            role: "test".into(),
            supported_tools: Vec::new(),
        };
        let mut client = PhiloticClient::connect(identity)
            .await
            .expect("connect client");
        let response = client
            .send_request(IpcRequest::GetConfig {
                key: "telegram_bot_token".into(),
            })
            .await
            .expect("send request");

        match response {
            IpcResponse::ConfigData { key, value_json } => {
                assert_eq!(key, "telegram_bot_token");
                assert_eq!(value_json.as_deref(), Some("\"secret-token\""));
            }
            other => panic!("unexpected response: {other:?}"),
        }

        server.await.expect("join server");
        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn send_request_buffers_interleaved_push_messages() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let listener = UnixListener::bind(&socket_path).expect("bind test socket");

        let server = tokio::spawn({
            let socket_path = socket_path.clone();
            async move {
                let (mut stream, _) = listener.accept().await.expect("accept client");
                let buf = read_frame(&mut stream).await;
                let _req: IpcRequest = serde_json::from_slice(&buf).expect("decode register");
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::success("reg", None)).unwrap(),
                )
                .await;

                let buf = read_frame(&mut stream).await;
                let req: IpcRequest = serde_json::from_slice(&buf).expect("decode get_config");
                match req {
                    IpcRequest::GetConfig { key } => assert_eq!(key, "interleaved"),
                    other => panic!("unexpected config request: {other:?}"),
                }

                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::InboundTask {
                        source_node: "local-aiua-01".into(),
                        task_id: Uuid::nil(),
                        task_json: serde_json::json!({
                            "action": "send_reply",
                            "content": "pushed first"
                        })
                        .to_string(),
                    })
                    .unwrap(),
                )
                .await;

                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::ConfigData {
                        key: "interleaved".into(),
                        value_json: Some("\"ok\"".into()),
                    })
                    .unwrap(),
                )
                .await;

                let _ = std::fs::remove_file(&socket_path);
            }
        });

        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let identity = GuestIdentity {
            guest_id: "guest-test-2".into(),
            role: "test".into(),
            supported_tools: Vec::new(),
        };
        let mut client = PhiloticClient::connect(identity)
            .await
            .expect("connect client");
        let response = client
            .send_request(IpcRequest::GetConfig {
                key: "interleaved".into(),
            })
            .await
            .expect("send request");

        match response {
            IpcResponse::ConfigData { key, value_json } => {
                assert_eq!(key, "interleaved");
                assert_eq!(value_json.as_deref(), Some("\"ok\""));
            }
            other => panic!("unexpected config response: {other:?}"),
        }

        let pushed = client.recv_task().await.expect("receive buffered push");
        match pushed {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("decode pushed task");
                assert_eq!(payload["content"], "pushed first");
            }
            other => panic!("unexpected pushed response: {other:?}"),
        }

        server.await.expect("join server");
        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn recv_task_survives_select_cancellation_after_partial_frame_read() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let listener = UnixListener::bind(&socket_path).expect("bind test socket");

        let server = tokio::spawn({
            let socket_path = socket_path.clone();
            async move {
                let (mut stream, _) = listener.accept().await.expect("accept client");
                let buf = read_frame(&mut stream).await;
                let _req: IpcRequest = serde_json::from_slice(&buf).expect("decode register");
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::success("reg", None)).unwrap(),
                )
                .await;

                let payload = serde_json::to_vec(&IpcResponse::InboundTask {
                    source_node: "local-aiua-01".into(),
                    task_id: Uuid::nil(),
                    task_json: serde_json::json!({
                        "action": "send_reply",
                        "content": "partial frame survives cancellation",
                    })
                    .to_string(),
                })
                .unwrap();
                let len = u32::try_from(payload.len()).expect("frame length");
                let header = len.to_be_bytes();

                stream.write_all(&header).await.expect("write frame header");
                stream
                    .write_all(&payload[..8])
                    .await
                    .expect("write partial payload");
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                stream
                    .write_all(&payload[8..])
                    .await
                    .expect("write remaining payload");

                let _ = std::fs::remove_file(&socket_path);
            }
        });

        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let identity = GuestIdentity {
            guest_id: "guest-test-3".into(),
            role: "test".into(),
            supported_tools: Vec::new(),
        };
        let mut client = PhiloticClient::connect(identity)
            .await
            .expect("connect client");

        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(10)) => {}
            result = client.recv_task() => panic!("recv_task completed too early: {result:?}"),
        }

        let pushed = client.recv_task().await.expect("receive preserved push");
        match pushed {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("decode pushed task");
                assert_eq!(payload["content"], "partial frame survives cancellation");
            }
            other => panic!("unexpected pushed response: {other:?}"),
        }

        server.await.expect("join server");
        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[test]
    fn disconnect_detection_matches_unexpected_eof() {
        let err = anyhow::Error::new(std::io::Error::from(ErrorKind::UnexpectedEof));
        assert!(is_ipc_disconnect(&err));
    }
}
