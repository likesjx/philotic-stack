//! Native observation storage; no Muninn, provider or graph algorithm calls.
//! Signed authority is sensitive and requires protected transport. Default deployment: off.
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

#[cfg(unix)]
pub mod authority_control;
pub mod memgraph;
pub mod oauth;
pub mod response;

pub const ROLE: &str = "native-memory-runner";
pub const MAX_WIRE: usize = 65536;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    Denied,
    Invalid,
    Expired,
    Replay,
    Conflict,
    Unavailable,
    /// COMMIT was sent but its acknowledgement was not established. Retry the same event.
    Uncertain,
    Cancelled,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Remember {
        id: String,
        subject: String,
        content: String,
        source: String,
        source_version: String,
        lineage: String,
    },
    Recall {
        query: String,
        limit: u32,
    },
}
impl Command {
    pub fn operation(&self) -> &'static str {
        match self {
            Self::Remember { .. } => "remember",
            Self::Recall { .. } => "recall",
        }
    }
    fn valid(&self) -> bool {
        match self {
            Self::Remember {
                id,
                subject,
                content,
                source,
                source_version,
                lineage,
            } => {
                [id, subject, source, source_version, lineage]
                    .iter()
                    .all(|s| label(s))
                    && !content.trim().is_empty()
                    && content.len() <= 8192
            }
            Self::Recall { query, limit } => {
                !query.trim().is_empty() && query.len() <= 512 && (1..=50).contains(limit)
            }
        }
    }
}
fn label(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub event: String,
    pub owner: String,
    pub namespace: String,
    pub provider: Option<String>,
    pub command: Command,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claims {
    pub version: u32,
    pub issuer: String,
    pub key_id: String,
    pub caller: String,
    pub delegation: String,
    pub policy_revision: u64,
    pub destination: String,
    pub reply_node: String,
    pub reply_guest: String,
    pub runner: String,
    pub owner: String,
    pub namespace: String,
    pub operation: String,
    pub request_digest: [u8; 32],
    pub issued_at: u64,
    pub expires_at: u64,
    pub nonce: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub claims: Claims,
    pub request: Request,
    pub signature: Vec<u8>,
}
/// Hotel delivery metadata belongs outside the strict signed envelope. Unknown
/// outer transport fields are ignored and never participate in authorization.
#[derive(Serialize, Deserialize)]
pub struct RoutedRequest {
    pub action: String,
    pub signed_request: Envelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_handle: Option<String>,
}
impl RoutedRequest {
    pub fn new(signed_request: Envelope) -> Self {
        Self {
            action: "native_memory_request".into(),
            signed_request,
            authority_handle: None,
        }
    }
}
impl std::fmt::Debug for Envelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Envelope([REDACTED])")
    }
}
fn bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|_| Error::Invalid)
}
fn digest<T: Serialize>(value: &T) -> Result<[u8; 32]> {
    Ok(Sha256::digest(bytes(value)?).into())
}
fn signed_bytes(claims: &Claims) -> Result<Vec<u8>> {
    let mut b = b"philotic/native-memory/delegation/v1\0".to_vec();
    b.extend(bytes(claims)?);
    Ok(b)
}

