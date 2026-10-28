//! Fixed introspection contract retained for synthetic tests. Shared egress
//! attachment is refused; the isolated authority transport remains disabled.
//! No token-bearing task/session/mesh envelope is constructed here.
use crate::{
    ExecutionContext,
    transient::{Authority, Claims, Refusal},
};
use ansible_mesh_core::integration::{
    EgressPlacementDecision, HttpIntegrationAudit, HttpIntegrationRequest, IntegrationBinding,
    IntegrationTarget,
};
use async_trait::async_trait;
use base64::Engine;
use philotic_client::{IpcRequest, IpcResponse, PhiloticClient};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Explicit isolated registration; never borrow the ordinary runner ID.
    pub verifier_guest_id: String,
    pub isolation: Isolation,
    pub socket_path: PathBuf,
    pub server_uid: u32,
    pub peer_uid: u32,
    pub node_id: String,
    pub binding_id: String,
    pub binding_revision: u64,
    pub issuer: String,
    pub client_id: String,
    pub agent_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    DedicatedAccount,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExecutionMode {
    Ordinary,
    VerifierOnly,
}
pub fn execution_mode(
    args: &[std::ffi::OsString],
    profile: bool,
) -> Result<ExecutionMode, Refusal> {
    match (args, profile) {
        ([], false) => Ok(ExecutionMode::Ordinary),
        ([arg], true) if arg == "--oauth-verifier-only" => Ok(ExecutionMode::VerifierOnly),
        _ => Err(Refusal::Refused),
    }
}

/// Explicitly unavailable until a reviewed authenticated authority transport
/// exists. Current hotel accepts only its own UID, auto-subscribes Register,
/// and restricts audit to the shared egress role. A dedicated account cannot
/// use it as-is. Never silently claim that role or widen socket permissions.
pub async fn run_isolated(_profile: Profile, _guest_id: String) -> anyhow::Result<()> {
    anyhow::bail!(
        "isolated verifier authority is not qualified; no IPC registration or socket creation attempted"
    )
}
pub fn load_owned<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Result<T, Refusal> {
    use std::{
        io::Read,
        os::unix::fs::{MetadataExt, OpenOptionsExt},
    };
    // geteuid has no pointer arguments and no memory safety preconditions.
    let uid = unsafe { libc::geteuid() };
    if !path.is_absolute() || std::fs::canonicalize(path).map_err(|_| Refusal::Refused)? != path {
        return Err(Refusal::Refused);
    }
    for parent in path.ancestors().skip(1) {
        let m = std::fs::symlink_metadata(parent).map_err(|_| Refusal::Refused)?;
        if (m.uid() != 0 && m.uid() != uid)
            || (m.mode() & 0o022 != 0 && !(m.uid() == 0 && m.mode() & 0o1000 != 0))
        {
            return Err(Refusal::Refused);
        }
    }
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| Refusal::Refused)?;
    let before = f.metadata().map_err(|_| Refusal::Refused)?;
    if !before.is_file()
        || (before.uid() != 0 && before.uid() != uid)
        || before.mode() & 0o022 != 0
        || before.len() > 8192
    {
        return Err(Refusal::Refused);
    }
    let mut bytes = Vec::new();
    (&mut f)
        .take(8193)
        .read_to_end(&mut bytes)
        .map_err(|_| Refusal::Refused)?;
    let after = f.metadata().map_err(|_| Refusal::Refused)?;
    let named = std::fs::symlink_metadata(path).map_err(|_| Refusal::Refused)?;
    let identity = |m: &std::fs::Metadata| {
        (
            m.dev(),
            m.ino(),
            m.uid(),
            m.mode(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    };
    if bytes.len() > 8192
        || identity(&before) != identity(&after)
        || identity(&after) != identity(&named)
    {
        return Err(Refusal::Refused);
    }
    serde_json::from_slice(&bytes).map_err(|_| Refusal::Refused)
}
impl Profile {
    pub fn load(path: &std::path::Path) -> Result<Self, Refusal> {
        let p: Self = load_owned(path)?;
        p.validate()?;
        // geteuid has no memory safety preconditions.
        if p.server_uid != unsafe { libc::geteuid() } {
            return Err(Refusal::Refused);
        }
        Ok(p)
    }
    pub fn validate(&self) -> Result<(), Refusal> {
        // Deployment supplies identities/references/revision, never invented
        // defaults. This version has exactly one issuer and one fixed action.
        if self.issuer != "https://www.jaredlikes.com"
            || self.verifier_guest_id.is_empty()
            || self.server_uid == 0
            || !self.socket_path.is_absolute()
            || self.server_uid != self.peer_uid
            || self.binding_revision == 0
            || self.node_id.is_empty()
            || self.agent_id.is_empty()
            || self.binding_id.is_empty()
            || self.client_id.is_empty()
            || self.client_id.len() > 128
            || !self
                .client_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(Refusal::Refused);
        }
        Ok(())
    }
    fn validate_binding(&self, b: &IntegrationBinding) -> Result<(), Refusal> {
        self.validate()?;
        b.validate().map_err(|_| Refusal::Refused)?;
        let IntegrationTarget::Http(t) = &b.target else {
            return Err(Refusal::Refused);
        };
        if b.binding_id != self.binding_id
            || b.updated_at != self.binding_revision
            || !b.is_granted_to(&self.agent_id)
            || !b.grant_skills.is_empty()
            || b.requires_approval
            || t.base_url != format!("{}/oauth/introspect", self.issuer)
            || t.allowed_methods != ["POST"]
            || t.allowed_path_prefixes != ["/oauth/introspect"]
            || !t.allowed_request_headers.is_empty()
            || !t.default_headers.is_empty()
            || !t.allowed_redirect_hosts.is_empty()
            || t.max_redirects != 0
            || t.network_scope != ansible_mesh_core::integration::HttpNetworkScope::Public
            || t.timeout_secs > 3
            || t.max_request_bytes > 8192
            || t.max_response_bytes > 4096
            || !t.credential.as_ref().is_some_and(|c| {
                c.header.eq_ignore_ascii_case("authorization") && c.format == "Basic {}"
            })
        {
            return Err(Refusal::Refused);
        }
        Ok(())
    }
}

