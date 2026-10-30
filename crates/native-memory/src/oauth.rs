//! Native resource contract for a trusted, freshly fetched issuer introspection.
//! This is not a token verifier and must never parse caller-provided claims as authority.
//! Runtime activation also requires a coordinated commit/release fence; see rollout doc.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

pub const RECALL: &str = "native-memory:recall";
pub const REMEMBER: &str = "native-memory:remember";

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceConfig {
    pub issuer: String,
    pub resource: String,
    pub metadata_url: String,
    pub authority_socket: std::path::PathBuf,
    pub grant_id: String,
}
impl ResourceConfig {
    pub fn validate(&self) -> Result<()> {
        let issuer = url::Url::parse(&self.issuer).map_err(|_| Error::Invalid)?;
        let resource = url::Url::parse(&self.resource).map_err(|_| Error::Invalid)?;
        let metadata = url::Url::parse(&self.metadata_url).map_err(|_| Error::Invalid)?;
        for value in [&issuer, &resource, &metadata] {
            if value.scheme() != "https"
                || value.host_str().is_none()
                || !value.username().is_empty()
                || value.password().is_some()
                || value.fragment().is_some()
                || value.query().is_some()
            {
                return Err(Error::Invalid);
            }
        }
        if issuer.path() != "/"
            || self.issuer.ends_with('/')
            || metadata.origin() != resource.origin()
            || metadata.path()
                != format!("/.well-known/oauth-protected-resource{}", resource.path())
            || !self.authority_socket.is_absolute()
            || !crate::label(&self.grant_id)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

/// Pinned local mapping, independently provisioned at ingress and receiver.
/// Owner and namespace are deliberately absent from issuer token claims.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub issuer: String,
    pub resource: String,
    pub client_id: String,
    pub subject: String,
    pub grant_version: String,
    pub owner: String,
    pub namespace: String,
    pub operations: Vec<String>,
}

/// Confidential issuer response, not deserialized from an MCP argument.
/// Unknown issuer metadata may be ignored; authority fields remain typed.
#[derive(Clone, Deserialize)]
pub struct Introspection {
    pub active: bool,
    pub iss: Option<String>,
    pub aud: Option<String>,
    pub sub: Option<String>,
    pub client_id: Option<String>,
    pub grant_version: Option<String>,
    pub scope: Option<String>,
    pub iat: Option<u64>,
    pub exp: Option<u64>,
}

/// Non-secret authority coordinates to bind into signed request/result context.
/// Serialization does not confer authority. No bearer or introspection secret here.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Authority {
    issuer: String,
    resource: String,
    client_id: String,
    subject: String,
    grant_version: String,
    owner: String,
    namespace: String,
    operation: String,
    expires_at: u64,
}

fn version(value: &str) -> bool {
    let parts: Vec<_> = value.split(':').collect();
    parts.len() == 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && (p.len() == 1 || !p.starts_with('0'))
                && p.bytes().all(|b| b.is_ascii_digit())
                && p.parse::<u64>().is_ok_and(|v| v <= 9_007_199_254_740_991)
        })
}

impl Binding {
    /// Call only with a response fetched directly from the configured confidential
    /// issuer. Must run again under the final write/release fence, without caching.
    pub fn validate(&self, claims: &Introspection, operation: &str, now: u64) -> Result<Authority> {
        let required = match operation {
            "recall" => RECALL,
            "remember" => REMEMBER,
            _ => return Err(Error::Denied),
        };
        if !claims.active
            || claims.iss.as_deref() != Some(&self.issuer)
            || claims.aud.as_deref() != Some(&self.resource)
            || claims.sub.as_deref() != Some(&self.subject)
            || claims.client_id.as_deref() != Some(&self.client_id)
            || claims.grant_version.as_deref() != Some(&self.grant_version)
            || !version(&self.grant_version)
            || !self.operations.iter().any(|v| v == operation)
            || ![
                &self.issuer,
                &self.resource,
                &self.subject,
                &self.client_id,
                &self.owner,
                &self.namespace,
            ]
            .iter()
            .all(|s| crate::label(s))
        {
            return Err(Error::Denied);
        }
        let scope = claims.scope.as_deref().ok_or(Error::Denied)?;
        let selected: Vec<_> = scope.split(' ').collect();
        if selected.is_empty()
            || selected.len() > 2
            || selected.iter().any(|s| ![RECALL, REMEMBER].contains(s))
            || (selected.len() == 2 && selected[0] == selected[1])
            || !selected.contains(&required)
        {
            return Err(Error::Denied);
        }
        let issued = claims.iat.ok_or(Error::Denied)?;
        let expires = claims.exp.ok_or(Error::Denied)?;
        if issued > now || expires <= now || expires <= issued || expires - issued > 900 {
            return Err(Error::Expired);
        }
        Ok(Authority {
            issuer: self.issuer.clone(),
            resource: self.resource.clone(),
            client_id: self.client_id.clone(),
            subject: self.subject.clone(),
            grant_version: self.grant_version.clone(),
            owner: self.owner.clone(),
            namespace: self.namespace.clone(),
            operation: operation.into(),
            expires_at: expires,
        })
    }
}

