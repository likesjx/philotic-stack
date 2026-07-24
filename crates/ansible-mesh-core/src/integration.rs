//! Canonical contracts for governed outbound integrations.
//!
//! An integration binding is the durable, non-secret authority that lets an
//! agent use one constrained external capability. The binding travels through
//! the hotel/router and may be executed by a local or remote egress runner.
//! Credentials never travel with it: only a vault reference is carried, and
//! the executing hotel resolves that reference inside the runner boundary.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};

pub const HTTP_PROJECTED_TOOL_PREFIX: &str = "http:";
pub const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 30;
pub const DEFAULT_HTTP_MAX_REQUEST_BYTES: u64 = 256 * 1024;
pub const DEFAULT_HTTP_MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
pub const DEFAULT_HTTP_MAX_REDIRECTS: u8 = 3;

/// Canonical outbound traffic classes used by policy and placement.
///
/// Classification is explicit input. Executors must not infer policy from a
/// URL after route selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EgressTrafficClass {
    Communication,
    #[default]
    GeneralApi,
    Mcp,
    ModelProvider,
    MeshPeer,
    LocalResource,
    Artifact,
}

/// Behavior when a preferred exit hotel is not reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EgressFallback {
    #[default]
    Deny,
    LocalWithAudit,
}

/// Placement policy for the component that performs outbound network I/O.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum EgressPlacementPolicy {
    #[default]
    Local,
    PreferHotel {
        hotel_id: String,
        #[serde(default)]
        fallback: EgressFallback,
    },
    RequireHotel {
        hotel_id: String,
    },
    Deny,
}

/// Resolved execution target for one outbound request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum EgressPlacementDecision {
    ExecuteLocal { audit_fallback: bool },
    ExecuteAtHotel { hotel_id: String },
    Deny { reason: String },
}

/// Resolve policy against the current reachability of the named exit hotel.
///
/// This function is deliberately pure. The hotel owns reachability,
/// materialization, and routing; the selected runner owns network I/O.
pub fn decide_egress_placement(
    policy: &EgressPlacementPolicy,
    exit_hotel_reachable: bool,
) -> EgressPlacementDecision {
    match policy {
        EgressPlacementPolicy::Local => EgressPlacementDecision::ExecuteLocal {
            audit_fallback: false,
        },
        EgressPlacementPolicy::PreferHotel { hotel_id, fallback } => {
            if exit_hotel_reachable {
                EgressPlacementDecision::ExecuteAtHotel {
                    hotel_id: hotel_id.clone(),
                }
            } else {
                match fallback {
                    EgressFallback::Deny => EgressPlacementDecision::Deny {
                        reason: format!("preferred exit hotel '{hotel_id}' is unreachable"),
                    },
                    EgressFallback::LocalWithAudit => EgressPlacementDecision::ExecuteLocal {
                        audit_fallback: true,
                    },
                }
            }
        }
        EgressPlacementPolicy::RequireHotel { hotel_id } => {
            if exit_hotel_reachable {
                EgressPlacementDecision::ExecuteAtHotel {
                    hotel_id: hotel_id.clone(),
                }
            } else {
                EgressPlacementDecision::Deny {
                    reason: format!("required exit hotel '{hotel_id}' is unreachable"),
                }
            }
        }
        EgressPlacementPolicy::Deny => EgressPlacementDecision::Deny {
            reason: "external egress is disabled by placement policy".into(),
        },
    }
}

/// The execution protocol represented by a binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntegrationTarget {
    Http(HttpIntegrationTarget),
    /// Connects the shared placement/audit contract to the existing MCP
    /// manager without moving MCP protocol authority into the HTTP runner.
    Mcp {
        upstream_id: String,
    },
}

/// One durable authorization to use an external integration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationBinding {
    pub binding_id: String,
    pub owner_agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub target: IntegrationTarget,
    #[serde(default)]
    pub grant_agents: Vec<String>,
    /// Optional SkillDAG dependency gate. Empty means the binding is available
    /// whenever its agent grant is active; otherwise at least one named skill
    /// must be present in the session's effective skill set.
    #[serde(default)]
    pub grant_skills: Vec<String>,
    #[serde(default)]
    pub traffic_class: EgressTrafficClass,
    #[serde(default)]
    pub placement: EgressPlacementPolicy,
    /// High-agency integrations remain approval-gated when projected.
    #[serde(default = "default_true")]
    pub requires_approval: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Unix epoch seconds. Used for last-writer-wins registry updates.
    pub updated_at: u64,
}