pub struct RunnerAuthority<'a> {
    pub client: &'a mut PhiloticClient,
    pub profile: &'a Profile,
}
impl RunnerAuthority<'_> {
    async fn binding(&mut self) -> Result<IntegrationBinding, Refusal> {
        let response = self
            .client
            .send_request_with_timeout(
                IpcRequest::GetIntegrationBindings {},
                Duration::from_secs(1),
            )
            .await
            .map_err(|_| Refusal::Unavailable)?;
        let IpcResponse::IntegrationBindingsState {
            integration_bindings,
        } = response
        else {
            return Err(Refusal::Refused);
        };
        let mut matches = integration_bindings
            .into_iter()
            .filter(|b| b.binding.binding_id == self.profile.binding_id);
        let entry = matches.next().ok_or(Refusal::Refused)?;
        if matches.next().is_some()
            || entry.execution_node_id.as_deref() != Some(&self.profile.node_id)
            || !matches!(
                entry.placement,
                EgressPlacementDecision::ExecuteLocal {
                    audit_fallback: false
                }
            )
        {
            return Err(Refusal::Refused);
        }
        self.profile.validate_binding(&entry.binding)?;
        Ok(entry.binding)
    }
    async fn run(&mut self, token: &str) -> Result<Claims, Refusal> {
        self.run_with(token, &BoundedHttp).await
    }
    async fn run_with(
        &mut self,
        token: &str,
        executor: &dyn ProfileExecutor,
    ) -> Result<Claims, Refusal> {
        let before = self.binding().await?;
        let IntegrationTarget::Http(t) = &before.target else {
            return Err(Refusal::Refused);
        };
        let reference = t
            .credential
            .as_ref()
            .ok_or(Refusal::Refused)?
            .secret_ref
            .clone();
        let value = self
            .client
            .send_request_with_timeout(
                IpcRequest::GetSecret {
                    secret_ref: reference,
                },
                Duration::from_secs(1),
            )
            .await
            .map_err(|_| Refusal::Unavailable)?;
        let IpcResponse::SecretData {
            value_json: Some(value),
            ..
        } = value
        else {
            return Err(Refusal::Unavailable);
        };
        // Exact JSON string only; no trim/unwrapping fallback or new secret.
        let secret: String = serde_json::from_str(&value).map_err(|_| Refusal::Refused)?;
        let (claims, audit) = executor
            .execute(self.profile, &before, token, &secret)
            .await?;
        if self.binding().await? != before {
            return Err(Refusal::Refused);
        }
        let ack = self
            .client
            .send_request_with_timeout(
                IpcRequest::RecordIntegrationAudit { audit },
                Duration::from_secs(1),
            )
            .await
            .map_err(|_| Refusal::AuditRejected)?;
        if !matches!(ack, IpcResponse::Standard { ok: true, .. }) {
            return Err(Refusal::AuditRejected);
        }
        Ok(claims)
    }
}
#[async_trait]
trait ProfileExecutor: Sync {
    async fn execute(
        &self,
        p: &Profile,
        binding: &IntegrationBinding,
        token: &str,
        secret: &str,
    ) -> Result<(Claims, HttpIntegrationAudit), Refusal>;
}
struct BoundedHttp;
#[async_trait]
impl ProfileExecutor for BoundedHttp {
    async fn execute(
        &self,
        p: &Profile,
        binding: &IntegrationBinding,
        token: &str,
        secret: &str,
    ) -> Result<(Claims, HttpIntegrationAudit), Refusal> {
        execute_profile(p, binding, token, secret).await
    }
}
#[async_trait]
impl Authority for RunnerAuthority<'_> {
    async fn introspect(&mut self, token: &str) -> Result<Claims, Refusal> {
        let result = self.run(token).await;
        // A timed-out GetSecret may still have a late SecretData frame. Never
        // return this connection to recv_task's stale-response logging path.
        if result.is_err() {
            self.client.disconnect_after_cancelled_operation();
        }
        result
    }
    fn abandon(&mut self) {
        self.client.disconnect_after_cancelled_operation();
    }
}