impl Authority {
    /// Revalidate original authority coordinates against fresh introspection.
    /// Caller must introspect the same token: coordinates alone do not identify
    /// an individual token/family. Runtime integration must enforce that binding.
    pub fn revalidate(&self, binding: &Binding, current: &Introspection, now: u64) -> Result<()> {
        if binding.validate(current, &self.operation, now)? != *self {
            return Err(Error::Denied);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Binding, Introspection) {
        let b = Binding {
            issuer: "https://issuer.example".into(),
            resource: "https://mcp.example/native/mcp".into(),
            client_id: "dot".into(),
            subject: "synthetic".into(),
            grant_version: "0:7".into(),
            owner: "synthetic-owner".into(),
            namespace: "synthetic".into(),
            operations: vec!["recall".into(), "remember".into()],
        };
        let c = Introspection {
            active: true,
            iss: Some(b.issuer.clone()),
            aud: Some(b.resource.clone()),
            sub: Some(b.subject.clone()),
            client_id: Some(b.client_id.clone()),
            grant_version: Some(b.grant_version.clone()),
            scope: Some(format!("{RECALL} {REMEMBER}")),
            iat: Some(100),
            exp: Some(1000),
        };
        (b, c)
    }
    #[test]
    fn native_authority_denies_substitution_and_revocation() {
        let (b, c) = fixture();
        let original = b.validate(&c, "remember", 101).unwrap();
        for field in ["iss", "aud", "sub", "client_id", "grant_version", "scope"] {
            let mut value = serde_json::json!({"active":true,"iss":c.iss,"aud":c.aud,"sub":c.sub,"client_id":c.client_id,"grant_version":c.grant_version,"scope":c.scope,"iat":100,"exp":1000});
            value[field] = serde_json::json!("wrong");
            let changed: Introspection = serde_json::from_value(value).unwrap();
            assert!(original.revalidate(&b, &changed, 101).is_err());
        }
        let mut revoked = c.clone();
        revoked.active = false;
        assert!(original.revalidate(&b, &revoked, 101).is_err());
        assert!(original.revalidate(&b, &c, 1000).is_err());
        let mut moved = b.clone();
        moved.owner = "other".into();
        assert!(original.revalidate(&moved, &c, 101).is_err());
    }
    #[test]
    fn read_scope_never_becomes_write_or_legacy_access() {
        let (mut b, mut c) = fixture();
        c.scope = Some(RECALL.into());
        assert!(b.validate(&c, "recall", 100).is_ok());
        assert!(b.validate(&c, "remember", 100).is_err());
        for scope in [
            "memory:recall",
            "life:recall",
            "native-memory:access",
            "native-memory:recall native-memory:recall",
            "native-memory:recall ",
        ] {
            c.scope = Some(scope.into());
            assert!(b.validate(&c, "recall", 100).is_err());
        }
        c.scope = Some(REMEMBER.into());
        b.operations = vec!["recall".into()];
        assert!(b.validate(&c, "remember", 100).is_err());
    }
    #[test]
    fn malformed_versions_times_and_array_audiences_fail_closed() {
        let (mut b, mut c) = fixture();
        for v in ["0:01", "0:-1", "0:9007199254740992", "1", "0:1:2"] {
            b.grant_version = v.into();
            c.grant_version = Some(v.into());
            assert!(b.validate(&c, "recall", 101).is_err());
        }
        let (b, mut c) = fixture();
        c.iat = Some(102);
        assert!(b.validate(&c, "recall", 101).is_err());
        c.iat = Some(0);
        assert!(b.validate(&c, "recall", 101).is_err());
        assert!(
            serde_json::from_value::<Introspection>(
                serde_json::json!({"active":true,"aud":["https://mcp.example/native/mcp"]})
            )
            .is_err()
        );
    }
}