impl IntegrationBinding {
    pub fn is_granted_to(&self, agent_id: &str) -> bool {
        self.enabled
            && (self.owner_agent_id == agent_id
                || self
                    .grant_agents
                    .iter()
                    .any(|candidate| candidate == agent_id))
    }

    pub fn is_available_to(&self, agent_id: &str, effective_skills: &[String]) -> bool {
        self.is_granted_to(agent_id)
            && (self.grant_skills.is_empty()
                || self
                    .grant_skills
                    .iter()
                    .any(|required| effective_skills.iter().any(|active| active == required)))
    }

    pub fn projected_tool_name(&self) -> Option<String> {
        matches!(self.target, IntegrationTarget::Http(_))
            .then(|| projected_http_tool_name(&self.binding_id))
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_identifier("binding_id", &self.binding_id)?;
        validate_identifier("owner_agent_id", &self.owner_agent_id)?;
        for agent_id in &self.grant_agents {
            validate_identifier("grant agent_id", agent_id)?;
        }
        for skill_id in &self.grant_skills {
            validate_identifier("grant skill_id", skill_id)?;
        }
        if self.updated_at == 0 {
            return Err("updated_at must be non-zero".into());
        }
        if let EgressPlacementPolicy::PreferHotel { hotel_id, .. }
        | EgressPlacementPolicy::RequireHotel { hotel_id } = &self.placement
        {
            validate_identifier("placement hotel_id", hotel_id)?;
        }
        match &self.target {
            IntegrationTarget::Http(target) => target.validate(),
            IntegrationTarget::Mcp { upstream_id } => {
                validate_identifier("MCP upstream_id", upstream_id)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledIntegrationDependency {
    pub binding_id: String,
    pub projected_tool_name: String,
}

/// Compile the active SkillDAG edge into concrete binding-scoped tool
/// dependencies. Invalid, disabled, ungranted, denied, or non-HTTP bindings
/// produce no executable projection.
pub fn compile_integration_dependencies<'a>(
    bindings: impl IntoIterator<Item = &'a IntegrationBinding>,
    agent_id: &str,
    effective_skills: &[String],
) -> Vec<CompiledIntegrationDependency> {
    let mut compiled: Vec<_> = bindings
        .into_iter()
        .filter(|binding| binding.validate().is_ok())
        .filter(|binding| binding.is_available_to(agent_id, effective_skills))
        .filter(|binding| matches!(binding.target, IntegrationTarget::Http(_)))
        .filter(|binding| !matches!(binding.placement, EgressPlacementPolicy::Deny))
        .map(|binding| CompiledIntegrationDependency {
            binding_id: binding.binding_id.clone(),
            projected_tool_name: projected_http_tool_name(&binding.binding_id),
        })
        .collect();
    compiled.sort_by(|left, right| left.binding_id.cmp(&right.binding_id));
    compiled
}

fn default_true() -> bool {
    true
}

/// Network ranges a binding explicitly permits after DNS resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HttpNetworkScope {
    /// Globally routable addresses only. This is the safe API default.
    #[default]
    Public,
    Loopback,
    Tailnet,
    Private,
}

/// Vault credential injection. Only `secret_ref` crosses process boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpCredentialBinding {
    pub secret_ref: String,
    /// Header set by the runner, never by tool arguments.
    pub header: String,
    /// Exactly one `{}` placeholder is replaced with the resolved secret.
    pub format: String,
}

/// Bounded HTTP authority for an integration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpIntegrationTarget {
    /// Origin plus optional base path, e.g. `https://api.example.com/v1`.
    pub base_url: String,
    /// Uppercase HTTP methods. Empty is invalid.
    pub allowed_methods: Vec<String>,
    /// Paths are matched after joining against `base_url`. Empty means only
    /// the base path itself.
    #[serde(default)]
    pub allowed_path_prefixes: Vec<String>,
    /// Caller-settable request headers. Sensitive hop/auth headers are always
    /// denied even if mistakenly listed.
    #[serde(default)]
    pub allowed_request_headers: Vec<String>,
    /// Static non-secret headers injected by the runner.
    #[serde(default)]
    pub default_headers: BTreeMap<String, String>,
    /// Response headers retained for the caller; all others are dropped.
    #[serde(default)]
    pub response_header_allowlist: Vec<String>,
    /// Redirects remain opt-in and host-bound.
    #[serde(default)]
    pub allowed_redirect_hosts: Vec<String>,
    #[serde(default)]
    pub network_scope: HttpNetworkScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<HttpCredentialBinding>,
    #[serde(default = "default_http_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_http_max_request_bytes")]
    pub max_request_bytes: u64,
    #[serde(default = "default_http_max_response_bytes")]
    pub max_response_bytes: u64,
    #[serde(default = "default_http_max_redirects")]
    pub max_redirects: u8,
}