async fn execute_profile(
    p: &Profile,
    binding: &IntegrationBinding,
    token: &str,
    secret: &str,
) -> Result<(Claims, HttpIntegrationAudit), Refusal> {
    p.validate_binding(binding)?;
    let (basic, body) = encode_inputs(&p.client_id, token, secret)?;
    let correlation = uuid::Uuid::new_v4().to_string();
    let request = HttpIntegrationRequest {
        binding_id: binding.binding_id.clone(),
        method: "POST".into(),
        path: "/oauth/introspect".into(),
        query: BTreeMap::new(),
        headers: BTreeMap::new(),
        body: None,
    };
    let response = super::execute_encoded(
        binding,
        &request,
        ExecutionContext {
            executor_node_id: &p.node_id,
            placement: EgressPlacementDecision::ExecuteLocal {
                audit_fallback: false,
            },
            credential: Some(&basic),
            tool_name: "oauth.introspect",
            agent_id: &p.agent_id,
            caller_role: "mcp-membrane",
            session_id: "oauth-introspection-audit",
            turn_id: &correlation,
            correlation_id: &correlation,
        },
        body,
        "application/x-www-form-urlencoded",
    )
    .await
    .map_err(|_| Refusal::Unavailable)?;
    if response.status != 200 {
        return Err(Refusal::Unavailable);
    }
    let claims: Claims = serde_json::from_str(&response.body).map_err(|_| Refusal::Refused)?;
    Ok((claims, response.audit))
}

