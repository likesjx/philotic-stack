//! Strict plan snapshot parsing, not OS administrator authentication or custody.
#[derive(Debug)]
pub enum PlanValidationError {
    Denied,
    Invalid,
    Unavailable,
}
type Result<T> = std::result::Result<T, PlanValidationError>;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use PlanValidationError as Error;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenancePlan {
    version: u8,
    action: String,
    operation_id: String,
    operator: String,
    hotel: String,
    database_id: String,
    endpoint: String,
    owner: String,
    expected_endpoint_sha256: String,
    generation_sha256: String,
    approval_reference: String,
    issued_at: u64,
    authorize_until: u64,
    rule_expires_at: u64,
}

/// Independently established expectations; none may be sourced from the plan.
#[derive(Clone, Copy)]
pub struct PlanExpectation<'a> {
    pub exact_sha256: &'a str,
    pub operator: &'a str,
    pub hotel: &'a str,
    pub database_id: &'a str,
    pub endpoint: &'a str,
    pub owner: &'a str,
    pub generation_sha256: &'a str,
}

/// Validated data and a digest of the exact supplied bytes. The public parser
/// does not establish protected origin, administrator identity, or custody.
pub struct LoadedMaintenancePlan {
    plan: MaintenancePlan,
    exact_sha256: String,
}

impl LoadedMaintenancePlan {
    pub fn operation_id(&self) -> &str {
        &self.plan.operation_id
    }
    pub fn endpoint(&self) -> &str {
        &self.plan.endpoint
    }
    pub fn owner(&self) -> &str {
        &self.plan.owner
    }
    pub fn expected_endpoint_sha256(&self) -> &str {
        &self.plan.expected_endpoint_sha256
    }
    pub fn approval_reference(&self) -> &str {
        &self.plan.approval_reference
    }
    pub fn authorize_until(&self) -> u64 {
        self.plan.authorize_until
    }
    pub fn rule_expires_at(&self) -> u64 {
        self.plan.rule_expires_at
    }
    pub fn exact_sha256(&self) -> &str {
        &self.exact_sha256
    }
}

fn valid_label(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('.')
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
fn valid_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
/// Validate a bounded byte snapshot against caller-supplied expectations and
/// clock. This parser is not an authorization capability: any caller can supply
/// its own bytes and expectations. A trusted caller must independently establish
/// OS administrator identity, protected origin, custody, and current state.
pub fn validate_maintenance_plan_snapshot(
    bytes: &[u8],
    expected: &PlanExpectation<'_>,
    now: u64,
) -> Result<LoadedMaintenancePlan> {
    if bytes.len() > 65_536 {
        return Err(Error::Invalid);
    }
    let digest = format!("{:x}", Sha256::digest(bytes));
    if !valid_hash(expected.exact_sha256) || digest != expected.exact_sha256 {
        return Err(Error::Denied);
    }
    let plan: MaintenancePlan = serde_json::from_slice(bytes).map_err(|_| Error::Invalid)?;
    let uuid = uuid::Uuid::parse_str(&plan.operation_id)
        .is_ok_and(|u| !u.is_nil() && u.to_string() == plan.operation_id);
    if plan.version != 1
        || plan.action != "extend_native_preapprovals"
        || !uuid
        || ![
            &plan.operator,
            &plan.hotel,
            &plan.database_id,
            &plan.endpoint,
            &plan.owner,
        ]
        .iter()
        .all(|s| valid_label(s))
        || !valid_hash(&plan.expected_endpoint_sha256)
        || !valid_hash(&plan.generation_sha256)
        || plan.approval_reference.is_empty()
        || plan.approval_reference.len() > 256
        || !plan
            .approval_reference
            .bytes()
            .all(|b| (0x21..=0x7e).contains(&b))
        || plan.operator != expected.operator
        || plan.hotel != expected.hotel
        || plan.database_id != expected.database_id
        || plan.endpoint != expected.endpoint
        || plan.owner != expected.owner
        || plan.generation_sha256 != expected.generation_sha256
        || plan.issued_at > now
        || plan.authorize_until <= plan.issued_at
        || plan.authorize_until <= now
        || plan.rule_expires_at <= now
        || plan.authorize_until > plan.issued_at.saturating_add(3600)
        || plan.rule_expires_at > now.saturating_add(86400)
    {
        return Err(Error::Invalid);
    }
    Ok(LoadedMaintenancePlan {
        plan,
        exact_sha256: digest,
    })
}

/// Read a point-in-time protected snapshot and validate the exact bytes. The
/// caller must independently establish trusted ownership, OS administrator
/// intent, ACL custody, writer isolation and the expected values. The uid and
/// gid inputs are policy comparisons, not proof of caller identity.
#[cfg(unix)]
pub fn read_maintenance_plan_with_owner(
    path: &std::path::Path,
    uid: u32,
    gid: u32,
    expected: &PlanExpectation<'_>,
) -> Result<LoadedMaintenancePlan> {
    let bytes =
        crate::protected_config::read_with_owner(path, uid, gid).map_err(|_| Error::Denied)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error::Unavailable)?
        .as_secs();
    validate_maintenance_plan_snapshot(&bytes, expected, now)
}