impl HttpIntegrationTarget {
    pub fn validate(&self) -> Result<(), String> {
        let parsed = ParsedHttpUrl::parse(&self.base_url)?;
        if parsed.username_or_password {
            return Err("base_url must not contain userinfo".into());
        }
        if !parsed.path.starts_with('/') {
            return Err("base_url path must be absolute".into());
        }
        if self.allowed_methods.is_empty() {
            return Err("allowed_methods must contain at least one method".into());
        }
        for method in &self.allowed_methods {
            if !valid_http_method(method) {
                return Err(format!("invalid or non-uppercase HTTP method '{method}'"));
            }
        }
        for prefix in &self.allowed_path_prefixes {
            validate_path_prefix(prefix)?;
        }
        for header in self
            .allowed_request_headers
            .iter()
            .chain(self.default_headers.keys())
            .chain(self.response_header_allowlist.iter())
        {
            validate_header_name(header)?;
        }
        for header in &self.allowed_request_headers {
            if forbidden_caller_header(header) {
                return Err(format!(
                    "caller-controlled header '{header}' is forbidden at the egress boundary"
                ));
            }
        }
        for header in self.default_headers.keys() {
            if forbidden_caller_header(header) {
                return Err(format!(
                    "static header '{header}' must use the credential binding or executor-owned transport handling"
                ));
            }
        }
        for host in &self.allowed_redirect_hosts {
            validate_host(host)?;
        }
        if let Some(credential) = &self.credential {
            validate_identifier("credential secret_ref", &credential.secret_ref)?;
            validate_header_name(&credential.header)?;
            if credential.format.matches("{}").count() != 1 {
                return Err("credential format must contain exactly one '{}' placeholder".into());
            }
            if self
                .allowed_request_headers
                .iter()
                .any(|header| header.eq_ignore_ascii_case(&credential.header))
            {
                return Err(format!(
                    "credential header '{}' cannot also be caller-controlled",
                    credential.header
                ));
            }
        }
        if self.timeout_secs == 0 || self.timeout_secs > 300 {
            return Err("timeout_secs must be between 1 and 300".into());
        }
        if self.max_request_bytes == 0 || self.max_response_bytes == 0 {
            return Err("request and response byte limits must be non-zero".into());
        }
        if self.max_redirects > 10 {
            return Err("max_redirects must be at most 10".into());
        }
        Ok(())
    }

    pub fn method_allowed(&self, method: &str) -> bool {
        self.allowed_methods.iter().any(|item| item == method)
    }

    pub fn path_allowed(&self, path: &str) -> bool {
        let base_path = ParsedHttpUrl::parse(&self.base_url)
            .map(|url| url.path)
            .unwrap_or_else(|_| "/".into());
        if self.allowed_path_prefixes.is_empty() {
            return path == base_path || path == format!("{}/", base_path.trim_end_matches('/'));
        }
        self.allowed_path_prefixes.iter().any(|prefix| {
            path == prefix || path.starts_with(&format!("{}/", prefix.trim_end_matches('/')))
        })
    }
}

fn default_http_timeout_secs() -> u64 {
    DEFAULT_HTTP_TIMEOUT_SECS
}
fn default_http_max_request_bytes() -> u64 {
    DEFAULT_HTTP_MAX_REQUEST_BYTES
}
fn default_http_max_response_bytes() -> u64 {
    DEFAULT_HTTP_MAX_RESPONSE_BYTES
}
fn default_http_max_redirects() -> u8 {
    DEFAULT_HTTP_MAX_REDIRECTS
}

/// Model-facing arguments accepted by `http:<binding>.request`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpIntegrationRequest {
    #[serde(default)]
    pub binding_id: String,
    pub method: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
}

/// Sanitized result returned by the runner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpIntegrationResponse {
    pub request_id: String,
    pub status: u16,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub body: String,
    pub content_type: Option<String>,
    pub response_bytes: u64,
    pub audit: HttpIntegrationAudit,
}

/// Secret-free, content-free execution evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpIntegrationAudit {
    pub binding_id: String,
    pub tool_name: String,
    pub agent_id: String,
    pub caller_role: String,
    pub session_id: String,
    pub turn_id: String,
    pub correlation_id: String,
    pub traffic_class: EgressTrafficClass,
    pub executor_node_id: String,
    pub placement: EgressPlacementDecision,
    pub target_origin: String,
    pub method: String,
    pub path: String,
    pub policy_revision: u64,
    pub approval_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    pub credential_injected: bool,
    pub redirect_count: u8,
    pub request_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_status: Option<u16>,
    pub response_bytes: u64,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub duration_ms: u64,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
}