/// Local policy, never accepted from routed task arguments. An observation grant
/// does not authorize canonical facts, goal/role/commitment modification or sharing.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub issuer: String,
    pub reply_node: String,
    pub reply_guest: String,
    pub principal: String,
    pub owner: String,
    pub namespace: String,
    pub destination: String,
    pub runner: String,
    pub operations: Vec<String>,
    pub revision: u64,
    pub expires_at: u64,
    pub revoked: bool,
}
impl Grant {
    fn permits(&self, r: &Request, now: u64) -> bool {
        !self.revoked
            && now < self.expires_at
            && self.principal == self.owner
            && r.owner == self.owner
            && r.namespace == self.namespace
            && r.provider.is_none()
            && self.operations.iter().any(|o| o == r.command.operation())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedIssuer {
    pub key_id: String,
    pub public_key: [u8; 32],
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub issuers: BTreeMap<String, TrustedIssuer>,
    pub grants: BTreeMap<String, Grant>,
}
impl Policy {
    pub fn load(path: &Path) -> Result<Self> {
        load_private_json(path)
    }
}

/// Ingress holds signing material locally. Caller IDs must be results of external
/// authentication, never fields lifted from tool arguments. No Debug/Serialize.
pub struct Ingress {
    issuer: String,
    key_id: String,
    key: SigningKey,
    grants: BTreeMap<String, Grant>,
}
impl Ingress {
    pub fn new(
        issuer: String,
        key_id: String,
        key: SigningKey,
        grants: BTreeMap<String, Grant>,
    ) -> Self {
        Self {
            issuer,
            key_id,
            key,
            grants,
        }
    }
    pub fn issue(
        &self,
        authenticated_token_id: &str,
        command: Command,
        event: String,
        nonce: String,
        now: u64,
    ) -> Result<Envelope> {
        let g = self
            .grants
            .get(authenticated_token_id)
            .ok_or(Error::Denied)?;
        let request = Request {
            event,
            owner: g.owner.clone(),
            namespace: g.namespace.clone(),
            provider: None,
            command,
        };
        if !request.command.valid() || !label(&request.event) || !label(&nonce) {
            return Err(Error::Invalid);
        }
        if !g.permits(&request, now) || g.issuer != self.issuer {
            return Err(Error::Denied);
        }
        let claims = Claims {
            version: 1,
            issuer: self.issuer.clone(),
            key_id: self.key_id.clone(),
            caller: g.principal.clone(),
            delegation: authenticated_token_id.into(),
            policy_revision: g.revision,
            destination: g.destination.clone(),
            reply_node: g.reply_node.clone(),
            reply_guest: g.reply_guest.clone(),
            runner: g.runner.clone(),
            owner: g.owner.clone(),
            namespace: g.namespace.clone(),
            operation: request.command.operation().into(),
            request_digest: digest(&request)?,
            issued_at: now,
            expires_at: now.saturating_add(60).min(g.expires_at),
            nonce,
        };
        let signature = self.key.sign(&signed_bytes(&claims)?).to_bytes().to_vec();
        Ok(Envelope {
            claims,
            request,
            signature,
        })
    }
}
/// Deliberately contains local signing key material; never route, log or persist
/// this object in task storage. Loading it is opt-in and requires rollout approval.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngressConfig {
    #[serde(default)]
    pub oauth: Option<oauth::ResourceConfig>,
    pub issuer: String,
    pub key_id: String,
    pub signing_seed: [u8; 32],
    pub grants: BTreeMap<String, Grant>,
    #[serde(default)]
    pub response_verifiers: Vec<response::ResponseVerifier>,
}
impl IngressConfig {
    /// Opt-in local signing config. Reject group/world access and symlinks on
    /// Unix instead of relying on operators to remember a private umask.
    pub fn load(path: &Path) -> Result<Self> {
        let config: Self = load_private_json(path)?;
        if let Some(oauth) = &config.oauth {
            oauth.validate()?;
        }
        Ok(config)
    }
    pub fn into_ingress(self) -> Ingress {
        Ingress::new(
            self.issuer,
            self.key_id,
            SigningKey::from_bytes(&self.signing_seed),
            self.grants,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub id: String,
    pub owner: String,
    pub subject: String,
    pub authenticated_writer: String,
    pub content: String,
    pub source: String,
    pub source_version: String,
    pub lineage: String,
    pub kind: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub event: String,
    pub status: String,
    pub operation: String,
    pub committed_at: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub receipt: Receipt,
    pub observations: Vec<Observation>,
}

/// Local SQLite milestone, not the final Memgraph/LifeGraph adapter. FULL sync,
/// atomic operation + receipt + replay record; no success returned before commit.
pub struct Runner {
    db: Connection,
    destination: String,
    runner: String,
}
impl Runner {
    pub fn open(path: &Path, destination: String, runner: String) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
            {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(Error::Unavailable),
            }
            let metadata = std::fs::symlink_metadata(path).map_err(|_| Error::Unavailable)?;
            if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o077 != 0 {
                return Err(Error::Denied);
            }
        }
        let db = Connection::open(path).map_err(|_| Error::Unavailable)?;
        db.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(|_| Error::Unavailable)?;
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
          CREATE TABLE IF NOT EXISTS observations(owner TEXT, namespace TEXT, id TEXT, body TEXT NOT NULL, PRIMARY KEY(owner,namespace,id));
          CREATE TABLE IF NOT EXISTS receipts(owner TEXT, namespace TEXT, event TEXT, digest BLOB NOT NULL, receipt TEXT NOT NULL, PRIMARY KEY(owner,namespace,event));
          CREATE TABLE IF NOT EXISTS nonces(issuer TEXT, nonce TEXT, digest BLOB NOT NULL, PRIMARY KEY(issuer,nonce));").map_err(|_|Error::Unavailable)?;
        Ok(Self {
            db,
            destination,
            runner,
        })
    }
    pub fn execute_wire(
        &mut self,
        wire: &[u8],
        policy: &Policy,
        now: u64,
        cancelled: &AtomicBool,
    ) -> Result<Response> {
        if wire.len() > MAX_WIRE {
            return Err(Error::Invalid);
        }
        let envelope: Envelope = serde_json::from_slice(wire).map_err(|_| Error::Invalid)?;
        self.execute(&envelope, policy, now, cancelled)
    }
    pub fn execute(
        &mut self,
        e: &Envelope,
        policy: &Policy,
        now: u64,
        cancelled: &AtomicBool,
    ) -> Result<Response> {
        self.execute_current(e, || Ok((policy.clone(), now)), cancelled)
    }
    /// Runtime reloads local policy and wall clock before execution and commit.
    pub fn execute_current(
        &mut self,
        e: &Envelope,
        mut current: impl FnMut() -> Result<(Policy, u64)>,
        cancelled: &AtomicBool,
    ) -> Result<Response> {
        if cancelled.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        let (policy, now) = current()?;
        verify(e, &policy, &self.destination, &self.runner, now)?;
        let r = &e.request;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| Error::Unavailable)?;
        let operation_digest = digest(&(e.claims.caller.as_str(), r))?;
        let nonce_digest = digest(&e.claims)?;
        let prior_nonce: Option<Vec<u8>> = tx
            .query_row(
                "SELECT digest FROM nonces WHERE issuer=?1 AND nonce=?2",
                params![e.claims.issuer, e.claims.nonce],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| Error::Unavailable)?;
        if prior_nonce.as_deref().is_some_and(|v| v != nonce_digest) {
            return Err(Error::Replay);
        }
        let prior: Option<(Vec<u8>, String)> = tx
            .query_row(
                "SELECT digest,receipt FROM receipts WHERE owner=?1 AND namespace=?2 AND event=?3",
                params![r.owner, r.namespace, r.event],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|_| Error::Unavailable)?;
        let receipt = if let Some((old, receipt)) = prior {
            if old != operation_digest {
                return Err(Error::Conflict);
            }
            serde_json::from_str(&receipt).map_err(|_| Error::Unavailable)?
        } else {
            if let Command::Remember {
                id,
                subject,
                content,
                source,
                source_version,
                lineage,
            } = &r.command
            {
                let observation = Observation {
                    id: id.clone(),
                    owner: r.owner.clone(),
                    subject: subject.clone(),
                    authenticated_writer: e.claims.caller.clone(),
                    content: content.clone(),
                    source: source.clone(),
                    source_version: source_version.clone(),
                    lineage: lineage.clone(),
                    kind: "observation".into(),
                };
                let body = String::from_utf8(bytes(&observation)?).map_err(|_| Error::Invalid)?;
                let existing: Option<String> = tx
                    .query_row(
                        "SELECT body FROM observations WHERE owner=?1 AND namespace=?2 AND id=?3",
                        params![r.owner, r.namespace, id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(|_| Error::Unavailable)?;
                if existing.is_some() {
                    return Err(Error::Conflict);
                }
                tx.execute(
                    "INSERT INTO observations VALUES(?1,?2,?3,?4)",
                    params![r.owner, r.namespace, id, body],
                )
                .map_err(|_| Error::Unavailable)?;
            }
            let receipt = Receipt {
                event: r.event.clone(),
                status: "committed".into(),
                operation: r.command.operation().into(),
                committed_at: now,
            };
            tx.execute(
                "INSERT INTO receipts VALUES(?1,?2,?3,?4,?5)",
                params![
                    r.owner,
                    r.namespace,
                    r.event,
                    operation_digest.as_slice(),
                    serde_json::to_string(&receipt).map_err(|_| Error::Invalid)?
                ],
            )
            .map_err(|_| Error::Unavailable)?;
            receipt
        };
        tx.execute(
            "INSERT OR IGNORE INTO nonces VALUES(?1,?2,?3)",
            params![e.claims.issuer, e.claims.nonce, nonce_digest.as_slice()],
        )
        .map_err(|_| Error::Unavailable)?;
        let mut observations = Vec::new();
        if let Command::Recall { query, limit } = &r.command {
            let mut stmt=tx.prepare("SELECT body FROM observations WHERE owner=?1 AND namespace=?2 AND instr(lower(json_extract(body,'$.content')),lower(?3))>0 ORDER BY id LIMIT ?4").map_err(|_|Error::Unavailable)?;
            let rows = stmt
                .query_map(params![r.owner, r.namespace, query, limit], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|_| Error::Unavailable)?;
            for row in rows {
                observations.push(
                    serde_json::from_str(&row.map_err(|_| Error::Unavailable)?)
                        .map_err(|_| Error::Unavailable)?,
                );
            }
        }
        if cancelled.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        let (policy, now) = current()?;
        verify(e, &policy, &self.destination, &self.runner, now)?;
        if cancelled.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        tx.commit().map_err(|_| Error::Unavailable)?;
        Ok(Response {
            receipt,
            observations,
        })
    }
}
/// Constructed only after signature, local receiver policy and request checks.
/// Never deserialize this from a routed task or caller JSON.
pub struct VerifiedRequest {
    envelope: Envelope,
    binding: [u8; 32],
}
impl VerifiedRequest {
    pub fn validate(
        e: &Envelope,
        policy: &Policy,
        destination: &str,
        runner: &str,
        now: u64,
    ) -> Result<Self> {
        verify(e, policy, destination, runner, now)?;
        Ok(Self {
            envelope: e.clone(),
            binding: digest(e)?,
        })
    }
    pub fn envelope(&self) -> &Envelope {
        &self.envelope
    }
    pub fn binding(&self) -> [u8; 32] {
        self.binding
    }
}
fn verify(e: &Envelope, p: &Policy, destination: &str, runner: &str, now: u64) -> Result<()> {
    let c = &e.claims;
    let r = &e.request;
    if !label(&r.event) || !label(&c.nonce) || !r.command.valid() {
        return Err(Error::Invalid);
    }
    let issuer = p.issuers.get(&c.issuer).ok_or(Error::Denied)?;
    if issuer.key_id != c.key_id {
        return Err(Error::Denied);
    }
    let key = VerifyingKey::from_bytes(&issuer.public_key).map_err(|_| Error::Denied)?;
    let signature = Signature::from_slice(&e.signature).map_err(|_| Error::Denied)?;
    key.verify_strict(&signed_bytes(c)?, &signature)
        .map_err(|_| Error::Denied)?;
    if c.issued_at > now
        || c.expires_at <= now
        || c.expires_at <= c.issued_at
        || c.expires_at - c.issued_at > 60
    {
        return Err(Error::Expired);
    }
    let g = p.grants.get(&c.delegation).ok_or(Error::Denied)?;
    if c.version != 1
        || c.issuer != g.issuer
        || c.reply_node != g.reply_node
        || c.reply_guest != g.reply_guest
        || c.destination != destination
        || c.runner != runner
        || c.destination != g.destination
        || c.runner != g.runner
        || c.caller != g.principal
        || c.policy_revision != g.revision
        || c.owner != r.owner
        || c.namespace != r.namespace
        || c.operation != r.command.operation()
        || c.request_digest != digest(r)?
        || !g.permits(r, now)
    {
        return Err(Error::Denied);
    }
    Ok(())
}

/// Read local secret-bearing config without following a symlink.
pub(crate) fn load_private_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|_| Error::Denied)?;
    let metadata = file.metadata().map_err(|_| Error::Unavailable)?;
    if !metadata.file_type().is_file() || metadata.len() > 1_048_576 {
        return Err(Error::Denied);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::Denied);
        }
    }
    let mut bytes = Vec::new();
    file.take(1_048_577)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Unavailable)?;
    if bytes.len() > 1_048_576 {
        return Err(Error::Denied);
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)
}