pub(super) fn encode_inputs(
    client_id: &str,
    token: &str,
    secret: &str,
) -> Result<(String, Vec<u8>), Refusal> {
    if !(43..=128).contains(&secret.len())
        || !secret
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(Refusal::Refused);
    }
    // Client/secret alphabets above need no percent escaping for the issuer's
    // encodeURIComponent Basic convention. Token is separately form-encoded.
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("{}:{secret}", client_id));
    let mut encoded =
        reqwest::Url::parse("https://unused.invalid").map_err(|_| Refusal::Refused)?;
    encoded.query_pairs_mut().append_pair("token", token);
    let body = encoded.query().ok_or(Refusal::Refused)?.as_bytes().to_vec();
    Ok((basic, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn shared_mode_refuses_profile_and_isolated_mode_cannot_register() {
        assert_eq!(execution_mode(&[], false), Ok(ExecutionMode::Ordinary));
        assert!(execution_mode(&[], true).is_err());
        assert!(execution_mode(&["--oauth-verifier-only".into()], false).is_err());
        assert_eq!(
            execution_mode(&["--oauth-verifier-only".into()], true),
            Ok(ExecutionMode::VerifierOnly)
        );
        let (p, _) = fixture();
        assert!(run_isolated(p, "synthetic-only".into()).await.is_err());
        assert!(!std::path::Path::new("/unbound/synthetic.sock").exists());
    }
    use ansible_mesh_core::integration::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixListener,
    };
    fn fixture() -> (Profile, IntegrationBinding) {
        let p = Profile {
            verifier_guest_id: "synthetic-isolated-verifier".into(),
            isolation: Isolation::DedicatedAccount,
            socket_path: "/unbound/synthetic.sock".into(),
            server_uid: 1,
            peer_uid: 1,
            node_id: "synthetic-node".into(),
            binding_id: "synthetic-introspection".into(),
            binding_revision: 1,
            issuer: "https://www.jaredlikes.com".into(),
            client_id: "synthetic_client".into(),
            agent_id: "mcp:synthetic".into(),
        };
        let b = IntegrationBinding {
            binding_id: p.binding_id.clone(),
            owner_agent_id: p.agent_id.clone(),
            display_name: None,
            target: IntegrationTarget::Http(HttpIntegrationTarget {
                base_url: format!("{}/oauth/introspect", p.issuer),
                allowed_methods: vec!["POST".into()],
                allowed_path_prefixes: vec!["/oauth/introspect".into()],
                allowed_request_headers: vec![],
                default_headers: BTreeMap::new(),
                response_header_allowlist: vec![],
                allowed_redirect_hosts: vec![],
                network_scope: HttpNetworkScope::Public,
                credential: Some(HttpCredentialBinding {
                    secret_ref: "secret://synthetic/introspection".into(),
                    header: "authorization".into(),
                    format: "Basic {}".into(),
                }),
                timeout_secs: 3,
                max_request_bytes: 8192,
                max_response_bytes: 4096,
                max_redirects: 0,
            }),
            grant_agents: vec![],
            grant_skills: vec![],
            traffic_class: EgressTrafficClass::GeneralApi,
            placement: EgressPlacementPolicy::Local,
            requires_approval: false,
            enabled: true,
            updated_at: 1,
        };
        (p, b)
    }
    #[test]
    fn exact_profile_rejects_policy_scope_and_endpoint_expansion() {
        let (p, b) = fixture();
        assert!(p.validate_binding(&b).is_ok());
        for n in 0..11 {
            let mut b = b.clone();
            match n {
                0 => b.enabled = false,
                1 => b.updated_at += 1,
                2 => b.owner_agent_id = "other".into(),
                3 => b.grant_skills.push("unproven-skill".into()),
                10 => b.requires_approval = true,
                _ => {
                    let IntegrationTarget::Http(t) = &mut b.target else {
                        unreachable!()
                    };
                    match n {
                        4 => t.base_url = "https://evil.invalid/oauth/introspect".into(),
                        5 => t.allowed_methods.push("GET".into()),
                        6 => t.max_redirects = 1,
                        7 => t.timeout_secs = 30,
                        8 => t.max_response_bytes = 8192,
                        _ => t.credential.as_mut().unwrap().format = "Bearer {}".into(),
                    }
                }
            }
            assert!(p.validate_binding(&b).is_err(), "mutation {n}");
        }
    }
    async fn send(stream: &mut tokio::net::UnixStream, value: &IpcResponse) {
        let data = serde_json::to_vec(value).unwrap();
        stream.write_u32(data.len() as u32).await.unwrap();
        stream.write_all(&data).await.unwrap();
    }
    async fn read(stream: &mut tokio::net::UnixStream) -> Value {
        let n = stream.read_u32().await.unwrap();
        let mut bytes = vec![0; n as usize];
        stream.read_exact(&mut bytes).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
    use serde_json::{Value, json};
    struct SyntheticHttp;
    #[async_trait]
    impl ProfileExecutor for SyntheticHttp {
        async fn execute(
            &self,
            p: &Profile,
            b: &IntegrationBinding,
            token: &str,
            secret: &str,
        ) -> Result<(Claims, HttpIntegrationAudit), Refusal> {
            assert_eq!(token, "SYNTHETIC_ACCESS_TOKEN");
            assert_eq!(secret, "SYNTHETIC_CREDENTIAL");
            p.validate_binding(b)?;
            let audit: HttpIntegrationAudit = serde_json::from_value(json!({
                "binding_id":b.binding_id,"tool_name":"oauth.introspect","agent_id":p.agent_id,"caller_role":"mcp-membrane","session_id":"oauth-introspection-audit","turn_id":"synthetic-correlation","correlation_id":"synthetic-correlation","traffic_class":"general_api","executor_node_id":p.node_id,"placement":{"decision":"execute_local","audit_fallback":false},"target_origin":p.issuer,"method":"POST","path":"/oauth/introspect","policy_revision":b.updated_at,"approval_required":true,"credential_ref":"secret://synthetic/introspection","credential_injected":true,"redirect_count":0,"request_bytes":32,"response_status":200,"response_bytes":16,"started_at_ms":1,"finished_at_ms":2,"duration_ms":1,"outcome":"http_200","failure_code":null
            })).unwrap();
            Ok((
                serde_json::from_value(json!({"active":false})).unwrap(),
                audit,
            ))
        }
    }
    #[tokio::test]
    async fn real_sdk_keeps_token_out_of_hotel_frames_and_requires_audit_ack() {
        for audit_ok in [true, false] {
            let path = std::fs::canonicalize("/tmp").unwrap().join(format!(
                "ih{}.sock",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ));
            let listener = UnixListener::bind(&path).unwrap();
            let (p, b) = fixture();
            let server_b = b.clone();
            let server = tokio::spawn(async move {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut frames = vec![];
                for step in 0..5 {
                    let r = read(&mut s).await;
                    let op = r["operation"].as_str().unwrap();
                    frames.push(r.clone());
                    let response = match step {
                        0 => {
                            assert_eq!(op, "register");
                            assert_eq!(r["payload"]["role"], "egress-http-runner");
                            IpcResponse::success("reg", None)
                        }
                        1 | 3 => {
                            assert_eq!(op, "get_integration_bindings");
                            IpcResponse::IntegrationBindingsState {
                                integration_bindings: vec![
                                    philotic_client::IntegrationBindingEntry {
                                        binding: server_b.clone(),
                                        placement: EgressPlacementDecision::ExecuteLocal {
                                            audit_fallback: false,
                                        },
                                        execution_node_id: Some("synthetic-node".into()),
                                        exit_hotel_reachable: true,
                                    },
                                ],
                            }
                        }
                        2 => {
                            assert_eq!(op, "get_secret");
                            IpcResponse::SecretData {
                                secret_ref: "secret://synthetic/introspection".into(),
                                value_json: Some(
                                    serde_json::to_string("SYNTHETIC_CREDENTIAL").unwrap(),
                                ),
                            }
                        }
                        _ => {
                            assert_eq!(op, "record_integration_audit");
                            if audit_ok {
                                IpcResponse::success("audit", None)
                            } else {
                                IpcResponse::error("audit", "FORBIDDEN", "synthetic rejection")
                            }
                        }
                    };
                    send(&mut s, &response).await;
                }
                let serialized = serde_json::to_string(&frames).unwrap();
                assert!(!serialized.contains("SYNTHETIC_ACCESS_TOKEN"));
                assert!(!serialized.contains("SYNTHETIC_CREDENTIAL"));
                assert!(!serialized.contains("emit_task"));
            });
            let mut client = PhiloticClient::connect_at(
                path.to_str().unwrap(),
                philotic_client::GuestIdentity {
                    guest_id: "synthetic-egress".into(),
                    role: "egress-http-runner".into(),
                    supported_tools: vec![],
                },
            )
            .await
            .unwrap();
            let mut a = RunnerAuthority {
                client: &mut client,
                profile: &p,
            };
            let outcome = a.run_with("SYNTHETIC_ACCESS_TOKEN", &SyntheticHttp).await;
            if audit_ok {
                assert!(outcome.is_ok());
            } else {
                assert!(matches!(outcome, Err(Refusal::AuditRejected)));
            }
            server.await.unwrap();
            std::fs::remove_file(path).unwrap();
        }
    }
    #[derive(Clone)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Self;
        fn make_writer(&'a self) -> Self {
            self.clone()
        }
    }
    #[tokio::test]
    async fn late_secret_after_timeout_cannot_enter_sdk_stale_response_logs() {
        use tracing::instrument::WithSubscriber;
        let (p, b) = fixture();
        let path = std::fs::canonicalize("/tmp").unwrap().join(format!(
            "il{}.sock",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            assert_eq!(read(&mut s).await["operation"], "register");
            send(&mut s, &IpcResponse::success("reg", None)).await;
            assert_eq!(read(&mut s).await["operation"], "get_integration_bindings");
            send(
                &mut s,
                &IpcResponse::IntegrationBindingsState {
                    integration_bindings: vec![philotic_client::IntegrationBindingEntry {
                        binding: b,
                        placement: EgressPlacementDecision::ExecuteLocal {
                            audit_fallback: false,
                        },
                        execution_node_id: Some("synthetic-node".into()),
                        exit_hotel_reachable: true,
                    }],
                },
            )
            .await;
            assert_eq!(read(&mut s).await["operation"], "get_secret");
            tokio::time::sleep(Duration::from_millis(1100)).await;
            let late = serde_json::to_vec(&IpcResponse::SecretData {
                secret_ref: "synthetic".into(),
                value_json: Some("DO_NOT_LOG_LATE_SYNTHETIC_SECRET".into()),
            })
            .unwrap();
            if s.write_u32(late.len() as u32).await.is_ok() {
                let _ = s.write_all(&late).await;
            }
        });
        let logs = Capture(Default::default());
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(logs.clone())
            .finish();
        async {
            let mut client = PhiloticClient::connect_at(
                path.to_str().unwrap(),
                philotic_client::GuestIdentity {
                    guest_id: "synthetic-egress".into(),
                    role: "egress-http-runner".into(),
                    supported_tools: vec![],
                },
            )
            .await
            .unwrap();
            let mut authority = RunnerAuthority {
                client: &mut client,
                profile: &p,
            };
            assert!(matches!(
                authority.introspect("SYNTHETIC_ACCESS_TOKEN").await,
                Err(Refusal::Unavailable)
            ));
            assert!(client.recv_task().await.is_err());
            server.await.unwrap();
        }
        .with_subscriber(subscriber)
        .await;
        assert!(
            !String::from_utf8_lossy(&logs.0.lock().unwrap())
                .contains("DO_NOT_LOG_LATE_SYNTHETIC_SECRET")
        );
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn profile_source_rejects_symlink_unsafe_modes_oversize_and_fifo_without_reading() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = std::fs::canonicalize("/tmp").unwrap().join(format!(
            "ip{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (mut p, _) = fixture();
        p.server_uid = std::fs::metadata(&dir).unwrap().uid();
        p.peer_uid = p.server_uid;
        let path = dir.join("p.json");
        std::fs::write(&path, serde_json::to_vec(&p).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Profile::load(&path).is_ok());
        let link = dir.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(Profile::load(&link).is_err());
        std::fs::remove_file(link).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(Profile::load(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, vec![b'x'; 8193]).unwrap();
        assert!(Profile::load(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        let fifo = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(Profile::load(&path).is_err());
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