pub fn projected_http_tool_name(binding_id: &str) -> String {
    format!("{HTTP_PROJECTED_TOOL_PREFIX}{binding_id}.request")
}

pub fn parse_projected_http_tool_name(name: &str) -> Option<&str> {
    let rest = name.strip_prefix(HTTP_PROJECTED_TOOL_PREFIX)?;
    let binding_id = rest.strip_suffix(".request")?;
    (!binding_id.is_empty()).then_some(binding_id)
}

pub fn forbidden_caller_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "host"
            | "content-length"
            | "transfer-encoding"
            | "connection"
            | "upgrade"
            | "te"
            | "trailer"
    )
}

pub fn ip_matches_scope(ip: IpAddr, scope: HttpNetworkScope) -> bool {
    match scope {
        HttpNetworkScope::Loopback => ip.is_loopback(),
        HttpNetworkScope::Tailnet => match ip {
            IpAddr::V4(v4) => {
                let octets = v4.octets();
                octets[0] == 100 && (64..128).contains(&octets[1])
            }
            IpAddr::V6(_) => false,
        },
        HttpNetworkScope::Private => is_private_ip(ip),
        HttpNetworkScope::Public => is_public_ip(ip),
    }
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6.is_unspecified()
                || is_ipv6_documentation(v6)
        }
    }
}

fn is_public_ip(ip: IpAddr) -> bool {
    !ip.is_loopback()
        && !is_private_ip(ip)
        && !ip.is_multicast()
        && match ip {
            IpAddr::V4(v4) => !is_special_v4(v4),
            IpAddr::V6(v6) => !v6.is_unicast_link_local(),
        }
}

fn is_special_v4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 0
        || octets[0] >= 224
        || (octets[0] == 100 && (64..128).contains(&octets[1]))
        || (octets[0] == 169 && octets[1] == 254)
}

fn is_ipv6_documentation(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    segments[0] == 0x2001 && segments[1] == 0x0db8
}

fn validate_identifier(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 128 {
        return Err(format!("{label} must contain 1..=128 characters"));
    }
    if !value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
    }) {
        return Err(format!("{label} contains unsupported characters"));
    }
    Ok(())
}

fn valid_http_method(method: &str) -> bool {
    matches!(
        method,
        "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
    )
}

fn validate_path_prefix(prefix: &str) -> Result<(), String> {
    if !prefix.starts_with('/') || prefix.contains("..") || prefix.contains(['?', '#']) {
        return Err(format!(
            "path prefix '{prefix}' must be absolute and cannot contain '..', '?' or '#'"
        ));
    }
    Ok(())
}

fn validate_header_name(header: &str) -> Result<(), String> {
    if header.is_empty()
        || !header.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
    {
        return Err(format!("invalid HTTP header name '{header}'"));
    }
    Ok(())
}

fn validate_host(host: &str) -> Result<(), String> {
    if host.is_empty()
        || host.len() > 253
        || host.contains('/')
        || host.contains('@')
        || host.contains(char::is_whitespace)
    {
        return Err(format!("invalid redirect host '{host}'"));
    }
    Ok(())
}

/// Minimal URL shape validation kept dependency-free for the shared contract.
struct ParsedHttpUrl {
    path: String,
    username_or_password: bool,
}

