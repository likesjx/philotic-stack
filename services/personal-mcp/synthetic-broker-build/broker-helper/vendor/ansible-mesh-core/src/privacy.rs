//! Transitional shared policy foundation; not yet wired into live dispatch.
//!
//! Identity, roles, policies and provider classification must come from server
//! authorities, never model output or request hints. Authorization is evaluated
//! again at dispatch against current source policies: a read grant is not an
//! egress grant, and a copy cannot erase a source restriction or revocation.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Adapter implemented by the server's authenticated connection/session owner.
/// Implementing this for an unverified request would violate the trust contract.
/// Display names, role incarnation IDs and model-provided agent IDs are invalid.
pub trait ServerAuthenticatedIdentity {
    fn stable_agent_id(&self) -> &str;
    fn roles(&self) -> BTreeSet<String>;
}

/// Deliberately not deserializable from a request payload.
#[derive(Debug)]
pub struct AuthenticatedAgent {
    id: String,
    roles: BTreeSet<String>,
}

impl AuthenticatedAgent {
    pub fn roles(&self) -> BTreeSet<String> {
        self.roles.clone()
    }
    pub fn stable_agent_id(&self) -> &str {
        &self.id
    }
    pub fn from_server(context: &impl ServerAuthenticatedIdentity) -> Result<Self, Denial> {
        let id = context.stable_agent_id();
        if id.trim().is_empty() {
            return Err(Denial::Unauthenticated);
        }
        Ok(Self {
            id: id.into(),
            roles: context.roles(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingOperation {
    Inference,
    Embedding,
    SemanticResolution,
    SpeechToText,
    TextToSpeech,
}

/// Classify the actual endpoint at the provider boundary, not its brand name.
/// A local process proxying to a cloud service is External.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderBoundary {
    LocalTrusted,
    External,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourcePolicy {
    pub owner: String,
    pub creator: String,
    pub creator_read_grant: bool,
    pub read_roles: BTreeSet<String>,
    /// Private always forbids external processing, even for the owner.
    pub private: bool,
    /// Explicit, operation-specific permission; read ACL never populates this.
    pub external_operations: BTreeSet<ProcessingOperation>,
    /// Canonical source IDs; trusted storage must persist these immutably.
    pub sources: Vec<String>,
}

impl ResourcePolicy {
    /// Conservative default for a newly captured record.
    pub fn private(owner: String, creator: String) -> Self {
        Self {
            owner,
            creator,
            creator_read_grant: true,
            read_roles: BTreeSet::new(),
            private: true,
            external_operations: BTreeSet::new(),
            sources: Vec::new(),
        }
    }

    /// Revocation applies to the creator privilege; independent RBAC grants
    /// still apply. Persistence/transaction ownership remains with the server.
    pub fn revoke_creator(&mut self, actor: &AuthenticatedAgent) -> Result<(), Denial> {
        if actor.id != self.owner || self.owner.trim().is_empty() {
            return Err(Denial::OwnerRequired);
        }
        self.creator_read_grant = false;
        Ok(())
    }
}

/// Read the current canonical policy, including every copy/derivative source.
/// Storage failure and missing policy both return None and deny access.
pub trait PolicyAuthority {
    fn policy(&self, resource: &str) -> Option<&ResourcePolicy>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    Unauthenticated,
    MissingPolicy,
    InvalidPolicy,
    SourceCycle,
    LineageLimit,
    ReadForbidden,
    OwnerRequired,
    UnknownProvider,
    ExternalForbidden,
    EmptyInput,
}

enum Action {
    Read,
    Process(ProcessingOperation, ProviderBoundary),
}

// Fail closed on pathological lineage instead of exhausting the server stack
// or repeatedly expanding a large diamond graph. These limits are per decision.
struct Traversal {
    path: BTreeSet<String>,
    remaining: usize,
}

impl Traversal {
    fn new() -> Self {
        Self {
            path: BTreeSet::new(),
            remaining: 4096,
        }
    }
}

fn check(
    authority: &impl PolicyAuthority,
    actor: &AuthenticatedAgent,
    resource: &str,
    action: &Action,
    traversal: &mut Traversal,
) -> Result<(), Denial> {
    if traversal.path.len() >= 128 || traversal.remaining == 0 {
        return Err(Denial::LineageLimit);
    }
    traversal.remaining -= 1;
    if !traversal.path.insert(resource.into()) {
        return Err(Denial::SourceCycle);
    }
    let policy = authority.policy(resource).ok_or(Denial::MissingPolicy)?;
    if resource.trim().is_empty()
        || policy.owner.trim().is_empty()
        || policy.creator.trim().is_empty()
    {
        return Err(Denial::InvalidPolicy);
    }
    let can_read = actor.id == policy.owner
        || (actor.id == policy.creator && policy.creator_read_grant)
        || !actor.roles.is_disjoint(&policy.read_roles);
    if !can_read {
        return Err(Denial::ReadForbidden);
    }
    if let Action::Process(operation, boundary) = action {
        match boundary {
            ProviderBoundary::Unknown => return Err(Denial::UnknownProvider),
            ProviderBoundary::External
                if policy.private || !policy.external_operations.contains(operation) =>
            {
                return Err(Denial::ExternalForbidden);
            }
            _ => {}
        }
    }
    for source in &policy.sources {
        check(authority, actor, source, action, traversal)?;
    }
    traversal.path.remove(resource);
    Ok(())
}

pub fn authorize_read(
    authority: &impl PolicyAuthority,
    actor: Option<&AuthenticatedAgent>,
    resource: &str,
) -> Result<(), Denial> {
    check(
        authority,
        actor.ok_or(Denial::Unauthenticated)?,
        resource,
        &Action::Read,
        &mut Traversal::new(),
    )
}

/// Gate the complete set of payload/context sources immediately before a
/// provider invocation. Re-run for every retry, fallback, embedding, dedupe,
/// STT and TTS call. A denial must end that attempt without external fallback.
pub fn authorize_processing(
    authority: &impl PolicyAuthority,
    actor: Option<&AuthenticatedAgent>,
    resources: &[String],
    operation: ProcessingOperation,
    boundary: ProviderBoundary,
) -> Result<(), Denial> {
    let actor = actor.ok_or(Denial::Unauthenticated)?;
    if resources.is_empty() {
        return Err(Denial::EmptyInput);
    }
    let mut traversal = Traversal::new();
    for resource in resources {
        check(
            authority,
            actor,
            resource,
            &Action::Process(operation, boundary),
            &mut traversal,
        )?;
    }
    Ok(())
}