impl ParsedHttpUrl {
    fn parse(value: &str) -> Result<Self, String> {
        let rest = value
            .strip_prefix("https://")
            .or_else(|| value.strip_prefix("http://"))
            .ok_or_else(|| "base_url must use http or https".to_string())?;
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        if authority.is_empty() {
            return Err("base_url must contain a host".into());
        }
        if value.contains('#') || value.contains('?') {
            return Err("base_url must not contain query or fragment components".into());
        }
        let path = rest
            .get(authority_end..)
            .filter(|suffix| !suffix.is_empty())
            .unwrap_or("/")
            .to_string();
        Ok(Self {
            path,
            username_or_password: authority.contains('@'),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> IntegrationBinding {
        IntegrationBinding {
            binding_id: "weather-api".into(),
            owner_agent_id: "agent-jane".into(),
            display_name: Some("Weather API".into()),
            target: IntegrationTarget::Http(HttpIntegrationTarget {
                base_url: "https://api.weather.example/v1".into(),
                allowed_methods: vec!["GET".into()],
                allowed_path_prefixes: vec!["/v1/forecast".into()],
                allowed_request_headers: vec!["accept-language".into()],
                default_headers: BTreeMap::from([("accept".into(), "application/json".into())]),
                response_header_allowlist: vec!["content-type".into()],
                allowed_redirect_hosts: vec![],
                network_scope: HttpNetworkScope::Public,
                credential: Some(HttpCredentialBinding {
                    secret_ref: "vault/weather".into(),
                    header: "authorization".into(),
                    format: "Bearer {}".into(),
                }),
                timeout_secs: 10,
                max_request_bytes: 1024,
                max_response_bytes: 8192,
                max_redirects: 0,
            }),
            grant_agents: vec!["agent-bjork".into()],
            grant_skills: vec![],
            traffic_class: EgressTrafficClass::GeneralApi,
            placement: EgressPlacementPolicy::PreferHotel {
                hotel_id: "vps-jane".into(),
                fallback: EgressFallback::Deny,
            },
            requires_approval: true,
            enabled: true,
            updated_at: 1,
        }
    }

    #[test]
    fn binding_round_trips_and_projects() {
        let binding = binding();
        binding.validate().unwrap();
        let json = serde_json::to_string(&binding).unwrap();
        assert_eq!(
            serde_json::from_str::<IntegrationBinding>(&json).unwrap(),
            binding
        );
        assert_eq!(
            binding.projected_tool_name().as_deref(),
            Some("http:weather-api.request")
        );
        assert!(binding.is_granted_to("agent-bjork"));
        assert_eq!(
            parse_projected_http_tool_name("http:weather-api.request"),
            Some("weather-api")
        );
    }

    #[test]
    fn binding_validation_rejects_ambient_authority() {
        let mut binding = binding();
        if let IntegrationTarget::Http(target) = &mut binding.target {
            target.allowed_request_headers.push("Authorization".into());
        }
        assert!(binding.validate().unwrap_err().contains("forbidden"));

        if let IntegrationTarget::Http(target) = &mut binding.target {
            target.allowed_request_headers.clear();
            target.allowed_methods = vec!["get".into()];
        }
        assert!(binding.validate().unwrap_err().contains("non-uppercase"));
    }

    #[test]
    fn path_matching_stays_on_segment_boundaries() {
        let IntegrationTarget::Http(target) = binding().target else {
            unreachable!()
        };
        assert!(target.path_allowed("/v1/forecast"));
        assert!(target.path_allowed("/v1/forecast/hourly"));
        assert!(!target.path_allowed("/v1/forecaster"));
        assert!(!target.path_allowed("/admin"));
    }

    #[test]
    fn address_scopes_fail_closed() {
        assert!(ip_matches_scope(
            "8.8.8.8".parse().unwrap(),
            HttpNetworkScope::Public
        ));
        assert!(!ip_matches_scope(
            "127.0.0.1".parse().unwrap(),
            HttpNetworkScope::Public
        ));
        assert!(ip_matches_scope(
            "127.0.0.1".parse().unwrap(),
            HttpNetworkScope::Loopback
        ));
        assert!(ip_matches_scope(
            "100.64.12.2".parse().unwrap(),
            HttpNetworkScope::Tailnet
        ));
        assert!(!ip_matches_scope(
            "100.128.0.1".parse().unwrap(),
            HttpNetworkScope::Tailnet
        ));
        assert!(ip_matches_scope(
            "192.168.1.2".parse().unwrap(),
            HttpNetworkScope::Private
        ));
    }

    #[test]
    fn placement_requires_reachable_exit_or_explicit_fallback() {
        assert_eq!(
            decide_egress_placement(
                &EgressPlacementPolicy::RequireHotel {
                    hotel_id: "vps-jane".into()
                },
                false
            ),
            EgressPlacementDecision::Deny {
                reason: "required exit hotel 'vps-jane' is unreachable".into()
            }
        );
        assert_eq!(
            decide_egress_placement(
                &EgressPlacementPolicy::PreferHotel {
                    hotel_id: "vps-jane".into(),
                    fallback: EgressFallback::LocalWithAudit,
                },
                false
            ),
            EgressPlacementDecision::ExecuteLocal {
                audit_fallback: true
            }
        );
    }

    #[test]
    fn skill_dependencies_compile_only_for_active_grants() {
        let mut weather = binding();
        weather.grant_skills = vec!["weather.research".into()];
        assert!(compile_integration_dependencies([&weather], "agent-bjork", &[]).is_empty());
        assert_eq!(
            compile_integration_dependencies(
                [&weather],
                "agent-bjork",
                &["weather.research".into()]
            ),
            vec![CompiledIntegrationDependency {
                binding_id: "weather-api".into(),
                projected_tool_name: "http:weather-api.request".into(),
            }]
        );
    }
}
